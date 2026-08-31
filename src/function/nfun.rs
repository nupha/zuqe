// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ops::DerefMut;

use gc_lite::{GcNode, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_C_FUNCTION, JS_CLASS_C_FUNCTION_DATA,
    atom::Atom,
    builtins::iterator::make_iterator_next_result,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    object::{NativeFunction, Object, ObjectImpl},
    runtime::js_check_stack_overflow,
    stack_frame::{ExecMode, FrameAllocError, FrameState, StackFrameArena},
    value::JSValue,
};

use super::{CallContext, IteratorState, JS_CALL_FLAG_CONSTRUCTOR, js_function_set_properties};

pub type CallbackFn = fn(ctx: &mut ScopedContext, cc: CallContext) -> JSResult;
pub type GetterFn = fn(ctx: &mut ScopedContext, cc: CallContext) -> JSResult;
pub type SetterFn = fn(ctx: &mut ScopedContext, cc: CallContext) -> JSResult;
pub type IteratorFn =
    fn(ctx: &mut ScopedContext, cc: CallContext, iter_state: &mut IteratorState) -> JSResult;

pub type JSCFunctionFF = unsafe fn(f64) -> f64;
pub type JSCFunctionFFF = unsafe fn(f64, f64) -> f64;

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum Callback {
    Func(CallbackFn),
    Getter(GetterFn),
    Setter(SetterFn),
    Constructor(CallbackFn),
    ConstructorOrFunc(CallbackFn),
    /// Like `ConstructorOrFunc`, but a plain call keeps the receiver in
    /// `this` instead of forcing it to undefined (ECMA-402 legacy
    /// constructors, e.g. `Intl.DateTimeFormat.call(obj)`).
    LegacyConstructorOrFunc(CallbackFn),
    IteratorNext(IteratorFn),

    FF(JSCFunctionFF),
    FFF(JSCFunctionFFF),
}

impl PartialEq for Callback {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Func(a), Self::Func(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::Getter(a), Self::Getter(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::Setter(a), Self::Setter(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::Constructor(a), Self::Constructor(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::ConstructorOrFunc(a), Self::ConstructorOrFunc(b)) => {
                std::ptr::fn_addr_eq(*a, *b)
            }
            (Self::LegacyConstructorOrFunc(a), Self::LegacyConstructorOrFunc(b)) => {
                std::ptr::fn_addr_eq(*a, *b)
            }
            (Self::IteratorNext(a), Self::IteratorNext(b)) => std::ptr::fn_addr_eq(*a, *b),

            (Self::FF(a), Self::FF(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::FFF(a), Self::FFF(b)) => std::ptr::fn_addr_eq(*a, *b),

            _ => false,
        }
    }
}

/// object payload for native function
#[derive(Debug)]
pub struct NFuncPayload {
    pub(crate) callback: Callback,
    pub(crate) length: u8,
    pub(crate) magic: i16,
    pub(crate) realm: ContextPtr,

    /// binding values attached to native func
    pub(crate) data: Option<Box<[JSValue]>>,
}

impl GcTrace for NFuncPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.realm.trace(tr);
        if let Some(data) = &self.data {
            data.iter().for_each(|x| x.trace(tr));
        }
    }
}

impl NFuncPayload {
    /// get nth binding data
    pub fn data(&self, nth: usize) -> Option<JSValue> {
        self.data.as_ref().and_then(|x| x.get(nth).copied())
    }

    /// Call native callback
    fn call(&mut self, this_value: JSValue, args: &[JSValue], flags: i32) -> JSResult {
        let mut cc = CallContext::new(this_value, args, self.magic as _, self.data.as_deref_mut());

        self.realm.with_new_scope(|cx| {
            let ret_val: JSResult = match self.callback {
                Callback::Func(f) => f(cx, cc),
                Callback::Getter(f) => {
                    // A getter may be invoked directly as a function (e.g.
                    // `desc.get.call(obj, extraArg)`), so the argument count is
                    // not fixed. `cc.arg` is safe regardless of count.
                    f(cx, cc)
                }
                Callback::Setter(f) => {
                    // A setter may be called with 0 or more arguments when
                    // invoked directly as a function; `cc.arg(0)` defaults to
                    // UNDEFINED when no value is passed.
                    f(cx, cc)
                }
                Callback::IteratorNext(f) => {
                    let mut iter_state = IteratorState::NotDone;
                    let r = f(cx, cc, &mut iter_state);
                    if let Ok(v) = r
                        && iter_state != IteratorState::NotParsed
                    {
                        make_iterator_next_result(cx, v, iter_state == IteratorState::Done)
                    } else {
                        r
                    }
                }
                Callback::Constructor(f) => {
                    if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                        Err(cx.throw_type_error("must be called with new"))
                    } else {
                        f(cx, cc)
                    }
                }
                Callback::ConstructorOrFunc(f) => {
                    if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                        cc.set_this(JSValue::UNDEFINED); // set this_value to undefined
                    }
                    f(cx, cc)
                }
                Callback::LegacyConstructorOrFunc(f) => {
                    // Constructed: `this` is new_target. Plain call: keep the
                    // receiver (possibly undefined) for the legacy path.
                    f(cx, cc)
                }

                Callback::FF(f) => {
                    //debug_assert!(data.is_none(), "binding data not allowed");
                    cc.arg(0)
                        .to_number(cx)
                        .map(|n| JSValue::from(unsafe { f(n.to_f64()) }))
                }
                Callback::FFF(f) => {
                    //debug_assert!(data.is_none(), "binding data not allowed");
                    let d1 = cc.arg(0).to_number(cx).map(|n| n.to_f64());
                    let d1 = match d1 {
                        Ok(v) => v,
                        Err(e) => return Err(e),
                    };
                    let d2 = cc.arg(1).to_number(cx).map(|n| n.to_f64());
                    let d2 = match d2 {
                        Ok(v) => v,
                        Err(e) => return Err(e),
                    };
                    Ok(JSValue::from(unsafe { f(d1, d2) }))
                }
            };

            ret_val
        })
    }
}

