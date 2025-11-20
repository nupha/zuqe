use std::collections::HashMap;

use gc_lite::GcRef;

use crate::{
    atom::Atom,
    object::{
        JS_PROP_INITIAL_HASH_SIZE, ShapeHandle, ObjectRef, ShapeImpl, shape::shape_hasher_u32,
        shape_initial_hash,
    },
};

#[derive(Debug)]
pub(crate) struct ShapeRegistry {
    pub(super) lut: HashMap<u32, Vec<GcRef<ShapeImpl>>>,
}

impl ShapeRegistry {
    pub fn new() -> Self {
        Self {
            lut: HashMap::with_capacity(64),
        }
    }

    /// register a shape
    pub(crate) fn register(&mut self, shape: &ShapeHandle) -> Result<(), i32> {
        debug_assert!(shape.use_count() > 0);

        if self.lut.len() == self.lut.capacity() {
            self.lut
                .try_reserve(JS_PROP_INITIAL_HASH_SIZE)
                .map_err(|_| -1)?;
        }

        let hash = shape.hash();
        let id = shape.id();
        let sh = shape.gc_ref();

        if let Some(lst) = self.lut.get_mut(&hash) {
            if !lst.iter().any(|s| unsafe { s.as_ref().id() == id }) {
                lst.push(sh);
            }
        } else {
            self.lut.insert(hash, vec![sh]);
        }

        Ok(())
    }

    /// unregister a shape
    pub(super) fn unregister(&mut self, shape: &ShapeHandle) {
        let hash = shape.hash();
        let shape_id = shape.id();

        if let Some(lst) = self.lut.get_mut(&hash) {
            if lst.len() == 1 && unsafe { lst[0].as_ref().id() } == shape_id {
                self.lut.remove(&hash);
            } else {
                lst.iter()
                    .position(|s| unsafe { s.as_ref().id() == shape_id })
                    .map(|i| lst.swap_remove(i));
            }
        }
    }

    fn is_registered2(&self, shape_id: usize, shape_hash: u32) -> bool {
        self.lut
            .get(&shape_hash)
            .is_some_and(|lst| lst.iter().any(|s| unsafe { s.as_ref().id() == shape_id }))
    }

    #[inline(always)]
    pub fn is_registered(&self, shape: &ShapeHandle) -> bool {
        self.is_registered2(shape.id(), shape.hash())
    }

    /// increase use count
    pub(super) fn inc_use(&mut self, shape: &ShapeHandle) {
        let hash = shape.hash();
        let shape_id = shape.id();

        if let Some(lst) = self.lut.get_mut(&hash)
            && let Some(slot) = lst
                .iter_mut()
                .find(|s| unsafe { s.as_ref().id() == shape_id })
        {
            unsafe {
                slot.as_mut().use_count += 1;
            }
        }
    }

    /// decrease shape use count. when use count reaches zero, unregister the shape.
    pub(super) fn dec_use(&mut self, shape: &ShapeHandle) {
        let shape_id = shape.id();
        let hash = shape.hash();

        let remove_key = if let Some(lst) = self.lut.get_mut(&hash) {
            debug_assert!(!lst.is_empty());

            let remove_idx = if let Some((i, slot)) = lst
                .iter()
                .enumerate()
                .find(|(_, s)| unsafe { s.as_ref().id() == shape_id })
            {
                unsafe {
                    if slot.as_ref().use_count == 1 {
                        i as i32 // remove from lst
                    } else {
                        slot.as_mut().use_count -= 1;
                        -1
                    }
                }
            } else {
                -1
            };

            if remove_idx >= 0 {
                if lst.len() == 1 {
                    true // remove key
                } else {
                    lst.swap_remove(remove_idx as usize);
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

        if remove_key {
            self.lut.remove(&hash);
        }
    }

    /// find an empty shape matching given prototype.
    /// Return None if not found.
    pub fn find_shape_by_proto(&self, proto: Option<ObjectRef>) -> Option<ShapeHandle> {
        let hash = shape_initial_hash(proto.map_or(0, |o| o.id()));

        self.lut.get(&hash).and_then(|lst| {
            lst.iter()
                .find(|sh| {
                    let s = unsafe { sh.as_ref() };
                    // no shape props, and proto equals to given
                    s.prop_defs.is_empty() && s.proto == proto
                })
                .copied()
                .map(|s| unsafe {
                    s.as_mut().use_count += 1; // increase refcount to return
                    ShapeHandle::from(s)
                })
        })
    }

    /// find a registered shape matching base_shape plus extras (prop_atom, prop_flags).
    /// Return None if not found
    pub(super) fn find_shape_with_extend_prop(
        &self,
        base_shape: &ShapeHandle,
        prop: &Atom,
        prop_flags: u32,
    ) -> Option<ShapeHandle> {
        // calc hash for base_shape + (atom, prop_flags)
        let mut hash = base_shape.hash;
        hash = shape_hasher_u32(hash, prop.id());
        hash = shape_hasher_u32(hash, prop_flags);

        let base_len = base_shape.prop_defs.len();
        let ttl_len = base_len + 1;

        if let Some(found) = self.lut.get(&hash).and_then(|lst| {
            lst.iter().find_map(|sh| {
                let s = unsafe { sh.as_mut() };
                if s.proto == base_shape.proto && s.prop_defs.len() == ttl_len {
                    Some(*sh)
                } else {
                    None
                }
            })
        }) && base_shape
            .prop_defs
            .iter()
            .zip(unsafe { found.as_ref().prop_defs.iter() })
            .all(|(a, b)| a.1.atom == b.1.atom && a.1.flags() == b.1.flags())
        {
            let shape = unsafe { found.as_mut() };

            // match extra props
            if shape
                .prop_defs
                .last()
                .is_some_and(|(_, pr)| pr.atom.eq(prop) && pr.flags() == prop_flags)
            {
                shape.use_count += 1; // increase refcount to return the shape
                return Some(ShapeHandle::from(found));
            }
        }

        None
    }

    pub fn clear(&mut self) {
        self.lut.clear();
    }
}
