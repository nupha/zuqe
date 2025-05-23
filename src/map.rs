use std::ptr::NonNull;

use crate::{
    context::JSContext,
    gc::{JS_MarkFunc, JS_MarkValue},
    list_entry,
    mem::{js_free_rt, js_malloc_typed, js_realloc2},
    runtime::JSRuntime,
    types::{
        init_list_head, list_add_tail, list_empty, list_foreach, list_foreach_safe,
        list_foreach_with_ret, list_head,
    },
    value::{
        JSValue, JSValueConst, JS_FreeValueRT, JS_TAG_IS_FLOAT64, JS_VALUE_GET_FLOAT64,
        JS_VALUE_GET_TAG,
    },
};

#[repr(C)]
pub struct JSMapRecord {
    pub ref_count: i32, // used during enumeration to avoid freeing the record
    pub empty: bool,    // TRUE if the record is deleted
    pub map: *mut JSMapState,
    pub next_weak_ref: *mut JSMapRecord,
    pub link: list_head,
    pub hash_link: list_head,

    pub key: JSValue,
    pub value: JSValue,
}

#[cfg(debug_assertions)]
impl Drop for JSMapRecord {
    fn drop(&mut self) {
        println!("JSMapRecord::drop({self:p})");

        debug_assert_eq!(self.ref_count, 0);
        debug_assert!(self.map.is_null());
        debug_assert!(self.next_weak_ref.is_null());
        debug_assert!(list_empty(&mut self.link));
        debug_assert!(list_empty(&mut self.hash_link));
    }
}

#[repr(C)]
pub struct JSMapState {
    /// is WeakSet/WeakMap?
    pub is_weak: bool,
    /// list of JSMapRecord.link
    pub records: list_head,
    pub record_count: u32,

    pub hash_table: *mut list_head,
    pub hash_size: u32, // must be a power of two

    // count at which a hash table resize is needed
    pub record_count_threshold: u32,
}

#[cfg(debug_assertions)]
impl Drop for JSMapState {
    fn drop(&mut self) {
        debug_assert!(list_empty(&mut self.records));
        debug_assert!(list_empty(self.hash_table));
    }
}

impl JSMapState {
    /// Create a new record in the map. New record is registered to hash table.
    /// Record is freed by gc, don't free it manually.
    pub fn add_record(
        &mut self,
        ctx: &mut JSContext,
        key: JSValue,
    ) -> Result<NonNull<JSMapRecord>, i32> {
        // new record
        let mut rec = unsafe {
            Box::from_raw(
                js_malloc_typed::<JSMapRecord>(ctx, 1, true)
                    .ok_or(-1)?
                    .as_ptr(),
            )
        };

        debug_assert!((self as *mut Self).is_aligned());

        rec.ref_count = 1;
        rec.map = self as *mut _;
        rec.empty = false;
        rec.key = unsafe {
            if self.is_weak {
                let p = key.object_unchecked();
                // Add the weak reference
                debug_assert!(p.first_weak_ref.is_aligned());
                rec.next_weak_ref = p.first_weak_ref;
                p.first_weak_ref = rec.as_mut() as *mut _;
                key
            } else {
                key.dup()
            }
        };

        unsafe {
            // insert to state hash table
            let h = key.hash() & (self.hash_size - 1);
            list_add_tail(&mut (*rec).hash_link, self.hash_table.offset(h as isize));
            list_add_tail(&mut (*rec).link, &mut self.records);

            self.record_count += 1;

            if self.record_count >= self.record_count_threshold {
                map_hash_resize(ctx, self);
            }
        }

        Ok(unsafe { NonNull::new_unchecked(Box::into_raw(rec)) })
    }

    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        let s = val
            .object_unchecked()
            .isa_mut()
            .map_state_replace(std::ptr::null_mut())
            .unwrap();

        if !s.is_null() {
            // if the object is deleted we are sure that no iterator is using it
            list_foreach_safe(&mut (*s).records, |el| {
                let mr = list_entry!(el, JSMapRecord, link);
                if !(*mr).empty {
                    if (*s).is_weak {
                        delete_weak_ref(mr);
                    } else {
                        JS_FreeValueRT(rt, (*mr).key);
                    }
                    JS_FreeValueRT(rt, (*mr).value);
                }

                drop(Box::from_raw(mr));
            });

            js_free_rt(rt, (*s).hash_table.cast());
            drop(Box::from_raw(s));
        }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
        let s = val.object_unchecked().isa().map_state_ptr();
        if !s.is_null() {
            list_foreach(&mut (*s).records, |el| {
                let mr = list_entry!(el, JSMapRecord, link);
                if !(*s).is_weak {
                    JS_MarkValue(rt, (*mr).key, mark_func);
                }
                JS_MarkValue(rt, (*mr).value, mark_func);
            });
        }
    }
}

