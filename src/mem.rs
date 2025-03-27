use std::{
    ffi::{c_char, c_void},
    ptr::NonNull,
};

use crate::{context::JSContext, gc::JS_RunGC, runtime::JSRuntime, types::unlikely};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub const MALLOC_OVERHEAD: usize = 0;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub const MALLOC_OVERHEAD: usize = 8;

pub const JS_DEFAULT_STACK_SIZE: usize = 1024 * 1024; // 1MB default stack size

#[derive(Debug)]
#[repr(C)]
pub struct JSMallocState {
    pub malloc_count: usize,
    pub malloc_size: usize,
    pub malloc_limit: usize,
    pub opaque: *mut c_void,
}

impl Default for JSMallocState {
    fn default() -> Self {
        Self {
            malloc_count: 0,
            malloc_size: 0,
            malloc_limit: 0,
            opaque: std::ptr::null_mut(),
        }
    }
}

#[derive(Copy, Clone)]
#[repr(C)]
pub struct JSMallocFunctions {
    pub js_malloc: unsafe fn(*mut JSMallocState, usize) -> *mut c_void,
    pub js_free: unsafe fn(*mut JSMallocState, *mut c_void),
    pub js_realloc: unsafe fn(*mut JSMallocState, *mut c_void, usize) -> *mut c_void,
    pub js_malloc_usable_size: Option<unsafe fn(*const c_void) -> usize>,
}

pub unsafe fn js_def_malloc(s: *mut JSMallocState, nbytes: usize) -> *mut c_void {
    // Do not allocate zero bytes: behavior is platform dependent
    assert!(nbytes != 0);

    #[cfg(target_pointer_width = "64")]
    const ADDR_ALIGN: usize = 8;
    #[cfg(not(target_pointer_width = "64"))]
    const ADDR_ALIGN: usize = 4;

    let ly = std::alloc::Layout::from_size_align(nbytes, ADDR_ALIGN).unwrap();
    let size = ly.size();

    if unlikely((*s).malloc_size + size > (*s).malloc_limit) {
        return std::ptr::null_mut();
    }

    // let ptr = libc::malloc(size);
    // if ptr.is_null() {
    //     return std::ptr::null_mut();
    // }

    let mut ptr = std::ptr::null_mut::<c_void>();
    if libc::posix_memalign(std::ptr::addr_of_mut!(ptr), ly.align(), size) != 0 {
        return std::ptr::null_mut();
    }
    assert!(!ptr.is_null());
    debug_assert!(ptr.is_aligned());

    (*s).malloc_count += 1;
    (*s).malloc_size += js_def_malloc_usable_size(ptr); //+ MALLOC_OVERHEAD;

    ptr
}

pub unsafe fn js_def_free(s: *mut JSMallocState, ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }

    (*s).malloc_count -= 1;
    (*s).malloc_size -= js_def_malloc_usable_size(ptr) + MALLOC_OVERHEAD;

    libc::free(ptr);
}

pub unsafe fn js_def_realloc(s: *mut JSMallocState, ptr: *mut c_void, size: usize) -> *mut c_void {
    if ptr.is_null() {
        if size == 0 {
            return std::ptr::null_mut();
        }
        return js_def_malloc(s, size);
    }

    let old_size = js_def_malloc_usable_size(ptr);
    if size == 0 {
        (*s).malloc_count -= 1;
        (*s).malloc_size -= old_size + MALLOC_OVERHEAD;
        libc::free(ptr.cast());
        return std::ptr::null_mut();
    }
    if (*s).malloc_size + size - old_size > (*s).malloc_limit {
        return std::ptr::null_mut();
    }

    let new_ptr = libc::realloc(ptr as *mut libc::c_void, size);
    if new_ptr.is_null() {
        return std::ptr::null_mut();
    }

    (*s).malloc_size += js_def_malloc_usable_size(new_ptr) - old_size;
    new_ptr
}

