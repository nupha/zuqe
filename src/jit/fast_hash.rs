// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The fast hash the JIT's hot maps use.
//!
//! Two maps sit on a hot path: the coordinator's per-function records (keyed
//! by the bytecode node's address) and the tier-2 feedback table (keyed by the
//! bytecode pc). Neither key is something JavaScript can control, so
//! `RandomState`'s SipHash buys no protection here, only cost: every lookup
//! pays a SipHash13 (~20 instructions) instead of the ~6 an avalanche mixer
//! needs.
//!
//! The mixing is splitmix64's finalizer. It has to avalanche, not just
//! multiply: a map picks its bucket from the hash's low bits, and the keys are
//! aligned (node addresses) or dense (bytecode pcs), so a hash that keeps
//! their low bits files them all under one bucket and turns every lookup into
//! a walk over the same chain.

use std::hash::{BuildHasher, Hasher};

/// splitmix64's finalizer: a full avalanche over the key's bits.
#[inline]
fn mix(value: u64) -> u64 {
    let mut z = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The hasher itself (see the module docs).
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct FastHasher(u64);

impl Hasher for FastHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.0 = mix(value as u64);
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.0 = mix(value);
    }

    /// The JIT's maps only hold integer keys; the fold keeps the hasher
    /// correct for any other key type.
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
        for &byte in bytes {
            acc = (acc ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = mix(acc);
    }
}

/// The builder the maps take as their `BuildHasher`.
#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct FastHasherBuilder;

impl BuildHasher for FastHasherBuilder {
    type Hasher = FastHasher;

    #[inline]
    fn build_hasher(&self) -> FastHasher {
        FastHasher(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash has to spread the keys over the buckets. A map picks its
    /// bucket from the hash's low bits, and the bytecode pcs are dense (they
    /// count up the instruction stream), so a hash that keeps their low bits
    /// would file every site under one bucket.
    #[test]
    fn the_hash_spreads_dense_keys() {
        let hash_of = |pc: u32| {
            let mut h = FastHasher::default();
            h.write_u32(pc);
            h.finish()
        };
        // The low bits pick the bucket (128 of them is a small table).
        let buckets: std::collections::HashSet<u64> =
            (0..64u32).map(|pc| hash_of(pc) & 0x7f).collect();
        assert!(
            buckets.len() > 32,
            "the hash clusters the keys: {} buckets for 64 keys",
            buckets.len()
        );
    }

    /// The node addresses the GC hands out are aligned, so their low bits are
    /// zero: the same trap as the dense keys, one step further.
    #[test]
    fn the_hash_spreads_aligned_keys() {
        let hash_of = |ptr: u64| {
            let mut h = FastHasher::default();
            h.write_u64(ptr);
            h.finish()
        };
        let keys = [
            0x1000u64, 0x2000, 0x3000, 0x4000, 0x5000, 0x6000, 0x7000, 0x8000,
        ];
        let buckets: std::collections::HashSet<u64> =
            keys.iter().map(|k| hash_of(*k) & 0x7f).collect();
        assert!(
            buckets.len() > keys.len() / 2,
            "the hash clusters the keys: {buckets:?}"
        );
    }
}
