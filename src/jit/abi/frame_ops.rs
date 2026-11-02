//! Frame operations: catch offsets, rest parameters, special objects,
//! captured variables and the safepoint.

use super::*;

/// `nip_catch`: `[catch_offset, …, value, ^] -> […, value, ^]`; unwinds the
/// operand stack down to the innermost `CatchOffset` and writes `value` there.
pub(super) unsafe extern "C" fn h_nip_catch(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp_base = f.sp_base;
        let mut sp = f.sp.offset(-1);
        let value = *sp;
        while sp > sp_base && !(*sp.offset(-1)).is_catch_offset() {
            sp = sp.offset(-1);
        }
        if sp == sp_base {
            let _ = scope.throw_internal_error("nip_catch");
            HELPER_EXCEPTION
        } else {
            *sp.offset(-1) = value;
            f.sp = sp;
            HELPER_OK
        }
    }
}

/// `rest`: build the rest-parameter array from the *actual* arguments, exactly
/// like the interpreter's `REST` (`args[start..]`, `f.imm` is `start`). The
/// frame's `arg_base` is the buffer start the interpreter's own `args` slice
/// points at, so a short call yields an empty array and never the `undefined`
/// padding up to the declared count.
pub(super) unsafe extern "C" fn h_rest(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let start = f.imm as usize;
        let real = f.arg_real_count as usize;
        let values = if start < real {
            std::slice::from_raw_parts(f.arg_base, real)[start..].to_vec()
        } else {
            Vec::new()
        };
        let value = match crate::array::Array::from_values(scope, values) {
            Ok(arr) => JSValue::from(arr),
            Err(_) => JSValue::EXCEPTION,
        };
        push_status(f, value)
    }
}

/// `special_object`: the frame-entry special objects the parser emits. Mirrors
/// the interpreter's arm for every kind except `ImportMeta` (module state,
/// which the verifier keeps rejecting). `imm` is the `SpecialObject` id.
pub(super) unsafe extern "C" fn h_special_object(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let args = std::slice::from_raw_parts(f.arg_base, f.arg_real_count as usize);
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        let func_obj = (*f.arena).frame(frame_idx).func_obj;
        let value = match std::mem::transmute::<u8, SpecialObject>(f.imm as u8) {
            SpecialObject::Arguments => js_build_arguments(scope, args),
            SpecialObject::MappedArguments => {
                match js_build_mapped_arguments(
                    scope,
                    args,
                    f.arg_base,
                    f.var_base,
                    &mut *f.arena,
                    frame_idx,
                    (*f.bytecode).arg_count,
                    (*f.bytecode).var_count,
                    func_obj,
                ) {
                    Some(obj) => JSValue::from(obj),
                    // Like the interpreter's arm: the failure pushes nothing.
                    None => return HELPER_EXCEPTION,
                }
            }
            SpecialObject::ThisFunc => JSValue::from(func_obj),
            SpecialObject::NewTarget => f.new_target,
            SpecialObject::HomeObject => f.home_obj.map_or(JSValue::UNDEFINED, JSValue::from),
            SpecialObject::VarObject => scope
                .create_base_object_with_proto(None)
                .map_or(JSValue::EXCEPTION, JSValue::from),
            SpecialObject::ImportMeta => {
                let _ = scope.throw_internal_error("special_object");
                JSValue::EXCEPTION
            }
        };
        push_status(f, value)
    }
}

/// `push_this` for a sloppy function: `undefined`/`null` becomes the global
/// object and any other primitive is objectified (the interpreter's arm).
/// Strict functions never reach this — their `this` is the frame's raw value,
/// which the compiler pushes inline.
pub(super) unsafe extern "C" fn h_push_this(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let this_obj = f.this;
        let value = if this_obj.is_undefined_or_null() {
            JSValue::from(scope.global_obj())
        } else if this_obj.is_object() {
            this_obj
        } else {
            let Ok(objectified) = this_obj.objectify(scope) else {
                // The interpreter breaks before `SP_PUT!` on this path too.
                return HELPER_EXCEPTION;
            };
            objectified.into()
        };
        *f.sp = value;
        f.sp = f.sp.add(1);
        HELPER_OK
    }
}

