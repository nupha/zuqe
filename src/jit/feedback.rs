// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Tier-2 type feedback and call-site feedback, recorded while a function is
//! still interpreted.
//!
//! Two tables hang off `BytecodeImpl`, keyed by the bytecode pc (no per-op hash
//! lookup: the key *is* the pc), and are copied into `CompileSnapshot` at
//! compile time, where tier-2 specialization turns the observed type classes
//! into speculative unboxed code with guards. Copying applies the profitability
//! gate: under `Automatic` only sites with enough samples reach the compiler,
//! forced `Tier2` keeps every recorded site.
//!
//! Recording is observation-only — it can never change a result — and runs only
//! when the `jit` feature is on and the tier policy can still reach tier 2
//! (`Tier2`/`Automatic`); `Interpreter` and `Tier1` record nothing.
//!
//! Shape/array feedback stays absent on purpose: the property inline cache made
//! the inline cache itself polymorphic, and the helper-guard split keeps guards
//! out of generated code, so the compiler has no consumer for observed shapes —
//! the runtime cache already is the shape feedback.
//!
//! # Call-site feedback (the direct-call path)
//!
//! A second table records, per call site, the callee the interpreter saw there.
//! A site that only ever calls one bytecode function is *monomorphic*: the
//! compiler can then emit a guard ("the callee is exactly this one") and call
//! that function's compiled code directly instead of going through the general
//! call path. The callee is held by the table, so its identity stays unique — a
//! collected bytecode node's address could be reused by another node, and a
//! stale guard would then match the wrong function.

use std::{cell::UnsafeCell, collections::HashMap};

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{JSValue, JSValueTag, function::Bytecode, jit::fast_hash::FastHasherBuilder};

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
pub(super) const fn type_class(v: JSValue) -> u16 {
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
/// `result` is the class of the *computed value* (result classes). It lets
/// the tier-2 gate tell "this `add_loc`/`inc_loc` site only ever produces
/// `Int`" from "it overflows to `Float64` every iteration" — the second keeps
/// speculating and pays its overflow bail on every trip. Sites with no result
/// (branch conditions) keep it 0.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct SiteFeedback {
    pub(super) lhs: u16,
    pub(super) rhs: u16,
    pub(super) result: u16,
    pub(super) samples: u16,
}

/// How many distinct sites one function may record. Past the cap the table
/// stops growing but keeps updating existing sites.
const MAX_SITES: usize = 1024;

/// How many call sites one function may record; same bound as [`MAX_SITES`].
const MAX_CALL_SITES: usize = 256;

// ── Call sites (the direct-call path) ──

/// One call site: the callee the interpreter saw there.
#[derive(Debug)]
struct CallSiteFeedback {
    /// The opcode offset of the call site.
    pc: u32,
    /// The callee's identity (its bytecode node's address); 0 when the callee
    /// was not a bytecode function (a native function, a bound function, …) —
    /// such a site is never monomorphic.
    callee_id: u64,
    /// The callee seen at this site, kept alive so its identity stays unique:
    /// a collected bytecode node's address could be reused by another node,
    /// and a stale guard would then match the wrong function.
    callee: Option<Bytecode>,
    /// A second, different callee turned up here: the site is polymorphic and
    /// the monomorphic guard would miss from then on.
    polymorphic: bool,
}

/// A call site as the compiler sees it: the callee's identity and whether the
/// site turned out polymorphic.
///
/// The callee is a plain identity (its bytecode node's address), not a
/// reference: the snapshot crosses to the compiler thread and must stay
/// pointer-free. The table keeps the callee alive, so the address stays unique
/// for as long as the site exists.
#[derive(Clone, Copy, Debug)]
pub(super) struct CallSite {
    /// The opcode offset of the call site.
    pub(super) pc: u32,
    /// The callee's identity (the bytecode node's address); 0 when the callee
    /// was not a bytecode function.
    pub(super) callee: u64,
    /// Whether a second, different callee turned up at this site.
    pub(super) polymorphic: bool,
    /// Index into `CompileSnapshot::inline_callees` when the compiler may
    /// inline the callee's body at this site.
    pub(super) inline: Option<u32>,
}

/// The identity of a callee: the address of its bytecode node. Stable for as
/// long as the node is alive — the call-site table keeps it alive.
fn callee_id(bc: &Bytecode) -> u64 {
    bc.gc_node_ptr().as_ptr() as u64
}

