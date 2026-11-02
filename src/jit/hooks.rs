//! The interpreter hooks. Every one is a no-op unless a JIT is
//! attached, so the interpreter pays a load and a branch.

use super::*;

// ── Interpreter hooks (no-ops unless a JIT is attached) ──

/// Called on entry to a bytecode function.
#[inline]
pub(crate) fn note_enter(rt: &RuntimeImpl, bc: &Bytecode) {
    if let Some(state) = rt.jit.as_deref() {
        state.note_enter(bc);
    }
}

/// Called when a backward branch is taken (loop back edge). Returns whether the
/// interpreter should try the frame's native code at that loop header (back-edge OSR).
#[inline]
pub(crate) fn note_backedge(rt: &RuntimeImpl, bc: &Bytecode) -> bool {
    match rt.jit.as_deref() {
        Some(state) => state.note_backedge(bc),
        None => false,
    }
}

/// Poll from the interpreter's trampoline if a back edge asked for it (deferred polling).
#[cfg(feature = "jit")]
#[inline]
pub(crate) fn poll_deferred(rt: &RuntimeImpl) {
    if let Some(state) = rt.jit.as_deref() {
        state.poll_deferred();
    }
}

/// Automatic poll at an engine safe point (eval return, event-loop tick), so a
/// host never has to drive compilation itself. No-op unless a JIT is attached,
/// `JitConfig::auto_poll` is on, no native activation is in flight and the queue
/// is non-empty.
#[cfg(feature = "jit")]
#[inline]
pub(crate) fn poll_if_queued(rt: &RuntimeImpl) {
    if let Some(state) = rt.jit.as_deref() {
        state.poll_if_queued();
    }
}

/// Whether a JIT is attached and not in the fully inert `Interpreter` policy.
///
/// Call-site feedback is recorded under every policy except `Interpreter`, so
/// `note_call` must be gated on this rather than on [`records_feedback`].
#[inline]
pub(crate) fn is_active(rt: &RuntimeImpl) -> bool {
    rt.jit.as_deref().is_some_and(|j| !j.disabled())
}

/// Whether the current frame should record tier-2 feedback.
///
/// Evaluated once per interpreter frame so hot arms can skip the `note_*`
/// call entirely when no JIT is attached or the policy cannot reach tier 2.
#[inline]
pub(crate) fn records_feedback(rt: &RuntimeImpl) -> bool {
    rt.jit.as_deref().is_some_and(|j| j.records_feedback())
}

/// Tier-2 feedback hook: record the operand types and the result type of a
/// binary site (type feedback / result classes). A no-op unless a JIT is attached under a
/// tier-2-capable policy.
#[inline]
pub(crate) fn note_binary(
    rt: &RuntimeImpl,
    bc: &Bytecode,
    site: u32,
    lhs: JSValue,
    rhs: JSValue,
    result: JSValue,
) {
    if let Some(state) = rt.jit.as_deref() {
        state.note_binary(bc, site, lhs, rhs, result);
    }
}

/// Tier-2 feedback hook: record the type of a single-value site — a branch
/// condition or a local update (type feedback).
#[inline]
pub(crate) fn note_value(rt: &RuntimeImpl, bc: &Bytecode, site: u32, value: JSValue) {
    if let Some(state) = rt.jit.as_deref() {
        state.note_value(bc, site, value);
    }
}

/// Tier-2 feedback hook: record an `inc_loc`/`dec_loc` site — the value before
/// the update and the value after it.
#[inline]
pub(crate) fn note_local_update(
    rt: &RuntimeImpl,
    bc: &Bytecode,
    site: u32,
    before: JSValue,
    after: JSValue,
) {
    if let Some(state) = rt.jit.as_deref() {
        state.note_local_update(bc, site, before, after);
    }
}

/// Call-site feedback hook: record the callee seen at a call site (the direct-call path).
/// A no-op unless a JIT is attached and the function is still interpreted.
#[inline]
pub(crate) fn note_call(rt: &mut RuntimeImpl, bc: &Bytecode, site: u32, callee: JSValue) {
    // `FeedbackTable.calls[].callee` is a strong edge from the caller bytecode
    // node to the callee bytecode node (BytecodeImpl::trace relies on it). The
    // caller may be old and the callee young, so this edge needs the
    // generational barrier.
    #[cfg(feature = "gc_generations")]
    {
        let callee_bc = crate::function::BCWrapper::try_from(callee)
            .ok()
            .map(|bfun| bfun.payload().bytecode());
        if let Some(cb) = callee_bc {
            rt.bind_node(bc.gc_node_ptr(), cb.gc_node_ptr());
        }
    }

    if let Some(state) = rt.jit.as_deref() {
        state.note_call(bc, site, callee);
    }
}
