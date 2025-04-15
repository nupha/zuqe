use std::os::raw::{c_int, c_void};

// 从quickjs.h转换的宏定义
#[cfg(target_pointer_width = "64")]
pub const JS_PTR64: () = ();

#[inline(always)]
pub fn likely(x: bool) -> bool {
    x
}

#[inline(always)]
pub fn unlikely(x: bool) -> bool {
    x
}

#[derive(Debug)]
#[repr(C)]
pub struct JSRefCountHeader {
    pub ref_count: c_int,
}

#[repr(C)]
pub struct JSSharedArrayBufferFunctions {
    pub sab_alloc: Option<unsafe fn(*mut c_void, usize) -> *mut c_void>,
    pub sab_free: Option<unsafe fn(*mut c_void, *mut c_void)>,
    pub sab_dup: Option<unsafe fn(*mut c_void, *mut c_void)>,
    pub sab_opaque: *mut c_void,
}

impl JSSharedArrayBufferFunctions {
    pub fn new() -> Self {
        Self {
            sab_alloc: None,
            sab_free: None,
            sab_dup: None,
            sab_opaque: std::ptr::null_mut(),
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct list_head {
    pub prev: *mut Self,
    pub next: *mut Self,
}

impl Default for list_head {
    fn default() -> Self {
        Self {
            prev: std::ptr::null_mut(),
            next: std::ptr::null_mut(),
        }
    }
}

#[inline(always)]
pub unsafe fn init_list_head(head: *mut list_head) {
    debug_assert!(head.is_aligned());

    (*head).prev = head;
    (*head).next = head;
}

#[inline(always)]
pub unsafe fn list_del(el: *mut list_head) {
    let prev = (*el).prev;
    let next = (*el).next;
    debug_assert!(!prev.is_null());
    debug_assert!(!next.is_null());

    (*prev).next = next;
    (*next).prev = prev;

    (*el).prev = std::ptr::null_mut();
    (*el).next = std::ptr::null_mut();
}

/* insert 'el' between 'prev' and 'next' */
#[inline(always)]
unsafe fn __list_add(el: *mut list_head, prev: *mut list_head, next: *mut list_head) {
    (*prev).next = el;
    (*el).prev = prev;
    (*el).next = next;
    (*next).prev = el;
}

/* add 'el' at the head of the list 'head' (= after element head) */
#[inline(always)]
pub unsafe fn list_add(el: *mut list_head, head: *mut list_head) {
    debug_assert!(!head.is_null());
    __list_add(el, head, (*head).next);
}

/* add 'el' at the end of the list 'head' (= before element head) */
#[inline]
pub unsafe fn list_add_tail(el: *mut list_head, head: *mut list_head) {
    debug_assert!(!(*head).prev.is_null());
    __list_add(el, (*head).prev, head);
}

pub unsafe fn list_find(el: *mut list_head, head: *mut list_head) -> bool {
    let mut e = (*head).next;
    while e != head {
        if e == el {
            return true;
        }
        e = (*e).next;
    }
    false
}

pub unsafe fn list_count(head: *const list_head) -> usize {
    let mut e = (*head).next;
    let mut cnt = 0;
    while e as usize != head as usize {
        cnt += 1;
        e = (*e).next;
    }
    cnt
}

#[inline(always)]
pub fn list_empty(el: *const list_head) -> bool {
    unsafe { (*el).next as *const list_head == el }
}

pub unsafe fn list_foreach(head: *mut list_head, mut f: impl FnMut(*mut list_head)) {
    let mut el = (*head).next;
    while el != head {
        f(el);
        el = (*el).next;
    }
}

pub unsafe fn list_foreach_safe(head: *mut list_head, mut f: impl FnMut(*mut list_head)) {
    let mut el = (*head).next;
    let mut el1 = (*el).next;
    while el != head {
        f(el);

        el = el1;
        el1 = (*el).next;
    }
}

pub unsafe fn list_foreach_with_ret<R>(
    head: *mut list_head,
    mut f: impl FnMut(*mut list_head) -> Option<R>,
) -> Option<R> {
    let mut el = (*head).next;
    while el != head {
        if let Some(r) = f(el) {
            return Some(r);
        };
        el = (*el).next;
    }

    None
}

pub unsafe fn list_foreach_safe_with_ret<R>(
    head: *mut list_head,
    mut f: impl FnMut(*mut list_head) -> Option<R>,
) -> Option<R> {
    let mut el = (*head).next;
    let mut el1 = (*el).next;
    while el != head {
        if let Some(r) = f(el) {
            return Some(r);
        };

        el = el1;
        el1 = (*el).next;
    }

    None
}

/* return the pointer of type 'type *' containing 'el' as field 'member' */
#[macro_export]
macro_rules! list_entry {
    ($el:expr, $ty:ident, $member:ident) => {
        $el.cast::<u8>()
            .sub(std::mem::offset_of!($ty, $member))
            .cast::<$ty>()
    };
}

// 类似 C 的 container_of 实现
#[macro_export]
macro_rules! container_of {
    ($ptr:expr, $container:ty, $field:ident) => {
        ($ptr
            .cast::<u8>()
            .sub(std::mem::offset_of!($container, $field)))
        .cast::<$container>()
    };
}
