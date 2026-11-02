//! The call path: the general call helpers, the monomorphic direct
//! path and its guard, and the builtin `apply` fast path.
//!
//! The site id baked into `imm2` is resolved here back through the
//! feedback table — the guard and the recorded callee must stay in
//! this module together.

use super::*;

/// `throw`: `[value, ^] -> [^]`, raising `value`. Always `HELPER_EXCEPTION`, so
/// the native frame leaves with the pc the interpreter would have reported for
/// this instruction: `cur_pc_offset` is *post*-instruction in both engines (the
/// interpreter's `PC_FETCH!` already stepped over the opcode) and
/// `do_build_backtrace` subtracts one byte to find the line, so the exit passes
/// `pc + size` like every other helper failure.
///
/// The value is read before the pop (the frame's `cur_sp_offset` still covers
/// its slot, so a GC inside the raise sees it) and the pop happens before the
/// raise, mirroring the interpreter's `SP_GO!(-1)`.
pub(super) unsafe extern "C" fn h_throw(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let value = *sp.offset(-1);
        f.sp = sp.offset(-1);
        let _ = scope_of(f).throw_value(value);
        HELPER_EXCEPTION
    }
}

/// `throw_error <atom> <type>`: the engine-generated error named by `imm2`.
/// `0 -> 0`; always raises.
pub(super) unsafe extern "C" fn h_throw_error(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let atom = crate::atom::Atom::from_id(f.imm);
        let type_ = f.imm2 as u8;
        crate::bytecode::engine_ops::throw_error(scope_of(f), atom, type_);
        HELPER_EXCEPTION
    }
}

