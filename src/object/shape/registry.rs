// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    RuntimeImpl,
    atom::Atom,
    object::{
        JS_PROP_INITIAL_HASH_SIZE, Object, Shape, shape::shape_hasher_u32, shape_initial_hash,
    },
};

impl RuntimeImpl {
    /// register a shape
    pub(crate) fn register_shape(&mut self, shape: &Shape) -> Result<(), i32> {
        debug_assert!(shape.use_count > 0);

        if self.shape_registry.len() == self.shape_registry.capacity() {
            self.shape_registry
                .try_reserve(JS_PROP_INITIAL_HASH_SIZE)
                .map_err(|_| -1)?;
        }

        let hash = shape.hash();
        let shape = shape.gc_ref();
        let weak = shape.downgrade(self.gc_heap_mut());

        const APPEND: usize = usize::MAX;
        const EXIST: usize = usize::MAX - 1;

        let upsert = self.shape_registry.get(&hash).map(|lst| {
            let heap = self.gc_heap();
            let mut idx = APPEND;
            for (i, w) in lst.iter().enumerate() {
                if let Some(s) = w.upgrade(heap) {
                    if s == shape {
                        return EXIST; // already exist
                    }
                } else {
                    idx = i; // reuse the free slot
                }
            }
            idx
        });

        match upsert {
            Some(APPEND) => {
                self.shape_registry.get_mut(&hash).unwrap().push(weak);
            }
            Some(EXIST) => {}
            Some(i) => unsafe {
                *self
                    .shape_registry
                    .get_mut(&hash)
                    .unwrap()
                    .get_unchecked_mut(i) = weak;
            },
            None => {
                self.shape_registry.insert(hash, vec![weak]);
            }
        }

        Ok(())
    }

    /// unregister a shape
    pub(super) fn unregister_shape(&mut self, shape: &Shape) {
        let hash = shape.hash();
        let shape = shape.gc_ref();

        if let Some(mut lst) = self.shape_registry.remove(&hash) {
            let heap = self.gc_heap_mut();
            lst.retain(|w| w.upgrade(heap).is_some_and(|s| s != shape));
            if !lst.is_empty() {
                self.shape_registry.insert(hash, lst);
            }
        }
    }

    pub(crate) fn is_shape_registered(&self, shape: &Shape) -> bool {
        let shape_hash = shape.hash();
        let shape = shape.gc_ref();

        self.shape_registry.get(&shape_hash).is_some_and(|lst| {
            lst.iter()
                .any(|w| w.upgrade(self.gc_heap()).is_some_and(|sh| sh == shape))
        })
    }

    /// find shape that matching `proto` and have not props.
    /// Returned shape increase use count by 1.
    /// Return None if not found.
    pub(crate) fn find_shape_by_proto(&self, proto: Option<Object>) -> Option<Shape> {
        let hash = shape_initial_hash(proto.map_or(0, |o| o.id()));
        let heap = self.gc_heap();

        self.shape_registry.get(&hash).and_then(|lst| {
            lst.iter()
                .filter_map(|w| w.upgrade(heap))
                .find(|sh| sh.proto == proto && sh.prop_defs.count() == 0)
                .map(|s| {
                    let mut sh = Shape::from(s);
                    sh.use_count += 1;
                    sh
                })
        })
    }

    /// find a registered shape matching base_shape plus extras (prop_atom, prop_flags).
    /// Return None if not found
    pub(super) fn find_shape_with_extend_prop(
        &self,
        base_shape: &Shape,
        prop: Atom,
        prop_flags: u32,
    ) -> Option<Shape> {
        // calc hash for base_shape + (atom, prop_flags)
        let mut hash = base_shape.hash;
        hash = shape_hasher_u32(hash, prop.id());
        hash = shape_hasher_u32(hash, prop_flags);

        let base_len = base_shape.prop_defs.len();
        let ttl_len = base_len + 1;

        let heap = self.gc_heap();

        if let Some(found) = self.shape_registry.get(&hash).and_then(|lst| {
            lst.iter()
                .filter_map(|w| w.upgrade(heap))
                .find(|sh| sh.proto == base_shape.proto && sh.prop_defs.len() == ttl_len)
        }) && base_shape
            .prop_defs
            .iter()
            .zip(found.prop_defs.iter())
            .all(|((_, a), (_, b))| a == b)
        {
            // match extra props
            if found
                .prop_defs
                .last()
                .is_some_and(|pr| pr.atom == prop && pr.flags() == prop_flags)
            {
                let mut sh = Shape::from(found);
                sh.use_count += 1;
                return Some(sh);
            }
        }

        None
    }
}