impl NativeFunction {
    /// Call native function
    pub fn class_call(
        ctx: &mut ScopedContext,
        func: JSValue,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let mut rt = ctx.rt_ptr();
        let mut nfunc = Self::try_from(func).unwrap();

        #[cfg(debug_assertions)]
        {
            debug_assert_eq!(nfunc.class_id(), JS_CLASS_C_FUNCTION);
            nfunc.gc_node().debug_assert_node_valid(rt.gc_heap_mut());
        }

        let args_length = nfunc.payload().length as usize;

        // better to always check stack overflow
        if js_check_stack_overflow(&rt, std::mem::size_of::<JSValue>() * args_length) {
            return Err(ctx.throw_stack_overflow());
        }

        // Allocates a lightweight arena frame for argument passing, skipping the
        // trampoline loop (native functions need no bytecode dispatch). Saves and
        // restores `active_trampoline_frame` so that nested `BytecodeFunction::call`
        // builds the correct frame chain.

        let cap = std::cmp::max(args_length, args.len()); // only covers the arg region (no var / operand stack).
        let prev_frame = rt.active_frame_index;
        let frame_idx = {
            let idx = match rt.frame_arena.alloc_frame(*nfunc, ExecMode::empty(), cap) {
                Ok(idx) => idx,
                Err(e) => {
                    return Err(match e {
                        FrameAllocError::Oom => ctx.throw_stack_fail(),
                        FrameAllocError::Exceeded => ctx.throw_stack_exceeded(),
                    });
                }
            };
            rt.frame_arena.frame_mut(idx).prev = prev_frame;
            idx
        };

        // Save/restore active_trampoline_frame so nested BytecodeFunction::call
        // sees this frame in the chain. (方案 C key step)
        rt.active_frame_index = frame_idx;

        // Copy args into arena buffer; missing slots are padded with UNDEFINED
        // by init_frame_args (var_count = 0 = no var region).
        rt.frame_arena
            .init_frame_args(frame_idx, args_length as u16, 0, args);

        // Get arg slice from arena for native callback.
        // IMPORTANT: use arg_real_count (actual args before padding), NOT arg_count
        // (total buffer capacity). The old legacy StackFrame::arg_values() returns
        // arg_real_count — matching this ensures constructors (Array, Date, String)
        // see the correct number of arguments: 0 for `super()`, not 1 with undefined.
        let nargs = unsafe {
            let f = rt.frame_arena.frame(frame_idx);
            let ptr = f.buf.data_ptr();
            if f.arg_real_count > 0 {
                std::slice::from_raw_parts(ptr, f.arg_real_count as usize)
            } else {
                &[]
            }
        };

        let result = nfunc.payload_mut().call(this_obj, nargs, flags);

        // Cleanup: restore active_trampoline_frame and free frame (on both success and error paths).
        rt.active_frame_index = prev_frame;

        debug_assert!(!rt.frame_arena.has_var_refs(frame_idx));

        if rt.frame_arena.frame(frame_idx).state == FrameState::Active {
            rt.frame_arena.free_frame(frame_idx);
        }

        result
    }