/// All feedback sites of one function, keyed by bytecode pc (the opcode
/// offset, matching `Instruction::pc()` the compiler indexes by).
///
/// Keys are hashed with the JIT's fast mixer: the key is the pc, a value
/// JavaScript cannot steer, so SipHash's cost buys nothing here.
#[derive(Debug, Default)]
struct FeedbackTable {
    sites: HashMap<u32, SiteFeedback, FastHasherBuilder>,
    /// Observations recorded so far, saturating — the hotness policy's
    /// "measured work" (one add per recorded opcode, no scan of the sites).
    total_samples: u32,
    /// The call sites (the direct-call path), in the order they were first seen. The
    /// index in this vector is the *site id* the compiled code carries: the
    /// helper reads the expected callee back through it.
    calls: Vec<CallSiteFeedback>,
    /// Opcode offset -> the site id, for the recording path.
    call_ids: HashMap<u32, u32, FastHasherBuilder>,
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
    /// by instruction pc). `min_samples` is the profitability gate:
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

    /// Record the callee seen at the call site `pc` (the direct-call path).
    ///
    /// Returns the callee that this call actually installed (the `None`→`Some`
    /// transition of a site) — and only then does the caller owe a GC barrier.
    /// A site that saw two different callees is polymorphic from then on — the
    /// monomorphic guard would miss every time — so it stops being updated.
    fn record_call(&mut self, pc: u32, callee: Option<Bytecode>) -> Option<Bytecode> {
        let had_callee = self
            .call_ids
            .get(&pc)
            .is_some_and(|&id| self.calls[id as usize].callee.is_some());
        let id = callee.as_ref().map_or(0, callee_id);
        self.record_call_id(pc, id, callee);
        // Only a site that had no callee before can have stored one now. This
        // includes a site that first saw a native callee (`None`) and then a
        // bytecode one; `record_call_id` writes that transition.
        if had_callee {
            return None;
        }
        self.call_ids
            .get(&pc)
            .and_then(|&id| self.calls[id as usize].callee)
    }

    /// The recording path, split from [`Self::record_call`] so the identity
    /// can be exercised without a GC node: `callee` is the handle that keeps
    /// the identity unique, `id` the identity itself.
    fn record_call_id(&mut self, pc: u32, id: u64, callee: Option<Bytecode>) -> bool {
        let (site_id, is_new) = match self.call_ids.get(&pc) {
            Some(&site_id) => (site_id, false),
            None => {
                if self.calls.len() >= MAX_CALL_SITES {
                    return false;
                }
                let site_id = self.calls.len() as u32;
                self.calls.push(CallSiteFeedback {
                    pc,
                    callee_id: id,
                    callee,
                    polymorphic: false,
                });
                self.call_ids.insert(pc, site_id);
                (site_id, true)
            }
        };
        let site = &mut self.calls[site_id as usize];
        if site.polymorphic {
            return false;
        }
        if site.callee_id != id {
            site.polymorphic = true;
        }
        if site.callee.is_none() {
            site.callee = callee;
        }
        is_new
    }

    /// The call sites, in site-id order, for the compile snapshot. The order
    /// is the table's own: the site id is the index, and the compiled code
    /// carries it.
    ///
    /// No hotness gate here, unlike [`Self::snapshot`]: a thin site only costs
    /// one guard miss, and filtering would renumber the sites — the compiled
    /// code's id indexes this table at run time.
    fn snapshot_calls(&self) -> Vec<CallSite> {
        self.calls
            .iter()
            .map(|site| CallSite {
                pc: site.pc,
                callee: site.callee_id,
                polymorphic: site.polymorphic,
                inline: None,
            })
            .collect()
    }

    /// The callee recorded at the call site `id` — what the monomorphic guard
    /// compares the live callee against.
    fn call_callee(&self, id: u32) -> Option<Bytecode> {
        self.calls.get(id as usize).and_then(|site| site.callee)
    }

