use std::collections::HashMap;

use crate::{
    context::JSContext,
    object::{JSObjectWeakref, ObjectInner, ObjectPayload},
    runtime::JSRuntime,
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

    pub fn set_value(&mut self, val: JSValue) {
        match self {
            Self::Strong { value, .. } | Self::Weak { value, .. } => {
                *value = val;
            }
            Self::Removed => {
                unreachable!();
            }
        }
    }
}

#[derive(Debug)]
pub struct JSMapState {
    /// is WeakSet/WeakMap?
    is_weak: bool,
    pub(crate) slots: Vec<MapPair>,
    /// hash lookup table
    pub(crate) lut: HashMap<u32, Vec<usize>>,
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
        debug_assert_eq!(self.lock, 0);
        self.slots.retain(|v| !matches!(v, MapPair::Removed));
    }

    pub fn add(&mut self, ctx: &JSContext, key: JSValue, value: JSValue) -> Result<(), i32> {
        if self.is_weak && !key.is_object() {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }

        let hash = key.hash();
        let kv = if self.is_weak {
            MapPair::Weak {
                key: key.object().unwrap().downgrade(),
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

        Ok(())
    }

    /// find value by key.
    pub fn find(&self, key: &JSValue) -> Option<JSValue> {
        let key = MapPair::normalize_key(key.clone());
        let hash = key.hash();

        if let Some(lst) = self.lut.get(&hash) {
            lst.iter().find_map(|i| match self.slots.get(*i) {
                Some(MapPair::Weak { key: k, value }) => {
                    debug_assert!(self.is_weak);
                    if let Some(o) = value.object() {
                        k.upgrade()
                            .and_then(|k| if k.eq(o) { Some(value.clone()) } else { None })
                    } else {
                        None
                    }
                }
                Some(MapPair::Strong { key: k, value }) => {
                    debug_assert!(!self.is_weak);
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

    pub(crate) fn remove_weak_obj(&mut self, obj: &ObjectInner) {
        debug_assert!(self.is_weak);

        let hash = JSValue::id_to_hash(obj.id());

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

    /// remove an entry by key.
    pub fn remove(&mut self, key: &JSValue) {
        let key = MapPair::normalize_key(key.clone());
        let hash = key.hash();

        // remove from lut
        let purge_lut = if let Some(lst) = self.lut.get_mut(&hash) {
            lst.retain(|i| match self.slots.get(*i) {
                Some(MapPair::Strong { key: k, .. }) => {
                    debug_assert!(!self.is_weak);
                    if key.eq(k) {
                        self.slots[*i] = MapPair::Removed;
                        false
                    } else {
                        true
                    }
                }
                Some(MapPair::Weak { key: k, .. }) => {
                    debug_assert!(self.is_weak);
                    let kobj = unsafe { key.object_unchecked() };
                    if k.upgrade().map_or(false, |k| !k.eq(kobj)) {
                        self.slots[*i] = MapPair::Removed;
                        false
                    } else {
                        true
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

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    #[inline(always)]
    pub fn clear(&mut self) {
        self.lut.clear();
        self.slots.clear();
    }

    pub fn hook_finalizer(_: &mut JSRuntime, _: &ObjectPayload, _: usize) {}

    pub fn hook_mark(val: JSValue) {
        let p = unsafe { val.object_unchecked() };
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

// /// Remove the weak reference from the object weak reference list.
// /// we don't use a doubly linked list to save space,
// /// assuming a given object has few weak references to it
// pub unsafe fn delete_weak_ref(mr: *mut JSMapRecord) {
//     let mut pmr = std::ptr::addr_of_mut!((*mr).key.object_unchecked().first_weak_ref);
//     let mut mr1: *mut JSMapRecord;

//     loop {
//         mr1 = *pmr;
//         debug_assert!(!mr1.is_null());

//         if mr1 == mr {
//             break;
//         }
//         pmr = std::ptr::addr_of_mut!((*mr1).next_weak_ref);
//     }

//     *pmr = (*mr1).next_weak_ref;
// }

pub fn js_map_iterator_finalizer(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {
    unimplemented!();
}

pub fn js_map_iterator_mark(val: JSValue) {
    unimplemented!();
}
