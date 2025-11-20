use std::collections::HashMap;

use gc_lite::{Collect, GcRef, GcWeak};

use crate::{
    JS_CLASS_MAP, JS_CLASS_MAP_ITERATOR, JS_CLASS_SET, JS_CLASS_SET_ITERATOR, JS_CLASS_WEAKMAP,
    JS_CLASS_WEAKSET,
    class::ClassID,
    context::{ContextImpl, ContextRef},
    mapset::{
        js_map_constructor, js_map_funcs, js_map_iterator_proto_funcs, js_map_proto_funcs,
        js_set_funcs, js_set_iterator_proto_funcs, js_set_proto_funcs, js_weakmap_proto_funcs,
        js_weakset_proto_funcs,
    },
    object::{ObjectImpl, ObjectPayload, ObjectRef},
    object_cast,
    runtime::RuntimeImpl,
    utils::hash_usize,
    value::JSValue,
};

/// A key-value pair in a map, either strong or weak
#[derive(Debug, Clone)]
#[repr(u8)]
pub enum MapPair {
    Strong {
        key: JSValue,
        value: JSValue,
    },
    /// use weakref for key
    Weak {
        key: GcWeak<ObjectImpl>,
        value: JSValue,
    },
    Removed,
}

unsafe impl Collect for MapPair {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        match self {
            Self::Strong { key, value } => {
                key.trace(tracer);
                value.trace(tracer);
            }
            Self::Weak { value, .. } => {
                value.trace(tracer);
            }
            _ => {}
        }
    }
}

impl MapPair {
    pub fn normalize_key(key: JSValue) -> JSValue {
        if let JSValue::Float64(d) = key
            && d == 0.0
        {
            return JSValue::Int(0); // -0.0 to 0
        }
        key
    }
}

#[derive(Debug)]
pub struct JSMapState {
    /// is WeakSet/WeakMap?
    is_weak: bool,
    pub(crate) slots: Vec<MapPair>,
    /// hash lookup table
    pub(crate) lut: HashMap<u32, Vec<usize>>,
    /// removed count
    remove_count: u32,
    /// map lock count.
    /// when map is locked, it can't do slot compression. forEach will lock the map.
    lock: u16,
}

unsafe impl Collect for JSMapState {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.slots.trace(tracer);
    }
}

impl JSMapState {
    pub fn new(is_weak: bool, cap: usize) -> Self {
        JSMapState {
            is_weak,
            slots: Vec::with_capacity(cap),
            lut: HashMap::with_capacity(cap),
            remove_count: 0,
            lock: 0,
        }
    }

    #[inline(always)]
    pub const fn is_weak(&self) -> bool {
        self.is_weak
    }

    pub const fn lock(&mut self) {
        self.lock += 1;
    }

    pub fn unlock(&mut self) {
        debug_assert!(self.lock > 0);
        self.lock -= 1;
        if self.lock == 0 {
            self.compress_removed();
        }
    }

    /// compress removed slot items
    const fn compress_removed(&mut self) {
        // debug_assert_eq!(self.lock, 0);
        // if self.remove_count > 0 {
        //     self.slots.retain(|v| !matches!(v, MapPair::Removed));
        //     self.remove_count = 0;
        // }
    }

