// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    hash::{BuildHasher, Hasher},
    sync::atomic::{AtomicU64, Ordering},
};

/// get stack top level addr
#[inline(always)]
pub fn js_get_thread_stack_pointer() -> usize {
    // get_frame_address(0).unwrap()

    let d = 0;
    let p = &d as *const i32;
    p as usize
}

/// Lowest address of the current thread's native stack (its "bottom"), or 0
/// when the platform can't query it. Cached per thread — stack bounds are fixed
/// for the lifetime of a thread.
pub(crate) fn js_get_thread_stack_bottom() -> usize {
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

// ── The process's hash secret ──

/// The secret the content hashes fold in.
///
/// A script that can compute the hash of a string it chooses can craft many
/// strings with the same hash: they land in one bucket of the atom table (or
/// of a `Map`'s lookup table), and every lookup then walks the whole group -
/// a hash-flooding denial of service. The secret is random per process, so
/// the fold's result is not computable from the outside.
///
/// It is a *multiplier*, not a final xor: a final xor would leave the
/// collisions of the underlying polynomial intact (both sides shift by the
/// same amount), and the script could still solve for them.
static HASH_KEY: AtomicU64 = AtomicU64::new(0);

/// The process's hash secret. Random per process, so it is the same for
/// every runtime in the process (the string hash has no runtime to ask).
///
/// The secret is read on every hash, so it is a relaxed atomic load and not
/// a `OnceLock` (whose `get_or_init` is a call the optimizer cannot fold
/// away). `0` means "not seeded yet": the secret is stored odd, so it can
/// never be mistaken for that.
#[inline(always)]
pub(crate) fn hash_key() -> u64 {
    let key = HASH_KEY.load(Ordering::Relaxed);
    if key != 0 {
        return key;
    }
    // First call in this process: draw a secret. `RandomState`'s keys come
    // from the OS entropy pool; hashing a fixed value with a fresh one is
    // enough of a secret here.
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(0);
    let secret = h.finish() | 1;
    // Another thread may have won the race: whoever stored first decides, so
    // every runtime in the process folds in the same secret.
    let _ = HASH_KEY.compare_exchange(0, secret, Ordering::Relaxed, Ordering::Relaxed);
    HASH_KEY.load(Ordering::Relaxed)
}

/// Fold one codepoint into a content hash.
///
/// The secret is the multiplier, so the fold keeps its shape (a multiply-add
/// per codepoint) but only the holder of the secret can predict the result.
#[inline(always)]
pub(crate) const fn hash_fold(key: u64, h: u64, extra: u32) -> u64 {
    h.wrapping_mul(key).wrapping_add(extra as u64)
}

/// Mix a value into the process's secret: the result is not computable by a
/// script, so it cannot line values up on purpose (the collisions of a
/// *public* mixer are craftable - that is the whole point of the secret).
#[inline(always)]
pub(crate) fn hash_secret_mix(value: u64) -> u32 {
    (value.wrapping_mul(hash_key()) >> 32) as u32
}

/// calc hash for usize
///
/// The process's secret is in the multiplier, so a script cannot line values
/// up on purpose (see [`hash_key`]).
#[inline(always)]
pub(crate) fn hash_usize(id: usize) -> u32 {
    hash_secret_mix(id as u64)
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