pub unsafe fn js_malloc_usable_size_unknown(ptr: *const c_void) -> usize {
    0
}

/* default memory allocation functions with memory limitation */
pub unsafe fn js_def_malloc_usable_size(ptr: *const c_void) -> usize {
    // #if defined(__APPLE__)
    //     return malloc_size(ptr);
    // #elif defined(_WIN32)
    //     return _msize((void *)ptr);
    // #elif defined(EMSCRIPTEN)
    //     return 0;
    // #elif defined(__linux__)
    //     return malloc_usable_size((void *)ptr);
    // #else
    //     /* change this to `return 0;` if compilation fails */
    //     return malloc_usable_size((void *)ptr);
    // #endif

    0
}

pub const JS_DEFAULT_MALLOC_FUNCTIONS: JSMallocFunctions = JSMallocFunctions {
    js_malloc: js_def_malloc,
    js_free: js_def_free,
    js_realloc: js_def_realloc,
    js_malloc_usable_size: Some(js_def_malloc_usable_size),
};

#[repr(C)]
pub struct JSMemoryUsage {
    pub malloc_size: i64,
    pub malloc_limit: i64,
    pub memory_used_size: i64,
    pub malloc_count: i64,
    pub memory_used_count: i64,
    pub atom_count: i64,
    pub atom_size: i64,
    pub str_count: i64,
    pub str_size: i64,
    pub obj_count: i64,
    pub obj_size: i64,
    pub prop_count: i64,
    pub prop_size: i64,
    pub shape_count: i64,
    pub shape_size: i64,
    pub js_func_count: i64,
    pub js_func_size: i64,
    pub js_func_code_size: i64,
    pub js_func_pc2line_count: i64,
    pub js_func_pc2line_size: i64,
    pub c_func_count: i64,
    pub array_count: i64,
    pub fast_array_count: i64,
    pub fast_array_elements: i64,
    pub binary_object_count: i64,
    pub binary_object_size: i64,
}

pub unsafe fn js_trigger_gc(rt: *mut JSRuntime, size: usize) {
    let force_gc: bool;

    #[cfg(feature = "FORCE_GC_AT_MALLOC")]
    {
        force_gc = TRUE;
    }
    #[cfg(not(feature = "FORCE_GC_AT_MALLOC"))]
    {
        force_gc = ((*rt).malloc_state.malloc_size + size) > (*rt).malloc_gc_threshold;
    }

    if force_gc {
        #[cfg(feature = "DUMP_GC")]
        {
            println!("GC: size={}", (*rt).malloc_state.malloc_size);
        }
        JS_RunGC(rt);
        (*rt).malloc_gc_threshold =
            (*rt).malloc_state.malloc_size + ((*rt).malloc_state.malloc_size >> 1);
    }
}

pub unsafe fn js_malloc_rt(rt: *mut JSRuntime, nbytes: usize) -> *mut c_void {
    ((*rt).mf.js_malloc)(&mut (*rt).malloc_state, nbytes)
}

pub unsafe fn js_free_rt(rt: *mut JSRuntime, ptr: *mut c_void) {
    ((*rt).mf.js_free)(&mut (*rt).malloc_state, ptr);
}

pub unsafe fn js_realloc_rt(rt: *mut JSRuntime, ptr: *mut c_void, nbytes: usize) -> *mut c_void {
    ((*rt).mf.js_realloc)(&mut (*rt).malloc_state, ptr, nbytes)
}

pub unsafe fn js_malloc_usable_size_rt(rt: *mut JSRuntime, ptr: *const c_void) -> usize {
    (((*rt).mf.js_malloc_usable_size).unwrap())(ptr)
}

pub unsafe fn js_mallocz_rt(rt: *mut JSRuntime, nytes: usize) -> *mut c_void {
    let ptr = js_malloc_rt(rt, nytes);
    if !ptr.is_null() {
        std::ptr::write_bytes(ptr, 0, nytes);
    }
    ptr
}