// XXX: could normalize strings to speed up comparison
pub unsafe fn map_normalize_key(ctx: *mut JSContext, key: JSValueConst) -> JSValueConst {
    // convert -0.0 to +0.0
    if JS_TAG_IS_FLOAT64(JS_VALUE_GET_TAG(key)) && JS_VALUE_GET_FLOAT64(key) == 0.0 {
        JSValue::ZERO
    } else {
        key
    }
}

// XXX: better hash ?
#[deprecated(note = "use JSValue::hash() instead")]
pub unsafe fn map_hash_key(ctx: *const JSContext, key: JSValueConst) -> u32 {
    key.hash()
}

pub unsafe fn map_find_record(
    ctx: *mut JSContext,
    s: *mut JSMapState,
    key: JSValueConst,
) -> *mut JSMapRecord {
    let h = key.hash() & ((*s).hash_size - 1);

    list_foreach_with_ret((*s).hash_table.add(h as usize), |el| {
        let mr = list_entry!(el, JSMapRecord, hash_link);
        if (*mr).key.is_same0(key) {
            Some(mr) // pre exit with a value
        } else {
            None
        }
    })
    .unwrap_or(std::ptr::null_mut())
}

#[deprecated(note = "use JSMapState::add_record() instead")]
pub unsafe fn map_add_record(
    ctx: *mut JSContext,
    s: *mut JSMapState,
    key: JSValueConst,
) -> *mut JSMapRecord {
    // handle out ownership of new JSMapRecord, returns raw pointer
    s.as_mut()
        .unwrap()
        .add_record(ctx.as_mut().unwrap(), key)
        .map_or(std::ptr::null_mut(), NonNull::as_ptr)
}

pub unsafe fn map_hash_resize(ctx: *mut JSContext, s: *mut JSMapState) {
    // XXX: no reporting of memory allocation failure
    let mut new_hash_size = if (*s).hash_size == 1 {
        4
    } else {
        (*s).hash_size * 2
    };
    let mut slack: usize = 0;
    let new_hash_table = js_realloc2(
        ctx,
        (*s).hash_table.cast(),
        std::mem::size_of::<*mut list_head>() * new_hash_size as usize,
        &mut slack,
    )
    .cast::<list_head>();
    if new_hash_table.is_null() {
        return;
    }
    new_hash_size += (slack / std::mem::size_of::<*mut list_head>()) as u32;

    for i in 0..new_hash_size {
        init_list_head(new_hash_table.offset(i as isize));
    }

    list_foreach(&mut (*s).records, |el| {
        let mr = list_entry!(el, JSMapRecord, link);
        if !(*mr).empty {
            let h = (*mr).key.hash() & (new_hash_size - 1);
            list_add_tail(&mut (*mr).hash_link, new_hash_table.offset(h as isize));
        }
    });

    (*s).hash_table = new_hash_table;
    (*s).hash_size = new_hash_size;
    (*s).record_count_threshold = new_hash_size * 2;
}

/// Remove the weak reference from the object weak reference list.
/// we don't use a doubly linked list to save space,
/// assuming a given object has few weak references to it
pub unsafe fn delete_weak_ref(mr: *mut JSMapRecord) {
    let mut pmr = std::ptr::addr_of_mut!((*mr).key.object_unchecked().first_weak_ref);
    let mut mr1: *mut JSMapRecord;

    loop {
        mr1 = *pmr;
        debug_assert!(!mr1.is_null());

        if mr1 == mr {
            break;
        }
        pmr = std::ptr::addr_of_mut!((*mr1).next_weak_ref);
    }

    *pmr = (*mr1).next_weak_ref;
}

#[deprecated(note = "use JSMapState::hook_finalizer() instead")]
pub unsafe fn js_map_finalizer(rt: *mut JSRuntime, val: JSValue) {
    JSMapState::hook_finalizer(rt, val);
}

#[deprecated(note = "use JSMapState::hook_mark() instead")]
pub unsafe fn js_map_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    JSMapState::hook_mark(rt, val, mark_func);
}

pub unsafe fn js_map_iterator_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unimplemented!();
}

pub unsafe fn js_map_iterator_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    unimplemented!();
}
