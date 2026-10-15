// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Executable code memory.
//!
//! Minimal W^X: map RW, write the code, then flip the page to RX with no write
//! permission. The full cross-platform layer (Windows/macOS variants, W^X
//! splitting, fault injection) is epic G1; this covers the Unix hosts the JIT
//! can already target.

use super::error::JitError;

pub(crate) struct CodeMem {
    ptr: *mut u8,
    len: usize,
}

impl CodeMem {
    pub(crate) fn alloc(len: usize) -> Result<Self, JitError> {
        #[cfg(unix)]
        {
            let len = len.max(1);
            // SAFETY: a fresh anonymous private mapping; the length is rounded
            // to a page below so mprotect covers the whole region.
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    page_round_up(len),
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(JitError::Platform(format!(
                    "mmap failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(Self {
                ptr: ptr.cast(),
                len: page_round_up(len),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = len;
            Err(JitError::UnsupportedTarget("executable memory"))
        }
    }

    /// Copy `bytes` into the (writable) mapping. Must be called before
    /// [`Self::make_executable`].
    pub(crate) fn write(&self, bytes: &[u8]) -> Result<(), JitError> {
        if bytes.len() > self.len {
            return Err(JitError::Platform("code larger than mapping".into()));
        }
        // SAFETY: `self.ptr` is a live RW mapping of `self.len` bytes and
        // `bytes.len() <= self.len`.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.ptr, bytes.len());
        }
        Ok(())
    }

    /// Drop write permission and grant execute. No page ever holds both.
    pub(crate) fn make_executable(&self) -> Result<(), JitError> {
        #[cfg(unix)]
        {
            // SAFETY: `self.ptr`/`self.len` describe a mapping we own.
            let rc = unsafe {
                libc::mprotect(self.ptr.cast(), self.len, libc::PROT_READ | libc::PROT_EXEC)
            };
            if rc != 0 {
                return Err(JitError::Platform(format!(
                    "mprotect failed: {}",
                    std::io::Error::last_os_error()
                )));
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(JitError::UnsupportedTarget("executable memory"))
        }
    }

    pub(crate) const fn entry(&self) -> *const u8 {
        self.ptr
    }

    pub(crate) const fn len(&self) -> usize {
        self.len
    }
}

impl Drop for CodeMem {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: unmapping a mapping we own; it is never used afterwards.
        unsafe {
            libc::munmap(self.ptr.cast(), self.len);
        }
    }
}

#[cfg(unix)]
fn page_round_up(len: usize) -> usize {
    // SAFETY: `sysconf` is always safe to call for a valid name.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let page = if page == 0 { 4096 } else { page };
    len.div_ceil(page) * page
}
