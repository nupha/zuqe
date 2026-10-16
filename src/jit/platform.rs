// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Executable code memory.
//!
//! Minimal W^X: map RW, write the code, then flip the page to RX with no write
//! permission, and — on aarch64, where the instruction cache is not coherent
//! with the data cache — flush the i-cache for the published range. The rest of
//! the cross-platform layer (macOS `MAP_JIT` write protection, Windows
//! `VirtualAlloc`/`VirtualProtect`/`FlushInstructionCache`, W^X splitting) is
//! epic G1's remaining work; this covers the Unix hosts the JIT can target.
//!
//! Epic G1 portability notes (what the remaining hosts need):
//! * **macOS**: `mmap(..., MAP_JIT)` — required on Apple Silicon; writes go
//!   through `pthread_jit_write_protect_np` (or the `pthread_jit_write_with_callback_np`
//!   allow-list callback when the process is not entitled), and the flush is
//!   `sys_icache_invalidate`. Plain `mprotect` (what this file does) is enough
//!   only for Intel macOS.
//! * **Windows**: `VirtualAlloc(PAGE_READWRITE)` → `VirtualProtect(PAGE_EXECUTE_READ)`
//!   → `FlushInstructionCache(GetCurrentProcess(), ...)`.
//! * **aarch64**: done here (see [`flush_icache`]).

use super::error::JitError;

/// Fault injection for the platform layer: makes `CodeMem::alloc` fail as if
/// `mmap` had refused, so the compile pipeline's fallback path can be driven
/// from a test instead of needing a real allocation fault. Compiled only into
/// `cargo test` builds — production code has no switch to flip.
// Kept next to the `alloc` it guards rather than at the end of the file.
#[allow(clippy::items_after_test_module)]
#[cfg(test)]
pub(crate) mod fault {
    use std::cell::Cell;

    thread_local! {
        static ACTIVE: Cell<bool> = const { Cell::new(false) };
    }

    /// Arm (or disarm) the fault for the calling thread.
    pub(crate) fn set(active: bool) {
        ACTIVE.with(|flag| flag.set(active));
    }

    pub(super) fn active() -> bool {
        ACTIVE.with(Cell::get)
    }
}

pub(crate) struct CodeMem {
    ptr: *mut u8,
    len: usize,
}

impl CodeMem {
    pub(crate) fn alloc(len: usize) -> Result<Self, JitError> {
        #[cfg(test)]
        if fault::active() {
            return Err(JitError::Platform(
                "fault injection: mmap refused the mapping".into(),
            ));
        }
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
            // aarch64 does not snoop its i-cache from the data path: the bytes
            // just written are not visible to the fetch unit until it is
            // invalidated. The mapping can be a reused address (the code cache
            // munmaps and re-maps), so this is a correctness step, not a
            // nicety. x86_64 is cache-coherent and needs nothing.
            #[cfg(target_arch = "aarch64")]
            flush_icache(self.ptr.cast_const(), self.len);
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

/// Make freshly written instructions visible to the fetch unit (aarch64).
///
/// The sequence is the ARM-recommended one (Linux `arm64` `__flush_icache_range`):
/// clean the data cache to the point of unification, then invalidate the
/// i-cache, with the barriers those operations require. The cache line sizes
/// come from `CTR_EL0`, so this works on any implementation.
#[cfg(target_arch = "aarch64")]
fn flush_icache(start: *const u8, len: usize) {
    use std::arch::asm;

    let ctr_el0: u64;
    // SAFETY: reading a read-only system register.
    unsafe {
        asm!("mrs {}, ctr_el0", out(reg) ctr_el0, options(nomem, nostack));
    }
    let data_line = 4usize << ((ctr_el0 >> 16) & 0xf);
    let instruction_line = 4usize << (ctr_el0 & 0xf);
    let start = start as usize;
    let end = start + len;

    let mut address = start & !(data_line - 1);
    while address < end {
        // SAFETY: a cache maintenance instruction on an address in our mapping.
        unsafe {
            asm!("dc cvau, {}", in(reg) address, options(nostack));
        }
        address += data_line;
    }
    // SAFETY: barrier.
    unsafe {
        asm!("dsb ish", options(nostack));
    }

    let mut address = start & !(instruction_line - 1);
    while address < end {
        // SAFETY: as above.
        unsafe {
            asm!("ic ivau, {}", in(reg) address, options(nostack));
        }
        address += instruction_line;
    }
    // SAFETY: barriers; the `isb` also flushes the pipeline of prefetched
    // instructions.
    unsafe {
        asm!("dsb ish", "isb", options(nostack));
    }
}

#[cfg(unix)]
fn page_round_up(len: usize) -> usize {
    // SAFETY: `sysconf` is always safe to call for a valid name.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let page = if page == 0 { 4096 } else { page };
    len.div_ceil(page) * page
}

/// Real-hardware check of the whole publication path on aarch64 (this is the
/// platform the i-cache flush exists for; x86_64 cannot exercise it). Runs only
/// on an aarch64 runner — `scripts/ci-gate.sh`'s matrix lists it.
#[cfg(all(test, target_arch = "aarch64"))]
mod tests {
    use super::CodeMem;

    /// `mov w0, #imm16; ret`.
    fn return_code(value: u16) -> [u8; 8] {
        let mut code = [0u8; 8];
        code[..4].copy_from_slice(&(0x5280_0000u32 | (u32::from(value) << 5)).to_le_bytes());
        code[4..].copy_from_slice(&0xd65f_03c0u32.to_le_bytes());
        code
    }

    #[test]
    fn published_code_runs_after_the_icache_flush() {
        let code = return_code(42);
        let mem = CodeMem::alloc(code.len()).expect("alloc");
        mem.write(&code).expect("write");
        mem.make_executable().expect("make executable");
        let entry: unsafe extern "C" fn() -> i32 = unsafe { std::mem::transmute(mem.entry()) };
        assert_eq!(unsafe { entry() }, 42);
    }
}
