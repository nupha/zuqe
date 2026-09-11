// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cmp, mem::ManuallyDrop, ptr::NonNull};

use fallible_collections::TryCollect;

#[cfg(feature = "inline_cache")]
use crate::function::InlineCacheEntry;

use crate::{
    Array, JS_CLASS_MAPPED_ARGUMENTS, JS_CLASS_OBJECT, ScopeResult,
    array::alloc_fast_array_slots,
    atom::{Atom, AtomKind},
    builtins::{
        generator::{FUNC_RET_AWAIT, FUNC_RET_INITIAL_YIELD, FUNC_RET_YIELD, FUNC_RET_YIELD_STAR},
        iterator::ObjectIter,
    },
    bytecode::{
        OpCode, SpecialObject,
        arithmatic::{UnaryOp, op_binary_logic},
        operators::{
            js_append_enumerate, js_for_in_next, js_for_in_start, js_for_of_next,
            js_has_unscopable, js_iterator_get_value_done, js_method_set_properties,
            js_op_define_class_for_arena, js_operator_delete, js_operator_in,
            js_operator_instanceof,
        },
    },
    class::{
        OP_DEFINE_METHOD_ENUMERABLE, OP_DEFINE_METHOD_GETTER, OP_DEFINE_METHOD_METHOD,
        OP_DEFINE_METHOD_SETTER,
    },
    context::{ContextPtr, GlobalVarSetMode, ScopedContext},
    error::{
        Exception, JS_THROW_ERROR_DELETE_SUPER, JS_THROW_ERROR_ITERATOR_THROW, JS_THROW_VAR_REDECL,
        JS_THROW_VAR_RO, JS_THROW_VAR_UNINITIALIZED, JS_ThrowReferenceErrorNotDefined,
        JS_ThrowReferenceErrorUninitialized, JS_ThrowSyntaxErrorVarRedeclaration,
        is_backtrace_needed,
    },
    eval::{EvalFlags, EvalType},
    function::{
        BCWrapper, Bytecode, CallContext, FunctionKind, GcScopeKind, JS_CALL_FLAG_CONSTRUCTOR,
        VarRef, build_arg_list, js_build_arguments, js_function_apply,
    },
    module::{js_dynamic_import, js_import_meta},
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
        JS_PROP_HAS_WRITABLE, JS_PROP_THROW, JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty, Object,
        PayloadPtr, ThrowPolicy,
    },
    stack_frame::{
        ExecMode, FrameAllocError, FrameIndex, MAX_CALL_DEPTH, StackFrameArena, VirtualFrame,
    },
    utils::{read_from_ptr, unlikely},
    value::JSValue,
};

#[cfg(feature = "big_int")]
use crate::bigint::BigIntRef;

/// Action returned by the bytecode interpreter inner loop to the trampoline.
/// The inner loop processes opcodes normally; when it encounters a call,
/// return, tail call, or uncaught exception, it yields control to the
/// trampoline via this enum.
#[derive(Debug)]
pub(crate) enum ExecAction {
    /// Call a bytecode function: trampoline saves caller state and switches
    /// to the callee frame. Carries this_obj/new_target to avoid re-reading
    /// from the callee frame head in the trampoline handler.
    CallBytecode {
        callee_bc: Bytecode,
        callee_frame_idx: FrameIndex,
        /// SP offset in caller frame where return value should be stored.
        return_sp: usize,
        this_obj: JSValue,
        new_target: JSValue,
        /// `Some(instance)` for a flattened legacy (non-derived) constructor
        /// call: the trampoline replaces a non-object return value with the
        /// instance created at the call site (mirrors `do_call_internal`).
        ctor_this: Option<JSValue>,
    },
    /// Cross-realm call: trampoline switches to callee's context scope.
    CallBytecodeCrossRealm {
        callee_bc: Bytecode,
        callee_frame_idx: FrameIndex,
        callee_ctx_ptr: ContextPtr,
        return_sp: usize,
        this_obj: JSValue,
        new_target: JSValue,
        /// Same as `CallBytecode::ctor_this`.
        ctor_this: Option<JSValue>,
    },
    /// Tail-call optimization: recycle current frame, switch to callee
    /// without pushing a VirtualFrame.
    TailCall {
        callee_bc: Bytecode,
        callee_func_obj: Object,
        args: Vec<JSValue>,
        /// `this` for the callee (TAIL_CALL_METHOD); UNDEFINED for TAIL_CALL.
        this_obj: JSValue,
    },
    /// Frame returned normally or yielded (generator/async suspension).
    /// bool = true means the frame suspended (yield/await), not completed.
    Return(JSValue, bool),
    /// Uncaught exception in current frame; trampoline must propagate
    /// upward through the virtual call stack.
    Unwind,
    /// Exception thrown in inner loop — replaces old State::Exception.
    /// The outer arena loop checks for catch/finally and either continues
    /// (caught) or returns ExecAction::Unwind (uncaught).
    Exception,
}

/// A GC scope wrapper for trampoline use.
///
/// Stores the real `GcScope` (from `new_scope()`) inside `ManuallyDrop` so the
/// scope stays alive across trampoline iterations. On yield/complete, the
/// `ScopedContext` is taken out of `ManuallyDrop` via `into_inner()` and
/// dropped — this triggers `GcScope::Drop` which pops from the GC heap.
/// On CALL, the `ScopedContext` is transferred to a `VirtualFrame`'s
/// `ManuallyDrop` and restored on RETURN.
///
/// No `pop_scope()` is ever called explicitly — scopes are always popped by
/// `GcScope::Drop` (the anti-forget of `ManuallyDrop::take` + `drop`).
///
/// # Safety of `'static` transmute
///
/// The `'static` on `ScopedContext` is safe because:
/// - `ContextPtr` is `Copy` (a raw pointer) — the ctx reference survives
/// - `GcScope`'s lifetime is `PhantomData<&'s ()>` — zero runtime cost
/// - `ManuallyDrop` prevents auto-drop at the wrong time
/// - All scopes are drained (into_inner + drop) before the GC heap is destroyed
#[derive(Debug)]
struct TrampolineScope {
    inner: ManuallyDrop<ScopedContext<'static>>,
}

impl TrampolineScope {
    /// Push a new scope onto the GC heap and wrap it.
    #[inline(always)]
    unsafe fn new(ctx: &mut ContextPtr) -> Self {
        Self {
            inner: ManuallyDrop::new(unsafe {
                std::mem::transmute::<ScopedContext<'_>, ScopedContext<'static>>(ctx.new_scope())
            }),
        }
    }

    /// Extract the inner ScopedContext, consuming this wrapper.
    /// The caller becomes responsible for dropping it (which pops the scope).
    fn into_inner(mut self) -> ScopedContext<'static> {
        unsafe { ManuallyDrop::take(&mut self.inner) }
    }

    /// Drop the current inner scope (popping it from the GC heap) and install
    /// `saved` in its place. Used when returning/unwinding into a caller whose
    /// scope was parked in a `VirtualFrame`. The drop-then-install order keeps
    /// the GC scope stack LIFO.
    ///
    /// # Safety
    ///
    /// `saved` must be a scope previously extracted via `into_inner()` (or
    /// `ManuallyDrop::into_inner` of a `VirtualFrame::caller_scope`) that sits
    /// directly below the current scope on the GC scope stack.
    #[inline(always)]
    unsafe fn swap_in_saved(&mut self, saved: ScopedContext<'static>) {
        drop(unsafe { ManuallyDrop::take(&mut self.inner) });
        self.inner = ManuallyDrop::new(saved);
    }

    /// Borrow the inner scope mutably (for passing to `execute_with_arena`).
    #[inline(always)]
    fn scope_mut(&mut self) -> &mut ScopedContext<'static> {
        &mut self.inner
    }
}

