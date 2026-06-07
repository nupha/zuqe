// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Flat-array + intrusive singly-linked list shape registry using `GcRef`.
//!
//! Unlike the earlier attempt with `GcWeak`, this design stores `GcRef<ShapeImpl>`
//! (strong references) in both the bucket head and the `registry_next` links.
//! Since `GcRef` is `Copy` in gc-lite (no `Drop`), no reference counting overhead
//! is involved — the pointers are just raw non-null handles.
//!
//! **Lifecycle**: A shape is kept alive during GC by being traced through the
//! registry. Before each GC mark cycle, shapes with `use_count == 0` are
//! removed from the registry via `remove_unused()`. The GC then traces the
//! remaining shapes (which all have `use_count > 0`), following `registry_next`
//! chains, keeping them alive.
//!
//! **Transaction safety**: The unregister-modify-register window in
//! `modify_shape()` is GC-free, so the temporary absence from the registry
//! is safe.

use gc_lite::{GcRef, GcTraceCtx};

use crate::{
    RuntimeImpl,
    atom::Atom,
    object::{
        Object, Shape, ShapeImpl,
        shape::{shape_hasher_u32, shape_initial_hash},
    },
};

/// Flat-array + intrusive linked-list shape registry.
pub(crate) struct ShapeRegistry {
    /// Buckets holding the head of each intrusive linked list.
    buckets: Vec<Option<GcRef<ShapeImpl>>>,
    bucket_bits: u8,
    mask: usize,
    count: usize,
}

impl ShapeRegistry {
    const INITIAL_BITS: u8 = 6; // 64 buckets

