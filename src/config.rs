// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Engine tuning parameters.
//!
//! Hard-coded knobs that shape runtime behaviour: heaps, stacks, buffers and
//! algorithm thresholds. All are compile-time constants; values are reasonable
//! defaults and may be adjusted without changing semantics.
//!
//! Grouped by subsystem. Flag / type / magic constants (bit masks, enum
//! discriminants, header layouts, spec-mandated limits) belong to their owners
//! and are intentionally NOT centralized here.

// ── GC / heap ─────────────────────────────────────────────────────────

/// Per-partition GC arena size in bytes (bump + free-list allocator).
/// Each GC partition owns one arena; small nodes are bump-allocated here
/// instead of calling the system allocator for every object.
pub(crate) const GC_ARENA_CAP: usize = 64 * 1024;

/// Maximum allocation size in bytes that still uses the GC arena.
/// Larger nodes (and all root nodes) go directly to the system allocator.
pub(crate) const GC_ARENA_MAX_ALLOC: usize = 16 * 1024;

/// Number of GC-node allocations between `should_gc()` checks.
/// The heap-usage → `gc_req` check is batched to keep the hot allocation
/// path cheap; every allocation entry point routes through it.
pub(crate) const GC_ALLOC_CHECK_INTERVAL: u32 = 32;

// ── VM / execution stack ───────────────────────────────────────────────

/// Head-room in bytes kept above the current thread's real stack bottom when
/// the guard runs in **auto mode** (`set_thread_stack_limit(0)`): recursion may
/// use the whole thread stack except this reserve, which stays free for the
/// throw path so it never touches the OS guard page.
#[cfg(debug_assertions)]
pub(crate) const THREAD_STACK_LIMIT_MARGIN: usize = 512 * 1024;
#[cfg(not(debug_assertions))]
pub(crate) const THREAD_STACK_LIMIT_MARGIN: usize = 256 * 1024;

/// VM stack-frame bump-arena capacity in bytes. Frame headers and data are
/// bump-allocated from this pool; overflow spills to heap buffers.
pub(crate) const VM_STACK_ARENA_CAP: usize = 256 * 1024;

/// Default ceiling on total live virtual stack bytes (bump `data_used` plus
/// live heap frames). Crossing it raises a stack-depth error instead of
/// exhausting host memory.
pub(crate) const VM_STACK_LIMIT_DEFAULT: usize = 512 * 1024;

/// Number of spilled-frame heap buffers kept cached per arena.
/// Caches recently released buffers so bursts of deep/async calls do not
/// reallocate them from scratch.
pub(crate) const HEAP_BUF_CACHE_MAX: usize = 8;

/// Interrupt-callback poll interval in executed opcodes. Large enough to keep
/// the per-opcode cost negligible, small enough to call the callback often.
pub(crate) const JS_INTERRUPT_COUNTER_INIT: i32 = 10000;

/// Maximum bytecode stack depth of a single function. A hard upper bound
/// imposed by the `u16` stack-size encoding — not a tuning knob.
pub(crate) const VM_STACK_LEN_MAX: u16 = 65534;

// ── objects / shapes ───────────────────────────────────────────────────

/// Initial capacity of an object's property container.
pub(crate) const JS_PROP_INITIAL_SIZE: u32 = 2;

/// Initial property-shape table size as `log2(bucket count)`: 2^6 = 64 buckets.
pub(crate) const SHAPE_BUCKET_BITS: u8 = 6;

// ── bigint ─────────────────────────────────────────────────────────────

/// Minimum limb count before Karatsuba multiplication is used over the
/// schoolbook algorithm. Applies when both operands have at least this many
/// limbs and their lengths are within a factor of 4.
pub(crate) const KARATSUBA_THRESHOLD: usize = 32;
