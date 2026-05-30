// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ops::DerefMut;

use gc_lite::{GcRef, GcTrace, GcTraceCtx};
use smallvec::SmallVec;

use crate::{
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION,
    atom::Atom,
    class::ClassID,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    eval::{EvalFlags, EvalType},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_NORMAL, JS_PROP_TMASK, JSProperty, Object, ObjectImpl,
        ObjectPayload,
    },
    object_cast,
    runtime::js_check_stack_overflow,
    stack_frame::StackFrame,
    value::JSValue,
};

use super::{Bytecode, CallContext, JS_CALL_FLAG_COPY_ARGV, VarRef};

/// Special index for argument scope.
/// This is same to `FunctionDef::body_scope`, which was automatically pushed
/// when creating root function def in `compile_and_run()`.
pub const ARG_SCOPE_INDEX: i32 = 1;

/// to mark end of arugment vars chain.
pub const ARG_SCOPE_END: i32 = -2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FunctionKind {
    /// bytecode function
    Normal = 0,
    /// generator
    Generator = 1,
    /// async function
    Async = 2,
    /// async generator.
    AsyncGenerator = 3,
}

impl FunctionKind {
    #[inline(always)]
    pub const fn is_async(self) -> bool {
        matches!(self, Self::Async | Self::AsyncGenerator)
    }

    #[inline(always)]
    pub const fn is_generator(self) -> bool {
        matches!(self, Self::Generator | Self::AsyncGenerator)
    }

    /// get associated class id for function kind
    #[inline]
    pub const fn class_id(&self) -> ClassID {
        match self {
            Self::Normal => JS_CLASS_BYTECODE_FUNCTION,
            Self::Generator => JS_CLASS_GENERATOR_FUNCTION,
            Self::Async => JS_CLASS_ASYNC_FUNCTION,
            Self::AsyncGenerator => JS_CLASS_ASYNC_GENERATOR_FUNCTION,
        }
    }

    #[inline]
    pub const fn generator_class_id(&self) -> Option<ClassID> {
        match self {
            Self::Generator => Some(JS_CLASS_GENERATOR),
            Self::AsyncGenerator => Some(JS_CLASS_ASYNC_GENERATOR),
            Self::Normal | Self::Async => None,
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
/// * bytecode function
/// * async function
/// * generator function
/// * async generator function
#[derive(Debug)]
pub struct BFuncPayload {
    bytecode: Bytecode,
    closure_var_refs: Vec<VarRef>,

    /// 函数对象关联的宿主对象，主要用于以下场景：
    /// - 私有字段的访问控制：当类方法需要访问私有字段时，需要通过home object定位到对应的私有品牌（private brand）进行权限验证。
    /// - for 'super' access
    home_object: Option<Object>,
}

impl GcTrace for BFuncPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.bytecode.trace(tr);
        if let Some(x) = self.home_object {
            x.trace(tr)
        }
        for vr in self.closure_var_refs.iter() {
            vr.trace(tr);
        }
    }
}

impl BFuncPayload {
    pub(crate) fn new(bytecode: Bytecode, captured_vars: Vec<VarRef>) -> Box<Self> {
        Box::new(Self {
            bytecode,
            closure_var_refs: captured_vars,
            home_object: None,
        })
    }

    /// get bytecode
    #[inline(always)]
    pub const fn bytecode(&self) -> Bytecode {
        self.bytecode
    }

    #[inline(always)]
    pub const fn home_object(&self) -> Option<Object> {
        self.home_object
    }

    #[inline(always)]
    pub(crate) const fn set_home_object(&mut self, obj: Option<Object>) {
        self.home_object = obj;
    }

    /// captured variables
    #[inline(always)]
    pub(crate) fn closure_var_refs(&self) -> &[VarRef] {
        &self.closure_var_refs
    }

    /// captured variables
    #[inline(always)]
    pub(crate) fn closure_var_refs_mut(&mut self) -> &mut [VarRef] {
        &mut self.closure_var_refs
    }
}

/// An object which have Bytecode inside, could be:
/// * bytecode function
/// * generator function
/// * async function
/// * async generator function
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct BCWrapper(pub(super) GcRef<ObjectImpl>);

object_cast!(
    BCWrapper,
    JS_CLASS_BYTECODE_FUNCTION
        | JS_CLASS_GENERATOR_FUNCTION
        | JS_CLASS_ASYNC_FUNCTION
        | JS_CLASS_ASYNC_GENERATOR_FUNCTION
);

impl BCWrapper {
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
    pub fn payload(&self) -> &BFuncPayload {
        self.0.payload().bc_function().unwrap()
    }

