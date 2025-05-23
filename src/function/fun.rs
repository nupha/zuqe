use std::{borrow::Cow, ptr::NonNull};

use crate::{
    atom::{consts::*, JSAtom},
    class::JSClassID,
    context::JSContext,
    error::{JSResult, JS_ThrowTypeErrorRevokedProxy},
    function::JSVarRef,
    gc::{JS_MarkFunc, JS_MarkValue},
    proxy::JSProxyData,
    runtime::JSRuntime,
    value::{
        object::{JSObject, JSObjectIsa, JS_PROP_CONFIGURABLE, JS_PROP_NORMAL, JS_PROP_TMASK},
        JSTag, JSValue, JSValueConst, JS_FreeValueRT, JS_TAG_OBJECT, JS_UNDEFINED,
        JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BOUND_FUNCTION, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_C_FUNCTION, JS_CLASS_GENERATOR,
    JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_PROXY, JS_MKPTR,
};

use super::{CallArgs, JSFunctionBytecode};

/// Special index for argument scope.
/// This is same to `JSFunctionDef::body_scope`, which was automatically pushed
/// when creating root function def in `eval_internal()`.
pub const ARG_SCOPE_INDEX: i32 = 1;

/// to mark end of arugment vars chain.
pub const ARG_SCOPE_END: i32 = -2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSFunctionKindEnum {
    JS_FUNC_NORMAL = 0,
    JS_FUNC_GENERATOR = 1,
    JS_FUNC_ASYNC = 2,
    JS_FUNC_ASYNC_GENERATOR = 3, // JS_FUNC_GENERATOR | JS_FUNC_ASYNC
}

impl JSFunctionKindEnum {
    #[inline(always)]
    pub fn is_async(self) -> bool {
        matches!(self, Self::JS_FUNC_ASYNC | Self::JS_FUNC_ASYNC_GENERATOR)
    }
}

/// Class id for function kind
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

#[derive(Debug)]
pub struct JSObjectFunc {
    pub function_bytecode: *mut JSFunctionBytecode,

    /// closure variable refrences
    pub var_refs: Vec<NonNull<JSVarRef>>,

    /// 函数对象关联的宿主对象，主要用于以下场景：
    /// - 私有字段的访问控制：当类方法需要访问私有字段时，需要通过home object定位到对应的私有品牌（private brand）进行权限验证。
    /// - for 'super' access
    pub home_object: *mut JSObject,
}

impl Default for JSObjectFunc {
    fn default() -> Self {
        Self {
            function_bytecode: std::ptr::null_mut(),
            var_refs: Vec::new(),
            home_object: std::ptr::null_mut(),
        }
    }
}

impl JSValue {
    /// check if value `self` is a function or a callable object.
    /// raise exception if not.
    pub fn ensure_callable(self, ctx: &mut JSContext) -> Result<Self, i32> {
        if self.is_callable(ctx) {
            Ok(self)
        } else {
            ctx.JS_ThrowTypeError(c"not a function");
            Err(-1)
        }
    }
}

/* return NULL without exception if not a function or no bytecode */
#[deprecated(note = "use JSObjectIsa::func() to get JSFunctionBytecode")]
pub fn JS_GetFunctionBytecode(val: JSValueConst) -> *mut JSFunctionBytecode {
    val.object()
        .and_then(|o| o.isa().func())
        .map(|f| f.function_bytecode)
        .unwrap_or(std::ptr::null_mut())

    // if JS_VALUE_GET_TAG(val) != JS_TAG_OBJECT {
    //     return std::ptr::null_mut();
    // }

    // let p = val.object_unchecked();
    // if !js_class_has_bytecode(p.class_id()) {
    //     return std::ptr::null_mut();
    // }
    // p.u.func.function_bytecode
}

pub const fn js_class_has_bytecode(class_id: JSClassID) -> bool {
    matches!(
        class_id,
        JS_CLASS_BYTECODE_FUNCTION
            | JS_CLASS_GENERATOR_FUNCTION
            | JS_CLASS_ASYNC_FUNCTION
            | JS_CLASS_ASYNC_GENERATOR_FUNCTION
    )
}

// warning: the refcount of the context is not incremented. Return
// NULL in case of exception (case of revoked proxy only)
pub unsafe fn JS_GetFunctionRealm(ctx: *mut JSContext, func_obj: JSValueConst) -> *mut JSContext {
    if JS_VALUE_GET_TAG(func_obj) != JS_TAG_OBJECT {
        return ctx;
    }

    let realm: *mut JSContext;

    let p = func_obj.object_unchecked();

    match p.isa() {
        JSObjectIsa::C_FUNCTION(cfunc) => {
            realm = cfunc.realm;
        }
        JSObjectIsa::BYTECODE_FUNCTION(func)
        | JSObjectIsa::GENERATOR_FUNCTION(func)
        | JSObjectIsa::ASYNC_FUNCTION(func)
        | JSObjectIsa::ASYNC_GENERATOR_FUNCTION(func) => {
            realm = (*func.function_bytecode).realm;
        }
        JSObjectIsa::PROXY(proxy) => {
            let s = *proxy;
            if s.is_null() {
                return ctx;
            }
            if (*s).is_revoked {
                JS_ThrowTypeErrorRevokedProxy(ctx);
                return std::ptr::null_mut();
            } else {
                realm = JS_GetFunctionRealm(ctx, (*s).target);
            }
        }
        JSObjectIsa::BOUND_FUNCTION(bfunc) => {
            realm = JS_GetFunctionRealm(ctx, (*(*bfunc)).func_obj);
        }
        _ => {
            realm = ctx;
        }
    }

    return realm;
}

pub unsafe fn get_func_name<'a>(ctx: *mut JSContext, func: JSValueConst) -> Option<Cow<'a, str>> {
    // in order to avoid executing arbitrary code during the stack trace generation,
    // we only look at simple 'name' properties containing a string.
    if JS_VALUE_GET_TAG(func) == JSTag::Object as i32 {
        if let Some((prs, pr)) = (*JS_VALUE_GET_OBJ(func)).find_own_property(JS_ATOM_name) {
            if prs.flags() & JS_PROP_TMASK == JS_PROP_NORMAL {
                let val = pr.u.value;
                if JS_VALUE_GET_TAG(val) == JSTag::String as i32 {
                    return val.coerce_to_utf8(ctx.into()).ok();
                }
            }
        }
    }

    None
}

/// define `length`, `name` properties on func_obj
pub unsafe fn js_function_set_properties(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    name: JSAtom,
    len: i32,
) {
    let _ = func_obj.define_property_value(
        ctx.as_mut().unwrap(),
        JS_ATOM_length,
        JSValue::from_i32(len),
        JS_PROP_CONFIGURABLE, // ES6 feature non compatible with ES5.1: length is configurable
    );

    let _ = func_obj.define_property_value(
        ctx.as_mut().unwrap(),
        JS_ATOM_name,
        name.to_value(ctx.as_mut().unwrap(), true),
        JS_PROP_CONFIGURABLE,
    );
}

pub unsafe fn js_function_proto_v0(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    JS_UNDEFINED
}

pub fn js_function_proto(_: &mut JSContext, _: CallArgs) -> JSResult {
    Ok(JSValue::UNDEFINED)
}

#[deprecated(note = "use JSValue::ensure_callable() instead")]
pub unsafe fn check_function(ctx: *mut JSContext, obj: JSValueConst) -> Result<(), i32> {
    obj.ensure_callable(ctx.as_mut().unwrap())?;
    Ok(())
}

pub unsafe fn js_bytecode_function_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let p = JS_VALUE_GET_OBJ(val);
    let func = (*p).isa_mut().func_mut().unwrap();
    let p1 = func.home_object;
    if !p1.is_null() {
        JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, p1.cast()));
    }

    let b = func.function_bytecode;
    if !b.is_null() {
        for vr_ptr in func.var_refs.drain(..) {
            if let Some(vr) = Box::from_raw(vr_ptr.as_ptr()).free(rt) {
                // var ref is still alive
                Box::leak(vr);
            }
        }

        JS_FreeValueRT(rt, JS_MKPTR!(JSTag::FunctionBytecode, b.cast()));
    }
}

pub unsafe fn js_bytecode_function_mark(
    rt: *mut JSRuntime,
    val: JSValueConst,
    mark_func: JS_MarkFunc,
) {
    let p = JS_VALUE_GET_OBJ(val);
    // let var_refs = (*p).u.func.var_refs;
    let func = (*p).isa().func().unwrap();
    let b = func.function_bytecode;

    if !func.home_object.is_null() {
        JS_MarkValue(
            rt,
            JS_MKPTR!(JSTag::Object, func.home_object.cast()),
            mark_func,
        );
    }

    if !b.is_null() {
        for vr in func.var_refs.iter() {
            mark_func(rt, &mut (*vr.as_ptr()).u0.header);
        }

        // must mark the function bytecode because template objects may be part of a cycle
        JS_MarkValue(rt, JS_MKPTR!(JSTag::FunctionBytecode, b.cast()), mark_func);
    }
}