    /// Trace the recorded callees: they are GC nodes, and holding them is what
    /// keeps a recorded identity unique.
    fn trace(&self, gcx: &mut GcTraceCtx) {
        for site in &self.calls {
            if let Some(callee) = &site.callee {
                callee.trace(gcx);
            }
        }
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
    pub(super) fn record(&self, site: u32, lhs: u16, rhs: u16, result: u16) -> bool {
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
    pub(super) fn total_samples(&self) -> u32 {
        // SAFETY: single-threaded; a plain read of the count.
        match unsafe { &*self.0.get() } {
            Some(table) => table.total_samples(),
            None => 0,
        }
    }

    /// Copy the recorded sites (sorted by pc) for `CompileSnapshot`, keeping
    /// only sites with at least `min_samples` observations.
    pub(super) fn snapshot(&self, min_samples: u32) -> Vec<(u32, SiteFeedback)> {
        // SAFETY: single-threaded; called from `Jit::poll`, never mid-opcode.
        match unsafe { &*self.0.get() } {
            Some(table) => table.snapshot(min_samples),
            None => Vec::new(),
        }
    }

    /// Record the callee seen at the call site `site` (bytecode pc).
    ///
    /// Returns the callee this call installed, if any — the caller binds only
    /// that edge (see `jit::note_call`).
    pub(super) fn record_call(&self, site: u32, callee: Option<Bytecode>) -> Option<Bytecode> {
        // SAFETY: single-threaded; same discipline as `record` above.
        let cell = unsafe { &mut *self.0.get() };
        cell.get_or_insert_with(|| Box::new(FeedbackTable::default()))
            .record_call(site, callee)
    }

    /// The call sites, in site-id order, for `CompileSnapshot`.
    pub(super) fn snapshot_calls(&self) -> Vec<CallSite> {
        // SAFETY: single-threaded; called from `Jit::poll`, never mid-opcode.
        match unsafe { &*self.0.get() } {
            Some(table) => table.snapshot_calls(),
            None => Vec::new(),
        }
    }

    /// The callee recorded at the call site `id` — the monomorphic guard
    /// (the direct-call path) compares the live callee against it.
    pub(super) fn call_callee(&self, id: u32) -> Option<Bytecode> {
        // SAFETY: single-threaded; a plain read of the table.
        match unsafe { &*self.0.get() } {
            Some(table) => table.call_callee(id),
            None => None,
        }
    }

    /// Trace the recorded callees (they are GC nodes).
    pub(crate) fn trace(&self, gcx: &mut GcTraceCtx) {
        // SAFETY: single-threaded; a plain read of the table.
        if let Some(table) = unsafe { &*self.0.get() } {
            table.trace(gcx);
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

    /// Result classes: the result class merges like the operand masks, so a
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

    /// The profitability gate drops thin evidence, while `min_samples`
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

    /// The call site's callee edge must be bound once: `record_call` returns
    /// the handle only when it actually stored it. A site that first saw a
    /// native callee (`None`) and then a bytecode one is the transition that
    /// matters — a new site is not the only case.
    #[test]
    fn record_call_reports_only_the_none_to_some_transition() {
        use crate::{EvalFlags, EvalName, EvalType, Runtime, ScopedContext};

        let mut rt = Runtime::create().expect("runtime");
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .expect("context");
        let name = EvalName::Name("call-feedback.js".into());
        let v = ctx
            .eval(
                b"function probe_fn() {} probe_fn",
                &name,
                EvalType::Global,
                EvalFlags::empty(),
            )
            .expect("eval");
        let bc = crate::function::BCWrapper::try_from(v)
            .expect("bytecode function")
            .payload()
            .bytecode();

        let mut table = FeedbackTable::default();
        // Native callee first: nothing to bind.
        assert_eq!(table.record_call(7, None), None);
        // Same site, now a bytecode callee: this is the stored edge.
        assert_eq!(table.record_call(7, Some(bc)), Some(bc));
        // Already stored: no second bind.
        assert_eq!(table.record_call(7, Some(bc)), None);
    }

    /// The direct-call path: a site that keeps seeing the same callee stays
    /// monomorphic; a second, different callee makes it polymorphic.
    #[test]
    fn a_second_callee_makes_the_site_polymorphic() {
        let mut table = FeedbackTable::default();
        assert!(table.record_call_id(10, 0x1000, None));
        assert!(!table.record_call_id(10, 0x1000, None));
        let site = table.snapshot_calls()[0];
        assert_eq!(site.pc, 10);
        assert_eq!(site.callee, 0x1000);
        assert!(!site.polymorphic);

        // A different callee at the same site: the guard would miss from now
        // on, so the site is polymorphic.
        table.record_call_id(10, 0x2000, None);
        assert!(table.snapshot_calls()[0].polymorphic);
    }

    /// The site id is the index in the table's own order: the compiled code
    /// carries it, and the helper reads the expected callee back through it.
    #[test]
    fn the_site_id_is_the_index_in_the_table() {
        let mut table = FeedbackTable::default();
        table.record_call_id(30, 0x3000, None);
        table.record_call_id(10, 0x1000, None);
        table.record_call_id(20, 0x2000, None);
        let sites = table.snapshot_calls();
        assert_eq!(sites.len(), 3);
        // The order is the order the sites were first seen, not the pc order.
        assert_eq!(sites[0].pc, 30);
        assert_eq!(sites[1].pc, 10);
        assert_eq!(sites[2].pc, 20);
    }

    /// The call-site table is bounded: past the cap it stops growing.
    #[test]
    fn the_call_site_count_is_capped() {
        let mut table = FeedbackTable::default();
        for pc in 0..MAX_CALL_SITES as u32 {
            assert!(table.record_call_id(pc, u64::from(pc) + 1, None));
        }
        assert!(!table.record_call_id(MAX_CALL_SITES as u32, 1, None));
        assert_eq!(table.snapshot_calls().len(), MAX_CALL_SITES);
    }
}
