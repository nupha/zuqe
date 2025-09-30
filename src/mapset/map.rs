use std::{cell::RefCell, collections::HashMap};

use crate::{
    JS_CLASS_MAP, JS_CLASS_SET, JS_CLASS_WEAKMAP, JS_CLASS_WEAKSET,
    context::JSContext,
    object::{JSObject, JSObjectWeakref, ObjectInner, ObjectPayload},
    object_cast,
    runtime::JSRuntime,
    utils::hash_usize,
    value::JSValue,
};

/// A key-value pair in a map, either strong or weak
#[derive(Debug, Clone)]
#[repr(u8)]
pub(crate) enum MapPair {
    Strong {
        key: JSValue,
        value: JSValue,
    },
    /// use weakref for key
    Weak {
        key: JSObjectWeakref,
        value: JSValue,
    },
    Removed,
}

impl MapPair {
    pub fn normalize_key(key: JSValue) -> JSValue {
        if let JSValue::Float64(d) = key {
            if d == 0.0 {
                return JSValue::Int(0); // -0.0 to 0
            }
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
    pub fn is_weak(&self) -> bool {
        self.is_weak
    }

    pub fn lock(&mut self) {
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
    fn compress_removed(&mut self) {
        // debug_assert_eq!(self.lock, 0);
        // if self.remove_count > 0 {
        //     self.slots.retain(|v| !matches!(v, MapPair::Removed));
        //     self.remove_count = 0;
        // }
    }

    /// update or insert key-value pair.
    /// returns false if the value is updated, true if the key-value pair is inserted
    pub fn upsert(&mut self, ctx: &JSContext, key: JSValue, value: JSValue) -> Result<bool, i32> {
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
                    let Some(o) = key.as_object() else {
                        unreachable!()
                    };
                    k.upgrade().and_then(|k| {
                        if k.eq(o) {
                            *v = value.clone();
                            Some(*i)
                        } else {
                            None
                        }
                    })
                }
                Some(MapPair::Strong { key: k, value: v }) => {
                    debug_assert!(!self.is_weak);
                    if key.is_same0(k) {
                        *v = value.clone();
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
    pub fn find(&self, key: &JSValue) -> Option<JSValue> {
        let key = MapPair::normalize_key(key.clone());
        let hash = key.hash();

        if let Some(lst) = self.lut.get(&hash) {
            lst.iter().find_map(|i| match self.slots.get(*i) {
                Some(MapPair::Weak { key: k, value }) => {
                    if let Some(o) = key.as_object() {
                        k.upgrade()
                            .and_then(|k| if k.eq(o) { Some(value.clone()) } else { None })
                    } else {
                        None
                    }
                }
                Some(MapPair::Strong { key: k, value }) => {
                    if key.is_same0(k) {
                        Some(value.clone())
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
    pub fn remove(&mut self, key: &JSValue) {
        let key = MapPair::normalize_key(key.clone());
        let hash = key.hash();

        // remove from lut
        let purge_lut = if let Some(lst) = self.lut.get_mut(&hash) {
            lst.retain(|i| match self.slots.get(*i) {
                Some(MapPair::Strong { key: k, .. }) => {
                    if key.is_same0(k) {
                        self.slots[*i] = MapPair::Removed;
                        self.remove_count += 1;
                        false
                    } else {
                        true
                    }
                }
                Some(MapPair::Weak { key: k, .. }) => {
                    if let Some(k) = k.upgrade() {
                        if k.eq(unsafe { key.as_object_unchecked() }) {
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

    pub(crate) fn remove_weak_obj(&mut self, obj: &ObjectInner) {
        debug_assert!(self.is_weak);

        let hash = hash_usize(obj.id());

        // remove from lut
        let purge_lut = if let Some(lst) = self.lut.get_mut(&hash) {
            lst.retain(|i| match self.slots.get(*i) {
                Some(MapPair::Weak { key, .. }) => {
                    if key.upgrade().map_or(false, |k| k.id() != obj.id()) {
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
    pub fn clear(&mut self) {
        self.lut.clear();
        self.slots.clear();
        self.remove_count = 0;
    }

    pub fn class_finalizer(_: &mut JSRuntime, _: &ObjectPayload, _: usize) {}

    pub fn class_mark(val: JSValue) {
        let p = unsafe { val.as_object_unchecked() };
        if let Some(s) = p.0.payload.borrow().map_state() {
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
#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct MapSet(pub(crate) JSObject);

object_cast!(
    MapSet,
    JS_CLASS_MAP | JS_CLASS_SET | JS_CLASS_WEAKMAP | JS_CLASS_WEAKSET
);

impl JSContext {
    pub fn create_map_with_entries(&self, entries: JSMapState) -> Result<JSObject, i32> {
        self.create_object(ObjectPayload::MAP(RefCell::new(Box::new(entries))))
            .map_err(|_| -1)
    }
}
