// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Tier-2 type feedback (epic F1).
//!
//! While a function is interpreted, the interpreter records the observed type
//! classes of operand values at arithmetic, comparison, conversion and branch
//! sites. The table hangs off `BytecodeImpl` (no per-op hash lookup: the site
//! key *is* the bytecode pc) and is copied into `CompileSnapshot` at compile
//! time, where tier-2 specialization (epic F2) turns it into speculative
//! unboxed code with guards.
//!
//! Recording is observation-only — it can never change a result. Per plan D-5
//! it runs only when the `jit` feature is on and the tier policy can still
//! reach tier 2 (`Tier2`/`Automatic`); `Interpreter` and `Tier1` record nothing.
//! Copying the table into a snapshot applies the epic F5 profitability gate:
//! under `Automatic` only sites with enough samples reach the compiler, while
//! forced `Tier2` keeps every recorded site.
//!
//! Shape/array feedback stays absent on purpose. Epic F3 made the inline
//! cache itself polymorphic (up to `IC_WAYS` shapes per site, in the shared
//! interpreter/JIT helper), and D-21 keeps guards out of generated code — so
//! the compiler has no consumer for observed shapes, and the runtime cache
//! already is the shape feedback.

use std::{cell::UnsafeCell, collections::HashMap};

use crate::{JSValue, JSValueTag, jit::fast_hash::FastHasherBuilder};

// ── Observed type classes (bit mask) ──

pub(crate) const CLASS_INT: u16 = 1 << 0;
pub(crate) const CLASS_FLOAT: u16 = 1 << 1;
pub(crate) const CLASS_BOOL: u16 = 1 << 2;
pub(crate) const CLASS_NULL: u16 = 1 << 3;
pub(crate) const CLASS_UNDEF: u16 = 1 << 4;
pub(crate) const CLASS_STRING: u16 = 1 << 5;
pub(crate) const CLASS_OBJECT: u16 = 1 << 6;
pub(crate) const CLASS_BIGINT: u16 = 1 << 7;
pub(crate) const CLASS_SYMBOL: u16 = 1 << 8;
/// Everything else: exception sentinels, catch offsets, modules, raw bytecode
/// handles — values that must never drive a speculative numeric path.
pub(crate) const CLASS_OTHER: u16 = 1 << 9;

/// Which type classes a value belongs to. One class per value, so a site's
/// operand mask stays small and merging is a bitwise OR.
pub(crate) fn type_class(v: JSValue) -> u16 {
    match v.tag() {
        JSValueTag::Int => CLASS_INT,
        JSValueTag::Float64 => CLASS_FLOAT,
        JSValueTag::Bool => CLASS_BOOL,
        JSValueTag::Null => CLASS_NULL,
        JSValueTag::Undefined => CLASS_UNDEF,
        JSValueTag::String | JSValueTag::StringRope => CLASS_STRING,
        JSValueTag::Object => CLASS_OBJECT,
        JSValueTag::BigInt => CLASS_BIGINT,
        JSValueTag::Symbol => CLASS_SYMBOL,
        _ => CLASS_OTHER,
    }
}

// ── Per-site observations ──

/// What was observed at one feedback site: a type-class mask per operand
/// position plus a saturating sample count. Branch and conversion sites record
/// a single operand (`lhs` only, `rhs == 0`).
///
/// `result` is the class of the *computed value* (epic P7 / task 7.1). It lets
/// the tier-2 gate tell "this `add_loc`/`inc_loc` site only ever produces
/// `Int`" from "it overflows to `Float64` every iteration" — the second keeps
/// speculating and pays its overflow bail on every trip (see
/// `jit_handover.md` §6.23). Sites with no result (branch conditions) keep it 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SiteFeedback {
    pub(crate) lhs: u16,
    pub(crate) rhs: u16,
    pub(crate) result: u16,
    pub(crate) samples: u16,
}

/// How many distinct sites one function may record. Past the cap the table
/// stops growing but keeps updating existing sites. Shortcut: a fixed per-
/// function bound instead of tying the cap to `max_metadata_bytes`; raise it
/// (or key the cap to bytecode size) if large bodies ever miss specialization.
const MAX_SITES: usize = 1024;

/// All feedback sites of one function, keyed by bytecode pc (the opcode
/// offset, matching `Instruction::pc()` the compiler indexes by).
///
/// The map hashes its keys with the JIT's fast mixer: the key is the pc, a
/// value JavaScript cannot steer, and every recorded site pays one hash — so
/// SipHash's cost buys nothing here.
#[derive(Debug, Default)]
pub(crate) struct FeedbackTable {
    sites: HashMap<u32, SiteFeedback, FastHasherBuilder>,
    /// Observations recorded so far, saturating. The hotness policy reads this
    /// as "measured work" (one add per recorded opcode, no scan of the sites);
    /// a function that already executed a lot is worth compiling sooner.
    total_samples: u32,
}

