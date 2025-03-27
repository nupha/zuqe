use crate::{
    bytecode::arithmatic::js_same_value_zero,
    context::JSContext,
    list_entry,
    mem::{js_malloc_typed, js_realloc2},
    types::{
        init_list_head, list_add_tail, list_empty, list_foreach, list_foreach_with_ret, list_head,
    },
    value::{
        JSFloat64Union, JSTag, JSValue, JSValueConst, JS_DupValue, JS_FLOAT64_NAN,
        JS_TAG_IS_FLOAT64, JS_VALUE_GET_FLOAT64, JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG,
        JS_VALUE_GET_OBJ, JS_VALUE_GET_PTR, JS_VALUE_GET_STRING, JS_VALUE_GET_TAG,
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
    pub is_weak: bool,      // TRUE if WeakSet/WeakMap
    pub records: list_head, // list of JSMapRecord.link
    pub record_count: u32,
    pub hash_table: *mut list_head,
    pub hash_size: u32,              // must be a power of two
    pub record_count_threshold: u32, // count at which a hash table resize is needed
}

impl Drop for JSMapState {
    fn drop(&mut self) {
        debug_assert!(list_empty(&mut self.records));
        debug_assert!(list_empty(self.hash_table));
    }
}

impl JSMapState {
    /// Create a new record in the map.
    pub fn add_record(
        &mut self,
        ctx: &mut JSContext,
        key: JSValueConst,
    ) -> Result<Box<JSMapRecord>, i32> {
        // new record
        let mut mr = unsafe {
            Box::from_raw(
                js_malloc_typed::<JSMapRecord>(ctx, 1, true)
                    .ok_or(-1)?
                    .as_ptr(),
            )
        };

        mr.ref_count = 1;
        mr.map = self as *mut _;
        mr.empty = false;
        mr.key = unsafe {
            if self.is_weak {
                let p = JS_VALUE_GET_OBJ(key);
                // Add the weak reference
                mr.next_weak_ref = (*p).first_weak_ref;
                (*p).first_weak_ref = mr.as_mut() as *mut _;
                key
            } else {
                JS_DupValue(ctx, key)
            }
        };

        unsafe {
            // insert to state hash table
            let h = map_hash_key(ctx, key) & (self.hash_size - 1);
            list_add_tail(&mut (*mr).hash_link, self.hash_table.offset(h as isize));
            list_add_tail(&mut (*mr).link, &mut self.records);

            self.record_count += 1;
        }

        if self.record_count >= self.record_count_threshold {
            unsafe {
                map_hash_resize(ctx, self);
            }
        }

        Ok(mr)
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
pub unsafe fn map_hash_key(ctx: *mut JSContext, key: JSValueConst) -> u32 {
    let tag = JS_VALUE_GET_NORM_TAG(key);
    let h: u32;

    match tag {
        JSTag::Bool => {
            h = JS_VALUE_GET_INT(key) as u32;
        }
        JSTag::String => {
            h = (*JS_VALUE_GET_STRING(key)).content().make_hash(0);
        }
        JSTag::Object | JSTag::Symbol => {
            h = (JS_VALUE_GET_PTR(key) as usize * 3163) as u32;
        }
        JSTag::Int => {
            let d = (JS_VALUE_GET_INT(key) as f64) * 3163.0;
            let u = JSFloat64Union { d };
            h = (u.u32[0] ^ u.u32[1]) * 3163;
        }
        JSTag::Float64 => {
            let mut d = JS_VALUE_GET_FLOAT64(key);
            // normalize the NaN
            if d.is_nan() {
                d = JS_FLOAT64_NAN;
            }
            let u = JSFloat64Union { d };
            h = (u.u32[0] ^ u.u32[1]) * 3163;
        }
        _ => {
            h = 0; // XXX: bignum support
        }
    }

    h ^ (tag as u32) // XOR
}

pub unsafe fn map_find_record(
    ctx: *mut JSContext,
    s: *mut JSMapState,
    key: JSValueConst,
) -> *mut JSMapRecord {
    let h = map_hash_key(ctx, key) & ((*s).hash_size - 1);

    // list_for_each(el, &(*s).hash_table[h as usize]) {
    //     mr = list_entry(el, JSMapRecord, hash_link);
    //     if js_same_value_zero(ctx, (*mr).key, key) {
    //         return mr;
    //     }
    // }
    // std::ptr::null_mut()

    list_foreach_with_ret((*s).hash_table.add(h as usize), |el| {
        let mr = list_entry!(el, JSMapRecord, hash_link);
        if js_same_value_zero(ctx, (*mr).key, key) {
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
        .map_or(std::ptr::null_mut(), Box::into_raw)
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
            let h = map_hash_key(ctx, (*mr).key) & (new_hash_size - 1);
            list_add_tail(&mut (*mr).hash_link, new_hash_table.offset(h as isize));
        }
    });

    (*s).hash_table = new_hash_table;
    (*s).hash_size = new_hash_size;
    (*s).record_count_threshold = new_hash_size * 2;
}
