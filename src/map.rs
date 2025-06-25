use std::collections::HashMap;

use crate::{
    context::JSContext,
    gc::GCMarkFunc,
    runtime::JSRuntime,
    value::{
        JSValue, JSValueConst,
        object::{JSObject, JSObjectInner, JSObjectIsa, JSObjectWeakref},
    },
};

#[derive(Debug, Clone)]
pub enum MapKV {
    Strong {
        key: JSValue,
        value: JSValue,
    },
    Weak {
        key: JSObjectWeakref,
        value: JSValue,
    },
}

impl MapKV {
    pub fn set_value(&mut self, val: JSValue) {
        match self {
            Self::Strong { value, .. } | Self::Weak { value, .. } => {
                *value = val;
            }
        }
    }

    pub fn normalize_key(key: JSValue) -> JSValue {
        // convert -0.0 to +0.0
        if let JSValue::Float64(d) = key {
            if d == 0.0 {
                return JSValue::Int(0);
            }
        }

        key
    }
}

#[derive(Debug)]
pub struct JSMapState {
    /// is WeakSet/WeakMap?
    pub is_weak: bool,

    /// hash registry of MapKV
    pub items: HashMap<u32, Vec<MapKV>>,
}

impl JSMapState {
    fn add_weak_kv(&mut self, ctx: &JSContext, key: &JSObject, value: JSValue) {
        debug_assert!(self.is_weak);
        let hash = JSValue::Object(key.clone()).hash();

        self.items
            .entry(hash)
            .and_modify(|slots| {
                if let Some(i) = slots.iter().position(|kv| {
                    if let MapKV::Weak { key: k, .. } = kv {
                        k.upgrade().map_or(false, |x| x.eq(key))
                    } else {
                        unreachable!()
                    }
                }) {
                    slots[i].set_value(value.clone());
                } else {
                    slots.push(MapKV::Weak {
                        key: key.downgrade(),
                        value: value.clone(),
                    });
                }
            })
            .insert_entry(vec![MapKV::Weak {
                key: key.downgrade(),
                value,
            }]);
    }

    fn add_kv(&mut self, ctx: &JSContext, key: JSValue, value: JSValue) {
        debug_assert!(!self.is_weak);
        let hash = key.hash();

        self.items
            .entry(hash)
            .and_modify(|slots| {
                if let Some(i) = slots.iter().position(|kv| {
                    if let MapKV::Strong { key: k, value } = kv {
                        key.is_same0(k)
                    } else {
                        unreachable!()
                    }
                }) {
                    slots[i].set_value(value.clone());
                } else {
                    slots.push(MapKV::Strong {
                        key: key.clone(),
                        value: value.clone(),
                    });
                }
            })
            .insert_entry(vec![MapKV::Strong { key, value }]);
    }

    pub fn add(&mut self, ctx: &JSContext, key: JSValue, value: JSValue) -> Result<(), i32> {
        if self.is_weak {
            if let JSValue::Object(o) = key {
                self.add_weak_kv(ctx, &o, value);
                Ok(())
            } else {
                ctx.JS_ThrowTypeErrorNotAnObject();
                Err(-1)
            }
        } else {
            self.add_kv(ctx, MapKV::normalize_key(key), value);
            Ok(())
        }
    }

    /// find value by key
    pub fn find(&self, ctx: &JSContext, key: &JSValue) -> Option<JSValue> {
        if self.is_weak {
            if let JSValue::Object(o) = key {
                let hash = key.hash();
                if let Some(slots) = self.items.get(&hash) {
                    slots.iter().find_map(|kv| {
                        if let MapKV::Weak { key: k, value } = kv {
                            if k.upgrade().map_or(false, |k| k.eq(o)) {
                                Some(value.clone())
                            } else {
                                None
                            }
                        } else {
                            unreachable!()
                        }
                    })
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            let key = MapKV::normalize_key(key.clone());
            let hash = key.hash();

            if let Some(slots) = self.items.get(&hash) {
                slots.iter().find_map(|kv| {
                    if let MapKV::Strong { key: k, value } = kv {
                        if key.is_same0(k) {
                            Some(value.clone())
                        } else {
                            None
                        }
                    } else {
                        unreachable!()
                    }
                })
            } else {
                None
            }
        }
    }

    pub fn remove_weak(&mut self, obj: &JSObjectInner) {
        debug_assert!(self.is_weak);
        let hash = JSValue::id_to_hash(obj.id());
        let purge = if let Some(slots) = self.items.get_mut(&hash) {
            slots.retain(|kv| {
                if let MapKV::Weak { key, .. } = kv {
                    key.upgrade().map_or(false, |k| k.id() != obj.id())
                } else {
                    unreachable!()
                }
            });
            slots.is_empty()
        } else {
            false
        };
        if purge {
            self.items.remove(&hash);
        }
    }

    pub fn remove(&mut self, key: &JSValue) {
        let hash = key.hash();
        let purge = if let Some(slots) = self.items.get_mut(&hash) {
            slots.retain(|kv| match kv {
                MapKV::Weak { key: k, .. } => k
                    .upgrade()
                    .map_or(false, |o| !o.eq(unsafe { key.object_unchecked() })),
                MapKV::Strong { key: k, .. } => k != key,
            });
            slots.is_empty()
        } else {
            false
        };
        if purge {
            self.items.remove(&hash);
        }
    }

    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {}

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
        todo!();

        // let s = val.object_unchecked().isa().map_state();
        // if !s.is_null() {
        //     list_foreach(&mut (*s).records, |el| {
        //         let mr = list_entry!(el, JSMapRecord, link);
        //         if !(*s).is_weak {
        //             (*rt).gc_mark_value((*mr).key, mark_func);
        //         }
        //         (*rt).gc_mark_value((*mr).value, mark_func);
        //     });
        // }
    }
}

// XXX: could normalize strings to speed up comparison
#[deprecated(note = "use MapKV::normalize_key() instead")]
pub unsafe fn map_normalize_key(ctx: &JSContext, key: JSValueConst) -> JSValueConst {
    MapKV::normalize_key(key)
}

// XXX: better hash ?
#[deprecated(note = "use JSValue::hash() instead")]
pub unsafe fn map_hash_key(ctx: &JSContext, key: JSValueConst) -> u32 {
    key.hash()
}

#[deprecated(note = "use JSMapState::find() instead")]
pub unsafe fn map_find_record(
    ctx: &JSContext,
    s: *mut JSMapState,
    key: JSValueConst,
) -> *mut MapKV {
    unreachable!("unused");
}

#[deprecated(note = "use JSMapState::add_record() instead")]
pub unsafe fn map_add_record(ctx: &JSContext, s: *mut JSMapState, key: JSValueConst) -> *mut MapKV {
    unreachable!("unused");
}

#[deprecated]
pub unsafe fn map_hash_resize(ctx: &JSContext, s: *mut JSMapState) {
    unreachable!("unused")
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

pub fn js_map_iterator_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {
    unimplemented!();
}

pub unsafe fn js_map_iterator_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
    unimplemented!();
}