impl FeedbackTable {
    /// Merge one observation into `pc`; returns true when `pc` was a new site
    /// (used by the caller to count distinct sites into metrics).
    fn record(&mut self, pc: u32, lhs: u16, rhs: u16, result: u16) -> bool {
        // Cheap positive hits skip the map entirely; negative hits fall
        // through to the entry API below.
        self.total_samples = self.total_samples.saturating_add(1);
        if let Some(site) = self.sites.get_mut(&pc) {
            site.lhs |= lhs;
            site.rhs |= rhs;
            site.result |= result;
            site.samples = site.samples.saturating_add(1);
            return false;
        }
        if self.sites.len() >= MAX_SITES {
            return false;
        }
        self.sites.insert(
            pc,
            SiteFeedback {
                lhs,
                rhs,
                result,
                samples: 1,
            },
        );
        true
    }

    /// Sites sorted by pc, for `CompileSnapshot` (the compiler looks sites up
    /// by instruction pc). `min_samples` is the epic F5 profitability gate:
    /// sites the interpreter observed fewer than that many times are dropped,
    /// so a one-off observation can never buy a speculative fast path. Forced
    /// `Tier2` passes 1 and keeps everything.
    fn snapshot(&self, min_samples: u32) -> Vec<(u32, SiteFeedback)> {
        let mut sites: Vec<(u32, SiteFeedback)> = self
            .sites
            .iter()
            .filter(|(_, site)| site.samples as u32 >= min_samples)
            .map(|(&pc, site)| (pc, *site))
            .collect();
        sites.sort_unstable_by_key(|&(pc, _)| pc);
        sites
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.sites.len()
    }

    /// Observations recorded so far (see the field). Zero when nothing has been
    /// recorded, which is how the hotness policy tells "no measurement" (a
    /// baseline tier) from "measured, but little work".
    pub(crate) const fn total_samples(&self) -> u32 {
        self.total_samples
    }
}

/// Per-function feedback storage, hung off `BytecodeImpl`. Interior-mutable
/// because the interpreter records through `&Bytecode`, exactly like the
/// inline cache; single-threaded, so no synchronization is needed.
#[repr(transparent)]
pub(crate) struct FeedbackSlot(UnsafeCell<Option<Box<FeedbackTable>>>);

impl Default for FeedbackSlot {
    fn default() -> Self {
        Self(UnsafeCell::new(None))
    }
}

impl FeedbackSlot {
    /// Record one observation at `site` (bytecode pc). The table is allocated
    /// on the first record, so functions that never execute a feedback site
    /// pay only the `None` check. Returns true when `site` was new.
    pub(crate) fn record(&self, site: u32, lhs: u16, rhs: u16, result: u16) -> bool {
        // SAFETY: single-threaded interpreter; same discipline as
        // `BytecodeImpl::inline_cache` — each observation is a short,
        // non-reentrant update with no aliasing `&mut` held across a call.
        let cell = unsafe { &mut *self.0.get() };
        cell.get_or_insert_with(|| Box::new(FeedbackTable::default()))
            .record(site, lhs, rhs, result)
    }

    /// Observations recorded so far — the hotness policy's "measured work".
    /// Zero means "nothing recorded" (a baseline tier, or a function that never
    /// reached a feedback site), not "measured as nothing".
    pub(crate) fn total_samples(&self) -> u32 {
        // SAFETY: single-threaded; a plain read of the count.
        match unsafe { &*self.0.get() } {
            Some(table) => table.total_samples(),
            None => 0,
        }
    }

    /// Copy the recorded sites (sorted by pc) for `CompileSnapshot`, keeping
    /// only sites with at least `min_samples` observations (epic F5 gate).
    pub(crate) fn snapshot(&self, min_samples: u32) -> Vec<(u32, SiteFeedback)> {
        // SAFETY: single-threaded; called from `Jit::poll`, never mid-opcode.
        match unsafe { &*self.0.get() } {
            Some(table) => table.snapshot(min_samples),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_are_distinct_and_cover_values() {
        assert_eq!(type_class(JSValue::from(1)), CLASS_INT);
        assert_eq!(type_class(JSValue::from(1.5)), CLASS_FLOAT);
        assert_eq!(type_class(JSValue::from(true)), CLASS_BOOL);
        assert_eq!(type_class(JSValue::NULL), CLASS_NULL);
        assert_eq!(type_class(JSValue::UNDEFINED), CLASS_UNDEF);
        // Sentinels must land in OTHER: they can never justify speculation.
        assert_eq!(type_class(JSValue::EXCEPTION), CLASS_OTHER);
    }

    #[test]
    fn float_and_int_never_share_a_class() {
        assert_ne!(
            type_class(JSValue::from(0)),
            type_class(JSValue::from(-0.0))
        );
        let both = type_class(JSValue::from(1)) | type_class(JSValue::from(1.5));
        assert_eq!(both, CLASS_INT | CLASS_FLOAT);
    }

    #[test]
    fn record_reports_new_sites_then_merges() {
        let mut table = FeedbackTable::default();
        assert!(table.record(10, CLASS_INT, CLASS_INT, CLASS_INT));
        assert!(!table.record(10, CLASS_INT, CLASS_INT, CLASS_INT));
        // A widened observation merges instead of replacing: the site is now
        // known to be polymorphic, which is exactly what tier 2 must see.
        assert!(!table.record(10, CLASS_INT, CLASS_FLOAT, CLASS_INT));
        let snapshot = table.snapshot(1);
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].0, 10);
        assert_eq!(snapshot[0].1.lhs, CLASS_INT);
        assert_eq!(snapshot[0].1.rhs, CLASS_INT | CLASS_FLOAT);
        assert_eq!(snapshot[0].1.samples, 3);
    }

