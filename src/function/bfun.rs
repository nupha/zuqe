use std::{cell::RefCell, ops::Deref};

use smallvec::SmallVec;

use crate::{
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION,
    atom::{
        Atom,
        consts::{JS_ATOM_length, JS_ATOM_name, JS_ATOM_prototype},
    },
    class::JSClassID,
    context::JSContext,
    error::JSResult,
    eval::JS_EVAL_TYPE_INDIRECT,
    function::{Bytecode, CallContext, JS_CALL_FLAG_COPY_ARGV, JSVarRef},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_NORMAL, JS_PROP_TMASK, JSObject, JSProperty, ObjectPayload,
    },
    object_cast,
    runtime::{JSRuntime, js_check_stack_overflow},
    stack_frame::StackFrame,
    value::JSValue,
};

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
    pub const fn is_async(self) -> bool {
        matches!(self, Self::JS_FUNC_ASYNC | Self::JS_FUNC_ASYNC_GENERATOR)
    }

    #[inline(always)]
    pub const fn is_generator(self) -> bool {
        matches!(
            self,
            Self::JS_FUNC_GENERATOR | Self::JS_FUNC_ASYNC_GENERATOR
        )
    }

    /// get associated class id for function kind
    #[inline]
    pub const fn class_id(&self) -> JSClassID {
        match self {
            Self::JS_FUNC_NORMAL => JS_CLASS_BYTECODE_FUNCTION,
            Self::JS_FUNC_GENERATOR => JS_CLASS_GENERATOR_FUNCTION,
            Self::JS_FUNC_ASYNC => JS_CLASS_ASYNC_FUNCTION,
            Self::JS_FUNC_ASYNC_GENERATOR => JS_CLASS_ASYNC_GENERATOR_FUNCTION,
        }
    }

    #[inline]
    pub const fn generator_class_id(&self) -> Option<JSClassID> {
        match self {
            Self::JS_FUNC_GENERATOR => Some(JS_CLASS_GENERATOR),
            Self::JS_FUNC_ASYNC_GENERATOR => Some(JS_CLASS_ASYNC_GENERATOR),
            Self::JS_FUNC_NORMAL | Self::JS_FUNC_ASYNC => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FuncCallType {
    FUNC_CALL_NORMAL,
    FUNC_CALL_NEW,
    FUNC_CALL_SUPER_CTOR,
    FUNC_CALL_TEMPLATE,
}

/// Object payload for:
/// * bytecode function,
/// * async function,
/// * generator function,
/// * async generator function
#[derive(Debug)]
pub struct BCFuncPayload {
    bytecode: Bytecode,

    /// closure variable refrences
    pub(crate) var_refs: RefCell<Vec<JSVarRef>>,

    /// 函数对象关联的宿主对象，主要用于以下场景：
    /// - 私有字段的访问控制：当类方法需要访问私有字段时，需要通过home object定位到对应的私有品牌（private brand）进行权限验证。
    /// - for 'super' access
    home_object: RefCell<Option<JSObject>>,
}

impl BCFuncPayload {
    pub fn new(bytecode: Bytecode, var_refs: Vec<JSVarRef>) -> Box<Self> {
        Box::new(BCFuncPayload {
            bytecode,
            var_refs: RefCell::new(var_refs),
            home_object: RefCell::new(None),
        })
    }

    /// get function bytecode
    #[inline(always)]
    pub const fn bytecode(&self) -> &Bytecode {
        &self.bytecode
    }

    /// get function bytecode realm
    #[inline(always)]
    pub fn realm(&self) -> Option<JSContext> {
        self.bytecode.realm()
    }

    #[inline(always)]
    pub fn home_object(&self) -> Option<JSObject> {
        self.home_object.borrow().clone()
    }

    pub fn set_home_object(&self, obj: Option<JSObject>) {
        *self.home_object.borrow_mut() = obj;
    }
}

/// represents a bytecode function, generator function, async function or async generator function,
/// which have bytecode.
#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct BCFunction(pub(super) JSObject);

object_cast!(
    BCFunction,
    JS_CLASS_BYTECODE_FUNCTION
        | JS_CLASS_GENERATOR_FUNCTION
        | JS_CLASS_ASYNC_FUNCTION
        | JS_CLASS_ASYNC_GENERATOR_FUNCTION
);

impl BCFunction {
    #[inline(always)]
    pub fn is_async(&self) -> bool {
        matches!(
            self.class_id(),
            JS_CLASS_ASYNC_FUNCTION | JS_CLASS_ASYNC_GENERATOR_FUNCTION
        )
    }

    #[inline(always)]
    pub fn is_generator(&self) -> bool {
        matches!(
            self.class_id(),
            JS_CLASS_GENERATOR_FUNCTION | JS_CLASS_ASYNC_GENERATOR_FUNCTION
        )
    }

    /// get bcfunction payload
    #[inline]
    pub fn payload(&self) -> &BCFuncPayload {
        self.0.payload().bc_function().unwrap()
    }
}

impl JSObject {
    /// check if `self` is a function or a callable object.
    /// raise exception if not.
    pub fn ensure_function(&self, ctx: &JSContext) -> Result<&Self, i32> {
        if self.is_function() {
            Ok(self)
        } else {
            ctx.throw_type_error("not a function");
            Err(-1)
        }
    }
}

impl JSValue {
    /// check if value `self` is a function or a callable object.
    /// raise exception if not.
    pub fn ensure_function(&self, ctx: &JSContext) -> Result<&Self, i32> {
        if self.is_function() {
            Ok(self)
        } else {
            ctx.throw_type_error("not a function");
            Err(-1)
        }
    }
}

/// TODO: change func_obj type to JSObject
pub fn JS_GetFunctionRealm(ctx: &JSContext, func_obj: &JSValue) -> Result<JSContext, i32> {
    if !func_obj.is_object() {
        return Ok(ctx.clone());
    }

    let realm: JSContext;
    let p = unsafe { func_obj.as_object_unchecked() };

    match p.payload() {
        ObjectPayload::C_FUNCTION(cfunc) => {
            realm = cfunc.realm.upgrade().unwrap_or(ctx.clone());
        }
        ObjectPayload::BYTECODE_FUNCTION(fo)
        | ObjectPayload::GENERATOR_FUNCTION(fo)
        | ObjectPayload::ASYNC_FUNCTION(fo)
        | ObjectPayload::ASYNC_GENERATOR_FUNCTION(fo) => {
            realm = fo.bytecode().realm().unwrap_or(ctx.clone());
        }
        ObjectPayload::PROXY(px) => {
            if px.is_revoked() {
                ctx.throw_type_error_revoked_proxy();
                return Err(-1);
            } else {
                realm = JS_GetFunctionRealm(ctx, &px.target().clone().into())?;
            }
        }
        ObjectPayload::BOUND_FUNCTION(bfunc) => {
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
    if let Some(obj) = func.as_object() {
        if let Some((idx, prs_flags)) = obj.find_shape_property(&JS_ATOM_name)
            && prs_flags & JS_PROP_TMASK == JS_PROP_NORMAL
        {
            if let Some(JSProperty::Normal(JSValue::String(s))) =
                obj.0.prop_values.borrow().get(idx)
            {
                return Some(s.content().to_utf8(false).into_owned());
            } else {
                #[cfg(debug_assertions)]
                unreachable!();
                #[cfg(not(debug_assertions))]
                unsafe {
                    std::hint::unreachable_unchecked();
                }
            }
        }

        None
    } else {
        None
    }
}

/// define `length` and `name` properties on function object `func_obj`
pub fn js_function_set_properties(ctx: &JSContext, func_obj: &JSObject, name: Atom, len: i32) {
    let _ = func_obj.define_property_value(
        ctx,
        &JS_ATOM_length,
        JSValue::Int(len),
        JS_PROP_CONFIGURABLE, // ES6 feature non-compatible with ES5.1: length is configurable
    );
    let _ = func_obj.define_property_value(
        ctx,
        &JS_ATOM_name,
        name.force_string(ctx)
            .map_or(JSValue::EXCEPTION, JSValue::String),
        JS_PROP_CONFIGURABLE,
    );
}

pub fn js_function_proto(_: &JSContext, _: CallContext) -> JSResult {
    Ok(JSValue::UNDEFINED)
}

pub(crate) const fn js_bytecode_function_finalizer(
    _rt: &mut JSRuntime,
    _isa: &ObjectPayload,
    _id: usize,
) {
}

pub(crate) fn js_bytecode_function_mark(val: JSValue) {
    let isa = val.as_object_BCFunction().unwrap().payload();

    isa.bytecode().gc_mark();
    for vr in isa.var_refs.borrow().iter() {
        vr.gc_mark();
    }
    if let Some(ho) = isa.home_object.borrow().as_ref() {
        ho.gc_mark();
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct BytecodeFunction(JSObject);

object_cast!(BytecodeFunction, JS_CLASS_BYTECODE_FUNCTION);

impl BytecodeFunction {
    #[inline]
    pub fn payload(&self) -> &BCFuncPayload {
        self.0.payload().bc_function().unwrap()
    }

    /// call bytecode function
    pub fn call(
        &self,
        caller_ctx: &JSContext,
        this_obj: &JSValue,
        new_target: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSValue {
        let isa = self.payload();
        let bc = isa.bytecode();

        // how many args that should be copied to local buf?
        // `args.len()` is actual argument count
        // `b.arg_count` is the length when defining the function
        let arg_copy_num =
            if flags & JS_CALL_FLAG_COPY_ARGV != 0 || args.len() < bc.arg_count as usize {
                std::cmp::max(args.len(), bc.arg_count as usize)
            } else {
                0
            };

        // how many bytes to allocate for local buf (args+vars+stack)?
        let (buf_values_count, js_mode) = (
            arg_copy_num as usize + bc.var_count as usize + bc.stack_size as usize,
            bc.mode() as i32,
        );

        let mut sframe = StackFrame {
            cur_func: JSValue::Object(self.0.clone()),
            js_mode,
            ..Default::default()
        };
        let sf = &mut sframe;

        // ensure stack has minimal free space.
        if js_check_stack_overflow(caller_ctx.rt(), 32) {
            caller_ctx.throw_stack_overflow();
            return JSValue::EXCEPTION;
        }

        // make local buffer and running bytecodes with it.
        // local_buf layout:
        //
        // +------------------+
        // | argument values  | <- arg_buf (optional, if need to copy arg)
        // | variable values  | <- var_buf
        // | stack space      | <- stack_buf
        // +------------------+
        //
        if sf.local_buf.try_reserve_exact(buf_values_count).is_err() {
            caller_ctx.throw_out_of_memory();
            return JSValue::EXCEPTION;
        }

        // initialize args section
        if arg_copy_num != 0 {
            debug_assert!(arg_copy_num >= args.len());

            // copy actual arg values
            sf.local_buf.extend(args.iter().cloned());
            // padding with undefined
            sf.local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                arg_copy_num - args.len(),
            ));

            sf.arg_count = arg_copy_num as _;
            sf.arg_real_count = args.len() as _;
            sf.arg_base_ptr = sf.local_buf.as_mut_ptr()
        } else {
            sf.arg_count = args.len() as _;
            sf.arg_real_count = args.len() as _;
            sf.arg_base_ptr = args.as_ptr().cast::<JSValue>() as *mut _;
        }

        // initialize (vars + stack) section
        sf.local_buf.extend(std::iter::repeat_n(
            JSValue::UNDEFINED,
            (bc.var_count + bc.stack_size) as usize,
        ));
        debug_assert_eq!(sf.local_buf.len(), buf_values_count);

        sf.var_base_ptr = unsafe { sf.local_buf.as_mut_ptr().add(arg_copy_num as usize) };
        sf.var_count = bc.var_count;

        sf.prev_frame = caller_ctx.rt().cur_stack_frame;
        caller_ctx.rt_mut().cur_stack_frame = (sf as *mut StackFrame).cast();

        // to borrow short, clone home object
        let home_obj = isa.home_object.borrow().clone();

        let retval = bc.run(
            caller_ctx,
            home_obj.as_ref(),
            sf,
            this_obj,
            new_target,
            0,
            0,
            &isa.var_refs.borrow(),
            args,
            false,
        );

        debug_assert_eq!(
            caller_ctx.rt().cur_stack_frame,
            sf.prev_frame,
            "runtime current stack frame should be restored after running bytecodes"
        );

        // stack frame is about to drop
        sf.clear();

        retval
    }

    /// handle `Function` constructor. For example:
    ///
    /// ```js
    /// const f = new Function("a", "b", "return a+b")
    /// ```
    pub fn class_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
        debug_assert!((0..4).contains(&cc.magic()));

        // XXX: add a specific eval mode so that Function("}), ({") is rejected

        let new_target = cc.this();
        let func_kind = unsafe { std::mem::transmute::<u8, JSFunctionKindEnum>(cc.magic() as u8) };

        //
        // make function source script:
        // ([async] function[*] anonymous(a,b){<body>})
        //
        let mut buf = SmallVec::<[u8; 256]>::new();
        buf.push(b'(');

        if matches!(
            func_kind,
            JSFunctionKindEnum::JS_FUNC_ASYNC | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            buf.extend_from_slice(b"async ");
        }

        buf.extend_from_slice(b"function");
        if matches!(
            func_kind,
            JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            buf.push(b'*');
        }

        buf.extend_from_slice(b" anonymous(");

        // Note: the last argument is function body.
        if let Some((body, arglist)) = cc.arguments().split_last() {
            // args list
            for (i, a) in arglist.iter().enumerate() {
                if i != 0 {
                    buf.push(b',');
                }
                buf.extend_from_slice(a.to_string(ctx)?.content().to_utf8(false).as_bytes());
            }
            buf.extend_from_slice(b"\n) {\n");

            // function body
            buf.extend_from_slice(body.to_string(ctx)?.content().to_utf8(false).as_bytes());
        } else {
            // no arguments
            buf.extend_from_slice(b") {\n");
        }

        buf.extend_from_slice(b"\n})");

        // eval script
        let func_val = {
            // Note: to avoid ctx.0 borrow conflict, fetch global object in advance
            let global = JSValue::Object(ctx.global_obj());
            ctx.eval_internal(
                &global,
                buf.as_slice(),
                "<input>",
                JS_EVAL_TYPE_INDIRECT,
                -1,
            )?
        };

        if !new_target.is_undefined() {
            // set the prototype
            let mut proto = new_target.get_property(ctx, JS_ATOM_prototype.deref())?;
            if !proto.is_object() {
                let realm = JS_GetFunctionRealm(ctx, &new_target)?;
                proto = realm.0.borrow().class_proto[func_kind.class_id() as usize].clone();
            }
            func_val.set_prototype_internal(ctx, proto, true)?;
        }

        Ok(func_val)
    }
}