/// `fclosure`: close over the constant pool's nested bytecode. The captured
/// var refs come from the *current* frame's function object, exactly as the
/// interpreter resolves them (the nested bytecode's own free variables bind to
/// the enclosing activation). `imm` is the constant index.
pub(super) unsafe extern "C" fn h_fclosure(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let nested = (*f.cpools.add(f.imm as usize)).as_bytecode_unchecked();
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        let func_obj = (*f.arena).frame(frame_idx).func_obj;
        let mut bfun = BCWrapper::try_from(JSValue::from(func_obj)).ok();
        let captured: &mut [VarRef] = match bfun.as_mut() {
            Some(bfun) => &mut *bfun.payload_mut().captured_vars_mut(),
            None => &mut [],
        };
        let value = nested
            .make_closure_for_arena(scope, None, captured, &mut *f.arena, frame_idx)
            .map_or(JSValue::EXCEPTION, Into::into);
        push_status(f, value)
    }
}

/// The current frame's captured var refs, resolved from its func_obj exactly
/// as the interpreter does (each function in the call chain owns its own
/// `captured_vars`). Takes a raw pointer because it hands out `&mut` into GC
/// memory that the caller's `&JitExecFrame` does not exclusively own; the
/// returned lifetime is laundered from the raw pointer, not tied to `f`.
pub(super) unsafe fn captured_vars_of<'a>(f: *const JitExecFrame) -> &'a mut [VarRef] {
    unsafe {
        let frame_idx = FrameIndex::from_u32((*f).cur_frame_idx);
        let func_obj = (*(*f).arena).frame(frame_idx).func_obj;
        match BCWrapper::try_from(JSValue::from(func_obj)) {
            // `bfun` is a Copy `GcRef`, so the payload borrows GC memory that
            // outlives this call; launder it to reuse the caller's lifetime.
            Ok(mut bfun) => {
                let cv: *mut [VarRef] = bfun.payload_mut().captured_vars_mut();
                &mut *cv
            }
            Err(_) => &mut [],
        }
    }
}

/// The running function's bytecode, for the rare helper that must name a
/// binding (the uninitialized-var_ref error message).
unsafe fn current_bytecode(f: &JitExecFrame) -> Option<crate::function::Bytecode> {
    let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
    let func_obj = unsafe { (*f.arena).frame(frame_idx).func_obj };
    BCWrapper::try_from(JSValue::from(func_obj))
        .ok()
        .map(|bfun| bfun.payload().bytecode())
}

/// `get_var_ref`: push a captured binding. `imm` is the var_ref index.
pub(super) unsafe extern "C" fn h_get_var_ref(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let value = captured_vars_of(f)[f.imm as usize].value();
        *f.sp = value;
        f.sp = f.sp.add(1);
        HELPER_OK
    }
}

/// `put_var_ref`: write the top of stack back and pop it.
pub(super) unsafe extern "C" fn h_put_var_ref(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let value = *f.sp.offset(-1);
        let mut rt = scope_of(f).rt_ptr();
        captured_vars_of(f)[f.imm as usize].set_value(&mut rt, value);
        f.sp = f.sp.offset(-1);
        HELPER_OK
    }
}

/// `set_var_ref`: write the top of stack back, keeping it (assignment result).
pub(super) unsafe extern "C" fn h_set_var_ref(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let value = *f.sp.offset(-1);
        let mut rt = scope_of(f).rt_ptr();
        captured_vars_of(f)[f.imm as usize].set_value(&mut rt, value);
        HELPER_OK
    }
}