    /// Epic P7 (task 7.1): the result class merges like the operand masks, so a
    /// site that ever overflowed to `Float64` stays visibly non-int-only and the
    /// compiler's speculation gate can refuse it.
    #[test]
    fn result_class_merges_and_marks_overflow() {
        let mut table = FeedbackTable::default();
        table.record(10, CLASS_INT, CLASS_INT, CLASS_INT);
        table.record(10, CLASS_INT, CLASS_INT, CLASS_FLOAT);
        let site = table.snapshot(1)[0].1;
        assert_eq!(site.result, CLASS_INT | CLASS_FLOAT);
        assert_ne!(site.result, CLASS_INT, "an overflow site is not int-only");
    }

    #[test]
    fn site_count_is_capped_but_existing_sites_keep_updating() {
        let mut table = FeedbackTable::default();
        for pc in 0..MAX_SITES as u32 {
            assert!(table.record(pc, CLASS_INT, CLASS_INT, CLASS_INT));
        }
        assert!(!table.record(MAX_SITES as u32, CLASS_INT, CLASS_INT, CLASS_INT));
        assert_eq!(table.len(), MAX_SITES);
        // The cap must not freeze observations for sites already tracked; the
        // new class merges into the mask rather than replacing it.
        assert!(!table.record(7, CLASS_FLOAT, CLASS_FLOAT, CLASS_INT));
        assert_eq!(table.snapshot(1)[7].1.lhs, CLASS_INT | CLASS_FLOAT);
    }

    #[test]
    fn snapshot_is_sorted_by_pc() {
        let mut table = FeedbackTable::default();
        table.record(64, CLASS_INT, 0, CLASS_INT);
        table.record(3, CLASS_INT, 0, CLASS_INT);
        table.record(32, CLASS_INT, 0, CLASS_INT);
        let pcs: Vec<u32> = table.snapshot(1).iter().map(|&(pc, _)| pc).collect();
        assert_eq!(pcs, vec![3, 32, 64]);
    }

    /// Epic F5: the profitability gate drops thin evidence, while `min_samples`
    /// of 1 (what forced `Tier2` passes) keeps every site.
    #[test]
    fn snapshot_applies_the_profitability_gate() {
        let mut table = FeedbackTable::default();
        table.record(10, CLASS_INT, CLASS_INT, CLASS_INT); // samples == 1
        table.record(20, CLASS_INT, CLASS_INT, CLASS_INT);
        table.record(20, CLASS_INT, CLASS_INT, CLASS_INT);
        table.record(20, CLASS_INT, CLASS_INT, CLASS_INT); // samples == 3

        assert_eq!(table.snapshot(1).len(), 2, "Tier2 must keep every site");
        let gated = table.snapshot(3);
        assert_eq!(gated.len(), 1, "the thin site must be gated out");
        assert_eq!(gated[0].0, 20);
        assert!(
            table.snapshot(4).is_empty(),
            "nothing clears a gate above every sample count"
        );
    }

    #[test]
    fn empty_slot_snapshots_empty_without_allocating() {
        let slot = FeedbackSlot::default();
        assert!(slot.snapshot(1).is_empty());
        assert!(slot.record(4, CLASS_INT, CLASS_INT, CLASS_INT));
        assert_eq!(slot.snapshot(1).len(), 1);
    }

    #[test]
    fn sample_count_saturates_without_wrapping() {
        let mut table = FeedbackTable::default();
        table.record(0, CLASS_INT, CLASS_INT, CLASS_INT);
        for _ in 0..u16::MAX as usize + 4 {
            table.record(0, CLASS_INT, CLASS_INT, CLASS_INT);
        }
        assert_eq!(table.snapshot(1)[0].1.samples, u16::MAX);
    }
}
