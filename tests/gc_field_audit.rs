// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! zuqe's own run of the `gc_audit` library (L2b of `memo/gc_bind_guard.md`).
//!
//! The scanner lives in `src/gc/audit.rs` so an embedding crate can audit its
//! own `def_native_object!` payloads with the same code — see the module docs.
//! This file only carries zuqe's allowlist and its sanity checks.

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Permanent exemptions — these are **not** violations, and an entry here is
/// still a visible diff a reviewer must accept. Three kinds:
///
/// 1. *root containers* — a minor force-descends them, a barrier is pointless;
/// 2. *construction-only* — the P0 write-point scan found no late write, so the
///    instance is born young and never receives a value afterwards;
/// 3. *structural / design non-goals* — raw-pointer mirrors, `Cell` slots (the
///    guard is deliberately not `Copy`, so `Cell::get` cannot work through it),
///    untraced weak targets, and composite shapes (`Box<[T]>`,
///    `Option<Box<[T]>>`) that `memo/gc_bind_guard.md` §7 leaves to private
///    fields + setters.
///
/// Anything **not** listed here and not wrapped in `WriteBarrier`/
/// `WriteBarrierVec` (or, for container element types, covered by an
/// `impl GcBind`) fails the test with `file:line`.
const EXEMPT: &[&str] = &[
    "ContextImpl.iterator_proto — root container (context_list, force-descended)",
    "ContextImpl.async_iterator_proto — root container",
    "ContextImpl.throw_type_error — root container",
    "ContextImpl.array_proto_values — root container",
    "ContextImpl.eval_func — root container",
    "ContextImpl.global_obj — root container",
    "ContextImpl.global_var_obj — root container",
    "ContextImpl.loaded_modules — root container",
    "ProtoCtorPair.proto — class_slots, part of the ContextImpl root",
    "ProtoCtorPair.ctor — class_slots, part of the ContextImpl root",
    "ShapeImpl.proto — shape registry is a root, force-descended",
    "AsyncStateImpl.this_val — rooted via gc_pending_async_states",
    "AsyncStateImpl.resolving_funcs — rooted via gc_pending_async_states",
    "AsyncStateImpl.bytecode_func — rooted via gc_pending_async_states",
    "AGenPayload.async_state — construction only",
    "BFuncPayload.bytecode — construction only",
    "BoundFunctionPayload.func_obj — construction only",
    "BoundFunctionPayload.this_val — construction only",
    "BoundFunctionPayload.bind_args — construction only",
    "BytecodeImpl.realm — construction only",
    "BytecodeImpl.func_name — construction only",
    "BytecodeDebugInfo.filename — construction only",
    "GeneratorPayload.async_state — construction only",
    "IteratorHelperData.obj — construction only",
    "IteratorHelperData.next — construction only",
    "IteratorHelperData.func — construction only",
    "IteratorWrapData.obj — construction only",
    "IteratorWrapData.next — construction only",
    "IteratorZipData.padding — construction only",
    "JSAsyncFromSyncIteratorData.obj — construction only",
    "JSAsyncFromSyncIteratorData.next_method — construction only",
    "JSMapIteratorData.obj — construction only",
    "NFuncPayload.realm — construction only",
    "ProxyPayload.target — construction only",
    "ProxyPayload.handler — construction only",
    "RegExpStringIterPayload.iterating_regexp — construction only",
    "FinalizationRegistryData.callback — construction only",
    "DataViewPayload.buffer — construction only",
    "TypedArrayPayload.array_buffer — construction only",
    "PromiseResolvePayload.promise — construction only",
    "IteratorZipData.keys — construction only, no late write of the Atoms",
    "NFuncPayload.data — private to nfun.rs; `replace_data` stores and binds at the write site",
    "ContextImpl.error_protos — root container",
    "StackFrame.func_obj — VM stack frame, a traced root",
    "StackFrame.this_obj — VM stack frame, a traced root",
    "AsyncStackFrame.var_refs — VM stack frame, a traced root",
    "ElementsView.data — raw-pointer mirror of `ArrayPayload.values`, not a slot",
    "IteratorConcatData.iter — Cell slot; `set_iter` stores and binds (iter_concat.rs)",
    "IteratorConcatData.next — Cell slot; `set_next` stores and binds (iter_concat.rs)",
    "IteratorHelperData.inner — Cell slot; `set_inner` stores and binds (iter_helper.rs)",
    "IteratorHelperData.inner_next — Cell slot; `set_inner_next` stores and binds (iter_helper.rs)",
    "WeakRefTarget.Symbol.0 — weak target; WeakRefData is not traced at all",
    // `ArrayPayload.values` is a plain `Vec<JSValue>`: rule 2 accepts it because
    // `JSValue` has `GcBind`. Every mutator binds at its call site; `reverse`/
    // `sort` are in-place permutations a guard cannot express (array single-point
    // path — `docs/gc.md` Rule 5).
    "ArrayIterPayload.val — Cell slot, only late write is `UNDEFINED` (removal, no edge)",
    "ObjectImpl.shape — `Shape` is private and `set_shape` is the only write path (store+bind)",
    "ObjectImpl.prop_values — `bind_prop_slot` / `replace_prop_slot` are the only write paths",
    "ContextImpl.array_shape — root container; the shape registry is a traced root",
    "ContextImpl.func_shapes — root container; the shape registry is a traced root",
    "ModuleDefImpl.module_name — mdef setters bind",
    "ModuleDependency.module_name — `ModuleDef::add_dependency` binds (reqres.rs)",
    "ModuleDependency.attributes — `ModuleDef::add_dependency` binds (reqres.rs)",
    "ImportEntry.import_name — `ModuleDefImpl::add_import_entry` binds (esm.rs)",
    "ExportEntry.local_name — `ModuleDefImpl::add_export_internal` / `set_export_local_name` bind",
    "ExportEntry.export_name — `ModuleDefImpl::add_export_internal` binds (export.rs)",
    "ExportType.var_ref — `ModuleDefImpl::add_export_internal` / `set_export_var_ref` bind",
];

/// Rule 1 + rule 2: every GC payload field is wrapped or allowlisted.
#[test]
fn gc_payload_fields_are_wrapped_or_allowlisted() {
    let v = zuqe::gc_audit::gc_field_violations(&src_dir(), EXEMPT);
    assert!(
        v.is_empty(),
        "unwrapped GC payload fields (wrap in WriteBarrier/WriteBarrierVec or allowlist):\n{}",
        v.join("\n")
    );
}

/// Rule 3: a guard is only ever constructed, never assigned over a live field.
#[test]
fn guards_are_only_ever_constructed() {
    let v = zuqe::gc_audit::guard_construction_violations(&src_dir());
    assert!(
        v.is_empty(),
        "whole-field replacement bypasses the write barrier \
         (use the payload setter / `set` / `push`):\n{}",
        v.join("\n")
    );
}

/// The audit must be wired to the real payload set — it cannot pass by
/// scanning nothing.
#[test]
fn the_audit_sees_the_pilot_payloads() {
    let roots = zuqe::gc_audit::payload_root_types(&src_dir());
    for pilot in ["PromisePayload", "AGenPayload", "FinalizationRegistryData"] {
        assert!(
            roots.iter().any(|r| r == pilot),
            "root set lost the pilot payload {pilot}: {roots:?}"
        );
    }
    assert!(
        roots.len() >= 40,
        "root set suspiciously small: {} entries: {roots:?}",
        roots.len()
    );
}