/* Throw out of memory in case of error */
pub unsafe fn js_malloc(ctx: *mut JSContext, nbytes: usize) -> *mut c_void {
    let ptr = js_malloc_rt((*ctx).rt_mut(), nbytes);
    if unlikely(ptr.is_null()) {
        ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
        std::ptr::null_mut()
    } else {
        ptr
    }
}

pub unsafe fn js_mallocz(ctx: *mut JSContext, size: usize) -> *mut c_void {
    let ptr = js_mallocz_rt((*ctx).rt_mut(), size);
    if unlikely(ptr.is_null()) {
        ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
        return std::ptr::null_mut();
    }
    ptr
}

pub unsafe fn js_free(ctx: *mut JSContext, ptr: *mut c_void) {
    js_free_rt((*ctx).rt_mut(), ptr);
}

pub unsafe fn js_realloc(ctx: *mut JSContext, ptr: *mut c_void, size: usize) -> *mut c_void {
    let ret = js_realloc_rt((*ctx).rt_mut(), ptr, size);
    if unlikely(ret.is_null() && size != 0) {
        ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
        return std::ptr::null_mut();
    }
    ret
}

pub unsafe fn js_realloc2(
    ctx: *mut JSContext,
    ptr: *mut c_void,
    size: usize,
    pslack: *mut usize,
) -> *mut c_void {
    let ret = js_realloc_rt((*ctx).rt_mut(), ptr, size);
    if unlikely(ret.is_null() && size != 0) {
        ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
        return std::ptr::null_mut();
    }
    if !pslack.is_null() {
        let new_size = js_malloc_usable_size_rt((*ctx).rt_mut(), ret);
        *pslack = if new_size > size { new_size - size } else { 0 };
    }
    ret
}

pub unsafe fn js_malloc_usable_size(ctx: *mut JSContext, ptr: *const c_void) -> usize {
    js_malloc_usable_size_rt((*ctx).rt_mut(), ptr)
}

//
// rust alloc
//

pub unsafe fn js_malloc_rt_typed<T>(
    rt: *mut JSRuntime,
    count: usize,
    zeroed: bool,
) -> Option<NonNull<T>> {
    let ly = core::alloc::Layout::from_size_align_unchecked(
        std::mem::size_of::<T>() * count,
        std::mem::align_of::<T>(),
    );
    let ptr = if zeroed {
        std::alloc::alloc_zeroed(ly).cast::<T>()
    } else {
        std::alloc::alloc(ly).cast::<T>()
    };
    if !ptr.is_null() {
        let s = &mut (*rt).malloc_state;
        (*s).malloc_count += 1;
        (*s).malloc_size += js_def_malloc_usable_size(ptr.cast()); //+ MALLOC_OVERHEAD;
        Some(NonNull::new_unchecked(ptr))
    } else {
        None
    }
}

/* Throw out of memory in case of error */
pub unsafe fn js_malloc_typed<T>(
    ctx: *mut JSContext,
    count: usize,
    zeroed: bool,
) -> Option<NonNull<T>> {
    let ptr = js_malloc_rt_typed::<T>((*ctx).rt_mut(), count, zeroed);
    if ptr.is_none() {
        ctx.as_mut().unwrap().JS_ThrowOutOfMemory();
    }
    ptr
}

pub unsafe fn js_strndup(ctx: *mut JSContext, s: *const c_char, n: usize) -> *mut c_char {
    let ptr = js_malloc(ctx, n + 1) as *mut c_char;
    if !ptr.is_null() {
        std::ptr::copy_nonoverlapping(s, ptr, n);
        *ptr.add(n) = 0;
    }
    ptr
}

pub unsafe fn js_strdup(ctx: *mut JSContext, str: *const c_char) -> *mut c_char {
    let len = libc::strlen(str);
    js_strndup(ctx, str, len)
}

