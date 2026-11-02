// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! A `Send`, pointer-free copy of one bytecode function.
//!
//! The frontend never touches the runtime again: it consumes only this copy, so
//! it can run off the interpreter thread without holding any GC pointer. The
//! constant pool is reduced to per-index tags (a `push_const` can only be
//! specialized if the snapshot knows the constant is a number); the values
//! themselves stay in the runtime and are read through `ResolveConst` at
//! execution time.

use crate::{
    function::{Bytecode, BytecodeImpl, FunctionKind},
    jit::feedback::{CallSite, SiteFeedback},
    vm_stack::ExecMode,
};

/// One constant pool entry, reduced to the tag the verifier consults. Values
/// that can never be a number (`tag` neither `Int` nor `Float64`) are treated
/// as opaque by the abstract interpreter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConstMeta {
    pub(crate) tag: i16,
}

/// The opcode stream plus every scalar the frontend needs. No `GcRef`, no raw
/// pointer, no atom: safe to move to a worker.
#[derive(Clone, Debug)]
pub(crate) struct CompileSnapshot {
    /// The identity the interpreter's hotness hooks use; the `jit_dump`
    /// artifact is named after it.
    #[cfg_attr(not(feature = "jit_dump"), allow(dead_code))]
    pub(crate) function_id: u64,
    pub(crate) bytecode: Box<[u8]>,
    pub(crate) arg_count: u16,
    /// Locals, excluding arguments.
    pub(crate) var_count: u16,
    /// Global variable references (`get_var` / `put_var` indices).
    pub(crate) global_var_ref_count: u16,
    /// Captured variable references (`get_var_ref` / `put_var_ref` indices).
    pub(crate) closure_var_count: u16,
    pub(crate) stack_size: u16,
    pub(crate) mode: ExecMode,
    pub(crate) const_meta: Box<[ConstMeta]>,
    pub(crate) ic_count: u32,
    /// The number of global-variable site caches the live bytecode has.
    /// Zero for a function with no global references and for a persistent
    /// restore (the cache is not serialized); the compiler then keeps the
    /// helper for every global access.
    pub(crate) global_cache_count: u32,
    /// Tier-2 type feedback: `(pc, observations)` sorted by pc,
    /// copied out of the bytecode's feedback table at snapshot time, already
    /// filtered by the profitability gate. Empty under
    /// `Interpreter`/`Tier1` policies, which never record.
    pub(crate) feedback: Box<[(u32, SiteFeedback)]>,
    /// Call-site feedback (the direct-call path): the callee each call site saw, in
    /// site-id order — the index in this box is the site id the compiled code
    /// carries, and the helper reads the expected callee back through it.
    pub(crate) call_sites: Box<[CallSite]>,
    /// The callees the compiler may copy into this function (the direct-call
    /// path). A site's `CallSite::inline` indexes this box; no pointer is
    /// kept, so the snapshot still crosses to the compiler thread.
    pub(crate) inline_callees: Box<[InlineCallee]>,
    /// Whether the compiler may speculatively specialize this function. Cleared
    /// after a tier-down observed too many guard misses at
    /// its sites: the recompile keeps the int fast paths but not the
    /// speculation, so the guards stop being paid.
    pub(crate) speculate: bool,
}

/// A callee the compiler may inline into its caller.
///
/// This is the callee's own snapshot metadata, copied while the runtime
/// thread still owns the bytecode. The compiler verifies it like any other
/// snapshot and, if the body is within the inline subset, emits that body in
/// the caller's code.
///
/// `call_sites` / `inline_callees` are the callee's *own* monomorphic sites
/// a call inside the copied body can be copied too. The nesting is
/// bounded at snapshot time.
#[derive(Clone, Debug)]
pub(crate) struct InlineCallee {
    pub(crate) bytecode: Box<[u8]>,
    pub(crate) arg_count: u16,
    pub(crate) var_count: u16,
    pub(crate) stack_size: u16,
    pub(crate) mode: ExecMode,
    pub(crate) const_meta: Box<[ConstMeta]>,
    /// The callee's own call sites, in site-id order (as `CompileSnapshot`).
    pub(crate) call_sites: Box<[CallSite]>,
    /// The callees those sites may copy.
    pub(crate) inline_callees: Box<[InlineCallee]>,
}

/// How many copied bodies may be stacked at one call site. Deeper
/// call chains stay on the direct path.
const MAX_INLINE_DEPTH: u8 = 2;
/// Total bytecode bytes one snapshot may spend on nested copies. The
/// per-callee caps bound one body; this bounds the whole tree.
const MAX_INLINE_BYTES: usize = 64 * 1024;

/// The cheapest sanity gate for copying a callee's body: the body must be a
/// plain, small, non-closure function. The compiler decides usability from
/// the decoded opcodes; this only keeps the snapshot small.
fn inline_candidate(caller: &BytecodeImpl, callee: &Bytecode) -> bool {
    let bc = &**callee;
    bc.func_kind() == FunctionKind::Normal
        && !bc.mode().contains(ExecMode::ASYNC)
        && bc.realm() == caller.realm()
        && bc.closure_var_count == 0
        && bc.global_var_ref_count == 0
        && bc.arg_count <= 16
        && bc.var_count <= 16
        && bc.stack_size <= 32
        && bc.opcodes().len() <= 1024
        && bc.consts().len() <= 64
}