/// `regexp`: `[id, ^] -> [regexp, ^]`. A non-integer operand leaves the stack
/// untouched, exactly like the interpreter's `if let Some` with no else arm.
///
/// On failure this reports the error instead of parking the interpreter's
/// `EXCEPTION` sentinel on the stack and continuing; the two differ only when
/// allocating the object fails.
pub(super) unsafe extern "C" fn h_regexp(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let Some(re_id) = (*sp.offset(-1)).as_int() else {
            return HELPER_OK;
        };
        match crate::bytecode::engine_ops::make_regexp(scope_of(f), re_id) {
            Ok(value) => {
                *sp.offset(-2) = value;
                f.sp = sp.offset(-1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `push_bigint_i32 <value>`: `[^] -> [bigint, ^]`.
pub(super) unsafe extern "C" fn h_push_bigint_i32(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        match crate::bytecode::engine_ops::bigint_from_i32(scope_of(f), f.imm as i32) {
            Ok(value) => {
                *sp = value;
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `call <argc>`: `[func, arg1..argN, ^] -> [ret, ^]` (the direct-call design).
///
/// The callee runs through the interpreter's own `call_internal` on this very
/// frame, so the operand stack stays authoritative and a callee with a native
/// entry runs natively inside the helper (the trampoline's `invoke_native`).
/// The caller does **not** leave its native frame: only the C stack grows, one
/// Rust frame per nested JS call, bounded by `js_check_thread_stack_overflow`
/// (`BytecodeFunction::call`) so runaway recursion is still a catchable
/// `stack overflow`, not a crash.
///
/// On failure nothing is popped (`f.sp` keeps the pre-instruction height the
/// interpreter rethrows from) and the caller exits `HELPER_EXCEPTION`; the
/// interpreter's `find_catch_in_frame` then scans this frame's stack.
pub(super) unsafe extern "C" fn h_call(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let argc = f.imm as usize;
        let base = f.sp.offset(-(argc as isize) - 1);
        let func = *base;
        let args = std::slice::from_raw_parts(base.add(1), argc);
        match func.call_internal(scope, JSValue::UNDEFINED, JSValue::UNDEFINED, args, 0) {
            Ok(value) => {
                *base = value;
                f.sp = base.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `call_method <argc>`: `[this, func, arg1..argN, ^] -> [ret, ^]` (the direct-call design). Same
/// frame/native-entry contract as [`h_call`]; `this` is the receiver below the
/// callee.
pub(super) unsafe extern "C" fn h_call_method(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let argc = f.imm as usize;
        let base = f.sp.offset(-(argc as isize) - 2);
        let method_this = *base;
        let func = *base.add(1);
        let args = std::slice::from_raw_parts(base.add(2), argc);
        match func.call_internal(scope, method_this, JSValue::UNDEFINED, args, 0) {
            Ok(value) => {
                *base = value;
                f.sp = base.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `call <argc>` at a *monomorphic* site (the direct-call path):
/// `[func, arg1..argN, ^] -> [ret, ^]`.
///
/// The interpreter recorded one callee for this site, so the helper can enter
/// that function's compiled code directly: no `call_internal`, no trampoline,
/// no interpreter frame prologue. The guard is the recorded callee — a call
/// whose callee is not it (or one the direct path cannot serve) falls back
/// to the general path below.
pub(super) unsafe extern "C" fn h_call_inline(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let argc = f.imm as usize;
        let base = f.sp.offset(-(argc as isize) - 1);
        let func = *base;
        let args = std::slice::from_raw_parts(base.add(1), argc);
        if let Some(result) = try_direct_call(f, func, JSValue::UNDEFINED, args) {
            return match result {
                Ok(value) => {
                    *base = value;
                    f.sp = base.add(1);
                    HELPER_OK
                }
                Err(status) => status,
            };
        }
        // The general path.
        match func.call_internal(scope_of(f), JSValue::UNDEFINED, JSValue::UNDEFINED, args, 0) {
            Ok(value) => {
                *base = value;
                f.sp = base.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `call_method <argc>` at a *monomorphic* site (the direct-call path):
/// `[this, func, arg1..argN, ^] -> [ret, ^]`. Same guard and fallback as
/// [`h_call_inline`]; the receiver is the callee's `this`.
pub(super) unsafe extern "C" fn h_call_method_inline(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let argc = f.imm as usize;
        let base = f.sp.offset(-(argc as isize) - 2);
        let method_this = *base;
        let func = *base.add(1);
        let args = std::slice::from_raw_parts(base.add(2), argc);
        if let Some(result) = try_direct_call(f, func, method_this, args) {
            return match result {
                Ok(value) => {
                    *base = value;
                    f.sp = base.add(1);
                    HELPER_OK
                }
                Err(status) => status,
            };
        }
        // The general path.
        match func.call_internal(scope_of(f), method_this, JSValue::UNDEFINED, args, 0) {
            Ok(value) => {
                *base = value;
                f.sp = base.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// Tally one direct-call attempt and hand back what the caller does with it:
/// `Some(Ok/Err)` when the direct path served the call, `None` when the
/// general path must. A hit is a site the direct path served, a miss one it
/// turned away.
#[inline(always)]
fn note_direct(
    f: &mut JitExecFrame,
    direct: crate::jit::DirectCall,
) -> Option<Result<JSValue, u32>> {
    match direct {
        crate::jit::DirectCall::Done(value) => {
            f.inline_call_hits = f.inline_call_hits.saturating_add(1);
            Some(Ok(value))
        }
        crate::jit::DirectCall::Exception => {
            f.inline_call_hits = f.inline_call_hits.saturating_add(1);
            Some(Err(HELPER_EXCEPTION))
        }
        crate::jit::DirectCall::Interrupt => {
            f.inline_call_hits = f.inline_call_hits.saturating_add(1);
            Some(Err(HELPER_INTERRUPT))
        }
        crate::jit::DirectCall::Fallback => {
            f.inline_call_misses = f.inline_call_misses.saturating_add(1);
            None
        }
    }
}

/// Run the callee through the direct path: enter its compiled
/// code without the interpreter's call protocol.
///
/// `None` means "the general path must serve this call": the guard missed
/// (the callee is not the recorded one), or the direct path cannot serve it
/// (a generator, a cross-realm callee, one without compiled code). `Err` is
/// the helper status the call must exit with (an exception or an interrupt).
///
/// **Must be inlined**: it sits on the hot path of every monomorphic call, and
/// the optimizer's own judgement leaves it out of line — an extra call frame
/// per call site.
#[inline(always)]
unsafe fn try_direct_call(
    f: &mut JitExecFrame,
    func: JSValue,
    this_obj: JSValue,
    args: &[JSValue],
) -> Option<Result<JSValue, u32>> {
    // The guard: the callee must be exactly the recorded one.
    let Some(recorded) = (unsafe { recorded_callee(f) }) else {
        f.inline_call_misses = f.inline_call_misses.saturating_add(1);
        return None;
    };
    if !callee_is(func, recorded) {
        f.inline_call_misses = f.inline_call_misses.saturating_add(1);
        return None;
    }
    let direct = {
        let scope = unsafe { scope_of(f) };
        crate::jit::direct_call::call_native_direct(scope, func, this_obj, JSValue::UNDEFINED, args)
    };
    note_direct(f, direct)
}

/// The callee the interpreter recorded at this call site (the direct-call path's
/// guard). `imm2` carries the site id the compiled code baked in.
unsafe fn recorded_callee(f: &JitExecFrame) -> Option<crate::function::Bytecode> {
    unsafe { recorded_callee_at(f, f.imm2) }
}

/// [`recorded_callee`] for an explicit site id. The inline guard encodes the
/// method-call flag in the top bit of `imm2`, so it reads the id itself.
unsafe fn recorded_callee_at(f: &JitExecFrame, site: u32) -> Option<crate::function::Bytecode> {
    // SAFETY: `f.bytecode` points at the running function's bytecode, which
    // its function object keeps alive for the whole activation.
    let bc = unsafe { &*f.bytecode };
    bc.feedback.call_callee(site)
}

/// Whether the live callee is exactly the recorded one.
fn callee_is(func: JSValue, recorded: crate::function::Bytecode) -> bool {
    match crate::function::BCWrapper::try_from(func) {
        Ok(bfun) => bfun.payload().bytecode().gc_node_ptr() == recorded.gc_node_ptr(),
        Err(_) => false,
    }
}

/// `imm2`'s top bit marks a `call_method` inline site (the receiver sits
/// below the callee); the low bits are the call-site id.
pub(crate) const INLINE_METHOD_BIT: u32 = 1 << 31;

/// The guard for an *inlined* call site.
///
/// Returns 1 when the callee on the stack is the recorded one: the caller's
/// generated code then runs the callee's body inline. A miss returns 0 and the
/// caller takes the general call path, exactly as the direct helper would.
/// The counters keep the existing "monomorphic fast path" meaning: a hit is a
/// site the guard served, whether inline or direct.
pub(super) unsafe extern "C" fn h_inline_guard(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let argc = f.imm as usize;
        let method = f.imm2 & INLINE_METHOD_BIT != 0;
        let site = f.imm2 & !INLINE_METHOD_BIT;
        let base = f.sp.offset(-(argc as isize) - if method { 2 } else { 1 });
        let func = if method { *base.add(1) } else { *base };
        let Some(recorded) = recorded_callee_at(f, site) else {
            f.inline_call_misses = f.inline_call_misses.saturating_add(1);
            return 0;
        };
        if callee_is(func, recorded) {
            f.inline_call_hits = f.inline_call_hits.saturating_add(1);
            f.frame_inline_hits = f.frame_inline_hits.saturating_add(1);
            1
        } else {
            f.inline_call_misses = f.inline_call_misses.saturating_add(1);
            0
        }
    }
}

/// `call_constructor <argc>`: `[func, new_target, arg1..argN, ^] -> [ret, ^]`
/// (the direct-call path). `call_internal` owns the constructor protocol (legacy instance
/// creation, derived-constructor `this`, `super()` prototype fix-up).
pub(super) unsafe extern "C" fn h_call_constructor(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let argc = f.imm as usize;
        let base = f.sp.offset(-(argc as isize) - 2);
        let func = *base;
        let new_target = *base.add(1);
        let args = std::slice::from_raw_parts(base.add(2), argc);
        match func.call_internal(
            scope,
            JSValue::UNDEFINED,
            new_target,
            args,
            JS_CALL_FLAG_CONSTRUCTOR,
        ) {
            Ok(value) => {
                *base = value;
                f.sp = base.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `apply <magic>`: `[func, this_arg, array_arg, ^] -> [ret, ^]` (the direct-call design), reusing
/// the interpreter's `js_function_apply` (which flattens `array_arg` through
/// `build_arg_list`). Mirroring the interpreter's observable result, the
/// window is kept until success so the failure exit reports the
/// pre-instruction height.
pub(super) unsafe extern "C" fn h_apply(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let magic = f.imm as i32;
        let sp = f.sp;
        let func = *sp.offset(-3);
        let this_arg = *sp.offset(-2);
        let array_arg = *sp.offset(-1);
        let scope = scope_of(f);
        match js_function_apply(
            scope,
            CallContext::new(func, &[this_arg, array_arg], magic, None),
        ) {
            Ok(value) => {
                let base = sp.offset(-3);
                *base = value;
                f.sp = base.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `call_method <argc>` at a site that only ever called a native builtin:
/// `[this, func, arg1..argN, ^] -> [ret, ^]`.
///
/// The builtin worth recognizing here is `Function.prototype.apply`: its whole
/// body is a call, and the general path reaches it through `call_internal` →
/// the native frame → `js_function_apply` → `build_arg_list` → `call_internal`
/// again. When the receiver is a bytecode function with installed code and
/// `arguments[1]` is a packed array, its elements already are the argument
/// list: borrow them and enter the receiver's compiled code directly. Every
/// other shape (another native, a replaced `apply`, a sparse array-like, a
/// receiver the direct path cannot serve) takes the general path below, which
/// owns the full apply semantics.
pub(super) unsafe extern "C" fn h_call_apply(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let argc = f.imm as usize;
        let base = f.sp.offset(-(argc as isize) - 2);
        let method_this = *base;
        let func = *base.add(1);
        let window = std::slice::from_raw_parts(base.add(2), argc);
        let this_arg = window.first().copied().unwrap_or(JSValue::UNDEFINED);
        let array_arg = window.get(1).copied().unwrap_or(JSValue::UNDEFINED);

        if let Some(result) = try_builtin_apply(f, method_this, func, this_arg, array_arg) {
            return match result {
                Ok(value) => {
                    *base = value;
                    f.sp = base.add(1);
                    HELPER_OK
                }
                Err(status) => status,
            };
        }

        // The general path (the call_method helper's body).
        match func.call_internal(scope_of(f), method_this, JSValue::UNDEFINED, window, 0) {
            Ok(value) => {
                *base = value;
                f.sp = base.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// Serve `Function.prototype.apply` when `func` is that builtin. `None` leaves
/// the call to the general path.
#[inline]
unsafe fn try_builtin_apply(
    f: &mut JitExecFrame,
    method_this: JSValue,
    func: JSValue,
    this_arg: JSValue,
    array_arg: JSValue,
) -> Option<Result<JSValue, u32>> {
    let Ok(native) = NativeFunction::try_from(func) else {
        f.inline_call_misses = f.inline_call_misses.saturating_add(1);
        return None;
    };
    if native.payload().callback != Callback::Func(js_function_apply) {
        f.inline_call_misses = f.inline_call_misses.saturating_add(1);
        return None;
    }

    let scope = unsafe { scope_of(f) };
    // `js_function_apply` resolves the receiver first: a non-callable receiver
    // must throw before the argument array is touched.
    let Ok(func_obj) = method_this.ensure_callable(scope) else {
        return Some(Err(HELPER_EXCEPTION));
    };
    if array_arg.is_undefined_or_null() {
        return Some(apply_direct_call(f, scope, func_obj, this_arg, &[]));
    }
    let Some(array_obj) = array_arg.as_object() else {
        let _ = scope.throw_type_error_not_an_object();
        return Some(Err(HELPER_EXCEPTION));
    };
    // Same length rule as `build_arg_list`; a proxy or a getter can run here.
    let length = match array_obj.get_length_prop(scope) {
        Ok(length) => length as u32,
        Err(_) => return Some(Err(HELPER_EXCEPTION)),
    };
    if length > JS_MAX_LOCAL_VARS as u32 {
        let _ = scope.throw_internal_error("too many arguments");
        return Some(Err(HELPER_EXCEPTION));
    }
    // A packed array's elements are the argument list. The callee's frame
    // copies them before any user code runs, and the caller's operand stack
    // keeps the array (and its elements) alive for the whole call.
    if array_obj.is_fast_array()
        && let Some(elements) = array_obj.payload_array()
        && elements.len() == length as usize
    {
        return Some(apply_direct_call(
            f,
            scope,
            func_obj,
            this_arg,
            elements.values(),
        ));
    }
    match build_arg_list(scope, array_arg) {
        Ok(args) => Some(apply_direct_call(f, scope, func_obj, this_arg, &args)),
        Err(_) => Some(Err(HELPER_EXCEPTION)),
    }
}

/// Call the receiver of `Function.prototype.apply`: its compiled code when it
/// has one, the general call otherwise.
fn apply_direct_call(
    f: &mut JitExecFrame,
    scope: &mut ScopedContext,
    func_obj: Object,
    this_arg: JSValue,
    args: &[JSValue],
) -> Result<JSValue, u32> {
    let direct = crate::jit::direct_call::call_native_direct(
        scope,
        func_obj.into(),
        this_arg,
        JSValue::UNDEFINED,
        args,
    );
    match note_direct(f, direct) {
        Some(result) => result,
        // The general path owns the full apply semantics.
        None => func_obj
            .call(scope, this_arg, args)
            .map_err(|_| HELPER_EXCEPTION),
    }
}
