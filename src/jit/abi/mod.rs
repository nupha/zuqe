// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The native entry ABI and the helper trampolines.
//!
//! Compiled code never re-implements JS semantics. A helper reads the operands
//! straight out of the frame buffer (exactly like the interpreter's own `sp`),
//! calls the interpreter's existing Rust function, writes the result back, and
//! reports failure through its return value. The frame buffer is the only place
//! operands live, so a GC triggered inside a helper sees them.

use crate::{
    JSValue, ScopedContext,
    bytecode::{
        OpCode, SpecialObject,
        arithmatic::{UnaryOp, op_binary_logic},
        operators::{js_operator_delete, js_operator_in, js_operator_instanceof},
    },
    context::GlobalVarSetMode,
    function::{
        BCWrapper, CallContext, Callback, GcScopeKind, JS_CALL_FLAG_CONSTRUCTOR, JS_MAX_LOCAL_VARS,
        NativeFunction, VarRef, build_arg_list, js_build_arguments, js_build_mapped_arguments,
        js_function_apply,
    },
    object::{JS_PROP_NORMAL, JS_PROP_TMASK, JSProperty, Object, ThrowPolicy},
    vm_stack::{FrameIndex, VmStack},
};
use gc_lite::GcHead;

mod frame;
pub(crate) use frame::*;

mod arith;
pub(crate) use arith::*;

mod globals;
use globals::*;

mod property;
pub(crate) use property::*;

mod frame_ops;
use frame_ops::*;

mod object_ops;
use object_ops::*;

mod with_ops;
use with_ops::*;

mod calls;
pub(crate) use calls::*;

mod table;
pub(crate) use table::*;

#[inline]
unsafe fn scope_of<'a>(f: &JitExecFrame) -> &'a mut ScopedContext<'static> {
    unsafe { &mut *(f.scope as *mut ScopedContext<'static>) }
}

/// Push `value` onto the operand stack and report whether it is the engine's
/// exception sentinel — the helper's "push the result, then fail if it raised"
/// epilogue.
///
/// SAFETY: `f.sp` addresses the frame's own operand stack for the whole
/// activation, which is exactly where the interpreter keeps it.
fn push_status(f: &mut JitExecFrame, value: JSValue) -> u32 {
    unsafe {
        *f.sp = value;
        f.sp = f.sp.add(1);
    }
    if value.is_exception() {
        HELPER_EXCEPTION
    } else {
        HELPER_OK
    }
}

unsafe extern "C" fn h_post_inc(f: *mut JitExecFrame) -> u32 {
    unsafe { post_inc_dec(f, true) }
}
unsafe extern "C" fn h_post_dec(f: *mut JitExecFrame) -> u32 {
    unsafe { post_inc_dec(f, false) }
}

unsafe extern "C" fn h_put_var(f: *mut JitExecFrame) -> u32 {
    unsafe { global_put(f, false) }
}
unsafe extern "C" fn h_put_var_init(f: *mut JitExecFrame) -> u32 {
    unsafe { global_put(f, true) }
}
