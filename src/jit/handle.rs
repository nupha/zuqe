//! The host-facing handle: attach a JIT to a runtime, poll it, read
//! its metrics.

use super::*;

/// Host handle to a runtime's JIT. Created with [`Jit::attach_to`]; holds a strong
/// reference to the runtime so it stays valid as long as the handle does.
#[repr(transparent)]
pub struct Jit {
    rt: Runtime,
}

impl Jit {
    /// Attach a JIT to `rt` with `cfg`. Fails if a JIT is already attached, or
    /// if the host has no Cranelift backend.
    pub fn attach_to(rt: &Runtime, cfg: JitConfig) -> Result<Jit, JitError> {
        let state = JitState::new(cfg)?;
        rt.jit_install(Box::new(state))?;
        Ok(Jit { rt: rt.clone() })
    }

    /// Drive the coordinator: dispatch attempts to workers, drain completions
    /// and install the finished code. Non-blocking: on return an attempt may
    /// still be in flight (`JitMetrics::functions_compiling > 0`), so call it
    /// again until the queue and the in-flight count are both zero.
    ///
    /// The runtime polls itself at engine safe points (eval return, event-loop
    /// tick), so a host normally does not need to call this. It stays public for
    /// hosts and tests that want to compile at an exact moment; pair it with
    /// [`JitConfigBuilder::auto_poll`]`(false)` when the timing must be fully
    /// explicit. Must not be called while generated code is running (a nested
    /// eval from a native callback); the automatic poll checks for that.
    pub fn poll(&self) {
        if let Some(state) = self.rt.jit() {
            state.poll();
        }
    }

    /// A snapshot of the JIT's counters.
    pub fn metrics(&self) -> JitMetrics {
        self.rt
            .jit()
            .map_or_else(JitMetrics::default, JitState::metrics)
    }

    /// The measured cost of a compile: nanoseconds per compiled bytecode,
    /// process-wide. `None` while too few compiles have been measured to
    /// trust the estimate.
    ///
    /// It is the *cost* side of the cost gate: with the host's
    /// estimate of what native execution saves
    /// ([`JitConfig::native_saving_ns_per_bytecode`]), it says how many
    /// times a function must run before its compile pays for itself.
    pub fn compile_rate_ns_per_bytecode(&self) -> Option<u64> {
        rate::ns_per_bytecode()
    }

    /// Name of the Cranelift target ISA selected for this host.
    pub fn target_isa(&self) -> Option<String> {
        self.rt.jit().map(|s| s.target_isa().name().to_owned())
    }
}
