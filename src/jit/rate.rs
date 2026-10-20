// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The process-wide compile rate: how many nanoseconds one bytecode of
//! compiled code costs.
//!
//! This is the input of the **cost gate** (plan §13.6.5 / IMP-2): a compile
//! only pays for itself if the function runs often enough. The rate is a
//! property of the engine and the machine, not of one runtime, so it is
//! measured **process-wide** — the test262 runner builds a fresh runtime per
//! test, and a per-runtime rate would never accumulate enough samples.
//!
//! The estimate is deliberately thin until it has a few samples: one compile
//! can be an outlier (the first Cranelift call pays the setup), and a rate
//! built on a single sample would raise the gate on a guess.

use std::sync::atomic::{AtomicU64, Ordering};

/// Nanoseconds spent compiling, process-wide.
static NANOS: AtomicU64 = AtomicU64::new(0);
/// Bytecodes compiled, process-wide (the *source* size, not the machine code).
static BYTES: AtomicU64 = AtomicU64::new(0);
/// How many compiles the estimate is based on.
static SAMPLES: AtomicU64 = AtomicU64::new(0);

/// How many compiles the estimate needs before it is used. Below this the
/// rate is a guess, and a wrong guess here costs compiles.
const MIN_SAMPLES: u64 = 8;

/// Record one finished compile: `nanos` for `bytes` source bytecodes.
pub(crate) fn record(nanos: u64, bytes: u64) {
    if bytes == 0 {
        return;
    }
    NANOS.fetch_add(nanos, Ordering::Relaxed);
    BYTES.fetch_add(bytes, Ordering::Relaxed);
    SAMPLES.fetch_add(1, Ordering::Relaxed);
}

/// The measured nanoseconds per compiled bytecode, or `None` while the
/// estimate is too thin to trust.
pub(crate) fn ns_per_bytecode() -> Option<u64> {
    if SAMPLES.load(Ordering::Relaxed) < MIN_SAMPLES {
        return None;
    }
    let bytes = BYTES.load(Ordering::Relaxed);
    if bytes == 0 {
        return None;
    }
    Some(NANOS.load(Ordering::Relaxed) / bytes)
}

/// How many times a function must be observed before its compile can pay for
/// itself.
///
/// The compile of a function with `b` bytecodes costs about
/// `rate × b` nanoseconds; running it natively instead of interpreted saves
/// about `saving × b` nanoseconds per activation. The size cancels, so the
/// break-even is a single number: `rate / saving` activations.
///
/// `saving` is the host's estimate of how much faster native code is per
/// bytecode (plan §13.6.4: the interpreter runs a bytecode in ~20–50 ns, the
/// machine code saves ~10–30 ns of that).
pub(crate) const fn payback_activations(
    rate_ns_per_bytecode: u64,
    saving_ns_per_bytecode: u64,
) -> u32 {
    if saving_ns_per_bytecode == 0 {
        return 0;
    }
    let activations = rate_ns_per_bytecode / saving_ns_per_bytecode;
    // A u32 threshold: the gate must not overflow the counters.
    if activations > u32::MAX as u64 {
        u32::MAX
    } else {
        activations as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_payback_is_the_rate_over_the_saving() {
        // 5.7 us per bytecode compiled, 20 ns saved per bytecode: ~285
        // activations before the compile pays for itself.
        assert_eq!(payback_activations(5_700, 20), 285);
        // A zero saving estimate turns the gate off.
        assert_eq!(payback_activations(5_700, 0), 0);
        // A huge rate saturates instead of wrapping.
        assert_eq!(payback_activations(u64::MAX, 1), u32::MAX);
    }
}