    /// get bcfunction payload
    #[inline]
    pub fn payload_mut(&mut self) -> &mut BFuncPayload {
        self.deref_mut().payload_mut().bc_function_mut().unwrap()
    }

    pub(crate) fn method_set_home_object(&mut self, home_obj: Option<Object>) {
        let fo = self.payload_mut();
        if fo.bytecode().need_home_object() {
            fo.set_home_object(home_obj);
        }
    }
}

impl ObjectImpl {
    /// Check if `self` is callable object, raise exception if not.
    pub fn ensure_callable(&self, ctx: &mut ScopedContext) -> Result<&Self, i32> {
        if self.is_callable() {
            Ok(self)
        } else {
            ctx.throw_type_error("not a function");
            Err(-1)
        }
    }

    /// get realm context of a function object
    pub(crate) fn realm(&self, mut default: ContextPtr) -> Result<ContextPtr, i32> {
        match self.payload() {
            ObjectPayload::C_FUNCTION(f) => Ok(f.realm),
            ObjectPayload::BOUND_FUNCTION(f) => f.func_obj.realm(default),
            ObjectPayload::BYTECODE_FUNCTION(f)
            | ObjectPayload::GENERATOR_FUNCTION(f)
            | ObjectPayload::ASYNC_FUNCTION(f)
            | ObjectPayload::ASYNC_GENERATOR_FUNCTION(f) => Ok(f.bytecode().realm()),

            #[cfg(feature = "builtin_proxy")]
            ObjectPayload::PROXY(px) => {
                if px.is_revoked() {
                    default.with_current_scope(|mut scope| {
                        scope.throw_type_error_revoked_proxy();
                    });
                    Err(-1)
                } else {
                    px.target().realm(default)
                }
            }

            _ => Ok(default),
        }
    }
}

pub(crate) fn get_func_name(func_obj: Object) -> Option<String> {
    // in order to avoid executing arbitrary code during the stack trace generation,
    // we only look at simple 'name' properties containing a string.
    if let Some((idx, prs_flags)) = func_obj.find_shape_property(Atom::CONST_name)
        && prs_flags & JS_PROP_TMASK == JS_PROP_NORMAL
    {
        if let JSProperty::Normal(JSValue::String(s)) = func_obj.prop_slot(idx) {
            return Some(s.content().encode_utf8(false).into_owned());
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
}

/// define `length` and `name` properties on function object `func_obj`
pub(crate) fn js_function_set_properties(
    ctx: &mut ScopedContext,
    mut func_obj: Object,
    name: Atom,
    len: i32,
) {
    let _ = func_obj.define_property_value(
        ctx,
        Atom::CONST_length,
        JSValue::Int(len),
        JS_PROP_CONFIGURABLE, // ES6 feature non-compatible with ES5.1: length is configurable
    );

    let name_val = name
        .force_string(ctx)
        .map_or(JSValue::EXCEPTION, JSValue::from);

    let _ = func_obj.define_property_value(ctx, Atom::CONST_name, name_val, JS_PROP_CONFIGURABLE);
}

pub(crate) const fn js_function_proto(_: &mut ScopedContext, _: CallContext) -> JSResult {
    Ok(JSValue::UNDEFINED)
}

/// A subtype of BCWrapper
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct BytecodeFunction(GcRef<ObjectImpl>);

object_cast!(BytecodeFunction, JS_CLASS_BYTECODE_FUNCTION);

impl BytecodeFunction {
    /// Handle `Function` constructor call like:
    ///
    /// ```js
    /// const f = new Function("a", "b", "return a+b")
    /// ```
    pub fn class_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
        debug_assert!((0..4).contains(&cc.magic()));

        // XXX: add a specific eval mode so that Function("}), ({") is rejected

        let new_target = cc.this();
        let func_kind = unsafe { std::mem::transmute::<u8, FunctionKind>(cc.magic() as u8) };

        //
        // make function source script:
        // ([async] function[*] anonymous(a,b){<body>})
        //
        let mut buf = SmallVec::<[u8; 256]>::new();
        buf.push(b'(');

        if matches!(
            func_kind,
            FunctionKind::Async | FunctionKind::AsyncGenerator
        ) {
            buf.extend_from_slice(b"async ");
        }

        buf.extend_from_slice(b"function");
        if matches!(
            func_kind,
            FunctionKind::Generator | FunctionKind::AsyncGenerator
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
                buf.extend_from_slice(a.to_string(ctx)?.content().encode_utf8(false).as_bytes());
            }
            buf.extend_from_slice(b"\n) {\n");

            // function body
            buf.extend_from_slice(body.to_string(ctx)?.content().encode_utf8(false).as_bytes());
        } else {
            // no arguments
            buf.extend_from_slice(b") {\n");
        }

        buf.extend_from_slice(b"\n})");

        // eval script
        let mut ret_val = ctx.eval(
            buf.as_slice(),
            "<input>",
            EvalType::Indirect,
            EvalFlags::empty(),
        )?;

        if !new_target.is_undefined() {
            // set the prototype
            let mut proto = new_target.get_property(ctx, Atom::CONST_prototype)?;
            if !proto.is_object() {
                proto = new_target
                    .as_object()
                    .map_or(Ok(ctx.into()), |o| o.realm(ctx.into()))?
                    .class_proto(func_kind.class_id())
                    .map_or(JSValue::NULL, JSValue::Object);
            }

            ret_val.set_prototype_internal(ctx, proto, true)?;
        }

        Ok(ret_val)
    }

    #[inline(always)]
    pub fn payload(&self) -> &BFuncPayload {
        self.0.payload().bc_function().unwrap()
    }

    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut BFuncPayload {
        self.deref_mut().payload_mut().bc_function_mut().unwrap()
    }

    /// Run Bytecode Function in given scope.
    pub(super) fn call(
        &mut self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        new_target: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let bc = self.payload().bytecode();

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
            bc.mode(),
        );

        let rt = ctx.rt_ptr();
        let mut stack_frame = StackFrame::new((*self).into(), js_mode, [this_obj, new_target]);

        // ensure stack has minimal free space.
        if js_check_stack_overflow(&rt, 32) {
            ctx.throw_stack_overflow();
            return Err(-1);
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
        if stack_frame
            .local_buf
            .try_reserve_exact(buf_values_count)
            .is_err()
        {
            ctx.throw_out_of_memory();
            return Err(-1);
        }

        // initialize args section
        if arg_copy_num != 0 {
            debug_assert!(arg_copy_num >= args.len());

            // copy actual arg values
            stack_frame.local_buf.extend(args.iter().cloned());
            // padding with undefined
            stack_frame.local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                arg_copy_num - args.len(),
            ));

            stack_frame.arg_count = arg_copy_num as _;
            stack_frame.arg_real_count = args.len() as _;
            stack_frame.arg_base_ptr = stack_frame.local_buf.as_mut_ptr()
        } else {
            stack_frame.arg_count = args.len() as _;
            stack_frame.arg_real_count = args.len() as _;
            stack_frame.arg_base_ptr = args.as_ptr().cast::<JSValue>() as *mut _;
        }

