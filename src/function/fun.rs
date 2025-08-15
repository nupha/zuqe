use std::ops::Deref;

use crate::{
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION,
    atom::{JSAtom, consts::*},
    class::JSClassID,
    context::JSContext,
    error::{JS_ThrowTypeErrorRevokedProxy, JSResult},
    function::JSVarRef,
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_NORMAL, JS_PROP_TMASK, JSObject, JSProperty, ObjectDetail,
    },
    runtime::JSRuntime,
    value::{JSValue, JSValueConst},
};

use super::{Bytecode, CallContext};

/// Special index for argument scope.
/// This is same to `JSFunctionDef::body_scope`, which was automatically pushed
/// when creating root function def in `eval_internal()`.
pub const ARG_SCOPE_INDEX: i32 = 1;

/// to mark end of arugment vars chain.
pub const ARG_SCOPE_END: i32 = -2;

/// kind of function
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSFunctionKindEnum {
    /// bytecode function
    JS_FUNC_NORMAL = 0,
    /// generator
    JS_FUNC_GENERATOR = 1,
    /// async function
    JS_FUNC_ASYNC = 2,
    /// async generator. JS_FUNC_GENERATOR | JS_FUNC_ASYNC
    JS_FUNC_ASYNC_GENERATOR = 3,
}

impl JSFunctionKindEnum {
    #[inline(always)]
    pub fn is_async(self) -> bool {
        matches!(self, Self::JS_FUNC_ASYNC | Self::JS_FUNC_ASYNC_GENERATOR)
    }

    /// get associated class id for function kind
    pub fn class_id(&self) -> JSClassID {
        match self {
            Self::JS_FUNC_NORMAL => JS_CLASS_BYTECODE_FUNCTION,
            Self::JS_FUNC_GENERATOR => JS_CLASS_GENERATOR,
            Self::JS_FUNC_ASYNC => JS_CLASS_ASYNC_FUNCTION,
            Self::JS_FUNC_ASYNC_GENERATOR => JS_CLASS_ASYNC_GENERATOR,
        }
    }
}

