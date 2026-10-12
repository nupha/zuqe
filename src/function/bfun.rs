// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcRef, GcTrace, GcTraceCtx};
use smallvec::SmallVec;

use crate::{
    EvalName, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BOUND_FUNCTION, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_C_FUNCTION,
    JS_CLASS_C_FUNCTION_DATA, JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION,
    atom::{Atom, AtomDescKind},
    bytecode::exec_stackless::execute_trampoline,
    class::ClassID,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    eval::{EvalFlags, EvalType},
    object::{
        BytecodeFunction, JS_PROP_CONFIGURABLE, JS_PROP_NORMAL, JS_PROP_TMASK, JSProperty, Object,
        ObjectImpl,
    },
    runtime::js_check_thread_stack_overflow,
    string::JS_ConcatString3,
    value::JSValue,
    vm_stack::{FrameAllocError, FrameState, VmStack},
};

use crate::object_cast;

use super::{Bytecode, CallContext, VarRef};

/// Special index for argument scope.
/// This is same to `FunctionDef::body_scope`, which was automatically pushed
/// when creating root function def in `compile_and_run()`.
pub(super) const ARG_SCOPE_INDEX: i32 = 1;
/// to mark end of arugment vars chain.
pub(super) const ARG_SCOPE_END: i32 = -2;

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
///
/// * bytecode function
/// * async function
/// * generator function
/// * async generator function
#[derive(Debug)]
pub struct BFuncPayload {
    bytecode: Bytecode,
    /// Closure VarRefs (captured variables only; globals are resolved by
    /// name at runtime and never captured).
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
    pub(crate) const fn new(bytecode: Bytecode, captured_vars: Vec<VarRef>) -> Self {
        Self {
            bytecode,
            closure_var_refs: captured_vars,
            home_object: None,
        }
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

    /// Get a closure variable reference by its closure-var index.
    #[inline(always)]
    pub(crate) fn get_closure(&self, nth: usize) -> VarRef {
        self.closure_var_refs[nth]
    }

    /// Set a closure variable reference by its closure-var index (0-based).
    #[inline(always)]
    pub(crate) fn set_closure(&mut self, nth: usize, vref: VarRef) {
        self.closure_var_refs[nth] = vref;
    }

    /// Set value of closure by its closure-var index (0-based).
    #[inline(always)]
    pub(crate) fn store_closure_value(
        &mut self,
        nth: usize,
        ctx: &mut ScopedContext,
        value: JSValue,
    ) {
        self.closure_var_refs[nth].set_value(&mut ctx.rt_ptr(), value);
    }

    #[inline(always)]
    pub(crate) fn captured_vars(&self) -> &[VarRef] {
        &self.closure_var_refs
    }

    #[inline(always)]
    pub(crate) fn captured_vars_mut(&mut self) -> &mut [VarRef] {
        &mut self.closure_var_refs
    }
}

/// Hand-written (not generated) because `BCWrapper` groups four distinct
/// function classes — BYTECODE_FUNCTION, GENERATOR_FUNCTION, ASYNC_FUNCTION,
/// ASYNC_GENERATOR_FUNCTION — each of which already maps to its own narrow
/// type, so no single class can name-match this type.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct BCWrapper(pub(crate) GcRef<ObjectImpl>);

object_cast!(
    BCWrapper,
    JS_CLASS_BYTECODE_FUNCTION
        | JS_CLASS_GENERATOR_FUNCTION
        | JS_CLASS_ASYNC_FUNCTION
        | JS_CLASS_ASYNC_GENERATOR_FUNCTION
);

/// An object which have Bytecode inside, could be:
/// * bytecode function
/// * generator function
/// * async function
/// * async generator function
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
        unsafe { self.0.as_ref() }.payload_bc_function().unwrap()
    }

    /// get bcfunction payload
    #[inline]
    pub fn payload_mut(&mut self) -> &mut BFuncPayload {
        unsafe { self.0.as_mut() }
            .payload_bc_function_mut()
            .unwrap()
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
            Err(ctx.throw_type_error("not a function"))
        }
    }

    /// get realm context of a function object
    pub(crate) fn realm(&self, mut default: ContextPtr) -> Result<ContextPtr, i32> {
        match self.class_id() {
            JS_CLASS_C_FUNCTION | JS_CLASS_C_FUNCTION_DATA => {
                Ok(self.payload_native_func().unwrap().realm)
            }
            JS_CLASS_BOUND_FUNCTION => self
                .payload_bound_function()
                .unwrap()
                .func_obj
                .realm(default),
            JS_CLASS_BYTECODE_FUNCTION
            | JS_CLASS_GENERATOR_FUNCTION
            | JS_CLASS_ASYNC_FUNCTION
            | JS_CLASS_ASYNC_GENERATOR_FUNCTION => {
                Ok(self.payload_bc_function().unwrap().bytecode().realm())
            }

            #[cfg(feature = "builtin_proxy")]
            crate::JS_CLASS_PROXY => {
                if self.payload_proxy().unwrap().is_revoked() {
                    Err(default
                        .with_current_scope(|mut scope| scope.throw_type_error_revoked_proxy()))
                } else {
                    self.payload_proxy().unwrap().target().realm(default)
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
        if let JSProperty::Normal(v) = func_obj.prop_slot(idx)
            && let Some(s) = v.as_string(true)
        {
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
) -> Result<(), i32> {
    let _ = func_obj.define_property_value(
        ctx,
        Atom::CONST_length,
        JSValue::from(len),
        JS_PROP_CONFIGURABLE, // ES6 feature non-compatible with ES5.1: length is configurable
    );

    let name_val = if name.desc_kind(ctx.rt()) == AtomDescKind::JS_ATOM_KIND_SYMBOL {
        // Well-known symbol functions: name should be [Symbol.xxx]
        let inner = name.force_string(ctx)?;
        JS_ConcatString3(ctx, c"[", JSValue::make_string(inner), c"]").map(JSValue::make_string)?
    } else {
        name.force_string(ctx).map(JSValue::make_string)?
    };

    func_obj
        .define_property_value(ctx, Atom::CONST_name, name_val, JS_PROP_CONFIGURABLE)
        .map(|_| ())
}

pub(crate) const fn js_function_proto(_: &mut ScopedContext, _: CallContext) -> JSResult {
    Ok(JSValue::UNDEFINED)
}

/// A subtype of BCWrapper
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

        let mut ret_val = ctx.eval(
            buf.as_slice(),
            &EvalName::Name("<class>".to_string()),
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
                    .map_or(JSValue::NULL, JSValue::from);
            }

            ret_val.set_prototype_internal(ctx, proto, true)?;
        }

        Ok(ret_val)
    }

    #[inline(always)]
    pub fn payload(&self) -> &BFuncPayload {
        unsafe { self.0.as_ref() }.payload_bc_function().unwrap()
    }

    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut BFuncPayload {
        unsafe { self.0.as_mut() }
            .payload_bc_function_mut()
            .unwrap()
    }

    /// Run Bytecode Function in given scope.
    pub(super) fn call(
        &mut self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        new_target: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let bc = self.payload().bytecode();

        // Each BytecodeFunction::call nests a native execute_trampoline
        // invocation. Most JS-to-JS calls are flattened inside the trampoline
        // (CALL/CALL_METHOD/CALL_CONSTRUCTOR inline dispatch), but the
        // remaining nested paths (generator resume, calls re-entering
        // through native code, Proxy traps, etc.) still consume native
        // stack — turn an impending overflow into a catchable error
        // instead of a hard crash.
        if js_check_thread_stack_overflow(ctx.rt(), 0) {
            return Err(ctx.throw_stack_overflow());
        }

        // ── Allocate arena frame and execute via trampoline ──
        // BytecodeFunction::call always uses the arena/trampoline path.
        // The legacy StackFrame path has been removed — all bytecode
        // function execution goes through execute_trampoline.
        // When active_trampoline_frame is null (first call from a
        // non-trampoline context, e.g. native callback), we create a
        // root arena frame — execute_trampoline handles this correctly.

        let func_obj: Object = Object::from(*self);
        let mut rt = ctx.rt_ptr();

        let parent_frame = rt.active_frame_index;
        let arena_ptr: *mut VmStack = &raw mut rt.vm_stack;

        // init_frame_args uses max(bc.arg_count, actual_args.len()) as the arg region size.
        // Allocate accordingly.
        let buf_cap = std::cmp::max(bc.arg_count as usize, args.len()) + bc.local_buf_size as usize;
        let frame_idx = unsafe {
            let arena = &mut *arena_ptr;
            let idx = match arena.alloc_frame(func_obj, bc.mode(), buf_cap) {
                Ok(idx) => idx,
                Err(e) => {
                    return Err(match e {
                        FrameAllocError::Oom => ctx.throw_stack_fail(),
                        FrameAllocError::Exceeded => ctx.throw_stack_exceeded(),
                    });
                }
            };
            arena.frame_mut(idx).prev = parent_frame;
            idx
        };

        // Copy all args into arena frame buffer
        unsafe {
            (&mut *arena_ptr).init_frame_args(frame_idx, bc.arg_count, bc.var_count, args);
        }

        // execute_trampoline manages active_trampoline_frame internally,
        // the guard saves and restores the outer value.
        let (ret_val, is_completed) = ctx.with_active_frame_guard(|cx| {
            execute_trampoline(bc, frame_idx, cx, this_obj, new_target)
        });

        debug_assert!(
            is_completed,
            "synchronous BytecodeFunction::call should not suspend"
        );

        // ── Frame lifecycle cleanup ──
        // The trampoline's Return/Unwind handlers skip free_frame for the
        // outermost frame (start_frame_idx). We own the frame — detach and
        // free it here. Only free if the frame still exists and is Active:
        // it may be Suspended if a nested BytecodeFunction::call's free_frame
        // took the non-LIFO path, or already popped entirely if a TailCall
        // recycled it and the replacement allocation then failed (Oom).
        unsafe {
            let arena = &mut *arena_ptr;
            if let Some(var_refs) = arena.try_var_refs_mut(frame_idx) {
                for vr in var_refs.iter_mut() {
                    vr.detach();
                }
            }

            if arena
                .try_frame(frame_idx)
                .is_some_and(|f| f.state == FrameState::Active)
            {
                arena.free_frame(frame_idx);
            }
        }

        if ret_val.is_exception() {
            Err(-1)
        } else {
            // The return value may be either:
            // 1. A newly allocated object that was properly promoted to the parent
            //    scope by promote_to_parent (above). Such objects retain their LOCAL
            //    flag.
            // 2. A pre-existing GC object (e.g., a string constant from the bytecode's
            //    constant pool, or an object from a higher scope). These are kept alive
            //    by their owning GcTrace path (BytecodeImpl::GcTrace traces consts()),
            //    not by LOCAL/ROOT flags.
            //
            // promote_to_parent correctly handles case 1. For case 2, the value is
            // not in the trampoline scope's cache, so promote_to_parent silently skips
            // it — this is correct because the value is already protected elsewhere.
            //
            // No debug assert here: testing is_root_or_local() would give false
            // positives for case 2, and genuine promotion failures for case 1 would
            // manifest as use-after-free crashes detected by the GC's integrity checks.

            Ok(ret_val)
        }
    }
}
