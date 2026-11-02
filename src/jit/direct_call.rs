// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The direct call: a monomorphic call site enters the
//! callee's compiled code without the interpreter's call protocol.
//!
//! The general path runs every call through `call_internal` → the
//! trampoline → the interpreter's frame prologue → the compiled entry. The
//! direct path skips the middle: the callee gets its own arena frame — so its
//! captured variables, its closures and the GC see it exactly as they would
//! in the trampoline — but the trampoline's per-call work (the scope swap,
//! the frame prologue, the dispatch loop) is gone.
//!
//! The guard is the *call-site feedback*: the interpreter recorded which
//! callee a monomorphic site calls, and the helper compares the live callee
//! with it. A mismatch — or a callee the direct path cannot serve — falls
//! back to the general path.

use crate::{
    JSValue, ScopeResult,
    bytecode::exec_stackless::{TrampolineScope, execute_trampoline},
    context::ScopedContext,
    error::{Exception, is_backtrace_needed},
    function::{BCWrapper, FunctionKind, GcScopeKind},
    object::Object,
    runtime::RuntimeImpl,
    vm_stack::{FrameAllocError, FrameState, VmStack},
};

/// The outcome of a direct call.
#[derive(Clone, Copy, Debug)]
pub(crate) enum DirectCall {
    /// The callee ran and returned this value.
    Done(JSValue),
    /// The callee threw.
    Exception,
    /// An interrupt unwound the callee.
    Interrupt,
    /// The callee cannot run this way — the caller must use the general call
    /// path.
    Fallback,
}