    pub(crate) fn new() -> Self {
        let size = 1usize << Self::INITIAL_BITS;
        let mut buckets = Vec::with_capacity(size);
        buckets.resize_with(size, || None);
        Self {
            buckets,
            bucket_bits: Self::INITIAL_BITS,
            mask: size - 1,
            count: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        // Drop all GcRefs in the linked lists by setting buckets to None.
        // Since GcRef is Copy, this just drops the Option (the pointer value
        // is forgotten). The GC will collect unreachable shapes next cycle.
        self.buckets.fill(None);
        self.count = 0;
    }

    /// Head-insert the shape. O(1).
    ///
    /// Panics in debug mode if the shape is already registered.
    pub(crate) fn insert(&mut self, shape: &Shape) -> Result<(), i32> {
        debug_assert!(!self.contains(shape), "insert: shape already registered");

        if self.count >= (self.buckets.len() * 3 / 4) {
            self.resize();
        }

        let idx = shape.hash() as usize & self.mask;
        let gc_ref = shape.gc_ref();

        // SAFETY: shape.use_count > 0 (guaranteed by caller), and the
        // shape is not yet in any linked list (debug_assert above).
        unsafe {
            (*ShapeImpl::gc_mut_ptr(gc_ref)).registry_next = self.buckets[idx];
        }
        self.buckets[idx] = Some(gc_ref);
        self.count += 1;

        Ok(())
    }

    /// Remove the shape from the registry using double-pointer walk.
    pub(crate) fn remove(&mut self, shape: &Shape) {
        let idx = shape.hash() as usize & self.mask;
        let target = shape.gc_ref();

        let mut slot: &mut Option<GcRef<ShapeImpl>> = &mut self.buckets[idx];
        loop {
            if let Some(cur) = slot {
                if *cur == target {
                    // Unlink: slot = cur.registry_next
                    *slot = unsafe { (*ShapeImpl::gc_mut_ptr(*cur)).registry_next };
                    self.count -= 1;
                    return;
                }
                slot = unsafe { &mut (*ShapeImpl::gc_mut_ptr(*cur)).registry_next };
            } else {
                break;
            }
        }
        // If we reach here, the shape was not found.
        debug_assert!(false, "remove: shape not found in registry");
    }

    /// Remove all shapes with `use_count == 0` from the registry.
    /// Called before each GC mark cycle.
    pub(crate) fn remove_unused(&mut self) {
        for idx in 0..self.buckets.len() {
            // Double-pointer walk: slot is the `&mut Option<GcRef>` that
            // points to the current node (either bucket[idx] or prev.registry_next).
            let mut slot: &mut Option<GcRef<ShapeImpl>> = &mut self.buckets[idx];
            loop {
                let should_remove = slot.as_ref().is_some_and(|gc_ref| unsafe {
                    (*ShapeImpl::gc_mut_ptr(*gc_ref)).use_count == 0
                });
                if should_remove {
                    let cur = slot.unwrap();
                    *slot = unsafe { (*ShapeImpl::gc_mut_ptr(cur)).registry_next };
                    self.count -= 1;
                    // Continue with the new head of the chain (same slot)
                } else if let Some(cur) = slot {
                    // Move to next node
                    slot = unsafe { &mut (*ShapeImpl::gc_mut_ptr(*cur)).registry_next };
                } else {
                    break;
                }
            }
        }
    }

    /// Trace all registered shapes through the GC trace context.
    /// Bucket heads (and their `registry_next` chains) are added to the
    /// traced set, which keeps them alive during the current GC cycle.
    pub(crate) fn trace_all(&self, gcx: &mut GcTraceCtx) {
        for bucket in self.buckets.iter() {
            if let Some(head) = bucket {
                gcx.add(*head);
            }
        }
    }

    /// Find a registered shape matching `base_shape` extended by `(prop, prop_flags)`.
    /// Returns the shape with `use_count` incremented, or `None`.
    pub(crate) fn find_extend(
        &self,
        base_shape: &Shape,
        prop: Atom,
        prop_flags: u32,
    ) -> Option<Shape> {
        let mut hash = base_shape.hash;
        hash = shape_hasher_u32(hash, prop.id());
        hash = shape_hasher_u32(hash, prop_flags);

        let idx = hash as usize & self.mask;
        let base_len = base_shape.prop_defs.len();
        let ttl_len = base_len + 1;

        let mut ptr = self.buckets[idx];
        while let Some(candidate) = ptr {
            if candidate.hash == hash
                && candidate.proto == base_shape.proto
                && candidate.prop_defs.len() == ttl_len
                && base_shape
                    .prop_defs
                    .iter()
                    .zip(candidate.prop_defs.iter())
                    .all(|((_, a), (_, b))| a == b)
                && candidate
                    .prop_defs
                    .last()
                    .is_some_and(|pr| pr.atom == prop && pr.flags() == prop_flags)
            {
                let mut sh = Shape::from(candidate);
                sh.use_count += 1;
                return Some(sh);
            }
            ptr = candidate.registry_next;
        }
        None
    }

    /// Find a registered empty shape (no properties) matching `proto`.
    /// Returns the shape with `use_count` incremented, or `None`.
    pub(crate) fn find_empty_by_proto(&self, proto: Option<Object>) -> Option<Shape> {
        let hash = shape_initial_hash(proto.map_or(0, |o| o.id()));
        let idx = hash as usize & self.mask;

        let mut ptr = self.buckets[idx];
        while let Some(candidate) = ptr {
            if candidate.proto == proto && candidate.prop_defs.count() == 0 {
                let mut sh = Shape::from(candidate);
                sh.use_count += 1;
                return Some(sh);
            }
            ptr = candidate.registry_next;
        }
        None
    }

    /// Check whether `shape` is already in the registry.
    pub(crate) fn contains(&self, shape: &Shape) -> bool {
        let idx = shape.hash() as usize & self.mask;
        let target = shape.gc_ref();

        let mut ptr = self.buckets[idx];
        while let Some(candidate) = ptr {
            if candidate == target {
                return true;
            }
            ptr = candidate.registry_next;
        }
        false
    }

    /// Resize the hash table to 2x capacity.
    fn resize(&mut self) {
        let new_bits = self.bucket_bits + 1;
        let new_size = 1usize << new_bits;
        let new_mask = new_size - 1;

        let mut new_buckets = Vec::with_capacity(new_size);
        new_buckets.resize_with(new_size, || None);

        // Rehash all entries, preserving registry_next links
        for bucket in self.buckets.iter() {
            let mut ptr = *bucket;
            while let Some(node) = ptr {
                // Save next before modifying
                let next = unsafe { (*ShapeImpl::gc_mut_ptr(node)).registry_next };
                let new_idx = node.hash as usize & new_mask;
                unsafe {
                    (*ShapeImpl::gc_mut_ptr(node)).registry_next = new_buckets[new_idx];
                }
                new_buckets[new_idx] = Some(node);
                ptr = next;
            }
        }

        self.buckets = new_buckets;
        self.bucket_bits = new_bits;
        self.mask = new_mask;
    }
}

/// Helper: get mutable pointer to ShapeImpl from GcRef.
impl ShapeImpl {
    unsafe fn gc_mut_ptr(gc_ref: GcRef<Self>) -> *mut Self {
        gc_ref.as_ptr().as_ptr() as *mut Self
    }
}

impl RuntimeImpl {
    pub(crate) fn register_shape(&mut self, shape: &Shape) -> Result<(), i32> {
        debug_assert!(shape.use_count > 0);
        self.shape_registry.insert(shape)
    }

    pub(crate) fn unregister_shape(&mut self, shape: &Shape) {
        self.shape_registry.remove(shape);
    }

    pub(crate) fn is_shape_registered(&self, shape: &Shape) -> bool {
        self.shape_registry.contains(shape)
    }

    pub(crate) fn find_shape_by_proto(&self, proto: Option<Object>) -> Option<Shape> {
        self.shape_registry.find_empty_by_proto(proto)
    }

    pub(super) fn find_shape_with_extend_prop(
        &self,
        base_shape: &Shape,
        prop: Atom,
        prop_flags: u32,
    ) -> Option<Shape> {
        self.shape_registry
            .find_extend(base_shape, prop, prop_flags)
    }
}