/// `get_var_ref_check`: push a captured binding unless it is still in the TDZ.
/// The error is raised here so the message names the binding, exactly like the
/// interpreter's `throw_reference_error_uninitialized2(_, _, true)`.
pub(super) unsafe extern "C" fn h_get_var_ref_check(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let idx = f.imm as u16;
        let value = captured_vars_of(f)[idx as usize].value();
        if value.is_uninitialized() {
            if let Some(b) = current_bytecode(f) {
                let _ = scope_of(f).throw_reference_error_uninitialized2(&b, idx, true);
            }
            return HELPER_EXCEPTION;
        }
        *f.sp = value;
        f.sp = f.sp.add(1);
        HELPER_OK
    }
}

/// `put_var_ref_check`: TDZ-checked `put_var_ref`.
pub(super) unsafe extern "C" fn h_put_var_ref_check(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let idx = f.imm as u16;
        if captured_vars_of(f)[idx as usize].value().is_uninitialized() {
            if let Some(b) = current_bytecode(f) {
                let _ = scope_of(f).throw_reference_error_uninitialized2(&b, idx, true);
            }
            return HELPER_EXCEPTION;
        }
        let value = *f.sp.offset(-1);
        let mut rt = scope_of(f).rt_ptr();
        captured_vars_of(f)[idx as usize].set_value(&mut rt, value);
        f.sp = f.sp.offset(-1);
        HELPER_OK
    }
}

/// `put_var_ref_check_init`: initialize a captured `this` binding, which must
/// still be in the TDZ (re-initialization is a ReferenceError).
pub(super) unsafe extern "C" fn h_put_var_ref_check_init(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let idx = f.imm as u16;
        if !captured_vars_of(f)[idx as usize].value().is_uninitialized() {
            if let Some(b) = current_bytecode(f) {
                let _ = scope_of(f).throw_reference_error_uninitialized2(&b, idx, true);
            }
            return HELPER_EXCEPTION;
        }
        let value = *f.sp.offset(-1);
        let mut rt = scope_of(f).rt_ptr();
        captured_vars_of(f)[idx as usize].set_value(&mut rt, value);
        f.sp = f.sp.offset(-1);
        HELPER_OK
    }
}

/// Loop back edge: deliver interrupts and run a deferred collection. Mirrors
/// the interpreter's per-opcode safepoint.
pub(super) unsafe extern "C" fn h_safepoint(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        // The back edge is the native tier's hotness signal: the interpreter
        // stops counting them once the code is installed, and the second
        // gate needs them.
        f.backedges = f.backedges.saturating_add(1);
        let scope = scope_of(f);
        if scope.poll_interrupts() {
            return HELPER_INTERRUPT;
        }

        let mut rt = scope.rt_ptr();
        if rt.gc_should_collect() {
            // A scope that is never cleared accumulates its LOCAL list, and
            // every `remove_local` then scans it linearly. The high-water mark
            // is sampled here, right before the clear, and reported as a bench
            // counter: a regression shows up as a peak that grows with the
            // iteration count.
            let heap = scope.gc_heap();
            let stack_id = scope.ctx_ptr.scope_stack_id();
            let max_depth = heap.scope_max_depth(stack_id);
            let mut peak = 0u32;
            for depth in 1..=max_depth {
                if let Some(s) = heap.scope(stack_id, depth) {
                    peak = peak.max(s.count().min(u32::MAX as usize) as u32);
                }
            }
            f.scope_local_peak = f.scope_local_peak.max(peak);

            // Only a `GcScopeKind::None` frame borrows a scope the trampoline
            // does not clear (a native caller's, whose LOCAL flags are its only
            // protection). A Light/Full frame always runs in a scope at or
            // above the trampoline's base depth — the scopes the interpreter
            // clears at its own safepoints — so clearing here is safe even
            // though the frame holds a borrowed *view* of it.
            if f.gc_scope_kind != GcScopeKind::None as u32 {
                scope.clear_locals();
            }
            rt.gc_collect_dispatch();
        }
        HELPER_OK
    }
}
