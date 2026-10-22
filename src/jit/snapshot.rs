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

// Several fields describe the frame for codegen (epic C) and are not read yet.
#![allow(dead_code)]

use crate::{
    function::{BytecodeImpl, GcScopeKind},
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
    pub(crate) function_id: u64,
    pub(crate) bytecode: Box<[u8]>,
    pub(crate) arg_count: u16,
    /// Locals, excluding arguments.
    pub(crate) var_count: u16,
    /// Global variable references (`get_var` / `put_var` indices).
    pub(crate) global_var_ref_count: u16,
    /// Captured variable references (`get_var_ref` / `put_var_ref` indices).
    pub(crate) closure_var_count: u16,
    pub(crate) local_buf_size: u16,
    pub(crate) stack_size: u16,
    pub(crate) mode: ExecMode,
    pub(crate) gc_scope_kind: GcScopeKind,
    pub(crate) const_meta: Box<[ConstMeta]>,
    pub(crate) ic_count: u32,
    /// Tier-2 type feedback (epic F1): `(pc, observations)` sorted by pc,
    /// copied out of the bytecode's feedback table at snapshot time, already
    /// filtered by the epic F5 profitability gate. Empty under
    /// `Interpreter`/`Tier1` policies, which never record.
    pub(crate) feedback: Box<[(u32, SiteFeedback)]>,
    /// Call-site feedback (D-6 route B): the callee each call site saw, in
    /// site-id order — the index in this box is the site id the compiled code
    /// carries, and the helper reads the expected callee back through it.
    pub(crate) call_sites: Box<[CallSite]>,
    /// Whether the compiler may speculatively specialize this function (epic
    /// F2). Cleared after tier-down (P7 7.2) observed too many guard misses at
    /// its sites: the recompile keeps the int fast paths but not the
    /// speculation, so the guards stop being paid.
    pub(crate) speculate: bool,
}

impl CompileSnapshot {
    /// Copy pointer-free details out of a live bytecode function. `function_id`
    /// is the identity the interpreter's hotness hooks use (the node address,
    /// stable for the runtime's lifetime because the GC never moves nodes).
    /// `min_samples` is the epic F5 profitability gate applied to the feedback
    /// copied in: sites observed fewer times never reach the compiler.
    pub(crate) fn from_bytecode(function_id: u64, bc: &BytecodeImpl, min_samples: u32) -> Self {
        #[cfg(feature = "inline_cache")]
        let ic_count = {
            let cache = unsafe { &*bc.inline_cache.get() };
            cache.len() as u32
        };
        #[cfg(not(feature = "inline_cache"))]
        let ic_count = 0;

        Self {
            function_id,
            bytecode: bc.opcodes().into(),
            arg_count: bc.arg_count,
            var_count: bc.var_count,
            global_var_ref_count: bc.global_var_ref_count,
            closure_var_count: bc.closure_var_count,
            local_buf_size: bc.local_buf_size,
            stack_size: bc.stack_size,
            mode: bc.mode(),
            gc_scope_kind: bc.gc_scope_kind(),
            const_meta: bc
                .consts()
                .iter()
                .map(|value| ConstMeta {
                    tag: value.tag() as i16,
                })
                .collect(),
            ic_count,
            feedback: bc.feedback.snapshot(min_samples).into_boxed_slice(),
            call_sites: bc.feedback.snapshot_calls().into_boxed_slice(),
            speculate: true,
        }
    }
}

/// The worker contract: a snapshot crosses to the compiler thread.
const _: fn() = || {
    fn assert_send<T: Send>() {}
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
            local_buf_size: var_count.saturating_add(stack_size),
            stack_size,
            mode: ExecMode::empty(),
            gc_scope_kind: GcScopeKind::Full,
            const_meta: Box::new([]),
            ic_count: 0,
            feedback: Box::new([]),
            call_sites: Box::new([]),
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
        assert_eq!(snap.local_buf_size, 10);
    }
}
