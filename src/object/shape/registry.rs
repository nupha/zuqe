use std::{collections::HashMap, ptr::NonNull};

use gc_lite::{GcTraceOp, GcWeak};

use crate::{
    RuntimeImpl,
    atom::Atom,
    object::{
        JS_PROP_INITIAL_HASH_SIZE, Object, ShapeHandle, ShapeImpl, shape::shape_hasher_u32,
        shape_initial_hash,
    },
};

#[derive(Debug)]
pub(crate) struct ShapeRegistry {
    pub(crate) lut: HashMap<u32, Vec<GcWeak<ShapeImpl>>>,
}

impl ShapeRegistry {
    pub fn new() -> Self {
        Self {
            lut: HashMap::with_capacity(64),
        }
    }

    #[deprecated]
    pub(crate) fn gc_trace(&self, mut tr: GcTraceOp) {
        let heap_ptr = NonNull::from_ref(tr.heap());

        tr.add_nodes(self.lut.values().flat_map(|lst| {
            lst.iter()
                .filter_map(|o| o.upgrade(unsafe { heap_ptr.as_ref() }))
                .map(|g| g.node_ptr())
        }));
    }

    pub fn clear(&mut self) {
        self.lut.clear();
    }
}

impl RuntimeImpl {
    /// register a shape
    pub(crate) fn register_shape(&mut self, shape: &ShapeHandle) -> Result<(), i32> {
        debug_assert!(shape.use_count() > 0);

        if self.shape_registry.lut.len() == self.shape_registry.lut.capacity() {
            self.shape_registry
                .lut
                .try_reserve(JS_PROP_INITIAL_HASH_SIZE)
                .map_err(|_| -1)?;
        }

        let hash = shape.hash();
        let id = shape.id();
        let sh = shape.gc_ref();
        let weak = sh.downgrade(self.gc_heap_mut());

        const PUSH: usize = usize::MAX;
        const EXIST: usize = usize::MAX - 1;

        let upsert = self.shape_registry.lut.get(&hash).map(|lst| {
            let heap = self.gc_heap();
            let mut idx = PUSH;
            for (i, w) in lst.iter().enumerate() {
                if let Some(s) = w.upgrade(heap) {
                    if s.id() == id {
                        return EXIST; // already exist
                    }
                } else {
                    idx = i; // reuse the slot - weaked
                }
            }
            idx
        });

        match upsert {
            Some(PUSH) => {
                self.shape_registry.lut.get_mut(&hash).unwrap().push(weak);
            }
            Some(EXIST) => {}
            Some(i) => unsafe {
                *self
                    .shape_registry
                    .lut
                    .get_mut(&hash)
                    .unwrap()
                    .get_unchecked_mut(i) = weak;
            },
            None => {
                self.shape_registry.lut.insert(hash, vec![weak]);
            }
        }

        Ok(())
    }

    /// unregister a shape
    pub(super) fn unregister_shape(&mut self, shape: &ShapeHandle) {
        let hash = shape.hash();
        let shape_id = shape.id();

        if let Some(mut lst) = self.shape_registry.lut.remove(&hash) {
            let heap = self.gc_heap_mut();
            lst.retain(|w| w.upgrade(heap).is_some_and(|s| s.id() != shape_id));
            if !lst.is_empty() {
                self.shape_registry.lut.insert(hash, lst);
            }
        }
    }

    pub fn is_shape_registered(&self, shape: &ShapeHandle) -> bool {
        let shape_id = shape.id();
        let shape_hash = shape.hash();

        self.shape_registry.lut.get(&shape_hash).is_some_and(|lst| {
            lst.iter().any(|w| {
                w.upgrade(self.gc_heap())
                    .is_some_and(|sh| sh.id() == shape_id)
            })
        })
    }

    /// find an empty shape matching given prototype.
    /// Returned shape increase use count by 1.
    /// Return None if not found.
    pub fn find_shape_by_proto(&self, proto: Option<Object>) -> Option<ShapeHandle> {
        let hash = shape_initial_hash(proto.map_or(0, |o| o.id()));
        let heap = self.gc_heap();

        self.shape_registry.lut.get(&hash).and_then(|lst| {
            lst.iter()
                .filter_map(|w| w.upgrade(heap))
                .find(|sh| sh.proto == proto && sh.prop_defs.count() == 0)
                .map(|mut s| {
                    s.use_count += 1; // increase refcount
                    ShapeHandle::from(s)
                })
        })
    }

    /// find a registered shape matching base_shape plus extras (prop_atom, prop_flags).
    /// Return None if not found
    pub(super) fn find_shape_with_extend_prop(
        &self,
        base_shape: &ShapeHandle,
        prop: Atom,
        prop_flags: u32,
    ) -> Option<ShapeHandle> {
        // calc hash for base_shape + (atom, prop_flags)
        let mut hash = base_shape.hash;
        hash = shape_hasher_u32(hash, prop.id());
        hash = shape_hasher_u32(hash, prop_flags);

        let base_len = base_shape.prop_defs.len();
        let ttl_len = base_len + 1;

        let heap = self.gc_heap();

        if let Some(mut found) = self.shape_registry.lut.get(&hash).and_then(|lst| {
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
                found.use_count += 1;
                return Some(ShapeHandle::from(found));
            }
        }

        None
    }
}
