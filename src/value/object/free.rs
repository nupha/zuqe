use crate::{
    gc::{remove_gc_object, JSGCPhaseEnum},
    runtime::JSRuntime,
    types::{list_add_tail, list_del},
    value::{JSTag, JS_FreeValueRT},
    JS_MKPTR,
};

use super::{free_property, js_free_shape, JSObject};

pub unsafe fn free_object(rt: *mut JSRuntime, p: *mut JSObject) {
    /* used to tell the object is invalid when freeing cycles */
    (*p).detailed.set_free_mark(true);

    let sh = (*p).shape_mut().unwrap(); // shape can't be null now

    // free all prop values
    for (prs, mut v) in sh.props.values().zip((*p).prop_values.drain(..)) {
        free_property(rt, &mut v, prs.flags());
    }

    // as an optimization we destroy the shape immediately without putting it in gc_zero_ref_count_list
    js_free_shape(rt, sh);

    // set shape to null means object is freed
    (*p).shape = std::ptr::null_mut();

    if !(*p).first_weak_ref.is_null() {
        reset_weak_ref(rt, p);
    }

    // call class finalizer
    if let Some(cb) = (*rt).class_array[(*p).class_id() as usize].finalizer {
        cb(rt, JS_MKPTR!(JSTag::Object, p.cast()));
    }

    (*(*p).u.func).home_object = std::ptr::null_mut();
    (*p).u.opaque = std::ptr::null_mut();

    remove_gc_object(&mut (*p).header);

    if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES && (*p).header.ref_count != 0 {
        list_add_tail(&mut (*p).header.link, &mut (*rt).gc_zero_ref_count_list);
    } else {
        drop(Box::from_raw(p));
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