    /// update or insert key-value pair.
    /// returns false if the value is updated, true if the key-value pair is inserted
    pub fn upsert(&mut self, ctx: &ContextImpl, key: JSValue, value: JSValue) -> Result<bool, i32> {
        let key = MapPair::normalize_key(key);

        if self.is_weak && !key.is_object() {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }

        let hash = key.hash();

        let idx = if let Some(lst) = self.lut.get(&hash) {
            lst.iter().find_map(|i| match self.slots.get_mut(*i) {
                Some(MapPair::Weak { key: k, value: v }) => {
                    debug_assert!(self.is_weak);
                    let Some(o) = key.as_object().copied() else {
                        unreachable!()
                    };
                    k.upgrade(ctx.rt().gc_heap()).and_then(|k| {
                        if ObjectRef(k) == o {
                            *v = value;
                            Some(*i)
                        } else {
                            None
                        }
                    })
                }
                Some(MapPair::Strong { key: k, value: v }) => {
                    debug_assert!(!self.is_weak);
                    if key.is_same0(*k) {
                        *v = value;
                        Some(*i)
                    } else {
                        None
                    }
                }
                _ => None,
            })
        } else {
            None
        };

        if idx.is_none() {
            // key not found, insert
            let kv = if self.is_weak {
                MapPair::Weak {
                    key: key.as_object().unwrap().downgrade(),
                    value,
                }
            } else {
                MapPair::Strong { key, value }
            };

            self.slots.push(kv);
            let idx = self.slots.len() - 1;

            self.lut
                .entry(hash)
                .and_modify(|lst| {
                    lst.push(idx);
                })
                .insert_entry(vec![idx]);

            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// find value by key.
    pub fn find(&self, key: JSValue) -> Option<JSValue> {
        let key = MapPair::normalize_key(key);
        let hash = key.hash();

        if let Some(lst) = self.lut.get(&hash) {
            lst.iter().find_map(|i| match self.slots.get(*i) {
                Some(MapPair::Weak { key: k, value }) => {
                    if let Some(o) = key.as_object() {
                        k.upgrade(o.rt().gc_heap()).and_then(|k| {
                            if ObjectRef(k) == *o {
                                Some(*value)
                            } else {
                                None
                            }
                        })
                    } else {
                        None
                    }
                }
                Some(MapPair::Strong { key: k, value }) => {
                    if key.is_same0(*k) {
                        Some(*value)
                    } else {
                        None
                    }
                }
                _ => None,
            })
        } else {
            None
        }
    }

    /// remove an entry by key.
    pub fn remove(&mut self, ctx: &ContextImpl, key: JSValue) {
        let key = MapPair::normalize_key(key);
        let hash = key.hash();

        // remove from lut
        let purge_lut = if let Some(lst) = self.lut.get_mut(&hash) {
            lst.retain(|i| match self.slots.get(*i) {
                Some(MapPair::Strong { key: k, .. }) => {
                    if key.is_same0(*k) {
                        self.slots[*i] = MapPair::Removed;
                        self.remove_count += 1;
                        false
                    } else {
                        true
                    }
                }
                Some(MapPair::Weak { key: k, .. }) => {
                    if let Some(k) = k.upgrade(ctx.rt().gc_heap()) {
                        if ObjectRef(k) == unsafe { *key.as_object_unchecked() } {
                            self.slots[*i] = MapPair::Removed;
                            self.remove_count += 1;
                            false
                        } else {
                            true
                        }
                    } else {
                        // generally, this shouldn't happen
                        #[cfg(debug_assertions)]
                        unreachable!();

                        #[cfg(not(debug_assertions))]
                        {
                            self.slots[*i] = MapPair::Removed;
                            self.remove_count += 1;
                            false
                        }
                    }
                }
                _ => false,
            });
            lst.is_empty()
        } else {
            false
        };
        if purge_lut {
            self.lut.remove(&hash);
        }

        if self.lock == 0 {
            self.compress_removed();
        }
    }

    pub(crate) fn remove_weak_obj(&mut self, obj: &ObjectImpl) {
        debug_assert!(self.is_weak);

        let hash = hash_usize(obj.id());

        // remove from lut
        let purge_lut = if let Some(lst) = self.lut.get_mut(&hash) {
            lst.retain(|i| match self.slots.get(*i) {
                Some(MapPair::Weak { key, .. }) => {
                    if key
                        .upgrade(obj.rt().gc_heap())
                        .is_some_and(|k| ObjectRef(k).id() != obj.id())
                    {
                        self.slots[*i] = MapPair::Removed;
                        false
                    } else {
                        true
                    }
                }
                Some(MapPair::Strong { .. }) => {
                    unreachable!();
                }
                _ => false,
            });
            lst.is_empty()
        } else {
            false
        };
        if purge_lut {
            self.lut.remove(&hash);
        }

        if self.lock == 0 {
            self.compress_removed();
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|kv| !matches!(kv, MapPair::Removed))
            .count()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.lut.clear();
        self.slots.clear();
        self.remove_count = 0;
    }

    pub const fn class_finalizer(_: &mut RuntimeImpl, _: &ObjectPayload, _: usize) {}

    #[deprecated]
    pub fn class_mark(val: JSValue) {
        let o = unsafe { val.to_object_unchecked() };
        if let Some(s) = o.payload().mapset() {
            for kv in s.slots.iter() {
                match kv {
                    MapPair::Strong { key, value } => {
                        key.gc_mark();
                        value.gc_mark();
                    }
                    MapPair::Weak { value, .. } => {
                        value.gc_mark();
                    }
                    MapPair::Removed => {}
                }
            }
        }
    }
}

/// Object representing a Map, Set, WeakMap, WeakSet instance.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct MapSet(pub(crate) GcRef<ObjectImpl>);

object_cast!(
    MapSet,
    JS_CLASS_MAP | JS_CLASS_SET | JS_CLASS_WEAKMAP | JS_CLASS_WEAKSET
);

impl MapSet {
    pub fn class_initialize(
        mut ctx: ContextRef,
        class_id: ClassID,
    ) -> Result<Option<ObjectRef>, i32> {
        let iter_proto = ctx.iterator_proto()?;

        match class_id {
            JS_CLASS_MAP => {
                let mut obj = ctx.create_base_object()?;
                obj.install_field_defs(&ctx, &js_map_proto_funcs)?;

                let mut ctor = ctx
                    .create_global_constructor_only("Map", js_map_constructor, 0, obj.into(), 0)
                    .unwrap();
                ctor.install_field_defs(&ctx, &js_map_funcs).unwrap();

                Ok(Some(obj))
            }
            JS_CLASS_SET => {
                let mut obj = ctx.create_base_object()?;
                obj.install_field_defs(&ctx, &js_set_proto_funcs)?;

                let mut ctor = ctx
                    .create_global_constructor_only("Set", js_map_constructor, 0, obj.into(), 1)
                    .unwrap();
                ctor.install_field_defs(&ctx, &js_set_funcs)?;

                Ok(Some(obj))
            }
            JS_CLASS_WEAKMAP => {
                let mut obj = ctx.create_base_object()?;
                obj.install_field_defs(&ctx, &js_weakmap_proto_funcs)?;

                ctx.create_global_constructor_only(
                    "WeakMap",
                    js_map_constructor,
                    0,
                    obj.into(),
                    2,
                )?;

                Ok(Some(obj))
            }
            JS_CLASS_WEAKSET => {
                let mut obj = ctx.create_base_object()?;
                obj.install_field_defs(&ctx, &js_weakset_proto_funcs)?;

                ctx.create_global_constructor_only(
                    "WeakSet",
                    js_map_constructor,
                    0,
                    obj.into(),
                    3,
                )?;

                Ok(Some(obj))
            }
            JS_CLASS_MAP_ITERATOR => {
                let mut proto = ctx.create_base_object_with_proto(Some(iter_proto))?;
                proto.install_field_defs(&ctx, &js_map_iterator_proto_funcs)?;
                Ok(Some(proto))
            }
            JS_CLASS_SET_ITERATOR => {
                let mut proto = ctx.create_base_object_with_proto(Some(iter_proto))?;
                proto.install_field_defs(&ctx, &js_set_iterator_proto_funcs)?;
                Ok(Some(proto))
            }
            _ => {
                unreachable!()
            }
        }
    }
}

impl ContextImpl {
    pub fn create_map_with_entries(&self, entries: JSMapState) -> Result<ObjectRef, i32> {
        self.create_object(ObjectPayload::MAP(Box::new(entries)))
            .map_err(|_| -1)
    }
}