    /// Call native function (with binding data) via arena frame.
    ///
    /// Similar to `class_call` but without argument padding (init_frame_args
    /// with bc_arg_count = 0 simply copies all provided args).
    pub fn class_call_with_data(
        ctx: &mut ScopedContext,
        func_val: JSValue,
        this_val: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let mut rt = ctx.rt_ptr();
        let mut nfunc = NativeFunction::try_from(func_val).unwrap();

        debug_assert_eq!(nfunc.class_id(), JS_CLASS_C_FUNCTION_DATA);
        debug_assert!(matches!(nfunc.payload().callback, Callback::Func(_)));

        // ── Allocate lightweight arena frame for GC safety ──
        // Even without argument padding, the arena frame ensures args are
        // traced by StackFrameArena::trace when the native callback triggers GC.
        let obj: Object = *nfunc;
        let buf_cap = args.len();
        let saved_active = rt.active_frame_index;
        let arena_ptr: *mut StackFrameArena = &raw mut rt.frame_arena;

        let frame_idx = unsafe {
            let arena = &mut *arena_ptr;
            let idx = match arena.alloc_frame(obj, ExecMode::empty(), buf_cap) {
                Ok(idx) => idx,
                Err(e) => {
                    return Err(match e {
                        FrameAllocError::Oom => ctx.throw_stack_fail(),
                        FrameAllocError::Exceeded => ctx.throw_stack_exceeded(),
                    });
                }
            };
            arena.frame_mut(idx).prev = saved_active;
            idx
        };

        rt.active_frame_index = frame_idx;

        unsafe {
            let arena = &mut *arena_ptr;
            arena.init_frame_args(frame_idx, 0, 0, args);
        }

        let nargs = unsafe {
            let arena = &*arena_ptr;
            let head = arena.frame(frame_idx);
            let ptr = head.buf.data_ptr();
            if head.arg_real_count > 0 {
                std::slice::from_raw_parts(ptr, head.arg_real_count as usize)
            } else {
                &[]
            }
        };

        let result = nfunc.payload_mut().call(this_val, nargs, 0);

        rt.active_frame_index = saved_active;

        unsafe {
            let arena = &mut *arena_ptr;
            debug_assert!(!arena.has_var_refs(frame_idx));
            if arena.frame(frame_idx).state == FrameState::Active {
                arena.free_frame(frame_idx);
            }
        }

        result
    }

    /// Create native function object.
    /// at least `length` arguments can be readable in arguments.
    pub(crate) fn create(
        ctx: &mut ScopedContext,
        callback: Callback,
        name: Atom,
        length: u8,
        magic: i16,
        proto: Object,
        binding_data: Option<Box<[JSValue]>>,
    ) -> Result<Self, i32> {
        let has_binding_data = binding_data.is_some();

        let mut func_obj = {
            let p = NFuncPayload {
                callback,
                length,
                magic,
                realm: ctx.gc_ref().into(),
                data: binding_data,
            };
            ctx.create_instance_with_proto_class(
                Some(proto),
                if has_binding_data {
                    JS_CLASS_C_FUNCTION_DATA
                } else {
                    JS_CLASS_C_FUNCTION
                },
                p,
            )?
        };

        if matches!(
            callback,
            Callback::Constructor(_)
                | Callback::ConstructorOrFunc(_)
                | Callback::LegacyConstructorOrFunc(_)
        ) {
            func_obj.set_flag_constructor(true);
        }

        // define `length` and `name` properties on function.
        js_function_set_properties(ctx, func_obj, name, length as i32)?;

        Ok(Self(func_obj.gc_ref()))
    }

    #[inline(always)]
    pub(crate) fn payload(&self) -> &NFuncPayload {
        unsafe { self.0.as_ref() }.payload_native_func().unwrap()
    }

    #[inline(always)]
    pub(crate) fn payload_mut(&mut self) -> &mut NFuncPayload {
        self.deref_mut().payload_native_func_mut().unwrap()
    }
}

impl ObjectImpl {
    /// Checks if Object is native function of specific type.
    /// Matches both the callback pointer and magic.
    pub fn match_native_callback(&self, callback: Callback, magic: i32) -> bool {
        if let Some(nf) = self.payload_native_func() {
            nf.magic == magic as i16 && callback.eq(&nf.callback)
        } else {
            false
        }
    }
}

impl JSValue {
    /// Checks if a JSValue is native function of specific type.
    /// Matches both the callback pointer and magic.
    pub fn match_native_callback(&self, callback: Callback, magic: i32) -> bool {
        self.as_object()
            .is_some_and(|o| o.match_native_callback(callback, magic))
    }
}

impl<'c> ScopedContext<'c> {
    /// Create native function object with callback, using function proto as proto object.
    /// at least `length` arguments will be readable in arguments.
    #[inline(always)]
    pub fn create_native_function(
        &mut self,
        callback: Callback,
        name: &str,
        length: u8,
        magic: i16,
    ) -> Result<NativeFunction, i32> {
        let atom = Atom::from_str(self, name)?;
        NativeFunction::create(
            self,
            callback,
            atom,
            length,
            magic,
            self.function_proto(),
            None,
        )
    }

    /// Create native function object with callback and binding data, using function proto as proto object.
    /// at least `length` arguments will be readable in arguments.
    #[inline(always)]
    pub fn create_native_function_with_data(
        &mut self,
        callback: Callback,
        name: &str,
        length: u8,
        magic: i16,
        binding_data: Box<[JSValue]>,
    ) -> Result<NativeFunction, i32> {
        let atom = Atom::from_str(self, name)?;
        NativeFunction::create(
            self,
            callback,
            atom,
            length,
            magic,
            self.function_proto(),
            Some(binding_data),
        )
    }
}