        // initialize (vars + stack) section
        stack_frame.local_buf.extend(std::iter::repeat_n(
            JSValue::UNDEFINED,
            (bc.var_count + bc.stack_size) as usize,
        ));
        debug_assert_eq!(stack_frame.local_buf.len(), buf_values_count);

        stack_frame.var_base_ptr = unsafe {
            stack_frame
                .local_buf
                .as_mut_ptr()
                .add(arg_copy_num as usize)
        };
        stack_frame.var_count = bc.var_count;

        let bf = self.payload_mut();

        // `ret_val` Ok(value) is protected by the current GC scope (allocated inside `execute()`).
        let ret_val = bc
            .execute(
                ctx,
                bf.home_object(),
                &mut stack_frame,
                this_obj,
                new_target,
                0,
                0,
                bf.closure_var_refs_mut(),
                args,
                false,
            )
            .err();

        // Hint: NO write barrier needed here for `bf.closure_var_refs()`.
        // Each `VarRef` in the closure list is a GC node; its value is already
        // protected by `VarRef::set_value()` which calls `GcHeap::bind()` on every
        // update via PUT_VAR_REF / SET_VAR_REF opcodes during bytecode execution.
        // Unlike async functions (see astate.rs), this stack frame is ephemeral and
        // destroyed immediately after `execute()` returns, so no cross-scope promotion
        // is required.

        ret_val
    }
}
