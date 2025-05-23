use std::mem::ManuallyDrop;

use crate::{
    gc::{remove_gc_object, JSGCPhaseEnum},
    runtime::JSRuntime,
    types::{list_add_tail, list_del},
    value::{JSTag, JS_FreeValueRT},
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_FLOAT64_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{free_property, JSObject, JSShape};

impl JSObject {
    /// Safely frees a GC object, releasing all associated resources.
    /// This includes:
    /// - Marking the object as invalid to prevent use during freeing cycles
    /// - Freeing all property values
    /// - Destroying the object's shape immediately
    /// - Resetting weak references
    /// - Calling the class finalizer if defined
    /// - Handling object memory cleanup based on GC phase
    /// The object must be in either tmp_obj_list or gc_zero_ref_count_list chain.
    pub unsafe fn free(rt: &mut JSRuntime, p: *mut Self) {
        // used to tell the object is invalid when freeing cycles
        (*p).detailed.set_free_mark(true);

        // shape can't be null
        let sh = (*p).shape_mut().unwrap();

        // free all prop values
        for (prs, mut v) in sh.props.values().zip((*p).prop_values.drain(..)) {
            free_property(rt, &mut v, prs.flags());
        }

        // as an optimization we destroy the shape immediately without putting it in gc_zero_ref_count_list
        JSShape::free(rt, sh.into());

        // set shape to null means object is freed
        (*p).shape = std::ptr::null_mut();

        if !(*p).first_weak_ref.is_null() {
            reset_weak_ref(rt, p);
        }

        // call class finalizer
        if let Some(cb) = rt.class_array[(*p).class_id() as usize].finalizer {
            cb(rt, JS_MKPTR!(JSTag::Object, p.cast()));
        }

        // ManuallyDrop union
        match (*p).class_id() {
            JS_CLASS_BYTECODE_FUNCTION => {
                // (*(*p).u.func).home_object = std::ptr::null_mut();
                // (*(*p).u.func).var_refs.clear();
                // ManuallyDrop::drop(&mut (*p).u.func);
            }
            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                // ManuallyDrop::drop(&mut (*(*p).u.array).data.values);
                // ManuallyDrop::drop(&mut (*p).u.array);
            }
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                // ManuallyDrop::drop(&mut (*p).u.array);
            }
            _ => {
                // (*p).u.opaque = std::ptr::null_mut();
            }
        }

        // fail safe
        (*p).detailed.set_class_id(0);

        remove_gc_object(&mut (*p).header);

        if rt.gc_phase == JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES && (*p).header.ref_count != 0 {
            // this means we are in free cycles process,
            // where tmp_obj_list object not always have ref_count==0, it could be 1 sometimes.
            // in this case, we put this object in `gc_zero_ref_count_list` to free object attached payload.
            // see `gc_free_cycles()` bottom half for detail.
            list_add_tail(&mut (*p).header.link, &mut rt.gc_zero_ref_count_list);
        } else {
            drop(Box::from_raw(p));
        }
    }
}

pub unsafe fn reset_weak_ref(rt: *mut JSRuntime, p: *mut JSObject) {
    // first pass to remove the records from the WeakMap/WeakSet lists
    let mut mr = (*p).first_weak_ref;
    debug_assert!(mr.is_aligned(), "{mr:p}");
    while !mr.is_null() {
        let s = (*mr).map;
        debug_assert!((*s).is_weak);
        debug_assert!(!(*mr).empty); // no iterator on WeakMap/WeakSet

        list_del(std::ptr::addr_of_mut!((*mr).hash_link));
        list_del(std::ptr::addr_of_mut!((*mr).link));
        mr = (*mr).next_weak_ref;
    }

    // second pass to free the values
    let mut mr = (*p).first_weak_ref;
    while !mr.is_null() {
        let mr_next = (*mr).next_weak_ref;

        JS_FreeValueRT(rt, (*mr).value);
        drop(Box::from_raw(mr));

        mr = mr_next;
    }

    (*p).first_weak_ref = std::ptr::null_mut();
}