/// One snapshot's callee copies: the call-site table with every monomorphic,
/// in-budget site marked `inline`, plus the bodies those marks index. The
/// enumerate-then-filter order is the site-id contract — the id the compiled
/// code bakes into `imm2` is the index in this table.
fn collect_callees(
    bc: &BytecodeImpl,
    child_depth: u8,
    copy: bool,
    budget: &mut usize,
) -> (Vec<CallSite>, Vec<InlineCallee>) {
    let mut call_sites = bc.feedback.snapshot_calls();
    let mut inline_callees = Vec::new();
    if !copy {
        return (call_sites, inline_callees);
    }
    for (id, site) in call_sites.iter_mut().enumerate() {
        if site.polymorphic || site.callee == 0 {
            continue;
        }
        let Some(callee) = bc.feedback.call_callee(id as u32) else {
            continue;
        };
        if !inline_candidate(bc, &callee) {
            continue;
        }
        let len = callee.opcodes().len();
        if len > *budget {
            break;
        }
        *budget -= len;
        site.inline = Some(inline_callees.len() as u32);
        inline_callees.push(InlineCallee::from_bytecode(&callee, child_depth, budget));
    }
    (call_sites, inline_callees)
}

impl InlineCallee {
    fn from_bytecode(bc: &BytecodeImpl, depth: u8, budget: &mut usize) -> Self {
        let (call_sites, inline_callees) =
            collect_callees(bc, depth + 1, depth < MAX_INLINE_DEPTH, budget);
        Self {
            bytecode: bc.opcodes().into(),
            arg_count: bc.arg_count,
            var_count: bc.var_count,
            stack_size: bc.stack_size,
            mode: bc.mode(),
            const_meta: bc
                .consts()
                .iter()
                .map(|value| ConstMeta {
                    tag: value.tag() as i16,
                })
                .collect(),
            call_sites: call_sites.into_boxed_slice(),
            inline_callees: inline_callees.into_boxed_slice(),
        }
    }

    /// The callee as a standalone snapshot, for the compiler's verifier. The
    /// nested call-site tables travel with it, so a copied body can copy its
    /// own monomorphic callees.
    pub(crate) fn snapshot(&self, function_id: u64) -> CompileSnapshot {
        CompileSnapshot {
            function_id,
            bytecode: self.bytecode.clone(),
            arg_count: self.arg_count,
            var_count: self.var_count,
            global_var_ref_count: 0,
            closure_var_count: 0,
            stack_size: self.stack_size,
            mode: self.mode,
            const_meta: self.const_meta.clone(),
            ic_count: 0,
            global_cache_count: 0,
            feedback: Box::new([]),
            call_sites: self.call_sites.clone(),
            inline_callees: self.inline_callees.clone(),
            speculate: false,
        }
    }
}

impl CompileSnapshot {
    /// Copy pointer-free details out of a live bytecode function. `function_id`
    /// is the identity the interpreter's hotness hooks use (the node address,
    /// stable for the runtime's lifetime because the GC never moves nodes).
    /// `min_samples` is the profitability gate applied to the feedback
    /// copied in: sites observed fewer times never reach the compiler.
    pub(crate) fn from_bytecode(function_id: u64, bc: &BytecodeImpl, min_samples: u32) -> Self {
        #[cfg(feature = "inline_cache")]
        let ic_count = {
            let cache = unsafe { &*bc.inline_cache.get() };
            cache.len() as u32
        };
        #[cfg(not(feature = "inline_cache"))]
        let ic_count = 0;

        #[cfg(feature = "jit")]
        let global_cache_count = unsafe { (&*bc.global_var_cache.get()).len() as u32 };
        #[cfg(not(feature = "jit"))]
        let global_cache_count = 0;

        // The direct-call path: keep a copy of each monomorphic callee whose
        // shape the compiler may inline. The call-site table holds the callee
        // alive, so the identity the guard carries stays valid.
        let mut budget = MAX_INLINE_BYTES;
        let (call_sites, inline_callees) = collect_callees(bc, 0, true, &mut budget);

        Self {
            function_id,
            bytecode: bc.opcodes().into(),
            arg_count: bc.arg_count,
            var_count: bc.var_count,
            global_var_ref_count: bc.global_var_ref_count,
            closure_var_count: bc.closure_var_count,
            stack_size: bc.stack_size,
            mode: bc.mode(),
            const_meta: bc
                .consts()
                .iter()
                .map(|value| ConstMeta {
                    tag: value.tag() as i16,
                })
                .collect(),
            ic_count,
            global_cache_count,
            feedback: bc.feedback.snapshot(min_samples).into_boxed_slice(),
            call_sites: call_sites.into_boxed_slice(),
            inline_callees: inline_callees.into_boxed_slice(),
            speculate: true,
        }
    }
}

/// The worker contract: a snapshot crosses to the compiler thread.
const _: fn() = || {
    const fn assert_send<T: Send>() {}
    assert_send::<CompileSnapshot>();
};

#[cfg(test)]
impl CompileSnapshot {
    /// Snapshot for tests that build bytecode by hand, with no runtime behind
    /// it. Constants are empty; index checks use the supplied counts.
    pub(crate) fn from_raw(
        bytecode: Vec<u8>,
        arg_count: u16,
        var_count: u16,
        closure_var_count: u16,
        stack_size: u16,
    ) -> Self {
        Self {
            function_id: 0,
            bytecode: bytecode.into(),
            arg_count,
            var_count,
            global_var_ref_count: closure_var_count,
            closure_var_count,
            stack_size,
            mode: ExecMode::empty(),
            const_meta: Box::new([]),
            ic_count: 0,
            global_cache_count: 0,
            feedback: Box::new([]),
            call_sites: Box::new([]),
            inline_callees: Box::new([]),
            speculate: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_snapshot_is_pointer_free() {
        let snap = CompileSnapshot::from_raw(vec![0u8; 4], 1, 2, 0, 8);
        assert_eq!(snap.bytecode.len(), 4);
        assert_eq!(snap.arg_count, 1);
        assert_eq!(snap.var_count, 2);
    }
}
