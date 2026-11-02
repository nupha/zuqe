//! The native entry: build a `JitExecFrame`, call the installed
//! code and fold its counters back.

use super::*;

/// Run the compiled entry for `bc`, if one is installed. Returns the exit kind
/// and the frame the code wrote its results into; `None` means "no native code,
/// interpret as usual".
///
/// `pc` is 0 at the function entry, or the OSR resume point otherwise: the pc
/// after a call the trampoline performed for us, or a loop header the
/// interpreter's back edge reported (back-edge OSR). The compiled dispatch
/// selects the block for it and exits `Retry` for a pc it has no entry for.
///
/// # Safety
/// The caller (the interpreter) must be at `pc`, with no pending exception, the
/// frame's `cur_sp_offset` holding the operand-stack height that `pc` expects
/// (the OSR prologue reads it), and the frame pointers must belong to `bc`'s
/// current activation.
#[allow(clippy::too_many_arguments)]
pub(crate) fn invoke_native(
    rt: &RuntimeImpl,
    bc: &Bytecode,
    scope: *mut u8,
    arg_base: *mut JSValue,
    var_base: *mut JSValue,
    sp_base: *mut JSValue,
    cur_sp_offset: *mut i32,
    this: JSValue,
    pc: u32,
    arg_real_count: u32,
    arena: *mut crate::vm_stack::VmStack,
    cur_frame_idx: u32,
    new_target: JSValue,
    home_obj: Option<crate::object::Object>,
) -> Option<(JitExitKind, JitExecFrame)> {
    let state = rt.jit.as_deref()?;
    let entry = bc.jit.entry()?;
    // The two global objects are context roots, so the frame can hold them
    // as plain values (no tracing); the sites' shapes are re-checked on every
    // access.
    let (global_obj, global_var_obj) = if scope.is_null() {
        (JSValue::UNDEFINED, JSValue::UNDEFINED)
    } else {
        // SAFETY: `scope` is the running `ScopedContext` (see the contract
        // above); it outlives this call.
        let scope = unsafe { &*(scope as *const crate::ScopedContext) };
        (
            JSValue::from(scope.global_obj()),
            JSValue::from(scope.global_var_obj()),
        )
    };
    let mut frame = JitExecFrame {
        scope,
        arg_base,
        var_base,
        sp_base,
        sp: sp_base,
        cpools: bc.consts().as_ptr(),
        cur_sp_offset,
        helpers: HELPERS.0.as_ptr().cast::<u8>(),
        this,
        gc_scope_kind: bc.gc_scope_kind() as u32,
        imm: 0,
        imm2: 0,
        pc,
        bytecode: &**bc,
        #[cfg(feature = "inline_cache")]
        ic_sites: bc.inline_cache_sites(),
        out_value: JSValue::UNDEFINED,
        out_pc: 0,
        out_sp: 0,
        arg_real_count,
        arena,
        cur_frame_idx,
        new_target,
        home_obj,
        guard_misses: 0,
        property_guard_hits: 0,
        property_guard_misses: 0,
        global_guard_hits: 0,
        global_guard_misses: 0,
        backedges: 0,
        mono_call_hits: 0,
        inline_body_hits: 0,
        nested_inline_hits: 0,
        mono_call_misses: 0,
        scratch: JSValue::UNDEFINED,
        global_obj,
        global_var_obj,
        global_var_cache: bc.global_var_cache_sites(),
        scope_local_peak: 0,
        lit_atoms: [0; LIT_ATOM_MAX],
    };
    // SAFETY: `entry` was produced by `compile` for this exact bytecode layout
    // and ABI, and the slot is only populated while the code cache keeps the
    // mapping alive; the frame outlives the call.
    //
    // `native_depth` brackets the call: while it is non-zero no automatic poll
    // may retire a mapping (the code could still be executing). A nested
    // activation (a native callee through the call helpers) keeps it > 0 for the
    // whole outer run. A panic would leave it high, which only disables the
    // automatic poll — the safe direction.
    state
        .native_depth
        .set(state.native_depth.get().saturating_add(1));
    let raw = unsafe { entry(&mut frame) };
    state
        .native_depth
        .set(state.native_depth.get().saturating_sub(1));
    let kind = JitExitKind::from_raw(raw);
    let mut m = state.metrics.get();
    m.note_native_exit(kind, &frame);
    state.metrics.set(m);
    // Tier-down: feed the same count to the function's record so a function whose
    // guards miss too often is recompiled without speculation.
    state.note_guard_misses(bc, frame.guard_misses);
    // The native tier's own work: the interpreter stopped counting this
    // function's entries and back edges when the code was installed, so the
    // second gate is fed from here. Only a function
    // that may still be promoted reports at all, so the check is a load next
    // to the entry pointer.
    if bc.jit.promotable() {
        state.note_native_work(bc, pc == 0, frame.backedges);
    }

    Some((kind, frame))
}
