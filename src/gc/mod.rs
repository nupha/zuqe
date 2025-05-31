mod gc;
mod gcobj;
mod mark;
mod scan;

pub use {gc::*, gcobj::*, mark::*};

//
// LEGACY FUNCTIONS (TO BE REMOVED)
//

/// Adds a new GC object to the garbage collector's tracking list
/// Initializes the object's mark and type, then adds it to the GC object list
#[deprecated]
pub unsafe fn add_gc_object(
    rt: *mut crate::runtime::JSRuntime,
    h: *mut JSGCObjectHeader,
    tp: JSGCObjectTypeEnum,
) {
    // // Initialize the object's mark and type
    // (*h).set_gc_mark(0);
    // (*h).set_gc_obj_type(tp);

    // // Add the object to the end of the GC object list
    // list_add_tail(&mut (*h).link, &mut (*rt).gc_obj_list);
}

/// Function type for marking objects during garbage collection
/// Parameters:
/// - rt: Pointer to the JavaScript runtime
/// - gp: Pointer to the garbage collected object header
#[deprecated]
pub type JS_MarkFunc = unsafe fn(rt: *mut crate::runtime::JSRuntime, gp: *mut JSGCObjectHeader);

/// Marks a JavaScript value during garbage collection
/// Only marks values that have reference counts (objects and function bytecode)
#[deprecated]
pub unsafe fn JS_MarkValue(
    rt: *mut crate::runtime::JSRuntime,
    val: crate::value::JSValue,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

/// Marks the realm associated with an auto-initialized property
/// This ensures the realm is preserved during garbage collection
#[deprecated]
pub unsafe fn js_autoinit_mark(
    rt: *mut crate::runtime::JSRuntime,
    pr: *mut crate::value::object::JSProperty,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

/// Recursively marks all child objects of a GC object
/// This ensures all referenced objects are preserved during garbage collection
#[deprecated]
pub(super) unsafe fn mark_children(
    rt: *mut crate::runtime::JSRuntime,
    gp: *mut JSGCObjectHeader,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

#[deprecated]
pub unsafe fn JS_MarkContext(
    rt: &mut crate::runtime::JSRuntime,
    ctx: &mut crate::context::JSContext,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

#[deprecated]
pub unsafe fn js_mark_module_def(
    rt: *mut crate::runtime::JSRuntime,
    m: &mut crate::module::JSModuleDef,
    mark_func: JS_MarkFunc,
) {
    unreachable!("unused");
}

/// Preserves objects with positive reference counts and their child objects.
/// Identifies objects with zero reference count in the GC object list
/// and moves them to a temporary list for removal.
#[deprecated]
pub unsafe fn gc_scan(rt: &mut crate::runtime::JSRuntime) {
    unreachable!("unused");
}

/// Frees objects in the temporary list, handling cyclic references
#[deprecated]
pub unsafe fn gc_free_cycles(rt: &mut crate::runtime::JSRuntime) {
    unreachable!("unused");
}

/// Frees a GC object based on its type
/// Handles different types of GC objects with their specific cleanup logic
#[deprecated(note = "use gc_free() instead")]
pub unsafe fn free_gc_object(rt: *mut crate::runtime::JSRuntime, gp: *mut JSGCObjectHeader) {
    unreachable!("unused");
}

/// Removes an object from either the main GC object list or temporary list
#[deprecated(note = "use set status to Zeroed instead")]
pub unsafe fn remove_gc_object(h: *mut JSGCObjectHeader) {
    unreachable!("unused");
}

/// Frees objects with zero reference count (in gc_zero_ref_count_list chain).
#[deprecated(note = "use JSRuntime::gc_free_zeroed() instead")]
pub unsafe fn free_zero_refcount(rt: *mut crate::runtime::JSRuntime) {
    (*rt).gc_free_zeroed();
}

/// Decrements the reference count of a child GC object
/// Moves the object to the temporary list if its refcount reaches zero
#[deprecated]
pub unsafe fn gc_decref_child(rt: *mut crate::runtime::JSRuntime, p: *mut JSGCObjectHeader) {
    unreachable!("unsed");
}

/// Decreases reference counts for all child objects in the GC object list.
/// Moves objects with zero reference count to a temporary list for cleanup.
#[deprecated(note = "use JSRuntime::gc_deref() instead")]
pub unsafe fn gc_decref(rt: &mut crate::runtime::JSRuntime) {
    unreachable!("unsed");
}

#[deprecated(note = "use GCObject::gc_dump() instead")]
pub unsafe fn JS_DumpGCObject(rt: *mut crate::runtime::JSRuntime, p: *mut JSGCObjectHeader) {
    unreachable!("unused");
}

#[deprecated(note = "use JSRuntime::gc_dump_objects() instead")]
pub unsafe fn JS_DumpGCObjects(rt: *mut crate::runtime::JSRuntime) {
    unreachable!("unused");
}