/// Class id for function kind
#[deprecated(note = "use JSFunctionKindEnum::class_id() instead")]
pub const func_kind_to_class_id: [JSClassID; 4] = [
    JS_CLASS_BYTECODE_FUNCTION, // 0 JS_FUNC_NORMAL
    JS_CLASS_GENERATOR,         // 1 JS_FUNC_GENERATOR
    JS_CLASS_ASYNC_FUNCTION,    // 2 JS_FUNC_ASYNC
    JS_CLASS_ASYNC_GENERATOR,   // 3 JS_FUNC_ASYNC_GENERATOR
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FuncCallType {
    FUNC_CALL_NORMAL,
    FUNC_CALL_NEW,
    FUNC_CALL_SUPER_CTOR,
    FUNC_CALL_TEMPLATE,
}

#[derive(Debug, Default)]
pub struct ObjFunc {
    pub bytecode: Option<Bytecode>,

    /// closure variable refrences
    pub var_refs: Vec<JSVarRef>,

    /// 函数对象关联的宿主对象，主要用于以下场景：
    /// - 私有字段的访问控制：当类方法需要访问私有字段时，需要通过home object定位到对应的私有品牌（private brand）进行权限验证。
    /// - for 'super' access
    pub home_object: Option<JSObject>,
}

impl ObjFunc {
    /// get function bytecode
    #[inline]
    pub fn bytecode(&self) -> Option<Bytecode> {
        self.bytecode.as_ref().cloned()
    }

    /// get function bytecode realm
    pub fn realm(&self) -> Option<JSContext> {
        self.bytecode
            .as_ref()
            .and_then(|b| b.0.borrow().realm.upgrade())
    }
}

impl JSValue {
    /// check if value `self` is a function or a callable object.
    /// raise exception if not.
    pub fn ensure_callable(&self, ctx: &JSContext) -> Result<&Self, i32> {
        if self.is_callable() {
            Ok(self)
        } else {
            ctx.throw_type_error("not a function");
            Err(-1)
        }
    }
}

pub(crate) const fn js_class_has_bytecode(class_id: JSClassID) -> bool {
    matches!(
        class_id,
        JS_CLASS_BYTECODE_FUNCTION
            | JS_CLASS_GENERATOR_FUNCTION
            | JS_CLASS_ASYNC_FUNCTION
            | JS_CLASS_ASYNC_GENERATOR_FUNCTION
    )
}

pub fn JS_GetFunctionRealm(ctx: &JSContext, func_obj: &JSValueConst) -> Result<JSContext, i32> {
    if !func_obj.is_object() {
        return Ok(ctx.clone());
    }

    let realm: JSContext;
    let p = unsafe { func_obj.object_unchecked() };

    match p.0.payload.borrow().deref() {
        ObjectDetail::C_FUNCTION(cfunc) => {
            realm = cfunc.realm.upgrade().unwrap_or(ctx.clone());
        }
        ObjectDetail::BYTECODE_FUNCTION(fo)
        | ObjectDetail::GENERATOR_FUNCTION(fo)
        | ObjectDetail::ASYNC_FUNCTION(fo)
        | ObjectDetail::ASYNC_GENERATOR_FUNCTION(fo) => {
            realm = fo
                .bytecode
                .as_ref()
                .unwrap()
                .0
                .borrow()
                .realm
                .upgrade()
                .unwrap_or(ctx.clone());
        }
        ObjectDetail::PROXY(s) => {
            if s.is_revoked {
                JS_ThrowTypeErrorRevokedProxy(ctx);
                return Err(-1);
            } else {
                realm = JS_GetFunctionRealm(ctx, &s.target)?;
            }
        }
        ObjectDetail::BOUND_FUNCTION(bfunc) => {
            realm = JS_GetFunctionRealm(ctx, &bfunc.func_obj)?;
        }
        _ => {
            realm = ctx.clone();
        }
    }

    Ok(realm)
}

pub fn get_func_name(func: &JSValue) -> Option<String> {
    // in order to avoid executing arbitrary code during the stack trace generation,
    // we only look at simple 'name' properties containing a string.
    if let Some(obj) = func.object() {
        if let Some((idx, prs_flags)) = obj.find_shape_property(JS_ATOM_name) {
            if prs_flags & JS_PROP_TMASK == JS_PROP_NORMAL {
                if let Some(JSProperty::Normal(JSValue::String(s))) =
                    obj.0.prop_values.borrow().get(idx)
                {
                    return Some(s.content().to_utf8().into_owned());
                } else {
                    unreachable!()
                }
            }
        }

        None
    } else {
        None
    }
}

/// define `length`, `name` properties on func_obj
pub unsafe fn js_function_set_properties(
    ctx: &JSContext,
    func_obj: &JSValue,
    name: JSAtom,
    len: i32,
) {
    func_obj
        .define_property_value(
            ctx,
            JS_ATOM_length,
            JSValue::Int(len),
            JS_PROP_CONFIGURABLE, // ES6 feature non-compatible with ES5.1: length is configurable
        )
        .unwrap();

    func_obj
        .define_property_value(
            ctx,
            JS_ATOM_name,
            name.to_string(ctx)
                .map_or(JSValue::EXCEPTION, JSValue::String),
            JS_PROP_CONFIGURABLE,
        )
        .unwrap();
}

pub fn js_function_proto(_: &JSContext, _: CallContext) -> JSResult {
    Ok(JSValue::UNDEFINED)
}

pub fn js_bytecode_function_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {}

pub fn js_bytecode_function_mark(val: JSValue) {
    let p = unsafe { val.object_unchecked() };

    let (bf, home_obj) =
        p.0.payload
            .borrow()
            .func()
            .map(|f| {
                for vr in f.var_refs.iter() {
                    vr.gc_mark();
                }
                (f.bytecode(), f.home_object.clone())
            })
            .unwrap();

    if let Some(ho) = home_obj {
        ho.gc_mark();
    }
    if let Some(b) = bf {
        b.gc_mark();
    }
}