/// Run a bytecode callee natively, without the interpreter's call protocol.
///
/// Only the shape the direct path can serve is accepted: a plain bytecode
/// function that lives in the caller's realm (a cross-realm callee needs a
/// scope of its own: the errors it throws must carry its realm's
/// prototypes). Anything else is [`DirectCall::Fallback`], and the caller
/// falls back to the general path.
///
/// The callee's scope follows the trampoline's rule: a callee that allocates
/// GC nodes (directly or through a helper) gets a scope of its own, and its
/// return value is promoted into the caller's scope; a callee that never
/// allocates runs in the caller's scope, the way the interpreter's inlined
/// NoScope call does.
pub(crate) fn call_native_direct(
    scope: &mut ScopedContext,
    callee: JSValue,
    this_obj: JSValue,
    new_target: JSValue,
    args: &[JSValue],
) -> DirectCall {
    // The callee must be a plain bytecode function with compiled code.
    let Ok(bfun) = BCWrapper::try_from(callee) else {
        return DirectCall::Fallback;
    };
    let bc = bfun.payload().bytecode();
    if bc.func_kind() != FunctionKind::Normal || bc.realm() != scope.ctx_ptr {
        return DirectCall::Fallback;
    }
    if !bc.jit.has_code() {
        return DirectCall::Fallback;
    }

    // An interrupt is control flow, not an exception: the general path polls
    // before it calls, and so does this one.
    if scope.poll_interrupts() {
        return DirectCall::Interrupt;
    }

    // The C stack still grows by one Rust frame per call, so the overflow
    // guard still applies (the direct call is not a tail call).
    let mut rt = scope.rt_ptr();
    if crate::runtime::js_check_thread_stack_overflow(&rt, 0) {
        let _ = scope.throw_stack_overflow();
        return DirectCall::Exception;
    }

    // The callee's scope, exactly as the trampoline would give it.
    let mut callee_scope = if bc.gc_scope_kind() == GcScopeKind::None {
        None
    } else {
        let mut realm_ctx = bc.realm();
        // SAFETY: the scope is dropped before this function returns, so it
        // never outlives the runtime.
        Some(unsafe { TrampolineScope::new(&mut realm_ctx) })
    };
    let callee_scope_ptr = match callee_scope.as_mut() {
        Some(s) => s.scope_mut() as *mut ScopedContext as *mut u8,
        None => scope as *mut ScopedContext as *mut u8,
    };

    // The callee's frame: the same one the trampoline would have built.
    let func_obj: Object = bfun.into();
    let parent = rt.active_frame_index;
    // The arena is reached through a raw root: the frame lives in the
    // runtime, and the borrow must not conflict with the scope's own.
    let arena_ptr: *mut VmStack = &raw mut rt.vm_stack;
    // SAFETY: the pointer was just taken from the runtime.
    let arena = unsafe { &mut *arena_ptr };
    let runtime: &RuntimeImpl = &rt;
    let cap = std::cmp::max(bc.arg_count as usize, args.len()) + bc.local_buf_size as usize;
    let frame_idx = match arena.alloc_frame(func_obj, bc.mode(), cap) {
        Ok(idx) => idx,
        Err(e) => {
            match e {
                FrameAllocError::Oom => {
                    let _ = scope.throw_stack_fail();
                }
                FrameAllocError::Exceeded => {
                    let _ = scope.throw_stack_exceeded();
                }
            }
            if let Some(s) = callee_scope {
                drop(s.into_inner());
            }
            return DirectCall::Exception;
        }
    };
    arena.frame_mut(frame_idx).prev = parent;
    // Seed the traced receiver root, exactly as the trampoline does for the
    // frames it builds (a GC during the native call must see it).
    arena.frame_mut(frame_idx).this_obj = this_obj;
    arena.init_frame_args(frame_idx, bc.arg_count, bc.var_count, args);

    let (arg_base, var_base, sp_base) = {
        let frame = arena.frame(frame_idx);
        let arg_base = frame.buf.data_ptr_unchecked();
        let var_base = unsafe { arg_base.add(frame.var_base_offset as usize) };
        let sp_base =
            unsafe { arg_base.add((frame.var_base_offset + frame.var_count as u32) as usize) };
        (arg_base, var_base, sp_base)
    };
    let cur_sp_offset: *mut i32 = &mut arena.frame_mut(frame_idx).cur_sp_offset;
    let home_obj = bfun.payload().home_object();

    // The callee's compiled code, entered directly.
    let (kind, frame) = match crate::jit::invoke_native(
        runtime,
        &bc,
        callee_scope_ptr,
        arg_base,
        var_base,
        sp_base,
        cur_sp_offset,
        this_obj,
        0,
        args.len() as u32,
        arena_ptr,
        frame_idx.as_usize() as u32,
        new_target,
        home_obj,
    ) {
        Some(pair) => pair,
        // The code was retired between the check above and here: fall back.
        None => {
            arena.detach_and_free(frame_idx, rt.gc_heap_mut());
            if let Some(s) = callee_scope {
                drop(s.into_inner());
            }
            return DirectCall::Fallback;
        }
    };

    let outcome = match kind {
        crate::jit::JitExitKind::Done => {
            // The value must outlive the callee's scope: promote it into the
            // caller's (the trampoline does the same).
            let value = frame.out_value;
            match callee_scope.as_mut() {
                Some(s) => DirectCall::Done(value.promote(s.scope_mut())),
                None => DirectCall::Done(value),
            }
        }
        crate::jit::JitExitKind::Interrupt => DirectCall::Interrupt,
        crate::jit::JitExitKind::Unwind => DirectCall::Exception,
        crate::jit::JitExitKind::Exception => {
            // The callee's frame is still alive: build the backtrace now, so
            // the error names the callee (the interpreter's frame runner does
            // the same before it unwinds).
            if let Exception::Error(err_val) = runtime.exception()
                && is_backtrace_needed(err_val)
            {
                scope.build_backtrace_to(err_val, None, 0);
            }
            // Resume the callee in the interpreter with the pending exception:
            // `execute_with_arena` starts with `throwed = true`, so the normal
            // catch scan runs in the *callee's* frame. Returning the exception
            // to the JIT caller here would skip a `catch` inside the callee.
            {
                let head = arena.frame_mut(frame_idx);
                head.cur_pc_offset = frame.out_pc as usize;
                head.cur_sp_offset = frame.out_sp;
            }
            let (value, _completed) =
                execute_trampoline(bc, frame_idx, scope, this_obj, new_target);
            if value.is_exception() {
                DirectCall::Exception
            } else {
                match callee_scope.as_mut() {
                    Some(s) => DirectCall::Done(value.promote(s.scope_mut())),
                    None => DirectCall::Done(value),
                }
            }
        }
        crate::jit::JitExitKind::Resume | crate::jit::JitExitKind::Retry => {
            // The callee's code could not go on (an opcode it cannot lower, or
            // a pc it has no entry for): hand the callee back to the
            // interpreter, which resumes it at the pc it reported.
            {
                let head = arena.frame_mut(frame_idx);
                head.cur_pc_offset = frame.out_pc as usize;
                head.cur_sp_offset = frame.out_sp;
            }
            let (value, _completed) =
                execute_trampoline(bc, frame_idx, scope, this_obj, new_target);
            if value.is_exception() {
                DirectCall::Exception
            } else {
                // The trampoline promoted the value into the callee's scope;
                // move it on to the caller's (the callee's scope goes away
                // below, and it still holds the values the native part left
                // on the callee's operand stack).
                match callee_scope.as_mut() {
                    Some(s) => DirectCall::Done(value.promote(s.scope_mut())),
                    None => DirectCall::Done(value),
                }
            }
        }
        // The baseline never emits these two (the bytecode ABI's protocol is unused),
        // so this arm is unreachable; if it ever fires, the call fails rather
        // than re-running the callee (a fallback would call it twice).
        crate::jit::JitExitKind::CallBytecode | crate::jit::JitExitKind::TailCall => {
            DirectCall::Exception
        }
    };

    // The frame is ours: detach its var refs and free it (the trampoline
    // leaves the outermost frame to its owner). The frame may be gone already
    // — a tail call in the interpreted continuation replaces it — so check
    // before freeing it (the same pattern as `BytecodeFunction::call`). The
    // detach carries the write barrier for the same reason as there: the
    // frame slot was the root, the (possibly old) VarRef node is the edge.
    if let Some(var_refs) = arena.try_var_refs_mut(frame_idx) {
        for vr in var_refs.iter_mut() {
            vr.detach_with_barrier(rt.gc_heap_mut());
        }
    }
    if arena
        .try_frame(frame_idx)
        .is_some_and(|f| f.state == FrameState::Active)
    {
        arena.free_frame(frame_idx);
    }
    if let Some(s) = callee_scope {
        drop(s.into_inner());
    }
    outcome
}