pub unsafe fn js_realloc_array(
    ctx: *mut JSContext,
    parray: *mut *mut c_void,
    elem_size: i32,
    psize: *mut i32,
    req_size: i32,
) -> i32 {
    let mut new_size = std::cmp::max(req_size, (*psize) * 3 / 2);
    let mut slack: usize = 0;
    let new_array = js_realloc2(
        ctx,
        *parray,
        (new_size as usize) * (elem_size as usize),
        &mut slack,
    );
    if new_array.is_null() {
        return -1;
    }
    new_size += (slack / elem_size as usize) as i32;
    *psize = new_size;
    *parray = new_array;
    0
}

pub unsafe fn js_resize_array(
    ctx: *mut JSContext,
    parray: *mut *mut c_void,
    elem_size: i32,
    psize: *mut i32,
    req_size: i32,
) -> i32 {
    if unlikely(req_size > *psize) {
        js_realloc_array(ctx, parray, elem_size, psize, req_size)
    } else {
        0
    }
}

pub unsafe fn ident_realloc(
    ctx: *mut JSContext,
    pbuf: *mut *mut u8,
    psize: *mut usize,
    static_buf: *mut u8,
) -> i32 {
    let buf = *pbuf;
    let size = *psize;
    let new_size = if size >= (usize::MAX / 3) * 2 {
        usize::MAX
    } else {
        size + (size >> 1)
    };
    let new_buf = if buf == static_buf {
        let b = js_malloc(ctx, new_size);
        if b.is_null() {
            return -1;
        }
        std::ptr::copy_nonoverlapping(buf, b.cast(), size);
        b
    } else {
        let b = js_realloc(ctx, buf.cast(), new_size);
        if b.is_null() {
            return -1;
        }
        b
    };

    *pbuf = new_buf.cast();
    *psize = new_size;

    0
}

pub mod alloca {
    use std::{
        mem::{ManuallyDrop, MaybeUninit},
        os::raw::c_void,
    };

    type Callback = unsafe extern "C-unwind" fn(ptr: *mut MaybeUninit<u8>, data: *mut c_void);

    #[link(name = "alloca", kind = "static")]
    extern "C-unwind" {
        pub fn c_alloca(size: usize) -> *mut u8;

        fn c_alloca_callback(size: usize, callback: Callback, opaque: *mut c_void);
    }

    unsafe extern "C-unwind" fn trampoline<F: FnOnce(*mut MaybeUninit<u8>)>(
        ptr: *mut MaybeUninit<u8>,
        data: *mut c_void,
    ) {
        let f = ManuallyDrop::take(&mut *(data as *mut ManuallyDrop<F>));
        f(ptr);
    }

    #[inline(always)]
    fn get_trampoline<F: FnOnce(*mut MaybeUninit<u8>)>(_closure: &F) -> Callback {
        trampoline::<F>
    }

    pub fn with_alloca<R>(size: usize, f: impl FnOnce(&mut [MaybeUninit<u8>]) -> R) -> R {
        let mut ret = MaybeUninit::<R>::uninit();

        let closure = |ptr| {
            let slice = unsafe { std::slice::from_raw_parts_mut(ptr, size) };
            ret.write(f(slice));
        };
        let trampoline = get_trampoline(&closure);
        let mut closure_data = ManuallyDrop::new(closure);

        unsafe {
            c_alloca_callback(
                size,
                trampoline,
                std::ptr::addr_of_mut!(closure_data).cast(),
            );
            ret.assume_init()
        }
    }

    pub fn with_alloca_zeroed<R>(size: usize, f: impl FnOnce(&mut [u8]) -> R) -> R {
        with_alloca(size, |memory| unsafe {
            std::ptr::write_bytes(memory.as_mut_ptr(), 0, size);
            f(std::mem::transmute(memory))
        })
    }
}
