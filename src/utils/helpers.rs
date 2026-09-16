// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

/// get stack top level addr
#[inline(always)]
pub fn js_get_stack_pointer() -> usize {
    // get_frame_address(0).unwrap()

    let d = 0;
    let p = &d as *const i32;
    p as usize
}

/// Lowest address of the current thread's native stack (its "bottom"), or 0
/// when the platform can't query it. Cached per thread — stack bounds are fixed
/// for the lifetime of a thread.
pub(crate) fn js_get_stack_bottom() -> usize {
    thread_local! {
        static STACK_BOTTOM: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    STACK_BOTTOM.with(|b| {
        if b.get() == 0 {
            b.set(current_thread_stack_bottom());
        }
        b.get()
    })
}

/// Compile-time: can the current target query a thread's stack bottom at all?
/// Keep the platform list here in sync with the `#[cfg]` arms of
/// `current_thread_stack_bottom`.
pub(crate) const STACK_BOTTOM_QUERYABLE: bool = cfg!(any(
    windows,
    target_os = "linux",
    target_os = "netbsd",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "macos",
));

#[cfg(windows)]
fn current_thread_stack_bottom() -> usize {
    unsafe extern "system" {
        // Windows 8+: the committed range of the current thread's stack.
        // Available on any thread (not just the main one).
        fn GetCurrentThreadStackLimits(low_limit: *mut usize, high_limit: *mut usize) -> i32;
    }
    let (mut low, mut high) = (0usize, 0usize);
    if unsafe { GetCurrentThreadStackLimits(&mut low, &mut high) } == 0 {
        return 0;
    }
    low
}

#[cfg(any(
    target_os = "linux",
    target_os = "netbsd",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn current_thread_stack_bottom() -> usize {
    unsafe {
        let mut attr: libc::pthread_attr_t = std::mem::zeroed();
        if current_thread_attr(&mut attr) != 0 {
            return 0;
        }
        let mut addr: *mut libc::c_void = std::ptr::null_mut();
        let mut size: libc::size_t = 0;
        let rc = libc::pthread_attr_getstack(&attr, &mut addr, &mut size);
        libc::pthread_attr_destroy(&mut attr);
        if rc != 0 { 0 } else { addr as usize }
    }
}

#[cfg(any(target_os = "linux", target_os = "netbsd"))]
unsafe fn current_thread_attr(attr: *mut libc::pthread_attr_t) -> i32 {
    libc::pthread_getattr_np(libc::pthread_self(), attr)
}

#[cfg(any(target_os = "freebsd", target_os = "dragonfly"))]
unsafe fn current_thread_attr(attr: *mut libc::pthread_attr_t) -> i32 {
    libc::pthread_attr_get_np(libc::pthread_self(), attr)
}

#[cfg(target_os = "macos")]
fn current_thread_stack_bottom() -> usize {
    unsafe {
        let addr = libc::pthread_get_stackaddr_np(libc::pthread_self()) as usize;
        let size = libc::pthread_get_stacksize_np(libc::pthread_self());
        // pthread_get_stackaddr_np returns the stack's HIGH end; the stack
        // grows down, so the bottom is `addr - size`. CAVEAT: on the main
        // thread these _np calls may report a conservative (smaller) size —
        // the guard then trips earlier than strictly needed. That is safe
        // (only conservative, never past the real bottom); host-created
        // worker threads report the exact bounds, which is our main case.
        if size == 0 { 0 } else { addr - size }
    }
}

#[cfg(not(any(
    windows,
    target_os = "linux",
    target_os = "netbsd",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "macos",
)))]
fn current_thread_stack_bottom() -> usize {
    0 // OpenBSD, Solaris, etc.: no portable current-thread stack-geometry query
}

//
// OPCODE UTILS
//

#[inline(always)]
pub const fn M2(op1: i32, op2: i32) -> i32 {
    op1 | (op2 << 8)
}

#[inline(always)]
pub(crate) const fn M3(op1: i32, op2: i32, op3: i32) -> i32 {
    op1 | (op2 << 8) | (op3 << 16)
}

#[inline(always)]
pub(crate) const fn M4(op1: i32, op2: i32, op3: i32, op4: i32) -> i32 {
    op1 | (op2 << 8) | (op3 << 16) | (op4 << 24)
}

#[inline(always)]
pub(crate) const unsafe fn read_from_ptr<T>(ptr: *const u8) -> T {
    unsafe { ptr.cast::<T>().read_unaligned() }
}

#[inline(always)]
pub(crate) const unsafe fn write_to_ptr<T>(ptr: *mut u8, val: T) {
    unsafe { ptr.cast::<T>().write_unaligned(val) }
}

// 成功时返回读取的字节数（usize）
pub(crate) fn get_leb128(pval: &mut u32, buf: &[u8]) -> Result<usize, i32> {
    let mut ptr = buf.iter();
    let mut v: u32 = 0;

    for i in 0..5 {
        match ptr.next() {
            Some(&byte) => {
                v |= ((byte & 0x7F) as u32) << (i * 7);
                if byte & 0x80 == 0 {
                    *pval = v;
                    return Ok(i + 1);
                }
            }
            None => {
                return Err(-1); // "Buffer too short"
            }
        }
    }

    *pval = 0;
    Err(-1) //  Invalid LEB128 encoding
}

pub(crate) fn get_sleb128(pval: &mut i32, buf: &[u8]) -> Result<usize, i32> {
    let mut val = 0u32;
    let ret = get_leb128(&mut val, buf)?;
    *pval = (val as i32 >> 1) ^ -(val as i32 & 1);
    Ok(ret)
}

/// calc hash for base_hash adding an extra u32
#[inline(always)]
pub(crate) const fn hash_u32_from_base(base_hash: u32, extra: u32) -> u32 {
    base_hash.wrapping_mul(263).wrapping_add(extra)
}

/// calc hash for usize
#[inline(always)]
pub(crate) const fn hash_usize(id: usize) -> u32 {
    id.wrapping_mul(3163) as u32
}

#[macro_export]
macro_rules! defer_fn {
    ($($code:tt)+) => {
        {
            #[must_use]
            struct Deferred<F: FnOnce()> {
                cleanup: Option<F>,
            }
            impl<F: FnOnce()> Drop for Deferred<F> {
                fn drop(&mut self) {
                    if let Some(f) = self.cleanup.take() {
                        f();
                    }
                }
            }
            Deferred {
                cleanup: Some(move || {
                    $($code)+
                }),
            }
        }
    };
}

#[macro_export]
macro_rules! defer {
    ($($code:tt)*) => {
        let _deferred = $crate::defer_fn!($($code)*);
    };
}
