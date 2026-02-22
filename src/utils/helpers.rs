// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

// fn get_frame_address(level: usize) -> Option<*const c_void> {
//     // NOTE: 除了libc, 还可以用backtrace实现

//     // 定义一个数组来存储调用栈的地址
//     const MAX_STACK_DEPTH: usize = 32;
//     let mut addrs: [*mut c_void; MAX_STACK_DEPTH] = [std::ptr::null_mut(); MAX_STACK_DEPTH];

//     // 使用 libc::backtrace 获取调用栈地址
//     let depth = unsafe { libc::backtrace(addrs.as_mut_ptr(), MAX_STACK_DEPTH as i32) };

//     // 检查 level 是否在有效范围内
//     if level < depth as usize {
//         Some(addrs[level])
//     } else {
//         None // 如果层级超出调用栈深度，返回 None
//     }
// }

/// get stack top level addr
#[inline(always)]
pub const fn js_get_stack_pointer() -> *const c_void {
    // get_frame_address(0).unwrap()
    std::ptr::null()
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
macro_rules! defer {
    ($($code:tt)*) => {
        let _deferred = {
            struct Deferred<F: FnOnce()> {
                cleanup: Option<F>,
            }
            impl<F: FnOnce()> Drop for Deferred<F> {
                fn drop(&mut self) {
                    if let Some(cleanup) = self.cleanup.take() {
                        cleanup();
                    }
                }
            }
            Deferred {
                cleanup: Some(|| {
                    $($code)*
                }),
            }
        };
    };
}
