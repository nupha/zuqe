use crate::{runtime::RuntimeImpl, value::JSValue};

use super::ObjectRef;

#[deprecated(note = "unused")]
pub unsafe fn reset_weak_ref(rt: *mut RuntimeImpl, p: *mut JSValue) {
    unreachable!("unused")
    // // first pass to remove the records from the WeakMap/WeakSet lists
    // let mut mr = (*p).first_weak_ref;
    // debug_assert!(mr.is_aligned(), "{mr:p}");
    // while !mr.is_null() {
    //     let s = (*mr).map;
    //     debug_assert!((*s).is_weak);
    //     debug_assert!(!(*mr).empty); // no iterator on WeakMap/WeakSet

    //     list_del(std::ptr::addr_of_mut!((*mr).hash_link));
    //     list_del(std::ptr::addr_of_mut!((*mr).link));
    //     mr = (*mr).next_weak_ref;
    // }

    // // second pass to free the values
    // let mut mr = (*p).first_weak_ref;
    // while !mr.is_null() {
    //     let mr_next = (*mr).next_weak_ref;

    //     JS_FreeValueRT(rt, (*mr).value);
    //     drop(Box::from_raw(mr));

    //     mr = mr_next;
    // }

    // (*p).first_weak_ref = std::ptr::null_mut();
}