impl std::ops::Deref for TrampolineScope {
    type Target = ScopedContext<'static>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::ops::DerefMut for TrampolineScope {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

#[allow(clippy::too_many_arguments)]
fn get_var_ref(
    ctx: &mut ScopedContext,
    var_idx: u16,
    is_arg: bool,
    arg_base_ptr: *mut JSValue,
    var_base_ptr: *mut JSValue,
    arena: &mut StackFrameArena,
    cur_frame_idx: FrameIndex,
    arg_count: u16,
    var_count: u16,
) -> Result<VarRef, i32> {
    debug_assert!(ctx.is_arena_mode());

    // Extract frame buffer range before any mutable borrow of arena.
    #[cfg(debug_assertions)]
    let frame_buf_range = {
        let head = arena.frame(cur_frame_idx);
        (head.buf.data_ptr(), head.buf.capacity())
    };

    let var_refs = arena.var_refs_or_default(cur_frame_idx);
    if let Some(vr) = var_refs
        .iter()
        .find(|x| x.is_match(var_idx, is_arg))
        .copied()
    {
        return Ok(vr);
    }
    let pvalue = unsafe {
        NonNull::new_unchecked(if is_arg {
            debug_assert!(var_idx < arg_count);
            arg_base_ptr.offset(var_idx as isize)
        } else {
            debug_assert!(var_idx < var_count);
            var_base_ptr.offset(var_idx as isize)
        })
    };
    let vref = VarRef::create_bind(ctx, var_idx, is_arg, pvalue)?;

    #[cfg(debug_assertions)]
    {
        let (buf_start, buf_cap) = frame_buf_range;
        let buf_start: *const JSValue = buf_start as *const JSValue;
        let buf_end = unsafe { buf_start.add(buf_cap as usize) };
        let ptr: *const JSValue = pvalue.as_ptr() as *const JSValue;
        debug_assert!(
            ptr >= buf_start && ptr < buf_end,
            "get_var_ref: VarRef::Bind pvalue {:p} out of frame buf [{:p}, {:p})",
            ptr,
            buf_start,
            buf_end
        );
    }

    var_refs.push(vref);
    Ok(vref)
}

impl Bytecode {
    /// Execute bytecodes using the arena-based stackless model.
    ///
    /// This variant works with FrameIndex + StackFrameArena instead of
    /// the legacy &mut StackFrame. It is called by execute_trampoline()
    /// and supports returning ExecAction for call/return/unwind control.
    ///
    /// Returns the ExecAction describing what the trampoline should do next.
    #[allow(clippy::too_many_arguments)]
    fn execute_with_arena(
        &self,
        realm_scope: &mut ScopedContext,
        arena: &mut StackFrameArena,
        cur_frame_idx: FrameIndex,
        gc_scope_kind: GcScopeKind,
        this_obj: JSValue,
        new_target: JSValue,
        func_home_obj: Option<Object>,
        pc_offset: usize,
        sp_offset: usize,
        args: &[JSValue],
        throwed: bool,
    ) -> ExecAction {
        debug_assert!(realm_scope.is_arena_mode());

        let mut rt = realm_scope.rt_ptr();
        let bytecodes = self.opcodes();
        debug_assert!(
            pc_offset < bytecodes.len(),
            "pc_offset={}, bytecodes.len()={}",
            pc_offset,
            bytecodes.len()
        );

        // Extract frame data pointers from the arena.
        // All raw pointers derived here remain stable because:
        // - Arena data is a fixed-capacity Box<[JSValue]> (never resized).
        // - FrameHead is in a Vec that may reallocate, but raw pointers
        //   into arena.data (not FrameHead) are used for data access.
        //
        // Split into immutable then mutable borrows: data_ptr and
        // stack_base_ptr take &self on arena, while frame_mut takes &mut.
        let (arg_base_ptr, var_base_ptr, sp_base, mode, func_obj) = {
            let frame = arena.frame(cur_frame_idx);
            let arg_base = frame.buf.data_ptr_unchecked();
            let var_base = unsafe { arg_base.add(frame.var_base_offset as usize) };
            let sp = unsafe {
                frame
                    .buf
                    .data_ptr_unchecked()
                    .add((frame.var_base_offset + frame.var_count as u32) as usize)
            };
            (arg_base, var_base, sp, frame.mode, frame.func_obj)
        };

        // Single raw root for the arena; everything below re-derives
        // frame-header access from it at each use. Never cache a `&mut`/raw
        // into the arena across interpreter-loop iterations: the loop creates
        // fresh `&mut StackFrameArena` reborrows (build_callee_frame →
        // alloc_frame → frames.push, find_catch_in_frame, ...), and under
        // Stacked Borrows a newer reborrow pops any borrow derived earlier.
        // fat-LTO + panic=abort lets LLVM exploit that violation (nounwind
        // store motion across calls), corrupting sp/pc bookkeeping — same
        // family as the `frames` incident notes in stack_frame/arena.rs.
        let arena_ptr: *mut StackFrameArena = arena;

        // Raw-root accessors: each use re-derives a fresh borrow through
        // arena_ptr (see the discipline note above).
        macro_rules! frame {
            ($idx:expr) => {
                unsafe { (*arena_ptr).frame($idx) }
            };
        }
        macro_rules! frame_mut {
            ($idx:expr) => {
                unsafe { (*arena_ptr).frame_mut($idx) }
            };
        }
        macro_rules! arena_mut {
            () => {
                unsafe { &mut *arena_ptr }
            };
        }

        // cur_frame_idx is fixed for the whole invocation (the loop only
        // allocates/frees callee frames), so these macros can index it
        // directly; they re-derive through the raw root on every use.
        macro_rules! save_sp {
            ($val:expr) => {
                unsafe { (*arena_ptr).frame_mut(cur_frame_idx).cur_sp_offset = $val }
            };
        }
        macro_rules! save_pc {
            ($val:expr) => {
                unsafe { (*arena_ptr).frame_mut(cur_frame_idx).cur_pc_offset = $val }
            };
        }

        let cpools = self.consts();

        let pc_base = bytecodes.as_ptr();
        let mut pc = unsafe { pc_base.add(pc_offset) };
        let mut sp = unsafe { sp_base.add(sp_offset) };

        save_sp!(sp_offset as i32);

        // Resolve captured_vars from the current frame's function object
        // (func_obj already extracted from the preamble frame lookup above).
        // Each function in the call chain has its own closure_var_refs.
        // We re-derive the correct slice from the current frame's func_obj.
        let captured_vars: &mut [VarRef] = {
            if let Ok(mut bfun) = BCWrapper::try_from(JSValue::from(func_obj)) {
                // SAFETY: bfun is a Copy (GcRef), so payload_mut borrows GC-managed
                // memory that outlives this function call. No GC runs during execution.
                let cv: *mut [VarRef] = bfun.payload_mut().captured_vars_mut();
                unsafe { &mut *cv }
            } else {
                // Non-bytecode functions have no closure_var_refs.
                // But execute_with_arena should never be called for them.
                &mut []
            }
        };

        if throwed {
            let cur_pc = unsafe { pc.offset_from(pc_base) as usize };

            // Add backtrace before catch lookup (matching exec.rs's
            // StackFrame::handle_exception behavior). If the error already
            // has a backtrace, is_backtrace_needed returns false and we skip.
            if let Exception::Error(err_val) = rt.exception()
                && is_backtrace_needed(err_val)
            {
                frame_mut!(cur_frame_idx).cur_pc_offset = cur_pc;
                realm_scope.build_backtrace_to(err_val, None, 0);
            }

            if let Some((new_pc, new_sp)) =
                unsafe { (*arena_ptr).find_catch_in_frame(cur_frame_idx, realm_scope) }
            {
                save_sp!(new_sp as i32);
                unsafe {
                    pc = pc_base.add(new_pc);
                    sp = sp_base.add(new_sp);
                }
            } else {
                // Exception not caught in this frame; propagate upward.
                return ExecAction::Unwind;
            }
        }

        // ── Stack access macros ──
        // These work with raw pointers and re-derived frame-header access,
        // identical to the legacy versions but using save_sp!/save_pc!.

        macro_rules! SP_VAL {
            (0) => {
                unsafe { *sp }
            };
            ($idx:expr) => {
                unsafe { *sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_REF {
            (0) => {
                unsafe { sp.as_ref().unwrap() }
            };
            ($idx:expr) => {
                unsafe { sp.offset($idx as isize).as_ref().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_REF {
            (0) => {
                unsafe { &*sp }
            };
            ($idx:expr) => {
                unsafe { &*sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_MUT {
            ($idx:expr) => {
                unsafe { sp.offset($idx as isize).as_mut().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_MUT {
            ($idx:expr) => {
                unsafe { &mut *sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_PUT {
            ($idx:expr, $val:expr) => {
                unsafe {
                    *sp.offset(($idx) as isize).as_mut().unwrap() = $val;
                }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_PUT {
            ($idx:expr, $val:expr) => {
                unsafe {
                    *sp.offset(($idx) as isize) = $val;
                }
            };
        }

        macro_rules! SP_GO {
            ($step:expr) => {
                sp = unsafe {
                    (*arena_ptr).frame_mut(cur_frame_idx).cur_sp_offset += ($step) as i32;
                    sp.offset(($step) as isize)
                }
            };
        }

        macro_rules! PC_GO {
            ($step:expr) => {
                pc = unsafe { pc.offset(($step) as isize) }
            };
        }
        macro_rules! PC_FETCH {
            (i32) => {
                unsafe {
                    let v = read_from_ptr::<i32>(pc);
                    pc = pc.add(4);
                    v
                }
            };
            (u32) => {
                unsafe {
                    let v = read_from_ptr::<u32>(pc);
                    pc = pc.add(4);
                    v
                }
            };
            (i16) => {
                unsafe {
                    let v = read_from_ptr::<i16>(pc);
                    pc = pc.add(2);
                    v
                }
            };
            (u16) => {
                unsafe {
                    let v = read_from_ptr::<u16>(pc);
                    pc = pc.add(2);
                    v
                }
            };
            (u8) => {
                unsafe {
                    let v = read_from_ptr::<u8>(pc);
                    pc = pc.add(1);
                    v
                }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! ARG_REF {
            ($idx:expr) => {
                unsafe { arg_base_ptr.offset(($idx) as _).as_mut().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! ARG_REF {
            ($idx:expr) => {
                unsafe { &mut *arg_base_ptr.offset(($idx) as _) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! VAR_REF {
        [$idx:expr] => {
            unsafe { (var_base_ptr.offset(($idx) as isize).as_mut().unwrap()) }
        };
    }
        #[cfg(not(debug_assertions))]
        macro_rules! VAR_REF {
        [$idx:expr] => {
            unsafe { (&mut *var_base_ptr.offset(($idx) as isize)) }
        };
    }

        #[cfg(debug_assertions)]
        macro_rules! CPOOL {
        [$idx:expr] => {
            cpools.as_ref()[($idx) as usize]
        };
    }
        #[cfg(not(debug_assertions))]
        macro_rules! CPOOL {
        [$idx:expr] => {
            unsafe {
                *cpools.as_ref().as_ptr().add(($idx) as usize)
            }
        };
    }

        // ── Opcode dispatch loop ──
        // The outer 'arena_loop handles exception catch re-entry.
        // CallBytecode / TailCall / Return return ExecAction to the trampoline.
        // Exception tries local catch and re-enters via continue 'arena_loop.
        'arena_loop: loop {
            let action: ExecAction = loop {
                let opcode = unsafe { std::mem::transmute::<u8, OpCode>(PC_FETCH!(u8)) };

                match opcode {
                    // ═══════════════════════════════════════════════════
                    // PUSH / CONST / VALUE ops
                    // ═══════════════════════════════════════════════════
                    OpCode::PUSH_I32 => {
                        // [^] -> [int_value, ^]
                        let a = PC_FETCH!(i32);
                        SP_PUT!(0, JSValue::from(a));
                        SP_GO!(1);
                    }
                    OpCode::PUSH_CONST => {
                        // [^] -> [const_val, ^]
                        let id = PC_FETCH!(u32);
                        SP_PUT!(0, CPOOL![id]);
                        SP_GO!(1);
                    }
                    OpCode::PUSH_ATOM_VALUE => {
                        let a = PC_FETCH!(u32);
                        SP_PUT!(
                            0,
                            Atom::from_id(a)
                                .to_value(realm_scope)
                                .unwrap_or(JSValue::EXCEPTION)
                        );
                        SP_GO!(1);
                    }
                    OpCode::UNDEFINED => {
                        SP_PUT!(0, JSValue::UNDEFINED);
                        SP_GO!(1);
                    }
                    OpCode::NULL => {
                        SP_PUT!(0, JSValue::NULL);
                        SP_GO!(1);
                    }
                    OpCode::PUSH_THIS => {
                        let val: JSValue = if !mode.contains(ExecMode::STRICT) {
                            if this_obj.is_undefined_or_null() {
                                JSValue::from(realm_scope.global_obj())
                            } else if this_obj.is_object() {
                                this_obj
                            } else {
                                if let Ok(v) = this_obj.objectify(realm_scope) {
                                    v.into()
                                } else {
                                    break ExecAction::Exception;
                                }
                            }
                        } else {
                            this_obj
                        };
                        SP_PUT!(0, val);
                        SP_GO!(1);
                    }
                    OpCode::PUSH_FALSE => {
                        SP_PUT!(0, JSValue::from(false));
                        SP_GO!(1);
                    }
                    OpCode::PUSH_TRUE => {
                        SP_PUT!(0, JSValue::from(true));
                        SP_GO!(1);
                    }

                    // ═══════════════════════════════════════════════════
                    // OBJECT / SPECIAL_OBJECT
                    // ═══════════════════════════════════════════════════
                    OpCode::OBJECT => {
                        // [^] -> [obj, ^]
                        let obj = realm_scope
                            .create_base_object()
                            .map_or(JSValue::EXCEPTION, JSValue::from);
                        SP_PUT!(0, obj);
                        SP_GO!(1);
                        if unlikely(SP_REF!(-1).is_exception()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::SPECIAL_OBJECT => {
                        let id = PC_FETCH!(u8);
                        debug_assert!(
                            (SpecialObject::Arguments as u8..=SpecialObject::ImportMeta as u8)
                                .contains(&id)
                        );
                        match unsafe { std::mem::transmute::<u8, SpecialObject>(id) } {
                            SpecialObject::Arguments => {
                                SP_PUT!(0, js_build_arguments(realm_scope, args));
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break ExecAction::Exception;
                                }
                            }
                            SpecialObject::MappedArguments => {
                                let object_proto = realm_scope.class_proto(JS_CLASS_OBJECT);
                                let mut obj = match realm_scope.create_instance_with_proto(
                                    object_proto,
                                    PayloadPtr::none(JS_CLASS_MAPPED_ARGUMENTS),
                                ) {
                                    Ok(o) => o,
                                    Err(_) => {
                                        break ExecAction::Exception;
                                    }
                                };

                                if obj
                                    .add_own_property(
                                        realm_scope,
                                        Atom::CONST_length,
                                        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                                        JSProperty::Normal(JSValue::from(args.len() as i32)),
                                    )
                                    .is_err()
                                {
                                    break ExecAction::Exception;
                                }

                                let mut abort = false;
                                for i in 0..cmp::min(args.len() as i32, self.arg_count as i32) {
                                    let content = match get_var_ref(
                                        realm_scope,
                                        i as u16,
                                        true,
                                        arg_base_ptr,
                                        var_base_ptr,
                                        arena_mut!(),
                                        cur_frame_idx,
                                        self.arg_count,
                                        self.var_count,
                                    ) {
                                        Ok(vr) => JSProperty::VarRef(vr),
                                        Err(_) => {
                                            abort = true;
                                            break;
                                        }
                                    };
                                    if obj
                                        .add_own_property(
                                            realm_scope,
                                            Atom::new_small_int(i as u32),
                                            JS_PROP_C_W_E | JS_PROP_VARREF,
                                            content,
                                        )
                                        .is_err()
                                    {
                                        abort = true;
                                        break;
                                    }
                                }
                                if abort {
                                    break ExecAction::Exception;
                                }

                                for (i, a) in args
                                    .iter()
                                    .enumerate()
                                    .skip(cmp::min(args.len(), self.arg_count as usize))
                                {
                                    if obj
                                        .define_property_value_u32(
                                            realm_scope,
                                            i as _,
                                            *a,
                                            JS_PROP_C_W_E,
                                        )
                                        .is_err()
                                    {
                                        abort = true;
                                        break;
                                    }
                                }
                                if abort {
                                    break ExecAction::Exception;
                                }

                                if obj
                                    .define_property_value(
                                        realm_scope,
                                        Atom::CONST_Symbol_iterator,
                                        realm_scope.array_proto_values,
                                        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
                                    )
                                    .is_err()
                                {
                                    break ExecAction::Exception;
                                }

                                if obj
                                    .define_property_value(
                                        realm_scope,
                                        Atom::CONST_callee,
                                        JSValue::from(func_obj),
                                        JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
                                    )
                                    .is_err()
                                {
                                    break ExecAction::Exception;
                                }

                                let ma = JSValue::from(obj);
                                SP_PUT!(0, ma);
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break ExecAction::Exception;
                                }
                            }
                            SpecialObject::ThisFunc => {
                                SP_PUT!(0, JSValue::from(func_obj));
                                SP_GO!(1);
                            }
                            SpecialObject::NewTarget => {
                                SP_PUT!(0, new_target);
                                SP_GO!(1);
                            }
                            SpecialObject::HomeObject => {
                                SP_PUT!(
                                    0,
                                    if let Some(ho) = func_home_obj {
                                        JSValue::from(ho)
                                    } else {
                                        JSValue::UNDEFINED
                                    }
                                );
                                SP_GO!(1);
                            }
                            SpecialObject::VarObject => {
                                SP_PUT!(
                                    0,
                                    realm_scope
                                        .create_base_object_with_proto(None)
                                        .map_or(JSValue::EXCEPTION, JSValue::from)
                                );
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break ExecAction::Exception;
                                }
                            }
                            SpecialObject::ImportMeta => {
                                SP_PUT!(
                                    0,
                                    js_import_meta(realm_scope)
                                        .map_or(JSValue::EXCEPTION, JSValue::from)
                                );
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break ExecAction::Exception;
                                }
                            }
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // REST params
                    // ═══════════════════════════════════════════════════
                    OpCode::REST => {
                        let start = PC_FETCH!(u16);
                        if let Ok(arr) = args
                            .iter()
                            .skip(start as usize)
                            .copied()
                            .try_collect::<Vec<_>>()
                            .map_err(|_| realm_scope.throw_out_of_memory())
                            .and_then(|vv| Array::from_values(realm_scope, vv))
                        {
                            SP_PUT!(0, JSValue::from(arr));
                            SP_GO!(1);
                        } else {
                            SP_PUT!(0, JSValue::EXCEPTION);
                            SP_GO!(1);
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Stack manipulation ops
                    // ═══════════════════════════════════════════════════
                    OpCode::DROP => {
                        SP_GO!(-1);
                    }
                    OpCode::NIP => {
                        SP_PUT!(-2, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::NIP1 => {
                        SP_PUT!(-3, SP_VAL!(-2));
                        SP_PUT!(-2, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::DUP => {
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_GO!(1);
                    }
                    OpCode::DUP1 => {
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_PUT!(-1, SP_VAL!(-2));
                        SP_GO!(1);
                    }
                    OpCode::DUP2 => {
                        SP_PUT!(0, SP_VAL!(-2));
                        SP_PUT!(1, SP_VAL!(-1));
                        SP_GO!(2);
                    }
                    OpCode::SWAP => unsafe {
                        std::ptr::swap(sp.offset(-1), sp.offset(-2));
                    },
                    OpCode::SWAP2 => unsafe {
                        std::ptr::swap(sp.offset(-1), sp.offset(-3));
                        std::ptr::swap(sp.offset(-2), sp.offset(-4));
                    },

                    // ═══════════════════════════════════════════════════
                    // Local variable / argument access
                    // ═══════════════════════════════════════════════════
                    OpCode::GET_LOC => {
                        // [^] -> [local_val, ^]
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(0, *VAR_REF![idx]);
                        SP_GO!(1);
                    }
                    OpCode::PUT_LOC => {
                        // [val, ^] -> [^]
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = SP_VAL!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::GET_ARG => {
                        // [^] -> [arg_val, ^]
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(0, *ARG_REF!(idx));
                        SP_GO!(1);
                    }
                    OpCode::PUT_ARG => {
                        let idx = PC_FETCH!(u16);
                        *ARG_REF!(idx) = SP_VAL!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::SET_ARG => {
                        let idx = PC_FETCH!(u16);
                        *ARG_REF![idx] = SP_VAL!(-1);
                    }
                    OpCode::SET_LOC => {
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = SP_VAL!(-1);
                    }
                    OpCode::CLOSE_LOC => {
                        let idx = PC_FETCH!(u16);
                        if let Some(vr) = arena
                            .try_var_refs_mut(cur_frame_idx)
                            .and_then(|vrs| vrs.iter_mut().find(|v| v.is_match(idx, false)))
                        {
                            vr.detach();
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Closure variable refs
                    // ═══════════════════════════════════════════════════
                    OpCode::GET_VAR_REF => {
                        let idx = PC_FETCH!(u16);
                        let val = captured_vars[idx as usize].value();
                        SP_PUT!(0, val);
                        SP_GO!(1);
                    }
                    OpCode::PUT_VAR_REF => {
                        let idx = PC_FETCH!(u16);
                        captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::SET_VAR_REF => {
                        // [value, ^] -> [value, ^]
                        // store stack top value to var_ref
                        let idx = PC_FETCH!(u16);
                        captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                    }
                    OpCode::MAKE_LOC_REF | OpCode::MAKE_ARG_REF | OpCode::MAKE_VAR_REF_REF => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(
                            0,
                            realm_scope
                                .create_base_object_with_proto(None)
                                .map_or(JSValue::EXCEPTION, JSValue::from)
                        );
                        SP_GO!(1);
                        if unlikely(SP_REF!(-1).is_exception()) {
                            break ExecAction::Exception;
                        }

                        let var_ref = if opcode == OpCode::MAKE_VAR_REF_REF {
                            Ok(captured_vars[idx as usize])
                        } else {
                            get_var_ref(
                                realm_scope,
                                idx,
                                opcode == OpCode::MAKE_ARG_REF,
                                arg_base_ptr,
                                var_base_ptr,
                                arena_mut!(),
                                cur_frame_idx,
                                self.arg_count,
                                self.var_count,
                            )
                        };
                        match var_ref {
                            Ok(vr) => {
                                if unsafe { SP_MUT!(-1).as_object_unchecked() }
                                    .add_own_property(
                                        realm_scope,
                                        atom,
                                        JS_PROP_WRITABLE | JS_PROP_VARREF,
                                        JSProperty::VarRef(vr),
                                    )
                                    .is_err()
                                {
                                    break ExecAction::Exception;
                                } else {
                                    SP_PUT!(
                                        0,
                                        atom.to_value(realm_scope).unwrap_or(JSValue::EXCEPTION)
                                    );
                                    SP_GO!(1);
                                }
                            }
                            Err(_) => {
                                break ExecAction::Exception;
                            }
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Type conversion / unary operations
                    // ═══════════════════════════════════════════════════
                    OpCode::PLUS | OpCode::NEG | OpCode::INC | OpCode::DEC => {
                        let op1 = SP_VAL!(-1);
                        if opcode == OpCode::PLUS && op1.is_number() {
                            // + before an int or float: do nothing
                            SP_PUT!(-1, op1);
                        } else if opcode == OpCode::PLUS && op1.is_bigint() {
                            // unary `+` on a BigInt throws (ToNumber)
                            let _ = realm_scope
                                .throw_type_error("Cannot convert a BigInt value to a number");
                            break ExecAction::Exception;
                        } else if let Ok(res) =
                            UnaryOp::new(realm_scope, op1, true).and_then(|unary| match opcode {
                                OpCode::PLUS => unary.plus(realm_scope),
                                OpCode::NEG => unary.neg(realm_scope),
                                OpCode::INC => unary.inc(realm_scope),
                                OpCode::DEC => unary.dec(realm_scope),
                                _ => unreachable!(),
                            })
                        {
                            SP_PUT!(-1, res);
                        } else {
                            SP_PUT!(-1, JSValue::UNDEFINED);
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::TO_PROPKEY => {
                        let key = SP_REF!(-1);
                        if !key.is_string_or_symbol() && !key.is_int() {
                            if let Ok(pk) = key.to_propkey(realm_scope) {
                                SP_PUT!(-1, pk);
                            } else {
                                break ExecAction::Exception;
                            }
                        }
                    }
                    OpCode::TO_OBJECT => {
                        if !SP_REF!(-1).is_object() {
                            if let Ok(o) = SP_VAL!(-1).objectify(realm_scope) {
                                SP_PUT!(-1, JSValue::from(o));
                            } else {
                                break ExecAction::Exception;
                            }
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // CALL / RETURN
                    // ═══════════════════════════════════════════════════
                    OpCode::CALL | OpCode::TAIL_CALL => {
                        // [func_obj, arg1, ..., argN, ^] -> [ret_val, ^]
                        let call_argc = PC_FETCH!(u16);
                        let call_argv = unsafe { sp.offset(-(call_argc as isize)) };

                        save_pc!(pc.offset_from(pc_base) as _);

                        let func = unsafe { *call_argv.offset(-1) };

                        // ── Inline bytecode dispatch ──
                        if let Ok(bfun) = BCWrapper::try_from(func) {
                            let bc = bfun.payload().bytecode();
                            let func_kind = bc.func_kind();

                            // Generator and async functions need their specific
                            // class_call path (creates AGenPayload/AsyncState and
                            // runs INITIAL_YIELD). Only plain (non-generator)
                            // bytecode functions can use the inline frame setup.
                            if func_kind == FunctionKind::Normal {
                                // init_frame_args uses max(bc.arg_count, actual_args)
                                // as the arg region size. Allocate accordingly to
                                // prevent buffer overflow into adjacent frames.
                                //
                                // N.B. buf_cap is now computed inside
                                // build_callee_frame; we no longer need it here.

                                // Init callee frame data from stack args.
                                let args_slice = unsafe {
                                    std::slice::from_raw_parts(call_argv, call_argc as usize)
                                };

                                if unlikely(opcode == OpCode::TAIL_CALL) {
                                    // Tail call: the trampoline will free the current
                                    // frame and allocate the callee frame using
                                    // build_callee_frame, so we only need to pass
                                    // the copied arguments.
                                    //
                                    // Cross-realm tail calls skip TCO: the TailCall
                                    // handler reuses the current TrampolineScope, so
                                    // a cross-realm callee would run against the
                                    // wrong realm (globals, error prototypes). Fall
                                    // through to call_internal instead — the nested
                                    // trampoline creates a scope bound to the
                                    // callee's own realm.
                                    if bc.realm() == realm_scope.ctx_ptr {
                                        break ExecAction::TailCall {
                                            callee_bc: bc,
                                            callee_func_obj: bfun.into(),
                                            args: args_slice.to_vec(),
                                            this_obj: JSValue::UNDEFINED,
                                        };
                                    }
                                } else {
                                    // Non-tail call: use build_callee_frame to
                                    // allocate and initialize the callee frame.
                                    let caller_depth = frame!(cur_frame_idx).depth;
                                    let callee_frame_idx = match build_callee_frame(
                                        arena_mut!(),
                                        caller_depth,
                                        bfun.into(),
                                        bc,
                                        args_slice,
                                    ) {
                                        Ok(idx) => idx,
                                        Err(e) => {
                                            match e {
                                                FrameAllocError::Oom => {
                                                    let _ = realm_scope.throw_stack_fail();
                                                }
                                                FrameAllocError::Exceeded => {
                                                    let _ = realm_scope.throw_stack_exceeded();
                                                }
                                            }
                                            // Route through the Exception
                                            // handler: find_catch_in_frame must
                                            // scan (and cleanse) this frame's
                                            // operand stack first. A bare Unwind
                                            // would let find_finally_in_frame
                                            // misread stray Ints (e.g. `1 + f()`)
                                            // as GOSUB markers and jump to a
                                            // garbage pc.
                                            break ExecAction::Exception;
                                        }
                                    };
                                    frame_mut!(callee_frame_idx).prev = cur_frame_idx;

                                    let return_sp = unsafe {
                                        sp.offset(-(call_argc as isize + 1)).offset_from(sp_base)
                                    } as usize;

                                    // Detect cross-realm calls: if the callee's realm
                                    // differs from the current realm, use the
                                    // CallBytecodeCrossRealm path so the trampoline
                                    // can properly switch GC scopes.
                                    let callee_ctx = bc.realm();
                                    break if callee_ctx != realm_scope.ctx_ptr {
                                        ExecAction::CallBytecodeCrossRealm {
                                            callee_bc: bc,
                                            callee_frame_idx,
                                            callee_ctx_ptr: callee_ctx,
                                            return_sp,
                                            this_obj: JSValue::UNDEFINED,
                                            new_target: JSValue::UNDEFINED,
                                            ctor_this: None,
                                        }
                                    } else {
                                        ExecAction::CallBytecode {
                                            callee_bc: bc,
                                            callee_frame_idx,
                                            return_sp,
                                            this_obj: JSValue::UNDEFINED,
                                            new_target: JSValue::UNDEFINED,
                                            ctor_this: None,
                                        }
                                    };
                                }
                            }

                            // Generator/async function: fall through to
                            // call_internal for proper class_call handling.
                        }

                        // Native function or non-Normal bytecode: use call_internal.
                        // call_internal internally preserves active_trampoline_frame.
                        let ret_val = func.call_internal(
                            realm_scope,
                            JSValue::UNDEFINED,
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if let Ok(r) = ret_val {
                            if unlikely(opcode == OpCode::TAIL_CALL) {
                                break ExecAction::Return(r, false);
                            } else {
                                SP_GO!(-(call_argc as i32 + 1));
                                SP_PUT!(0, r);
                                SP_GO!(1);
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::CALL_CONSTRUCTOR => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        save_pc!(pc.offset_from(pc_base) as _);

                        let func = unsafe { *call_argv.offset(-2) };
                        let ctor_new_target = unsafe { *call_argv.offset(-1) };

                        // ── Inline bytecode dispatch (mirrors CALL) ──
                        // Constructor chains (e.g. 100-level `super()` chains)
                        // must not recurse on the C stack through call_internal.
                        // Non-constructors fall through to call_internal, which
                        // throws the proper TypeError.
                        if let Ok(bfun) = BCWrapper::try_from(func)
                            && bfun.flags().is_constructor()
                            && bfun.payload().bytecode().func_kind() == FunctionKind::Normal
                        {
                            let bc = bfun.payload().bytecode();

                            // Legacy (non-derived) constructor: create the
                            // instance at the call site; on frame return a
                            // non-object result is replaced with the instance
                            // (mirrors do_call_internal). Derived constructors
                            // start with `this` uninitialized (TDZ until
                            // super() returns).
                            let (callee_this, ctor_this) = if !bc.attrs.is_derived_class_constructor
                            {
                                match realm_scope.ordinary_create_from_constructor_payload(
                                    ctor_new_target,
                                    PayloadPtr::none(JS_CLASS_OBJECT),
                                ) {
                                    Ok(o) => {
                                        let v = JSValue::from(o);
                                        (v, Some(v))
                                    }
                                    Err(_) => {
                                        let _ = realm_scope
                                            .throw_type_error("constructor must return an object");
                                        break ExecAction::Exception;
                                    }
                                }
                            } else {
                                (JSValue::UNDEFINED, None)
                            };

                            let args_slice = unsafe {
                                std::slice::from_raw_parts(call_argv, call_argc as usize)
                            };
                            let caller_depth = frame!(cur_frame_idx).depth;
                            let callee_frame_idx = match build_callee_frame(
                                arena_mut!(),
                                caller_depth,
                                bfun.into(),
                                bc,
                                args_slice,
                            ) {
                                Ok(idx) => idx,
                                Err(e) => {
                                    match e {
                                        FrameAllocError::Oom => {
                                            let _ = realm_scope.throw_stack_fail();
                                        }
                                        FrameAllocError::Exceeded => {
                                            let _ = realm_scope.throw_stack_exceeded();
                                        }
                                    }
                                    // Route through the Exception handler: see
                                    // the matching comment in the CALL branch.
                                    break ExecAction::Exception;
                                }
                            };
                            frame_mut!(callee_frame_idx).prev = cur_frame_idx;

                            let return_sp = unsafe {
                                sp.offset(-(call_argc as isize + 2)).offset_from(sp_base)
                            } as usize;

                            let callee_ctx = bc.realm();

                            break if callee_ctx != realm_scope.ctx_ptr {
                                ExecAction::CallBytecodeCrossRealm {
                                    callee_bc: bc,
                                    callee_frame_idx,
                                    callee_ctx_ptr: callee_ctx,
                                    return_sp,
                                    this_obj: callee_this,
                                    new_target: ctor_new_target,
                                    ctor_this,
                                }
                            } else {
                                ExecAction::CallBytecode {
                                    callee_bc: bc,
                                    callee_frame_idx,
                                    return_sp,
                                    this_obj: callee_this,
                                    new_target: ctor_new_target,
                                    ctor_this,
                                }
                            };
                        }

                        // Native function, non-Normal bytecode or non-constructor:
                        // use call_internal.
                        let ret_val = func.call_internal(
                            realm_scope,
                            JSValue::UNDEFINED,
                            ctor_new_target,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            JS_CALL_FLAG_CONSTRUCTOR,
                        );
                        if let Ok(val) = ret_val {
                            SP_GO!(-(call_argc + 2));
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::CALL_METHOD | OpCode::TAIL_CALL_METHOD => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        save_pc!(pc.offset_from(pc_base) as _);

                        let func = unsafe { *call_argv.offset(-1) };
                        let method_this = unsafe { *call_argv.offset(-2) };

                        // ── Inline bytecode dispatch (mirrors CALL) ──
                        // Method calls must not recurse on the C stack either
                        // (e.g. deep `o.m()` recursion or `super.m()` chains).
                        if let Ok(bfun) = BCWrapper::try_from(func) {
                            let bc = bfun.payload().bytecode();

                            if bc.func_kind() == FunctionKind::Normal {
                                let args_slice = unsafe {
                                    std::slice::from_raw_parts(call_argv, call_argc as usize)
                                };

                                if unlikely(opcode == OpCode::TAIL_CALL_METHOD) {
                                    // Cross-realm tail calls skip TCO (same reason
                                    // as TAIL_CALL above): fall through to the
                                    // call_internal fallback, which runs the callee
                                    // under its own realm scope and returns via
                                    // ExecAction::Return.
                                    if bc.realm() == realm_scope.ctx_ptr {
                                        break ExecAction::TailCall {
                                            callee_bc: bc,
                                            callee_func_obj: bfun.into(),
                                            args: args_slice.to_vec(),
                                            this_obj: method_this,
                                        };
                                    }
                                } else {
                                    let caller_depth = frame!(cur_frame_idx).depth;
                                    let callee_frame_idx = match build_callee_frame(
                                        arena_mut!(),
                                        caller_depth,
                                        bfun.into(),
                                        bc,
                                        args_slice,
                                    ) {
                                        Ok(idx) => idx,
                                        Err(e) => {
                                            match e {
                                                FrameAllocError::Oom => {
                                                    let _ = realm_scope.throw_stack_fail();
                                                }
                                                FrameAllocError::Exceeded => {
                                                    let _ = realm_scope.throw_stack_exceeded();
                                                }
                                            }

                                            // Route through the Exception
                                            // handler: find_catch_in_frame must
                                            // scan (and cleanse) this frame's
                                            // operand stack first. A bare Unwind
                                            // would let find_finally_in_frame
                                            // misread stray Ints (e.g. `1 + f()`)
                                            // as GOSUB markers and jump to a
                                            // garbage pc.
                                            break ExecAction::Exception;
                                        }
                                    };
                                    frame_mut!(callee_frame_idx).prev = cur_frame_idx;

                                    let return_sp = unsafe {
                                        sp.offset(-(call_argc as isize + 2)).offset_from(sp_base)
                                    } as usize;

                                    let callee_ctx = bc.realm();
                                    break if callee_ctx != realm_scope.ctx_ptr {
                                        ExecAction::CallBytecodeCrossRealm {
                                            callee_bc: bc,
                                            callee_frame_idx,
                                            callee_ctx_ptr: callee_ctx,
                                            return_sp,
                                            this_obj: method_this,
                                            new_target: JSValue::UNDEFINED,
                                            ctor_this: None,
                                        }
                                    } else {
                                        ExecAction::CallBytecode {
                                            callee_bc: bc,
                                            callee_frame_idx,
                                            return_sp,
                                            this_obj: method_this,
                                            new_target: JSValue::UNDEFINED,
                                            ctor_this: None,
                                        }
                                    };
                                }
                            }

                            // Generator/async function: fall through to
                            // call_internal for proper class_call handling.
                        }

                        // Native function or non-Normal bytecode: use call_internal.
                        let ret_val = func.call_internal(
                            realm_scope,
                            method_this,
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if let Ok(r) = ret_val {
                            if unlikely(opcode == OpCode::TAIL_CALL_METHOD) {
                                break ExecAction::Return(r, false);
                            } else {
                                SP_GO!(-(call_argc + 2));
                                SP_PUT!(0, r);
                                SP_GO!(1);
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::RETURN => {
                        SP_GO!(-1);
                        let ret = SP_VAL!(0);
                        break ExecAction::Return(ret, false);
                    }
                    OpCode::RETURN_UNDEF => {
                        break ExecAction::Return(JSValue::UNDEFINED, false);
                    }

                    // ═══════════════════════════════════════════════════
                    // Unary / comparison / binary logic
                    // ═══════════════════════════════════════════════════
                    OpCode::POST_INC | OpCode::POST_DEC => {
                        let op1 = SP_VAL!(-1);
                        if op1.is_bigint() {
                            // bigint: ToNumeric is the value itself; the old
                            // value is returned and the new value stored.
                            let res = UnaryOp::new(realm_scope, op1, true).and_then(|unary| {
                                if opcode == OpCode::POST_INC {
                                    unary.inc(realm_scope)
                                } else {
                                    unary.dec(realm_scope)
                                }
                            });
                            match res {
                                Ok(r) => {
                                    SP_PUT!(-1, op1);
                                    SP_PUT!(0, r);
                                    SP_GO!(1);
                                }
                                Err(_) => break ExecAction::Exception,
                            }
                        } else if let Ok(n1) = op1.to_number(realm_scope) {
                            SP_PUT!(-1, JSValue::from(n1));
                            if let Ok(res) = UnaryOp::new(realm_scope, JSValue::from(n1), true)
                                .and_then(|unary| {
                                    if opcode == OpCode::POST_INC {
                                        unary.inc(realm_scope)
                                    } else {
                                        unary.dec(realm_scope)
                                    }
                                })
                            {
                                SP_PUT!(0, res);
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::INC_LOC | OpCode::DEC_LOC => {
                        let idx = PC_FETCH!(u8) as i32;
                        let op1 = VAR_REF![idx];
                        if let Ok(res) =
                            UnaryOp::new(realm_scope, *op1, true).and_then(|unary| match opcode {
                                OpCode::INC_LOC => unary.inc(realm_scope),
                                OpCode::DEC_LOC => unary.dec(realm_scope),
                                _ => unreachable!(),
                            })
                        {
                            *op1 = res;
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::ADD_LOC => {
                        let idx = PC_FETCH!(u8) as i32;
                        let op1 = SP_VAL!(-1);
                        SP_GO!(-1);
                        let pv = VAR_REF![idx];
                        if let Ok(val) = JSValue::arith_add(realm_scope, *pv, op1) {
                            *pv = val;
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::NOT => {
                        if let Ok(res) = UnaryOp::new_int(realm_scope, SP_VAL!(-1))
                            .and_then(|unary| unary.not(realm_scope))
                        {
                            SP_PUT!(-1, res);
                        } else {
                            SP_PUT!(-1, JSValue::UNDEFINED);
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::SHL
                    | OpCode::SHR
                    | OpCode::SAR
                    | OpCode::AND
                    | OpCode::OR
                    | OpCode::XOR => {
                        if let Ok(new_sp) = op_binary_logic(realm_scope, opcode, sp) {
                            sp = new_sp;
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::LT | OpCode::LTE | OpCode::GT | OpCode::GTE => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        if let Ok(rel) = op1.arith_cmp(realm_scope, op2) {
                            let b = matches!(
                                (rel, opcode),
                                (Some(std::cmp::Ordering::Less), OpCode::LT | OpCode::LTE)
                                    | (Some(std::cmp::Ordering::Greater), OpCode::GT | OpCode::GTE)
                                    | (Some(std::cmp::Ordering::Equal), OpCode::LTE | OpCode::GTE)
                            );
                            SP_PUT!(-2, JSValue::from(b));
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::EQ | OpCode::NEQ => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        if let Ok(b) = op1.arith_eq(realm_scope, op2) {
                            SP_PUT!(-2, JSValue::from(b ^ (opcode == OpCode::NEQ)));
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::STRICT_EQ | OpCode::STRICT_NEQ => {
                        let mut b = SP_VAL!(-2).strict_eq(SP_VAL!(-1));
                        if opcode == OpCode::STRICT_NEQ {
                            b = !b;
                        }
                        SP_PUT!(-2, JSValue::from(b));
                        SP_GO!(-1);
                    }
                    OpCode::IN => {
                        if let Ok(b) = js_operator_in(realm_scope, SP_REF!(-1), SP_REF!(-2)) {
                            SP_PUT!(-2, JSValue::from(b));
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PRIVATE_IN => {
                        if unlikely(
                            crate::bytecode::operators::js_operator_private_in(realm_scope, sp)
                                .is_err(),
                        ) {
                            break ExecAction::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::INSTANCEOF => {
                        if unlikely(js_operator_instanceof(realm_scope, sp).is_err()) {
                            break ExecAction::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::TYPEOF => {
                        let op1 = SP_VAL!(-1);
                        let atom = op1.type_of();
                        SP_PUT!(
                            -1,
                            atom.force_string(realm_scope)
                                .map_or(JSValue::EXCEPTION, JSValue::make_string)
                        );
                    }
                    OpCode::DELETE => {
                        if unlikely(js_operator_delete(realm_scope, sp) != 0) {
                            break ExecAction::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Global variable access (var_ref indirect via captured_vars)
                    // ═══════════════════════════════════════════════════
                    OpCode::GET_VAR_UNDEF | OpCode::GET_VAR => {
                        let idx = PC_FETCH!(u16) as usize;
                        // resolve the global by name on global_var_obj/global_obj at each access. No VarRef cell.
                        let var_name = self.global_var_refs()[idx].var_name;
                        if let Ok(v) =
                            realm_scope.get_global_var(var_name, opcode == OpCode::GET_VAR)
                        {
                            SP_PUT!(0, v);
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PUT_VAR | OpCode::PUT_VAR_INIT => {
                        let idx = PC_FETCH!(u16) as usize;
                        // set_global_var handles TDZ (ReferenceError) and read-only (TypeError) checks.
                        let var_name = self.global_var_refs()[idx].var_name;
                        let res = realm_scope.set_global_var(
                            var_name,
                            SP_VAL!(-1),
                            if opcode == OpCode::PUT_VAR_INIT {
                                GlobalVarSetMode::Init
                            } else {
                                GlobalVarSetMode::Normal
                            },
                        );
                        SP_GO!(-1);

                        if res.is_err() {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::DELETE_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        match realm_scope
                            .global_obj()
                            .delete_property(realm_scope, atom, 0)
                        {
                            Ok(ret) => {
                                SP_PUT!(0, JSValue::from(ret));
                                SP_GO!(1);
                            }
                            Err(_) => {
                                break ExecAction::Exception;
                            }
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Local variable check / init
                    // ═══════════════════════════════════════════════════
                    OpCode::SET_LOC_UNINITIALIZED => {
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = JSValue::UNINITIALIZED;
                    }
                    OpCode::GET_LOC_CHECK | OpCode::GET_LOC_CHECKTHIS => {
                        let idx = PC_FETCH!(u16);
                        let value = VAR_REF![idx];
                        if unlikely(value.is_uninitialized()) {
                            // Per ES spec, this error belongs in the caller's realm
                            // (same pattern as CHECK_CTOR_RETURN).
                            let prev = frame!(cur_frame_idx).prev;
                            if !prev.is_null() {
                                let prev_func = frame!(prev).func_obj;
                                if let Ok(bfun) = BCWrapper::try_from(JSValue::from(prev_func)) {
                                    let mut caller_ctx_ptr = bfun.payload().bytecode().realm();
                                    let mut caller_scope = caller_ctx_ptr.new_scope();
                                    let _ = caller_scope
                                        .throw_reference_error_uninitialized2(self, idx, false);
                                    // caller_scope dropped → GC scope popped
                                    break ExecAction::Exception;
                                }
                            }

                            let _ =
                                realm_scope.throw_reference_error_uninitialized2(self, idx, false);
                            break ExecAction::Exception;
                        } else {
                            SP_PUT!(0, *value);
                            SP_GO!(1);
                        }
                    }
                    OpCode::PUT_LOC_CHECK => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(pval.is_uninitialized()) {
                            let _ =
                                realm_scope.throw_reference_error_uninitialized2(self, idx, false);
                            break ExecAction::Exception;
                        } else {
                            *pval = SP_VAL!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PUT_LOC_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(!pval.is_uninitialized()) {
                            let _ = realm_scope
                                .throw_reference_error("'this' can be initialized only once");
                            break ExecAction::Exception;
                        } else {
                            *pval = SP_VAL!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::SET_LOC_CHECK => {
                        // Sets a local variable with initialization check.
                        // Unlike PUT_LOC_CHECK (n_pop=1, n_push=0), this keeps the
                        // value on the stack (n_pop=1, n_push=1) so the assignment
                        // expression result is available as a value.
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(pval.is_uninitialized()) {
                            let _ =
                                realm_scope.throw_reference_error_uninitialized2(self, idx, false);
                            break ExecAction::Exception;
                        }
                        *pval = SP_VAL!(-1);
                        // Value stays on the stack for assignment expression result
                    }
                    OpCode::GET_VAR_REF_CHECK | OpCode::PUT_VAR_REF_CHECK => {
                        let idx = PC_FETCH!(u16);
                        let val = captured_vars[idx as usize].value();
                        if unlikely(val.is_uninitialized()) {
                            let _ =
                                realm_scope.throw_reference_error_uninitialized2(self, idx, true);
                            break ExecAction::Exception;
                        } else if opcode == OpCode::GET_VAR_REF_CHECK {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PUT_VAR_REF_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        if unlikely(!captured_vars[idx as usize].value().is_uninitialized()) {
                            let _ =
                                realm_scope.throw_reference_error_uninitialized2(self, idx, true);
                            break ExecAction::Exception;
                        } else {
                            captured_vars[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                            SP_GO!(-1);
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Constructor / brand / throw
                    // ═══════════════════════════════════════════════════
                    OpCode::CHECK_CTOR_RETURN => {
                        if !SP_REF!(-1).is_object() {
                            if unlikely(!SP_REF!(-1).is_undefined()) {
                                // Per ES [[Construct]], throw TypeError in the CALLER's realm,
                                // not the current function's realm.
                                // Retrive caller's realm from the prev frame.
                                let prev = frame!(cur_frame_idx).prev;
                                if !prev.is_null() {
                                    let prev_func = frame!(prev).func_obj;
                                    if let Ok(bfun) = BCWrapper::try_from(JSValue::from(prev_func))
                                    {
                                        let mut caller_ctx_ptr = bfun.payload().bytecode().realm();
                                        let mut caller_scope = caller_ctx_ptr.new_scope();
                                        let _ = caller_scope.throw_type_error(
                                            "derived class constructor must return \
                                             an object or undefined",
                                        );
                                        // caller_scope dropped → GC scope popped
                                        break ExecAction::Exception;
                                    }
                                }

                                // Fallback: no caller frame available.
                                let _ = realm_scope.throw_type_error(
                                    "derived class constructor must return an object or undefined",
                                );
                                break ExecAction::Exception;
                            } else {
                                SP_PUT!(0, JSValue::from(true));
                                SP_GO!(1);
                            }
                        } else {
                            SP_PUT!(0, JSValue::from(false));
                            SP_GO!(1);
                        }
                    }
                    OpCode::CHECK_CTOR => {
                        if new_target.is_undefined() {
                            let _ = realm_scope
                                .throw_type_error("class constructors must be invoked with 'new'");
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::INIT_CTOR => {
                        // Calls the super constructor and pushes the `this` object.
                        // Used at the start of derived class constructors,
                        // equivalent to `super(...arguments)`.
                        if new_target.is_undefined() {
                            let _ = realm_scope
                                .throw_type_error("class constructors must be invoked with 'new'");
                            break ExecAction::Exception;
                        }
                        // Get the super constructor from func_obj's prototype
                        let func_obj_val = JSValue::from(func_obj);
                        if let Ok(super_ctor) = func_obj_val.prototype(realm_scope) {
                            // Call the super constructor with all current args
                            let ret_val = super_ctor.call_internal(
                                realm_scope,
                                JSValue::UNDEFINED,
                                new_target,
                                args,
                                JS_CALL_FLAG_CONSTRUCTOR,
                            );
                            if let Ok(val) = ret_val {
                                SP_PUT!(0, val);
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::CHECK_BRAND => {
                        if let Ok(b) = SP_REF!(-2).check_brand(realm_scope, SP_VAL!(-1)) {
                            if unlikely(!b) {
                                let _ = realm_scope.throw_type_error("invalid brand on object");
                                break ExecAction::Exception;
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::ADD_BRAND => {
                        let mut func_obj = SP_VAL!(-2);
                        let home_object = SP_VAL!(-1);
                        SP_GO!(-2);
                        if unlikely(func_obj.add_brand(realm_scope, home_object).is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::THROW => {
                        SP_GO!(-1);
                        let _ = realm_scope.throw_value(SP_VAL!(0));
                        break ExecAction::Exception;
                    }
                    OpCode::THROW_ERROR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let type_ = PC_FETCH!(u8);
                        match type_ {
                            JS_THROW_VAR_RO => {
                                let _ = realm_scope.throw_type_error_read_only(atom);
                            }
                            JS_THROW_VAR_REDECL => {
                                let _ = JS_ThrowSyntaxErrorVarRedeclaration(realm_scope, atom);
                            }
                            JS_THROW_VAR_UNINITIALIZED => {
                                let _ = JS_ThrowReferenceErrorUninitialized(realm_scope, atom);
                            }
                            JS_THROW_ERROR_DELETE_SUPER => {
                                let _ = realm_scope
                                    .throw_reference_error("unsupported reference to 'super'");
                            }
                            JS_THROW_ERROR_ITERATOR_THROW => {
                                let _ = realm_scope
                                    .throw_type_error("iterator does not have a throw method");
                            }
                            _ => {
                                let _ = realm_scope.throw_internal_error(&format!(
                                    "invalid throw var type {type_}"
                                ));
                            }
                        }
                        break ExecAction::Exception;
                    }

                    // ═══════════════════════════════════════════════════
                    // Array / Apply
                    // ═══════════════════════════════════════════════════
                    OpCode::ARRAY_FROM => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        if let Ok(mut list) = alloc_fast_array_slots(realm_scope, call_argc as _) {
                            let call_argv = unsafe { sp.offset(-call_argc as isize) };
                            for i in 0..call_argc as usize {
                                let val = unsafe { *call_argv.add(i) };
                                list.push(val);
                            }
                            if let Ok(arr) = crate::array::Array::from_values(realm_scope, list) {
                                SP_GO!(-call_argc);
                                SP_PUT!(0, JSValue::from(arr));
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::APPLY => {
                        let magic = PC_FETCH!(u16) as i32;
                        let op_3 = SP_VAL!(-3);
                        let op_2 = SP_VAL!(-2);
                        let op_1 = SP_VAL!(-1);
                        SP_GO!(-3);
                        if let Ok(res) = js_function_apply(
                            realm_scope,
                            CallContext::new(op_3, &[op_2, op_1], magic, None),
                        ) {
                            SP_PUT!(0, res);
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Eval
                    // ═══════════════════════════════════════════════════
                    OpCode::EVAL | OpCode::TAIL_EVAL => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let scope_idx = PC_FETCH!(u16) as i32 - 1;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        save_pc!(pc.offset_from(pc_base) as _);
                        let func_val = unsafe { *call_argv.offset(-1) };
                        if func_val.is_same(realm_scope.eval_func) {
                            // ── Direct eval (intrinsic %eval%) ──
                            let ret_val = if call_argc >= 1 {
                                let obj = unsafe { &*call_argv };
                                obj.eval_object(
                                    realm_scope,
                                    cur_frame_idx,
                                    JSValue::UNDEFINED,
                                    EvalType::Direct,
                                    EvalFlags::empty(),
                                    scope_idx as _,
                                )
                            } else {
                                Ok(JSValue::UNDEFINED)
                            };
                            SP_GO!(-(call_argc + 1));
                            if let Ok(v) = ret_val {
                                SP_PUT!(0, v);
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            // ── eval was reassigned — treat as normal function call ──
                            // Use inline bytecode dispatch (same as CALL handler) to
                            // avoid C stack recursion through call_internal.
                            // Since EVAL in `return eval(...)` position is semantically
                            // a tail call (the result is immediately returned), use
                            // TailCall to avoid VirtualFrame push — this prevents C stack
                            // growth for deep recursion like tco-non-eval-function-dynamic.
                            if let Ok(bfun) = BCWrapper::try_from(func_val) {
                                let bc = bfun.payload().bytecode();
                                let func_kind = bc.func_kind();
                                if func_kind == FunctionKind::Normal {
                                    let args_slice = unsafe {
                                        std::slice::from_raw_parts(call_argv, call_argc as usize)
                                    };

                                    // TAIL_EVAL: the eval call is in tail position
                                    // (`return eval(...)`), so replace the current
                                    // frame instead of pushing one. Cross-realm tail
                                    // calls skip TCO (same reason as TAIL_CALL).
                                    if unlikely(opcode == OpCode::TAIL_EVAL)
                                        && bc.realm() == realm_scope.ctx_ptr
                                    {
                                        break ExecAction::TailCall {
                                            callee_bc: bc,
                                            callee_func_obj: bfun.into(),
                                            args: args_slice.to_vec(),
                                            this_obj: JSValue::UNDEFINED,
                                        };
                                    }

                                    // Treat as a normal non-tail call: push a fresh callee
                                    // frame via build_callee_frame. We must NOT use TailCall
                                    // here — `OP_eval` is a single opcode and the runtime
                                    // handler cannot tell whether the call is in tail position
                                    // (it often is not, e.g. `try { eval(); }`). A TailCall would
                                    // recycle the *current* frame, which is wrong when the call
                                    // is not a tail call and fatal when the current frame is the
                                    // outermost script frame (it would be freed, then the later
                                    // unwind dereferences a stale/empty frame -> panic in
                                    // BytecodeFunction::call). build_callee_frame enforces the
                                    // call-depth and byte limits itself, so runaway recursion is
                                    // still caught (and the error remains catchable).
                                    let caller_depth = frame!(cur_frame_idx).depth;
                                    let callee_frame_idx = match build_callee_frame(
                                        arena_mut!(),
                                        caller_depth,
                                        bfun.into(),
                                        bc,
                                        args_slice,
                                    ) {
                                        Ok(idx) => idx,
                                        Err(e) => {
                                            match e {
                                                FrameAllocError::Oom => {
                                                    let _ = realm_scope.throw_stack_fail();
                                                }
                                                FrameAllocError::Exceeded => {
                                                    let _ = realm_scope.throw_stack_exceeded();
                                                }
                                            }

                                            // Route through the Exception
                                            // handler: find_catch_in_frame must
                                            // scan (and cleanse) this frame's
                                            // operand stack first. A bare Unwind
                                            // would let find_finally_in_frame
                                            // misread stray Ints (e.g. `1 + f()`)
                                            // as GOSUB markers and jump to a
                                            // garbage pc.
                                            break ExecAction::Exception;
                                        }
                                    };
                                    frame_mut!(callee_frame_idx).prev = cur_frame_idx;

                                    let return_sp = unsafe {
                                        sp.offset(-(call_argc as isize + 1)).offset_from(sp_base)
                                    } as usize;

                                    break ExecAction::CallBytecode {
                                        callee_bc: bc,
                                        callee_frame_idx,
                                        return_sp,
                                        this_obj: JSValue::UNDEFINED,
                                        new_target: JSValue::UNDEFINED,
                                        ctor_this: None,
                                    };
                                }
                            }

                            // Generator/async bytecode or native function: fall through to call_internal.
                            let ret_val = func_val.call_internal(
                                realm_scope,
                                JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                                unsafe {
                                    std::slice::from_raw_parts(call_argv, call_argc as usize)
                                },
                                0,
                            );
                            SP_GO!(-(call_argc + 1));
                            if let Ok(v) = ret_val {
                                SP_PUT!(0, v);
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                        }
                    }
                    OpCode::APPLY_EVAL => {
                        let var_scope_idx = PC_FETCH!(u16) as i32 - 1;
                        let func_val = SP_REF!(-2);
                        if let Ok(arg_list) = build_arg_list(realm_scope, SP_VAL!(-1)) {
                            let ret_val = {
                                if func_val.is_same(realm_scope.eval_func) {
                                    if !arg_list.is_empty() {
                                        arg_list[0].eval_object(
                                            realm_scope,
                                            cur_frame_idx,
                                            JSValue::UNDEFINED,
                                            EvalType::Direct,
                                            EvalFlags::empty(),
                                            var_scope_idx,
                                        )
                                    } else {
                                        Ok(JSValue::UNDEFINED)
                                    }
                                } else {
                                    func_val.call(realm_scope, JSValue::UNDEFINED, &arg_list)
                                }
                            };
                            if let Ok(v) = ret_val {
                                SP_GO!(-2);
                                SP_PUT!(0, v);
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Property access
                    // ═══════════════════════════════════════════════════
                    OpCode::LNOT => {
                        let val: JSValue = SP_VAL!(-1);
                        let b = if val.is_int_or_bool() {
                            val.payload_i32() != 0
                        } else if let Ok(b) = val.to_bool() {
                            b
                        } else {
                            break ExecAction::Exception;
                        };
                        SP_PUT!(-1, JSValue::from(!b));
                    }

                    OpCode::GET_FIELD | OpCode::GET_FIELD2 => {
                        // get_field: [target, ^] -> [value, ^]
                        // get_field2: [target, ^] -> [target, value, ^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        #[cfg(feature = "inline_cache")]
                        let ic_idx = PC_FETCH!(u16) as usize;

                        let target = SP_VAL!(-1);

                        #[cfg(feature = "inline_cache")]
                        {
                            // IC fast path
                            let ic_cache = unsafe { &*self.inline_cache.get() };

                            if ic_idx < ic_cache.len()
                                && let Some(o) = target.as_object()
                                && let Some(shape) = o.shape()
                            {
                                let entry = &ic_cache[ic_idx];
                                let shape_ptr = unsafe { shape.gc_ref().as_ptr().as_ptr() };

                                if entry.shape_ptr == shape_ptr && entry.shape_uid == shape.uid {
                                    let slot = entry.slot as usize;
                                    // delete marks slot Deleted without changing shape
                                    if !matches!(o.prop_slot(slot), JSProperty::Deleted) {
                                        if let Ok(val) =
                                            o.prop_slot(slot).value(realm_scope, o.into())
                                        {
                                            if opcode == OpCode::GET_FIELD {
                                                SP_PUT!(-1, val);
                                            } else {
                                                SP_PUT!(0, val);
                                                SP_GO!(1);
                                            }
                                            continue;
                                        } else {
                                            break ExecAction::Exception;
                                        }
                                    }
                                }
                            }
                        }

                        // Slow path: lookup property
                        if let Ok(val) = target.get_property(realm_scope, atom) {
                            #[cfg(feature = "inline_cache")]
                            if let Some(o) = target.as_object()
                                && let Some(shape) = o.shape()
                                && let Some((idx, flags)) = o.find_shape_property(atom)
                            {
                                // update inline cache
                                let ic_cache = unsafe { &mut *self.inline_cache.get() }; // SAFETY: single-threaded.
                                if ic_idx < ic_cache.len() {
                                    ic_cache[ic_idx] = InlineCacheEntry {
                                        shape_ptr: unsafe { shape.gc_ref().as_ptr().as_ptr() },
                                        shape_uid: shape.uid,
                                        slot: idx as u32,
                                        flags: flags as u8,
                                    };
                                }
                            }

                            if opcode == OpCode::GET_FIELD {
                                SP_PUT!(-1, val);
                            } else {
                                SP_PUT!(0, val);
                                SP_GO!(1)
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    OpCode::PUT_FIELD => {
                        // [target, value, ^] -> [^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        #[cfg(feature = "inline_cache")]
                        let ic_idx = PC_FETCH!(u16) as usize;

                        let target = SP_VAL!(-2);
                        let value = SP_VAL!(-1);

                        #[cfg(feature = "inline_cache")]
                        {
                            // IC fast path: direct slot write when shape & flags match
                            let ic_cache = unsafe { &*self.inline_cache.get() };
                            if ic_idx < ic_cache.len()
                                && let Some(o) = target.as_object()
                                && let Some(shape) = o.shape()
                            {
                                let entry = &ic_cache[ic_idx];
                                let shape_ptr = unsafe { shape.gc_ref().as_ptr().as_ptr() };

                                if entry.shape_ptr == shape_ptr && entry.shape_uid == shape.uid {
                                    let slot = entry.slot as usize;

                                    // delete marks slot Deleted without changing shape
                                    if !matches!(o.prop_slot(slot), JSProperty::Deleted) {
                                        let prs_flags = entry.flags as u32;
                                        if prs_flags & crate::object::JS_PROP_TMASK
                                            == crate::object::JS_PROP_NORMAL
                                            && prs_flags & crate::object::JS_PROP_WRITABLE != 0
                                            && prs_flags & crate::object::JS_PROP_LENGTH == 0
                                        {
                                            let mut o =
                                                unsafe { SP_MUT!(-2).as_object_unchecked() };
                                            o.set_prop_slot(slot, JSProperty::Normal(value));

                                            // next instruction
                                            SP_GO!(-2);
                                            continue;
                                        }
                                    }
                                }
                            }
                        }

                        // Slow path: property lookup & set
                        SP_GO!(-2);

                        let r = target.set_property_internal(
                            realm_scope,
                            atom,
                            value,
                            None,
                            ThrowPolicy::StrictOnly,
                            false,
                        );
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }

                        #[cfg(feature = "inline_cache")]
                        if let Some(o) = target.as_object()
                            && let Some(shape) = o.shape()
                            && let Some((idx, flags)) = o.find_shape_property(atom)
                        {
                            // Update IC cache after successful slow path
                            let ic_cache = unsafe { &mut *self.inline_cache.get() };
                            if ic_idx < ic_cache.len() {
                                ic_cache[ic_idx] = InlineCacheEntry {
                                    shape_ptr: unsafe { shape.gc_ref().as_ptr().as_ptr() },
                                    shape_uid: shape.uid,
                                    slot: idx as u32,
                                    flags: flags as u8,
                                };
                            }
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Binary arithmetic
                    // ═══════════════════════════════════════════════════
                    OpCode::ADD
                    | OpCode::SUB
                    | OpCode::MUL
                    | OpCode::DIV
                    | OpCode::MOD
                    | OpCode::POW => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        let res = match opcode {
                            OpCode::ADD => JSValue::arith_add(realm_scope, op1, op2),
                            OpCode::SUB => JSValue::arith_sub(realm_scope, op1, op2),
                            OpCode::MUL => JSValue::arith_mul(realm_scope, op1, op2),
                            OpCode::DIV => JSValue::arith_div(realm_scope, op1, op2),
                            OpCode::MOD => JSValue::arith_mod(realm_scope, op1, op2),
                            OpCode::POW => JSValue::arith_pow(realm_scope, op1, op2),
                            _ => unreachable!(),
                        };

                        if let Ok(val) = res {
                            SP_PUT!(-2, val);
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Control flow
                    // ═══════════════════════════════════════════════════
                    OpCode::GOTO => {
                        let n = unsafe { read_from_ptr::<i32>(pc) }; // could be minus
                        PC_GO!(n);

                        if unlikely(realm_scope.poll_interrupts()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::IF_TRUE | OpCode::IF_FALSE => {
                        let b = opcode == OpCode::IF_TRUE;
                        let val = SP_VAL!(-1);
                        SP_GO!(-1);

                        // skip jump offset
                        PC_GO!(4);

                        let test = if val.is_int_or_bool() {
                            val.payload_i32() != 0
                        } else if val.is_undefined_or_null() {
                            false
                        } else if let Ok(x) = val.to_bool() {
                            x
                        } else {
                            break ExecAction::Exception;
                        };

                        if b == test {
                            // jump pc
                            pc = unsafe {
                                pc.offset((read_from_ptr::<i32>(pc.offset(-4)) - 4) as isize)
                            };
                        }

                        if unlikely(realm_scope.poll_interrupts()) {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Array element access
                    // ═══════════════════════════════════════════════════
                    OpCode::GET_ARRAY_EL => {
                        // [array, prop, ^] -> [value, ^]
                        let val = SP_REF!(-2)
                            .get_property_value(realm_scope, SP_VAL!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(-2, val);
                        SP_GO!(-1);

                        if unlikely(is_err) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL2 => {
                        // [array, prop, ^] -> [array, value, ^]
                        let val = SP_REF!(-2)
                            .get_property_value(realm_scope, SP_VAL!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(-1, val);

                        if unlikely(is_err) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL3 => {
                        // [array, prop, ^] -> [array, prop1, value, ^]
                        let arr = SP_REF!(-2);

                        if !arr.is_int() && !arr.is_string_or_symbol() {
                            // must be tested before ToPropertyKey
                            if arr.is_undefined_or_null() {
                                let _ = realm_scope.throw_type_error("value has no property ");
                                break ExecAction::Exception;
                            }

                            let Ok(prop1) = SP_VAL!(-1).to_propkey(realm_scope) else {
                                break ExecAction::Exception;
                            };
                            SP_PUT!(-1, prop1);
                        }

                        let val = arr
                            .get_property_value(realm_scope, SP_VAL!(-1))
                            .unwrap_or(JSValue::EXCEPTION);
                        let is_err = val.is_exception();
                        SP_PUT!(0, val);
                        SP_GO!(1);

                        if unlikely(is_err) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PUT_ARRAY_EL => {
                        // [arr, prop, value, ^] -> [^]
                        let mut arr = SP_VAL!(-3);
                        let ret = arr.set_property_value(
                            realm_scope,
                            SP_VAL!(-2),
                            SP_VAL!(-1),
                            ThrowPolicy::StrictOnly,
                            false,
                        );
                        SP_GO!(-3);
                        if unlikely(ret.is_err()) {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Misc
                    // ═══════════════════════════════════════════════════
                    OpCode::GET_SUPER => {
                        if let Ok(proto) = SP_REF!(-1).prototype(realm_scope) {
                            SP_PUT!(-1, proto);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::IMPORT => {
                        // Stack: ... specifier options
                        // Pop options, use specifier; push result Promise
                        let options = SP_VAL!(-1);
                        let specifier = SP_VAL!(-2);
                        if let Ok(res) = js_dynamic_import(realm_scope, specifier, options) {
                            SP_PUT!(-2, res);
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::REGEXP => {
                        #[cfg(feature = "builtin_regexp")]
                        if let Some(re_id) = SP_VAL!(-1).as_int() {
                            let re = realm_scope.rt().regexp_precompiled[re_id as usize].clone();
                            let regexp = crate::builtins::regexp::RegExp::from(
                                realm_scope,
                                JSValue::UNDEFINED,
                                re,
                            )
                            .map_or(JSValue::EXCEPTION, |o| o.into());
                            SP_PUT!(-2, regexp);
                            SP_GO!(-1);
                        }
                        #[cfg(not(feature = "builtin_regexp"))]
                        {
                            realm_scope.throw_internal_error("regex not supported");
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PUSH_BIGINT_I32 => {
                        // [^] -> [bigint_value, ^]
                        // i32-range BigInt literals are reconstructed here from
                        // the immediate (no constant-pool lookup, no parser-side
                        // GC allocation).
                        #[cfg(feature = "big_int")]
                        {
                            let a = PC_FETCH!(i32);
                            let big = BigIntRef::create(
                                realm_scope,
                                a < 0,
                                vec![a.unsigned_abs() as u64],
                            )
                            .map_or(JSValue::EXCEPTION, JSValue::make_bigint);
                            SP_PUT!(0, big);
                            SP_GO!(1);
                            if unlikely(SP_REF!(-1).is_exception()) {
                                break ExecAction::Exception;
                            }
                        }
                        #[cfg(not(feature = "big_int"))]
                        {
                            // `big_int` disabled: the emitter never produces
                            // this opcode, so reaching it is a bug.
                            unreachable!("PUSH_BIGINT_I32 not supported");
                        }
                    }
                    OpCode::NOP => {}

                    // ═══════════════════════════════════════════════════
                    // Try-catch
                    // ═══════════════════════════════════════════════════
                    OpCode::CATCH => {
                        let diff = unsafe { read_from_ptr::<i32>(pc) };
                        SP_PUT!(
                            0,
                            JSValue::make_catch_offset(pc.offset_from(pc_base) as i32 + diff)
                        );
                        SP_GO!(1);
                        PC_GO!(4);
                    }
                    OpCode::NIP_CATCH => {
                        // [catch_offset, ..., ret_val, ^] -> [ret_eval, ^]
                        SP_GO!(-1);
                        // keep ret_val
                        let ret_val = SP_VAL!(0);
                        // drop ... values
                        while sp > sp_base && !SP_REF!(-1).is_catch_offset() {
                            SP_GO!(-1);
                        }
                        if unlikely(sp == sp_base) {
                            let _ = realm_scope.throw_internal_error("nip_catch");
                            break ExecAction::Exception;
                        } else {
                            SP_PUT!(-1, ret_val);
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // For-in / For-of / Iterator
                    // ═══════════════════════════════════════════════════
                    OpCode::FOR_OF_START | OpCode::FOR_AWAIT_OF_START => {
                        // [obj, ^] -> [iter_obj, next_method, catchoffset(0), ^]
                        let is_async = opcode == OpCode::FOR_AWAIT_OF_START;

                        if let Ok((iter_obj, next_method)) = SP_REF!(-1)
                            .get_iterator_must(realm_scope, is_async)
                            .and_then(|o| {
                                o.get_property(realm_scope, Atom::CONST_next)
                                    .map(|m| (o, m))
                            })
                        {
                            SP_PUT!(-1, JSValue::from(iter_obj));
                            SP_PUT!(0, next_method);
                            SP_GO!(1);
                            SP_PUT!(0, JSValue::make_catch_offset(0));
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::FOR_OF_NEXT => {
                        // [iter_obj, next, ^] -> [value, done, ^]
                        let offset = -3 - PC_FETCH!(u8) as i32;
                        let r = unsafe { js_for_of_next(realm_scope, sp, offset) };
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                        SP_GO!(2);
                    }
                    OpCode::FOR_AWAIT_OF_NEXT => {
                        // Stack: [iter_obj, next, catch_offset]
                        // Calls next() on iter_obj and pushes the promise result.
                        // After: [iter_obj, next, catch_offset, promise]
                        // n_pop=3, n_push=4, net SP_GO!(1)

                        // disable the catch offset before calling next()
                        // so that exceptions from next() do NOT close the iterator.
                        let iter = SP_VAL!(-3);
                        let next = SP_VAL!(-2);
                        let saved_catch = SP_VAL!(-1);
                        SP_PUT!(-1, JSValue::UNDEFINED);

                        let result =
                            next.call_internal(realm_scope, iter, JSValue::UNDEFINED, &[], 0);
                        if let Ok(obj) = result {
                            // Restore the catch offset so that NIP_CATCH
                            // (emitted for `return` inside the loop body)
                            // can find it.  See P5 nip_catch bug.
                            SP_PUT!(-1, saved_catch);
                            SP_PUT!(0, obj);
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::ITERATOR_GET_VALUE_DONE => {
                        if unsafe { js_iterator_get_value_done(realm_scope, sp) } != 0 {
                            break ExecAction::Exception;
                        } else {
                            SP_GO!(1);
                        }
                    }
                    OpCode::ITERATOR_CHECK_OBJECT => {
                        if !SP_REF!(-1).is_object() {
                            let _ = realm_scope.throw_type_error("iterator must return an object");
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::ITERATOR_CLOSE => {
                        // [iter_obj, next, catch_offset, ^] -> [^]
                        SP_GO!(-2);

                        // SP[-1] is iter_obj
                        if let Some(o) = SP_VAL!(-1).as_object()
                            && ObjectIter::close_iter_internal(realm_scope, o).is_err()
                        {
                            break ExecAction::Exception;
                        }
                        SP_GO!(-1);
                    }
                    OpCode::ITERATOR_NEXT => {
                        // [iter_obj, next, catch_offset, val, ^] -> [iter_obj, next, catch_offset, val2, ^]
                        // [      -4,   -3,           -2,  -1, 0]
                        //
                        // SP[-3]: next_method, SP[-4]: iter_obj, SP[-1]: val
                        if let Ok(ret) = SP_REF!(-3).call(
                            realm_scope,
                            SP_VAL!(-4),
                            std::slice::from_ref(SP_REF!(-1)),
                        ) {
                            SP_PUT!(-1, ret);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::ITERATOR_CALL => {
                        // [iter_obj, next, catch_offset, val, ^] -> [iter_obj, next, catch_offset, val2, bool, ^]
                        // [      -4,   -3,           -2,  -1, 0]
                        let flags = PC_FETCH!(u8);

                        if let Ok(method) = SP_REF!(-4).get_property(
                            realm_scope,
                            if flags & 1 != 0 {
                                Atom::CONST_throw
                            } else {
                                Atom::CONST_return
                            },
                        ) {
                            let ret_flag = if unlikely(method.is_undefined_or_null()) {
                                true
                            } else {
                                if let Ok(ret) = method.call(
                                    realm_scope,
                                    SP_VAL!(-4),
                                    if flags & 2 != 0 {
                                        &[]
                                    } else {
                                        std::slice::from_ref(SP_REF!(-1))
                                    },
                                ) {
                                    SP_PUT!(-1, ret);
                                } else {
                                    break ExecAction::Exception;
                                }
                                false
                            };

                            SP_PUT!(0, JSValue::from(ret_flag));
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // FCLOSURE
                    // ═══════════════════════════════════════════════════
                    OpCode::FCLOSURE => {
                        // [^] -> [func_obj, ^]
                        let id = PC_FETCH!(u32);

                        let bc = unsafe { CPOOL![id].as_bytecode_unchecked() };
                        let r = bc
                            .make_closure_for_arena(
                                realm_scope,
                                None,
                                captured_vars,
                                arena_mut!(),
                                cur_frame_idx,
                            )
                            .map_or(JSValue::EXCEPTION, |f| f.into());

                        SP_PUT!(0, r);
                        SP_GO!(1);
                        if r.is_exception() {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Generator/Async suspension and return
                    // ═══════════════════════════════════════════════════
                    OpCode::AWAIT => {
                        break ExecAction::Return(JSValue::from(FUNC_RET_AWAIT), true);
                    }
                    OpCode::YIELD => {
                        break ExecAction::Return(JSValue::from(FUNC_RET_YIELD), true);
                    }
                    OpCode::YIELD_STAR | OpCode::ASYNC_YIELD_STAR => {
                        break ExecAction::Return(JSValue::from(FUNC_RET_YIELD_STAR), true);
                    }
                    OpCode::INITIAL_YIELD => {
                        break ExecAction::Return(JSValue::from(FUNC_RET_INITIAL_YIELD), true);
                    }
                    OpCode::RETURN_ASYNC => {
                        break ExecAction::Return(JSValue::UNDEFINED, true);
                    }

                    // ═══════════════════════════════════════════════════
                    // Subroutine call (try-finally)
                    // ═══════════════════════════════════════════════════
                    OpCode::GOSUB => {
                        let diff = unsafe { read_from_ptr::<i32>(pc) };
                        // Push return address as Int (offset to instruction after GOSUB)
                        SP_PUT!(0, JSValue::from(pc.offset_from(pc_base) as i32 + 4));
                        SP_GO!(1);
                        PC_GO!(diff);
                    }
                    OpCode::RET => {
                        if let Some(i) = SP_REF!(-1).as_int() {
                            let pos = i;
                            if unlikely(pos >= bytecodes.len() as i32) {
                                let _ = realm_scope.throw_internal_error("invalid ret value");
                                break ExecAction::Exception;
                            } else {
                                SP_GO!(-1);
                                pc = unsafe { pc_base.offset(pos as isize) };
                            }
                        } else {
                            let _ = realm_scope.throw_internal_error("invalid ret value");
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Stack manipulation (port from execute_with_realm_scope)
                    // ═══════════════════════════════════════════════════
                    OpCode::DUP3 => {
                        // [a, b, c, ^] -> [a, b, c, a, b, c, ^] where ^ is stack top
                        SP_PUT!(0, SP_VAL!(-3));
                        SP_PUT!(1, SP_VAL!(-2));
                        SP_PUT!(2, SP_VAL!(-1));
                        SP_GO!(3);
                    }
                    OpCode::INSERT2 => {
                        // [obj, a, ^] -> [a, obj, a, ^]
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_PUT!(-1, SP_VAL!(-2));
                        SP_PUT!(-2, SP_VAL!(0));
                        SP_GO!(1);
                    }
                    OpCode::INSERT3 => {
                        // [obj, prop, a, ^] -> [a, obj, prop, a, ^]
                        unsafe {
                            std::slice::from_raw_parts_mut(sp.offset(-3), 4).rotate_right(1);
                        };
                        SP_PUT!(-3, SP_VAL!(0));
                        SP_GO!(1);
                    }
                    OpCode::INSERT4 => {
                        // [this, obj, prop, a, ^] -> [a, this, obj, prop, a, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 5) };
                        lst.rotate_right(1);
                        SP_PUT!(-4, SP_VAL!(0));
                        SP_GO!(1);
                    }
                    OpCode::ROT3L => {
                        // [x, a, b, ^] -> [a, b, x, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-3), 3) };
                        lst.rotate_left(1);
                    }
                    OpCode::ROT4L => {
                        // [x, a, b, c, ^] -> [a, b, c, x, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 4) };
                        lst.rotate_left(1);
                    }
                    OpCode::ROT5L => {
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-5), 5) };
                        lst.rotate_left(1);
                    }
                    OpCode::ROT3R => {
                        // [a, b, x, ^] -> [x, a, b, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-3), 3) };
                        lst.rotate_right(1);
                    }
                    OpCode::PERM3 => {
                        // [obj, a, fixed, ^] -> [a, obj, fixed, ^]
                        unsafe {
                            std::ptr::swap(sp.offset(-2), sp.offset(-3));
                        }
                    }
                    OpCode::PERM4 => {
                        // [obj, prop, a, fixed, ^] -> [a, obj, prop, fixed, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 3) };
                        lst.rotate_right(1);
                    }
                    OpCode::PERM5 => {
                        // [this, obj, prop, a, fixed, ^] -> [a, this, obj, prop, fixed, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-5), 4) };
                        lst.rotate_right(1);
                    }
                    OpCode::IS_UNDEFINED_OR_NULL => {
                        SP_PUT!(-1, JSValue::from(SP_REF!(-1).is_undefined_or_null()));
                    }

                    // ═══════════════════════════════════════════════════
                    // For-in (port from execute_with_realm_scope)
                    // ═══════════════════════════════════════════════════
                    OpCode::FOR_IN_START => {
                        if js_for_in_start(realm_scope, sp).is_err() {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::FOR_IN_NEXT => {
                        if js_for_in_next(realm_scope, sp).is_err() {
                            break ExecAction::Exception;
                        }
                        SP_GO!(2);
                    }

                    // ═══════════════════════════════════════════════════
                    // Global varref / Ref operations (port from realm_scope)
                    // ═══════════════════════════════════════════════════
                    OpCode::MAKE_VAR_REF => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        if let Ok(vs) = realm_scope.build_global_varref(atom) {
                            if let Some((v0, v1)) = vs {
                                SP_PUT!(0, v0);
                                SP_PUT!(1, v1);
                            }
                            SP_GO!(2);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::GET_REF_VALUE => {
                        // [obj, prop, ^] -> [obj, value, ^]
                        if unlikely(SP_REF!(-2).is_undefined()) {
                            if let Ok(atom) = SP_REF!(-1).to_atom(realm_scope) {
                                let _ = JS_ThrowReferenceErrorNotDefined(realm_scope, atom);
                            }
                            break ExecAction::Exception;
                        }
                        // For WITH-scope references, ECMA-262 9.1.1.2.6 GetBindingValue
                        // step 2 requires HasProperty before Get. This also ensures the
                        // correct Proxy trap sequence for with-object proxies.
                        let obj_has_prop = SP_REF!(-1)
                            .to_atom(realm_scope)
                            .and_then(|atom| SP_REF!(-2).has_property(realm_scope, atom));
                        if let Ok(false) = obj_has_prop {
                            if rt.is_strict_mode() {
                                if let Ok(atom) = SP_REF!(-1).to_atom(realm_scope) {
                                    let _ = JS_ThrowReferenceErrorNotDefined(realm_scope, atom);
                                }
                                break ExecAction::Exception;
                            }
                            SP_PUT!(0, JSValue::UNDEFINED);
                        } else if obj_has_prop.is_ok()
                            && let Ok(val) =
                                SP_REF!(-2).get_property_value(realm_scope, SP_VAL!(-1))
                        {
                            SP_PUT!(0, val);
                        } else {
                            break ExecAction::Exception;
                        }
                        SP_GO!(1);
                    }
                    OpCode::GET_SUPER_VALUE => {
                        if let Ok(atom) = SP_REF!(-1).to_atom(realm_scope) {
                            let val = SP_REF!(-2).get_property_internal(
                                realm_scope,
                                atom,
                                SP_VAL!(-3),
                                false,
                            );
                            if let Ok(val) = val {
                                SP_PUT!(-3, val);
                                SP_GO!(-2);
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PUT_REF_VALUE => {
                        // [obj, prop, val, ^] -> [^]
                        let mut obj = SP_VAL!(-3);
                        let prop = SP_VAL!(-2);
                        let val = SP_VAL!(-1);
                        SP_GO!(-3);
                        let mut no_add = false;
                        if unlikely(obj.is_undefined()) {
                            if rt.is_strict_mode() {
                                if let Ok(atom) = prop.to_atom(realm_scope) {
                                    let _ = JS_ThrowReferenceErrorNotDefined(realm_scope, atom);
                                }
                                break ExecAction::Exception;
                            }
                            obj = JSValue::from(realm_scope.global_obj());
                        } else if rt.is_strict_mode() {
                            no_add = true;
                        }
                        // SetMutableBinding step 2: HasProperty (ECMA-262 9.1.1.2.5)
                        if let Ok(atom) = prop.to_atom(realm_scope) {
                            match obj.has_property(realm_scope, atom) {
                                Ok(false) | Err(_) if rt.is_strict_mode() => {
                                    let _ = JS_ThrowReferenceErrorNotDefined(realm_scope, atom);
                                    break ExecAction::Exception;
                                }
                                _ => {}
                            }
                        }
                        let ret = obj.set_property_value(
                            realm_scope,
                            prop,
                            val,
                            ThrowPolicy::StrictOnly,
                            no_add,
                        );
                        if ret.is_err() {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PUT_SUPER_VALUE => {
                        // [this_obj, obj, prop, val, ^] -> [^]
                        let this_obj = SP_VAL!(-4);
                        let obj = SP_VAL!(-3);
                        let prop = SP_VAL!(-2);
                        let val = SP_VAL!(-1);
                        SP_GO!(-4);
                        if unlikely(!obj.is_object()) {
                            let _ = realm_scope.throw_type_error_not_an_object();
                            break ExecAction::Exception;
                        }
                        if let Ok(atom) = prop.to_atom(realm_scope) {
                            debug_assert!(this_obj.is_object());
                            let ret = obj.set_property_internal(
                                realm_scope,
                                atom,
                                val,
                                this_obj.as_object(),
                                ThrowPolicy::StrictOnly,
                                false,
                            );
                            if unlikely(ret.is_err()) {
                                break ExecAction::Exception;
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::DEFINE_ARRAY_EL => {
                        // [obj, prop, value, ^] -> [obj, prop, ^]
                        let obj = SP_MUT!(-3);
                        let prop = SP_REF!(-2);
                        let value = SP_VAL!(-1);
                        let ret = prop.to_atom(realm_scope).and_then(|a| {
                            obj.define_property_value(
                                realm_scope,
                                a,
                                value,
                                JS_PROP_C_W_E | JS_PROP_THROW,
                            )
                        });
                        SP_GO!(-1);
                        if unlikely(ret.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::APPEND => {
                        // [dest, start_pos, src, ^] -> [dest, end_pos, ^]
                        if unsafe { js_append_enumerate(realm_scope, sp).is_ok() } {
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::COPY_DATA_PROPERTIES => {
                        // target source excludeList
                        let mask = PC_FETCH!(u8) as i32;
                        let r = SP_MUT!(-1 - (mask & 3)).copy_data_properties_from(
                            realm_scope,
                            SP_REF!(-1 - ((mask >> 2) & 7)),
                            SP_REF!(-1 - ((mask >> 5) & 7)),
                            false,
                        );
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Private Symbol (port from execute_with_realm_scope)
                    // ═══════════════════════════════════════════════════
                    OpCode::PRIVATE_SYMBOL => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        if let Ok(val) =
                            realm_scope.create_symbol_from_atom(atom, AtomKind::Private)
                        {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::GET_PRIVATE_FIELD => {
                        let op_2 = SP_VAL!(-2);
                        let op_1 = SP_VAL!(-1);
                        let val = op_2
                            .get_private_field(realm_scope, op_1)
                            .unwrap_or(JSValue::EXCEPTION);
                        SP_PUT!(-2, val);
                        SP_GO!(-1);
                        if unlikely(SP_REF!(-1).is_exception()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PUT_PRIVATE_FIELD => {
                        // [this_obj, name, value, ^] -> [^]
                        let ret =
                            SP_MUT!(-3).set_private_field(realm_scope, SP_VAL!(-1), SP_VAL!(-2));
                        SP_GO!(-3);
                        if unlikely(ret.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::DEFINE_PRIVATE_FIELD => {
                        // [this_obj, name, value, ^] -> [this_obj, ^]
                        let ret =
                            SP_MUT!(-3).define_private_field(realm_scope, SP_VAL!(-2), SP_VAL!(-1));
                        SP_GO!(-2);
                        if unlikely(ret.is_err()) {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // Object / Method / Class definition (port from realm_scope)
                    // ═══════════════════════════════════════════════════
                    OpCode::DEFINE_FIELD => {
                        // [this_obj, value, ^] -> [this_obj, ^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let ret = SP_MUT!(-2).define_property_value(
                            realm_scope,
                            atom,
                            SP_VAL!(-1),
                            JS_PROP_C_W_E | JS_PROP_THROW,
                        );
                        SP_GO!(-1);
                        if unlikely(ret.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::SET_NAME => {
                        // [obj, ^] -> [obj, ^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let r = SP_REF!(-1).as_object().ok_or(-1).and_then(|mut o| {
                            o.define_name(realm_scope, atom, JS_PROP_CONFIGURABLE)
                        });
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::SET_NAME_COMPUTED => {
                        // [name, value, ^] -> [name, value, ^]
                        let r = SP_MUT!(-1).as_object().unwrap().define_name_computed(
                            realm_scope,
                            SP_VAL!(-2),
                            JS_PROP_CONFIGURABLE,
                        );
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::SET_PROTO => {
                        // [obj, proto, ^] -> [obj, ^]
                        let proto = SP_VAL!(-1);
                        if proto.is_object() || proto.is_null() {
                            let r = SP_MUT!(-2).set_prototype_internal(realm_scope, proto, true);
                            if unlikely(r.is_err()) {
                                break ExecAction::Exception;
                            }
                        }
                        SP_GO!(-1);
                    }
                    OpCode::SET_HOME_OBJECT => {
                        // [home_obj, func, ^] -> [home_obj, func, ^]
                        if let Some(mut fo) = SP_MUT!(-1).as_object_BCWrapper() {
                            fo.method_set_home_object(SP_VAL!(-2).as_object());
                        } else {
                            #[cfg(debug_assertions)]
                            unreachable!("SET_HOME_OBJECT expect BytecodeFunction object");
                        }
                    }
                    OpCode::DEFINE_METHOD | OpCode::DEFINE_METHOD_COMPUTED => {
                        let (is_computed, atom, op_flags) =
                            if opcode == OpCode::DEFINE_METHOD_COMPUTED {
                                (true, Atom::NULL, PC_FETCH!(u8))
                            } else {
                                (false, Atom::from_id(PC_FETCH!(u32)), PC_FETCH!(u8))
                            };

                        let name = if atom.is_null() {
                            if let Ok(atom) = SP_REF!(-2).to_atom(realm_scope) {
                                atom
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            atom
                        };

                        let mut flags = JS_PROP_HAS_CONFIGURABLE
                            | JS_PROP_CONFIGURABLE
                            | JS_PROP_HAS_ENUMERABLE
                            | JS_PROP_THROW;

                        if op_flags & OP_DEFINE_METHOD_ENUMERABLE != 0 {
                            flags |= JS_PROP_ENUMERABLE
                        }

                        let func_obj: &mut JSValue;
                        let mut value = JSValue::UNDEFINED;
                        let mut getter = JSValue::UNDEFINED;
                        let mut setter = JSValue::UNDEFINED;

                        match op_flags & (OP_DEFINE_METHOD_GETTER | OP_DEFINE_METHOD_SETTER) {
                            OP_DEFINE_METHOD_METHOD => {
                                value = SP_VAL!(-1);
                                func_obj = &mut value;
                                flags |=
                                    JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE;
                            }
                            OP_DEFINE_METHOD_GETTER => {
                                getter = SP_VAL!(-1);
                                func_obj = &mut getter;
                                flags |= JS_PROP_HAS_GET;
                            }
                            OP_DEFINE_METHOD_SETTER => {
                                setter = SP_VAL!(-1);
                                func_obj = &mut setter;
                                flags |= JS_PROP_HAS_SET;
                            }
                            _ => unreachable!(),
                        }

                        let this_obj = SP_MUT!(if is_computed { -3 } else { -2 });
                        let mut ret =
                            js_method_set_properties(realm_scope, func_obj, name, flags, this_obj);

                        if ret.is_ok() {
                            ret = this_obj
                                .define_property(realm_scope, name, value, getter, setter, flags)
                                .map(|_| ());
                        }

                        if is_computed {
                            SP_GO!(-2);
                        } else {
                            SP_GO!(-1);
                        }

                        if ret.is_err() {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::DEFINE_CLASS | OpCode::DEFINE_CLASS_COMPUTED => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let class_flags = PC_FETCH!(u8) as u32;
                        let r = js_op_define_class_for_arena(
                            realm_scope,
                            sp,
                            atom,
                            class_flags as _,
                            captured_vars,
                            arena_mut!(),
                            cur_frame_idx,
                            opcode == OpCode::DEFINE_CLASS_COMPUTED,
                        );
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // With statement (port from execute_with_realm_scope)
                    // ═══════════════════════════════════════════════════
                    OpCode::WITH_GET_VAR
                    | OpCode::WITH_PUT_VAR
                    | OpCode::WITH_DELETE_VAR
                    | OpCode::WITH_MAKE_REF
                    | OpCode::WITH_GET_REF => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let diff = PC_FETCH!(i32);
                        let is_with = PC_FETCH!(u8) != 0;

                        let mut env_obj = SP_VAL!(-1);

                        if let Ok(ret) = env_obj.has_property(realm_scope, atom) {
                            let in_env_obj = if ret {
                                if is_with {
                                    if let Ok(b) = js_has_unscopable(realm_scope, env_obj, atom) {
                                        !b
                                    } else {
                                        break ExecAction::Exception;
                                    }
                                } else {
                                    true
                                }
                            } else {
                                false
                            };

                            if in_env_obj {
                                match opcode {
                                    OpCode::WITH_GET_VAR => {
                                        // Re-check HasProperty after @@unscopables (ECMA-262
                                        // 9.1.1.2.6 GetBindingValue steps 2-3). The getter
                                        // may have deleted the property. If so, return undefined
                                        // (non-strict) or throw ReferenceError (strict).
                                        if !env_obj.has_property(realm_scope, atom).unwrap_or(false)
                                        {
                                            if rt.is_strict_mode() {
                                                let _ = JS_ThrowReferenceErrorNotDefined(
                                                    realm_scope,
                                                    atom,
                                                );
                                                break ExecAction::Exception;
                                            }
                                            SP_PUT!(-1, JSValue::UNDEFINED);
                                        } else if let Ok(val) =
                                            env_obj.get_property(realm_scope, atom)
                                        {
                                            SP_PUT!(-1, val);
                                        } else {
                                            break ExecAction::Exception;
                                        }
                                    }
                                    OpCode::WITH_PUT_VAR => {
                                        // SetMutableBinding step 2: HasProperty (ECMA-262 9.1.1.2.5)
                                        match env_obj.has_property(realm_scope, atom) {
                                            Ok(false) | Err(_) if rt.is_strict_mode() => {
                                                let _ = JS_ThrowReferenceErrorNotDefined(
                                                    realm_scope,
                                                    atom,
                                                );
                                                break ExecAction::Exception;
                                            }
                                            _ => {}
                                        }
                                        let prop = atom
                                            .to_value(realm_scope)
                                            .unwrap_or(JSValue::EXCEPTION);
                                        let val = SP_VAL!(-2);
                                        let ret = env_obj.set_property_value(
                                            realm_scope,
                                            prop,
                                            val,
                                            ThrowPolicy::StrictOnly,
                                            false,
                                        );
                                        SP_GO!(-2);
                                        if unlikely(ret.is_err()) {
                                            break ExecAction::Exception;
                                        }
                                    }
                                    OpCode::WITH_DELETE_VAR => {
                                        if let Ok(b) = env_obj.delete_property(realm_scope, atom, 0)
                                        {
                                            SP_PUT!(-1, JSValue::from(b));
                                        } else {
                                            break ExecAction::Exception;
                                        }
                                    }
                                    OpCode::WITH_MAKE_REF => {
                                        // produce a pair object/propname on the stack
                                        SP_PUT!(
                                            0,
                                            atom.to_value(realm_scope)
                                                .unwrap_or(JSValue::EXCEPTION)
                                        );
                                        SP_GO!(1);
                                    }
                                    OpCode::WITH_GET_REF => {
                                        // GetBindingValue step 2: HasProperty (ECMA-262 9.1.1.2.6)
                                        let val = match env_obj.has_property(realm_scope, atom) {
                                            Ok(false) | Err(_) => JSValue::UNDEFINED,
                                            _ => env_obj
                                                .get_property(realm_scope, atom)
                                                .unwrap_or(JSValue::UNDEFINED),
                                        };
                                        SP_PUT!(0, val);
                                        SP_GO!(1);
                                    }
                                    _ => {
                                        #[cfg(debug_assertions)]
                                        unreachable!();
                                        #[cfg(not(debug_assertions))]
                                        unsafe {
                                            std::hint::unreachable_unchecked();
                                        }
                                    }
                                }

                                PC_GO!(diff - 5);
                            } else {
                                SP_GO!(-1);
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    // ═══════════════════════════════════════════════════
                    // short_opcode placeholders
                    // ═══════════════════════════════════════════════════
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_MINUS1 | OpCode::PUSH_0..=OpCode::PUSH_7 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_I8 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_I16 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_CONST8 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_FCLOSURE8 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_EMPTY_STRING => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::PUSH_GET_LENGTH => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::CALL0..=OpCode::CALL3 => {
                        todo!();
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::GET_LOC8..=OpCode::SET_VAR_REF3 => {
                        todo!()
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::GOTO8 | OpCode::GOTO16 => {
                        pc = pc.add(if opcode == OpCode::GOTO8 {
                            get_u8(pc)
                        } else {
                            read_from_ptr::<u16>(pc)
                        } as usize);

                        if js_poll_interrupts(realm_scope) != 0 {
                            break ExecAction::Exception;
                        }
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::IF_TRUE8 | OpCode::IF_FALSE8 => {
                        todo!()
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::IS_UNDEFINED => {
                        todo!()
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::IS_NULL => {
                        todo!()
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::TYPEOF_IS_UNDEFINED => {
                        todo!()
                    }
                    #[cfg(feature = "short_opcode")]
                    OpCode::TYPEOF_IS_FUNCTION => {
                        todo!()
                    }

                    _ => {
                        let err_pc = unsafe { pc.offset_from(pc_base) - 1 } as usize;
                        let _ = realm_scope.throw_internal_error(&format!(
                            "invalid opcode: 0x{:02x}, pc={err_pc}",
                            opcode as u8,
                        ));
                        break ExecAction::Exception;
                    }
                } // match opcode

                #[cfg(all(debug_assertions, feature = "debug_gc"))]
                {
                    if gc_scope_kind != GcScopeKind::None {
                        realm_scope.clear_locals();
                    }
                    rt.gc_collect();
                }
                #[cfg(not(all(debug_assertions, feature = "debug_gc")))]
                if rt.gc_req {
                    if gc_scope_kind != GcScopeKind::None {
                        realm_scope.clear_locals();
                    }
                    rt.gc_collect();
                }
            }; // opcodes inner loop

            match action {
                ExecAction::Return(ret_val, is_generator) => {
                    save_pc!(pc.offset_from(pc_base) as usize);
                    save_sp!(sp.offset_from(sp_base) as i32);
                    return ExecAction::Return(ret_val, is_generator);
                }

                ExecAction::Exception => {
                    // Interrupt bypasses catch handling — it is not a JS exception.
                    if rt.interrupt_code.is_some() {
                        return ExecAction::Unwind;
                    }

                    // Add backtrace before catch lookup (matching exec.rs's
                    // StackFrame::handle_exception behavior).
                    let cur_pc = unsafe { pc.offset_from(pc_base) as usize };
                    if let Exception::Error(err_val) = rt.exception()
                        && is_backtrace_needed(err_val)
                    {
                        frame_mut!(cur_frame_idx).cur_pc_offset = cur_pc;
                        realm_scope.build_backtrace_to(err_val, None, 0);
                    }

                    if let Some((new_pc, new_sp)) =
                        unsafe { (*arena_ptr).find_catch_in_frame(cur_frame_idx, realm_scope) }
                    {
                        unsafe {
                            save_sp!(new_sp as i32);
                            pc = pc_base.add(new_pc);
                            sp = sp_base.add(new_sp);
                        }
                        continue 'arena_loop;
                    }
                    return ExecAction::Unwind;
                }

                ExecAction::CallBytecode {
                    callee_bc,
                    callee_frame_idx,
                    return_sp,
                    this_obj,
                    new_target,
                    ctor_this,
                } => {
                    save_pc!(pc.offset_from(pc_base) as usize);
                    save_sp!(sp.offset_from(sp_base) as i32);
                    return ExecAction::CallBytecode {
                        callee_bc,
                        callee_frame_idx,
                        return_sp,
                        this_obj,
                        new_target,
                        ctor_this,
                    };
                }

                ExecAction::CallBytecodeCrossRealm {
                    callee_bc,
                    callee_frame_idx,
                    callee_ctx_ptr,
                    return_sp,
                    this_obj,
                    new_target,
                    ctor_this,
                } => {
                    save_pc!(pc.offset_from(pc_base) as usize);
                    save_sp!(sp.offset_from(sp_base) as i32);

                    return ExecAction::CallBytecodeCrossRealm {
                        callee_bc,
                        callee_frame_idx,
                        callee_ctx_ptr,
                        return_sp,
                        this_obj,
                        new_target,
                        ctor_this,
                    };
                }

                ExecAction::Unwind => {
                    unreachable!(
                        "Unwind not emitted from inner loop — goes through Exception handler"
                    )
                }

                ExecAction::TailCall {
                    callee_bc,
                    callee_func_obj,
                    args,
                    this_obj,
                } => {
                    return ExecAction::TailCall {
                        callee_bc,
                        callee_func_obj,
                        args,
                        this_obj,
                    };
                }
            }
        } // 'arena_loop
    }
}

/// Allocate and initialize a callee frame in the arena.
///
/// Computes the buffer capacity from the bytecode's declared arg count,
/// actual argument count, and local buffer size. Allocates a frame, copies
/// arguments, fills unset slots with `UNDEFINED`, and initializes
/// `this`/`new_target` protected values to `UNDEFINED`.
///
/// Returns the index of the newly created frame.
///
/// Used by both CALL (non-tail) and TAIL_CALL to eliminate code duplication
/// between the opcode inline dispatch and the trampoline handler.
///
/// `caller_depth` is the depth of the calling frame; the callee's depth is set
/// to `caller_depth + 1` so that runaway non-tail recursion drives the depth
/// toward `MAX_CALL_DEPTH` and trips a stack-depth error (see the note on
/// `MAX_CALL_DEPTH`). Exceeding the limit returns `FrameAllocError::Exceeded`,
/// which the callers translate into the "maximum call stack size exceeded"
/// error. The tail-call path passes `depth - 1` so the replacement frame
/// keeps the caller's depth — tail calls never deepen the chain (TCO).
#[inline(always)]
fn build_callee_frame(
    arena: &mut StackFrameArena,
    caller_depth: u32,
    func_obj: Object,
    bc: Bytecode,
    args: &[JSValue],
) -> Result<FrameIndex, FrameAllocError> {
    // saturating_add: with MAX_CALL_DEPTH = 0 (unlimited) an infinite tail-call
    // loop increments depth forever — saturate instead of overflowing u32.
    let callee_depth = caller_depth.saturating_add(1);
    if MAX_CALL_DEPTH > 0 && callee_depth > MAX_CALL_DEPTH {
        return Err(FrameAllocError::Exceeded);
    }

    let cap = std::cmp::max(bc.arg_count as usize, args.len()) + bc.local_buf_size as usize;
    let idx = arena.alloc_frame(func_obj, bc.mode(), cap)?;
    arena.frame_mut(idx).depth = callee_depth;
    arena.init_frame_args(idx, bc.arg_count, bc.var_count, args);

    Ok(idx)
}

/// Drop every caller scope still parked in the virtual call stack. Must run
/// before the trampoline returns early (yield/await suspension or async
/// completion): the scopes sit in `ManuallyDrop`, so skipping this drain
/// would leak them and corrupt the GC scope stack.
#[inline(always)]
fn drain_saved_scopes(virtual_frames: &mut Vec<VirtualFrame>) {
    while let Some(vf) = virtual_frames.pop() {
        if let Some(scope) = vf.caller_scope {
            drop(ManuallyDrop::into_inner(scope));
        }
    }
}

/// Exception-propagation path of popping one virtual frame: drop the callee
/// scope and swap the caller's saved scope back in (when the callee had one
/// — `None` means NoScope, nothing to restore), then resume from the pc/sp
/// saved in the caller's frame head.
/// Returns `(caller_bc, caller_frame_idx, pc_offset, sp_offset)`.
///
/// The normal Return path does NOT go through here: it promotes the return
/// value before the scope swap, writes it into the caller's stack, and
/// derives sp from `return_sp` instead of the frame head.
#[inline(always)]
fn restore_caller_frame(
    caller_vf: VirtualFrame,
    scope: &mut TrampolineScope,
    arena: &StackFrameArena,
) -> (Bytecode, FrameIndex, usize, usize) {
    if let Some(saved_caller_scope) = caller_vf.caller_scope {
        // Full/Light: drop callee scope and restore caller.
        unsafe { scope.swap_in_saved(ManuallyDrop::into_inner(saved_caller_scope)) };
    }
    // NoScope: scope was never swapped — skip drop/restore.

    let caller_head = arena.frame(caller_vf.frame_idx);
    (
        caller_vf.caller_bc,
        caller_vf.frame_idx,
        caller_head.cur_pc_offset,
        caller_head.cur_sp_offset as usize,
    )
}

/// Execute bytecode in a single trampoline loop, completely avoiding
/// C-stack recursion for JS-to-JS calls.
///
/// The trampoline maintains:
/// - `cur_bc`: currently executing bytecode
/// - `cur_frame_idx`: currently executing frame in the arena
/// - `cur_trampoline_scope`: GC scope for the current frame (via `TrampolineScope`)
/// - `virtual_frames`: caller chain (saved on call, restored on return)
///
/// Returns (result, is_completed) tuple:
/// - result: the return value (or EXCEPTION on uncaught error)
/// - is_completed: true if the function completed, false if suspended (yield/await)
pub(crate) fn execute_trampoline(
    start_bc: Bytecode,
    start_frame_idx: FrameIndex,
    start_scope: &mut ScopedContext,
    mut this_obj: JSValue,
    mut new_target: JSValue,
) -> (JSValue, bool) {
    let mut rt_ptr = start_scope.rt_ptr();
    let mut cur_bc = start_bc;
    let mut cur_frame_idx = start_frame_idx;

    // Get a stable raw pointer to the arena. The arena lives in RuntimeImpl
    // which has the same lifetime as the runtime. Using a raw pointer avoids
    // borrow conflicts between cur_trampoline_scope (which borrows rt) and arena access.
    let arena_ptr: *mut StackFrameArena = &raw mut rt_ptr.frame_arena;

    let mut realm_ctx = start_bc.realm();
    let mut cur_trampoline_scope = unsafe { TrampolineScope::new(&mut realm_ctx) };

    // Check for a pending runtime exception (e.g., .throw() on a suspended
    // async generator or async function). If present, the first
    // execute_with_arena call must handle it immediately.
    let mut throwed = rt_ptr.exception() != Exception::No;

    // Initialize PC/SP offsets from the frame head.
    // On first execution, both are 0 (fresh frame).
    // On resume after yield/await, they hold the saved position
    // from execute_with_arena's State::Done handler.
    let (mut pc_offset, mut sp_offset) = {
        let arena = unsafe { &*arena_ptr };
        let head = arena.frame(start_frame_idx);
        (head.cur_pc_offset, head.cur_sp_offset as usize)
    };

    let mut virtual_frames: Vec<VirtualFrame> = Vec::new();

    // args is derived from the current frame's arena data at the top of
    // each loop iteration — no need for an external parameter. This is
    // equivalent to how exec.rs passes fresh args via C-stack frames.
    let mut args;

    'trampoline: loop {
        let arena = unsafe { &mut *arena_ptr };

        rt_ptr.active_frame_index = cur_frame_idx;
        debug_assert!(cur_trampoline_scope.is_arena_mode());

        // Like exec.rs's execute_with_realm_scope, we use args directly.
        // Keep it in sync with the current frame — frames switch across
        // CallBytecode / TailCall / Return / Unwind, but deriving once at
        // the top of the loop is simpler and always correct.
        {
            let head = arena.frame(cur_frame_idx);
            let data = head.buf.data_ptr();
            args = unsafe { std::slice::from_raw_parts(data, head.arg_real_count as usize) };
        }

        // Derive func_home_obj from the current frame's function object.
        // Methods using `super` need this to look up their [[HomeObject]].
        let func_home_obj = {
            let head = arena.frame(cur_frame_idx);
            BCWrapper::try_from(JSValue::from(head.func_obj))
                .ok()
                .and_then(|bfun| bfun.payload().home_object())
        };

        let action = cur_bc.execute_with_arena(
            cur_trampoline_scope.scope_mut(),
            arena,
            cur_frame_idx,
            cur_bc.gc_scope_kind(),
            this_obj,
            new_target,
            func_home_obj,
            pc_offset,
            sp_offset,
            args,
            throwed,
        );

        match action {
            ExecAction::Exception => {
                unreachable!(
                    "Exception from execute_with_arena should be handled inside — \
                     trampoline only receives Unwind for uncaught exceptions"
                )
            }
            ExecAction::CallBytecode {
                callee_bc,
                callee_frame_idx: callee_idx,
                return_sp,
                this_obj: callee_this,
                new_target: callee_new_target,
                ctor_this,
            } => {
                let callee_gc_kind = callee_bc.gc_scope_kind();

                // caller_scope: None if callee is NoScope (no scope swap needed),
                // Some if callee is Light/Full (standard scope swap).
                let caller_scope = match callee_gc_kind {
                    GcScopeKind::None => {
                        // NoScope: don't push a new GC scope. callee executes
                        // within the caller's scope — safe because the
                        // callee's bytecode allocates no LOCAL GC nodes.
                        None
                    }
                    GcScopeKind::Light | GcScopeKind::Full => {
                        let mut callee_realm_ctx = callee_bc.realm();
                        let callee_trampoline_scope =
                            unsafe { TrampolineScope::new(&mut callee_realm_ctx) };

                        Some(ManuallyDrop::new(
                            std::mem::replace(&mut cur_trampoline_scope, callee_trampoline_scope)
                                .into_inner(),
                        ))
                    }
                };

                virtual_frames.push(VirtualFrame {
                    frame_idx: cur_frame_idx,
                    caller_bc: cur_bc,
                    return_sp,
                    caller_scope,
                    ctor_this,
                });

                cur_bc = callee_bc;
                cur_frame_idx = callee_idx;
                pc_offset = 0;
                sp_offset = 0;
                throwed = false;

                this_obj = callee_this;
                new_target = callee_new_target;
            }

            ExecAction::TailCall {
                callee_bc,
                callee_func_obj,
                args,
                this_obj: callee_this,
            } => {
                let arena = unsafe { &mut *arena_ptr };

                // Save the prev link before freeing the current frame — this
                // preserves the backtrace chain across TCO. After frame
                // replacement, the callee's prev points to the original caller
                // (skipping the tail-calling function), which matches ECMAScript
                // tail-call semantics.
                let orig_prev = {
                    let head = arena.frame(cur_frame_idx);
                    head.prev
                };

                // Tail calls replace the frame instead of deepening the call
                // chain, so the callee inherits the caller's depth unchanged:
                // `MAX_CALL_DEPTH` only bounds non-tail calls, and unbounded
                // tail recursion runs at constant depth (true TCO).
                // `build_callee_frame` adds 1, so pass `depth - 1`;
                // saturating_sub keeps a root-frame depth of 0 stable.
                let tail_depth = arena.frame(cur_frame_idx).depth.saturating_sub(1);

                // Detach any closure VarRefs that reference the outgoing frame
                // (otherwise captured variables become dangling pointers into
                // the arena data buffer), then recycle the caller frame *before*
                // allocating the callee so the bump arena's live region stays
                // bounded (TCO).
                arena.detach_and_free(cur_frame_idx);

                // Use build_callee_frame for consistent frame initialization
                // (same path as non-tail CALL in execute_with_arena). On failure
                // we must propagate the error as a normal *uncaught* exception
                // (not tear the whole trampoline down), so that enclosing
                // try/catch and finally blocks still run — `test262`'s
                // recursion.js relies on the recursion error being catchable.
                let new_idx = match build_callee_frame(
                    arena,
                    tail_depth,
                    callee_func_obj,
                    callee_bc,
                    &args,
                ) {
                    Ok(idx) => idx,
                    Err(e) => {
                        match e {
                            FrameAllocError::Oom => {
                                let _ = cur_trampoline_scope.scope_mut().throw_stack_fail();
                            }
                            FrameAllocError::Exceeded => {
                                let _ = cur_trampoline_scope.scope_mut().throw_stack_exceeded();
                            }
                        }

                        // The caller frame was already recycled above (TCO), so
                        // unwind it directly to the caller's caller. This mirrors
                        // the successful TailCall path (the tail call replaces the
                        // frame) while keeping the error catchable: each enclosing
                        // frame's `execute_with_arena` will run its catch/finally.
                        if let Some(caller_vf) = virtual_frames.pop() {
                            (cur_bc, cur_frame_idx, pc_offset, sp_offset) =
                                restore_caller_frame(caller_vf, &mut cur_trampoline_scope, arena);
                            throwed = true;

                            continue 'trampoline;
                        } else {
                            drop(cur_trampoline_scope.into_inner());
                            rt_ptr.active_frame_index = FrameIndex::NULL;
                            return (JSValue::EXCEPTION, true);
                        }
                    }
                };

                // Restore the backtrace chain: the tail-called function appears
                // to have been called directly by the original caller.
                arena.frame_mut(new_idx).prev = orig_prev;

                cur_bc = callee_bc;
                cur_frame_idx = new_idx;
                pc_offset = 0;
                sp_offset = 0;
                this_obj = callee_this;
                new_target = JSValue::UNDEFINED;
                throwed = false;
            }

            ExecAction::Return(ret_val, is_generator) => {
                // ── Frame return ──
                if is_generator {
                    if ret_val.is_undefined() {
                        // RETURN_ASYNC: async function completed normally.
                        // The actual return value is at SP[-1] in the frame.
                        let arena = unsafe { &mut *arena_ptr };
                        // Extract stack pointer and read return value.
                        let sp_offset = {
                            let head = arena.frame(cur_frame_idx);
                            head.cur_sp_offset
                        };
                        let sp_base = {
                            let head = arena.frame(cur_frame_idx);
                            head.stack_base_ptr()
                        };
                        let actual_ret =
                            unsafe { std::ptr::read(sp_base.offset(sp_offset as isize - 1)) };
                        // Detach all VarRefs before freeing the frame. The
                        // frame may hold closure variables captured by inner
                        // arrows (e.g., `new.target` in `async () => new.target`).
                        arena.detach_and_free(cur_frame_idx);
                        // Promote before scope pop so the value stays LOCAL
                        // in the parent scope (caller's scope).
                        actual_ret.promote(&cur_trampoline_scope);
                        drop(cur_trampoline_scope.into_inner());

                        // Drain virtual_frames to drop saved caller scopes.
                        drain_saved_scopes(&mut virtual_frames);
                        rt_ptr.active_frame_index = FrameIndex::NULL;

                        return (actual_ret, true);
                    }

                    // YIELD/AWAIT/YIELD_STAR: function suspended.
                    // Frame stays Active in arena; caller must take_frame.
                    //
                    // Note: no stack-pointer fix-up for YIELD_STAR here — the
                    // SUSPENDED_YIELD_STAR resume handler in a_gen.rs pushes
                    // completion_type onto the stack (bumping sp), and the
                    // DUP/IF_TRUE/DROP bytecode sequence that follows
                    // ASYNC_YIELD_STAR in expr_yield.rs consumes it before
                    // looping back to ITERATOR_NEXT, keeping the stack at
                    // depth 4 (matching label_loop).  Same principle as the
                    // legacy exec.rs path.
                    drop(cur_trampoline_scope.into_inner());

                    // Drain virtual_frames to drop saved caller scopes.
                    // These were placed in ManuallyDrop during CallBytecode.
                    // On early return (yield/await), we must explicitly drain
                    // them to prevent GC scope stack corruption.
                    drain_saved_scopes(&mut virtual_frames);
                    rt_ptr.active_frame_index = FrameIndex::NULL;
                    return (ret_val, false);
                }

                let arena = unsafe { &mut *arena_ptr };

                // ── Frame lifecycle management ──
                // The outermost frame (start_frame_idx) was allocated by
                // BytecodeFunction::call. The trampoline's Return/Unwind
                // handlers skip freeing it — the caller owns it and will
                // detach + free after execute_trampoline returns.
                // Inner frames (created by CallBytecode) are owned here.
                let is_outermost = virtual_frames.is_empty();
                if !is_outermost {
                    // Detach all VarRefs before freeing the frame. The frame may
                    // hold closure variables captured by inner functions (e.g.
                    // `var nextCount` in an async iterator factory captured by
                    // the `.next()` method). Without detach, VarRef raw pointers
                    // into arena.data become dangling after the region is reused.
                    // (The same pattern is used in the generator-completed path.)
                    arena.detach_and_free(cur_frame_idx);
                }
                // Promote before scope pop so the value stays LOCAL
                // in the parent scope (caller's scope, or start_scope).

                if let Some(caller_vf) = virtual_frames.pop() {
                    if let Some(saved_caller_scope) = caller_vf.caller_scope {
                        // Full/Light: promote, pop callee scope, restore caller.
                        ret_val.promote(&cur_trampoline_scope);
                        unsafe {
                            cur_trampoline_scope
                                .swap_in_saved(ManuallyDrop::into_inner(saved_caller_scope))
                        };
                    }
                    // NoScope: scope was never swapped —
                    // cur_trampoline_scope is already the caller's.
                    // No promote needed (callee had no LOCAL nodes),
                    // no drop needed (no callee scope was created).

                    // Legacy (non-derived) constructor call: a non-object
                    // return value is replaced with the instance created at
                    // the CALL_CONSTRUCTOR site (mirrors do_call_internal).
                    // The instance is LOCAL in the caller's scope, so it is
                    // still GC-protected here.
                    let ret_val = match caller_vf.ctor_this {
                        Some(inst) if !ret_val.is_object() => inst,
                        _ => ret_val,
                    };

                    cur_bc = caller_vf.caller_bc;
                    cur_frame_idx = caller_vf.frame_idx;

                    let caller_head = arena.frame(caller_vf.frame_idx);
                    let sp_ptr = caller_head.stack_base_ptr();
                    unsafe {
                        *sp_ptr.add(caller_vf.return_sp) = ret_val;
                    }

                    // Restore PC and adjust SP to match legacy CALL handler:
                    //   SP_GO!(-(call_argc + 1)) + SP_GO!(1)
                    // After call returns: sp = old_sp - call_argc
                    // return_sp = old_sp - call_argc - 1
                    // So: new_sp = return_sp + 1
                    pc_offset = caller_head.cur_pc_offset;
                    sp_offset = caller_vf.return_sp + 1;
                    throwed = false;
                } else {
                    // Outermost frame: the trampoline scope was created by
                    // TrampolineScope::new() at execute_trampoline entry —
                    // always promote and drop, even for NoScope start functions.
                    ret_val.promote(&cur_trampoline_scope);
                    drop(cur_trampoline_scope.into_inner());
                    rt_ptr.active_frame_index = FrameIndex::NULL;
                    return (ret_val, true);
                }
            }

            // ── Exception propagation ──
            ExecAction::Unwind => {
                let arena = unsafe { &mut *arena_ptr };

                // Before freeing the frame and propagating, check for a
                // finally block (GOSUB marker) in the current frame.
                // This handles cases where find_catch_in_frame didn't find
                // a CatchOffset but a GOSUB/THROW pattern exists (e.g.,
                // nested try-finally with cleanups, or try-finally without
                // catch where the CatchOffset was already consumed).
                if let Some((finally_pc, finally_sp)) =
                    arena.find_finally_in_frame(cur_frame_idx, cur_bc.opcodes().len())
                {
                    // Found a finally block. Save the exception value on
                    // the stack below the GOSUB marker so the THROW after
                    // the finally block can re-throw it.
                    let err_val = cur_trampoline_scope
                        .take_exception()
                        .to_value(&mut cur_trampoline_scope);
                    let sp_base = arena.frame(cur_frame_idx).stack_base_ptr();
                    unsafe {
                        // Write exception below the GOSUB marker position.
                        // After finally executes and RET pops the GOSUB
                        // marker, the exception is at TOS for THROW.
                        std::ptr::write(sp_base.add(finally_sp - 1), err_val);
                    }
                    pc_offset = finally_pc;
                    sp_offset = finally_sp + 1; // exception + GOSUB marker
                    throwed = false;
                    continue 'trampoline;
                }

                // Same rule as Return handler: inner frames are freed here,
                // the outermost frame is owned by BytecodeFunction::call.
                let is_outermost = virtual_frames.is_empty();
                if !is_outermost {
                    // Detach all VarRefs before freeing the frame (matching
                    // the Return handler). VarRef raw pointers into arena.data
                    // become dangling after the region is reused.
                    arena.detach_and_free(cur_frame_idx);
                }
                if let Some(caller_vf) = virtual_frames.pop() {
                    (cur_bc, cur_frame_idx, pc_offset, sp_offset) =
                        restore_caller_frame(caller_vf, &mut cur_trampoline_scope, arena);
                    throwed = true;

                    continue 'trampoline;
                } else {
                    // Outermost frame Unwind: trampoline scope must be dropped
                    // (it was created at execute_trampoline entry).
                    drop(cur_trampoline_scope.into_inner());
                    rt_ptr.active_frame_index = FrameIndex::NULL;
                    return (JSValue::EXCEPTION, true);
                }
            }

            ExecAction::CallBytecodeCrossRealm {
                callee_bc,
                callee_frame_idx: callee_idx,
                callee_ctx_ptr,
                return_sp,
                this_obj: callee_this,
                new_target: callee_new_target,
                ctor_this,
            } => {
                // ── Cross-realm call ──
                // Always push a scope bound to the callee's realm, even for
                // GcScopeKind::None callees: the callee may still throw (e.g.
                // a private brand-check TypeError), and the error object must
                // be created with the callee realm's prototypes — which come
                // from the active TrampolineScope. Cross-realm calls are rare,
                // so the extra scope push costs nothing in practice.
                let caller_scope = {
                    let mut callee_realm_ctx = callee_ctx_ptr;
                    let callee_trampoline_scope =
                        unsafe { TrampolineScope::new(&mut callee_realm_ctx) };

                    Some(ManuallyDrop::new(
                        std::mem::replace(&mut cur_trampoline_scope, callee_trampoline_scope)
                            .into_inner(),
                    ))
                };

                virtual_frames.push(VirtualFrame {
                    frame_idx: cur_frame_idx,
                    caller_bc: cur_bc,
                    return_sp,
                    caller_scope,
                    ctor_this,
                });

                cur_bc = callee_bc;
                cur_frame_idx = callee_idx;
                pc_offset = 0;
                sp_offset = 0;
                throwed = false;
                this_obj = callee_this;
                new_target = callee_new_target;
            }
        }
    }
}
