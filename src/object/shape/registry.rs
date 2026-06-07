// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Flat-array shape registry with per-bucket `Vec<GcWeak>`.
//!
//! Replaces the old `HashMap<u32, Vec<GcWeak<ShapeImpl>>>` with a flat array
//! of buckets. Each bucket is a `Vec<GcWeak>` containing all shapes that hash
//! to that slot. Dead weak refs (GC'd shapes) are skipped during search and
//! opportunistically cleaned up during traversal.
//!
//! This design avoids the chain-fragmentation problem of intrusive linked
//! lists: a dead `GcWeak` in a Vec only wastes a slot; it never breaks the
//! search path to subsequent entries.

use gc_lite::{GcHeap, GcWeak};

use crate::{
    RuntimeImpl,
    atom::Atom,
    object::{
        Object, Shape, ShapeImpl,
        shape::{shape_hasher_u32, shape_initial_hash},
    },
};

/// Flat-array shape registry.
pub(crate) struct ShapeRegistry {
    buckets: Vec<Vec<GcWeak<ShapeImpl>>>,
    bucket_bits: u8,
    mask: usize,
    count: usize,
}

impl ShapeRegistry {
    const INITIAL_BITS: u8 = 6; // 64 buckets

    pub(crate) fn new() -> Self {
        let size = 1usize << Self::INITIAL_BITS;
        let mut buckets = Vec::with_capacity(size);
        buckets.resize_with(size, || Vec::with_capacity(1));
        Self {
            buckets,
            bucket_bits: Self::INITIAL_BITS,
            mask: size - 1,
            count: 0,
        }
    }

    pub(crate) fn clear(&mut self) {
        for bucket in self.buckets.iter_mut() {
            bucket.clear();
        }
        self.count = 0;
    }

    /// Insert the shape. O(1) amortized.
    ///
    /// Panics in debug mode if the shape is already registered.
    pub(crate) fn insert(&mut self, shape: &Shape, heap: &mut GcHeap) -> Result<(), i32> {
        debug_assert!(
            !self.contains(shape, heap),
            "insert: shape already registered"
        );

        if self.count >= (self.buckets.len() * 3 / 4) {
            self.resize(heap);
        }

        let idx = shape.hash() as usize & self.mask;
        self.buckets[idx].push(shape.gc_ref().downgrade(heap));
        self.count += 1;

        Ok(())
    }

    /// Remove the shape from the registry.
    ///
    /// Handles duplicates gracefully (removes all copies).
    pub(crate) fn remove(&mut self, shape: &Shape, heap: &GcHeap) {
        let idx = shape.hash() as usize & self.mask;
        let target = shape.gc_ref();

        let bucket = &mut self.buckets[idx];
        let len_before = bucket.len();
        bucket.retain(|w| w.upgrade(heap).is_some_and(|candidate| candidate != target));
        let removed = len_before - bucket.len();
        debug_assert!(removed > 0, "remove: shape not found in registry");
        self.count -= removed;
    }

    /// Find a registered shape matching `base_shape` extended by `(prop, prop_flags)`.
    /// Returns the shape with `use_count` incremented by 1, or `None`.
    pub(crate) fn find_extend(
        &self,
        base_shape: &Shape,
        prop: Atom,
        prop_flags: u32,
        heap: &GcHeap,
    ) -> Option<Shape> {
        let mut hash = base_shape.hash;
        hash = shape_hasher_u32(hash, prop.id());
        hash = shape_hasher_u32(hash, prop_flags);

        let idx = hash as usize & self.mask;
        let base_len = base_shape.prop_defs.len();
        let ttl_len = base_len + 1;

        for weak in self.buckets[idx].iter() {
            let Some(candidate) = weak.upgrade(heap) else {
                continue;
            };
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
        }
        None
    }

    /// Find a registered empty shape (no properties) matching `proto`.
    /// Returns the shape with `use_count` incremented by 1, or `None`.
    pub(crate) fn find_empty_by_proto(
        &self,
        proto: Option<Object>,
        heap: &GcHeap,
    ) -> Option<Shape> {
        let hash = shape_initial_hash(proto.map_or(0, |o| o.id()));
        let idx = hash as usize & self.mask;

        for weak in self.buckets[idx].iter() {
            let Some(candidate) = weak.upgrade(heap) else {
                continue;
            };
            if candidate.proto == proto && candidate.prop_defs.count() == 0 {
                let mut sh = Shape::from(candidate);
                sh.use_count += 1;
                return Some(sh);
            }
        }
        None
    }

    /// Check whether `shape` is already in the registry.
    pub(crate) fn contains(&self, shape: &Shape, heap: &GcHeap) -> bool {
        let idx = shape.hash() as usize & self.mask;
        let target = shape.gc_ref();

        for weak in self.buckets[idx].iter() {
            if let Some(candidate) = weak.upgrade(heap) {
                if candidate == target {
                    return true;
                }
            }
        }
        false
    }

    /// Resize the hash table to 2x capacity.
    fn resize(&mut self, heap: &mut GcHeap) {
        let new_bits = self.bucket_bits + 1;
        let new_size = 1usize << new_bits;
        let new_mask = new_size - 1;

        // Build new buckets, skipping dead weak refs in the process
        let mut new_buckets = Vec::with_capacity(new_size);
        new_buckets.resize_with(new_size, || Vec::with_capacity(1));

        for bucket in self.buckets.iter() {
            for weak in bucket.iter() {
                if let Some(sh) = weak.upgrade(heap) {
                    let idx = sh.hash as usize & new_mask;
                    new_buckets[idx].push(*weak);
                }
            }
        }

        self.buckets = new_buckets;
        self.bucket_bits = new_bits;
        self.mask = new_mask;
    }
}

impl RuntimeImpl {
    pub(crate) fn register_shape(&mut self, shape: &Shape) -> Result<(), i32> {
        debug_assert!(shape.use_count > 0);
        self.shape_registry.insert(shape, &mut self.gc_heap)
    }

    pub(crate) fn unregister_shape(&mut self, shape: &Shape) {
        self.shape_registry.remove(shape, &self.gc_heap);
    }

    pub(crate) fn is_shape_registered(&self, shape: &Shape) -> bool {
        self.shape_registry.contains(shape, &self.gc_heap)
    }

    pub(crate) fn find_shape_by_proto(&self, proto: Option<Object>) -> Option<Shape> {
        self.shape_registry
            .find_empty_by_proto(proto, &self.gc_heap)
    }

    pub(super) fn find_shape_with_extend_prop(
        &self,
        base_shape: &Shape,
        prop: Atom,
        prop_flags: u32,
    ) -> Option<Shape> {
        self.shape_registry
            .find_extend(base_shape, prop, prop_flags, &self.gc_heap)
    }
}
