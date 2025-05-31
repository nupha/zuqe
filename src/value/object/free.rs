use crate::{runtime::JSRuntime, utils::list_del, value::JS_FreeValueRT};

use super::JSObject;

// impl JSObject {
//     /// Safely frees a JSObject, releasing all associated resources.
//     /// This includes:
//     /// - Marking the object as invalid to prevent use during freeing cycles
//     /// - Freeing all property values
//     /// - Destroying the object's shape immediately
//     /// - Resetting weak references
//     /// - Calling the class finalizer if defined
//     /// - Handling object memory cleanup based on GC phase
//     /// The object must be in either tmp_obj_list or gc_zero_ref_count_list chain.
//     pub unsafe fn dispose(rt: &mut JSRuntime, mut p: NonNull<Self>) -> Option<NonNull<Self>> {
//         p.as_mut().before_drop_clean_up(rt);

//         if rt.gc_phase == JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES && p.as_ref().ref_count() != 0 {
//             // we are in free cycles process,
//             // where Temp status object does not always have ref_count==0, it could be 1 sometimes.
//             // in this case, we put this object in Zeroed state to let it be freed in later
//             // gc_free_cycles() call.
//             p.as_mut().gc_set_status(GCStatus::Zeroed);
//             Some(p)
//         } else {
//             drop(Box::from_raw(p.as_ptr()));
//             None
//         }
//     }
// }

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
