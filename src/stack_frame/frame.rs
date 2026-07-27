// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::mem::ManuallyDrop;

use crate::{
    Object, ScopedContext,
    function::{Bytecode, VarRef},
    value::JSValue,
};

/// Maximum JS-to-JS call nesting depth before a `RangeError`-style
/// "maximum call stack size exceeded" is raised.
///
/// The stackless interpreter reuses the active frame for tail calls
/// (`TAIL_CALL`), so the *byte*-based virtual-stack limit
/// (`max_stack_bytes`) never trips for an infinite tail-recursive loop —
/// the frame is freed and reallocated in place, keeping `data_used`
/// constant. A separate depth counter is therefore required to bound
/// runaway recursion (e.g. `function f(){ f(); }`). The counter is the
/// call-chain length, so a flat `for` loop that calls a function many
/// times is unaffected (each call reuses the same caller depth).
///
/// 0 means no limit.
pub(crate) const MAX_CALL_DEPTH: u32 = 0;

/// Index into StackFrameArena::frames, equivalent to a pointer but not
/// constrained by Rust's borrow rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FrameIndex(pub(super) u32);

impl FrameIndex {
    pub const NULL: Self = Self(u32::MAX);

    #[inline(always)]
    pub const fn is_null(self) -> bool {
        self.0 == u32::MAX
    }

    /// Convert to usize for arena head indexing.
    /// Panics if this is FrameIndex::NULL.
    #[inline(always)]
    pub(crate) const fn as_usize(self) -> usize {
        debug_assert!(!self.is_null());
        self.0 as usize
    }
}

/// Frame lifecycle state within the arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameState {
    /// Frame is actively executing or on the virtual call stack.
    Active,
    /// Async frame has been suspended at a yield/await point.
    Suspended,
}

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct ExecMode: u8 {
        const STRICT = 1 << 0;
        const STRIP  = 1 << 1;
        const ASYNC  = 1 << 2;
    }
}

/// Fixed-size header for every stack frame in the arena.
///
/// StackFrame is stored in StackFrameArena::frames (a Vec). It can be safely
/// moved during Vec reallocation because:
/// - arg/var/stack raw pointers point into arena.data or a Heap Box,
///   never into StackFrame itself.
/// - FrameIndex is a pure index; the prev chain survives Vec moves.
/// - All external references use FrameIndex, not &StackFrame.
#[derive(Debug)]
pub(crate) struct StackFrame {
    /// Previous stack frame in the call chain (FrameIndex::NULL for root).
    pub(crate) prev: FrameIndex,
    /// JS-to-JS call nesting depth (0 for the root frame). Each call sets the
    /// callee's depth to `caller.depth + 1`, so an infinite (tail-)recursive
    /// loop drives this toward `MAX_CALL_DEPTH` and trips a stack error.
    pub(crate) depth: u32,
    pub(crate) state: FrameState,
    /// Execution mode flags (STRICT / STRIP / ASYNC).
    pub(crate) mode: ExecMode,

    /// Associated function object (Bytecode or Native).
    pub(crate) func_obj: Object,

    /// Program counter offset within the bytecode stream.
    pub(crate) cur_pc_offset: usize,
    /// Stack pointer offset relative to stack_base_ptr.
    pub(crate) cur_sp_offset: i32,

    /// Where the frame's data buffer lives.
    pub(crate) buf: FrameBuffer,

    /// Offsets relative to the data buffer start (buf).
    pub(crate) arg_base_offset: u32,
    /// Number of actual arguments passed.
    pub(crate) arg_real_count: u16,
    /// Number of formal parameters (>= arg_real_count).
    pub(crate) arg_count: u16,
    /// Start offset of local variables.
    pub(crate) var_base_offset: u32,
    /// Number of local variables.
    pub(crate) var_count: u16,
}

impl StackFrame {
    /// Get the stack base pointer: start of the operand stack region,
    /// immediately after the local variables area.
    #[inline(always)]
    pub(crate) fn stack_base_ptr(&self) -> *mut JSValue {
        unsafe {
            self.buf
                .data_ptr()
                .add((self.var_base_offset + self.var_count as u32) as usize)
        }
    }

    /// Get current stack top pointer (stack_base_ptr + cur_sp_offset).
    #[inline(always)]
    pub(crate) fn stack_cur_ptr(&self) -> *mut JSValue {
        debug_assert!(self.cur_sp_offset >= 0);
        unsafe { self.stack_base_ptr().add(self.cur_sp_offset as usize) }
    }
}

/// Location of a frame's data buffer (args + vars + operand stack).
#[derive(Debug)]
pub(crate) enum FrameBuffer {
    /// Data stored in StackFrameArena's shared fixed-capacity data slice
    /// (bump-allocated). `data_base` is the arena's data pointer (stable for
    /// the lifetime of the arena — `Box<[JSValue]>` never reallocates);
    /// `buf_offset` is the offset within that slice;
    /// `buf_capacity` is the allocated size in JSValue units.
    Arena {
        data_base: *mut JSValue,
        buf_offset: u32,
        buf_capacity: u32,
    },
    /// Data stored in an independent heap allocation.
    Heap(Box<[JSValue]>),
    /// Slot has been suspended/freed; data region is invalid.
    Empty,
}

impl FrameBuffer {
    #[inline(always)]
    pub(crate) fn data_ptr(&self) -> *mut JSValue {
        match self {
            FrameBuffer::Arena {
                data_base,
                buf_offset,
                ..
            } => unsafe { data_base.add(*buf_offset as usize) },
            FrameBuffer::Heap(b) => b.as_ptr() as *mut JSValue,
            FrameBuffer::Empty => std::ptr::null_mut(),
        }
    }

    /// Fast path: returns data pointer without checking the Empty variant.
    /// Caller must guarantee the buffer is not Empty (active frames always
    /// have Arena or Heap, never Empty at access time).
    #[inline(always)]
    pub(crate) fn data_ptr_unchecked(&self) -> *mut JSValue {
        match self {
            FrameBuffer::Arena {
                data_base,
                buf_offset,
                ..
            } => unsafe { data_base.add(*buf_offset as usize) },
            FrameBuffer::Heap(b) => b.as_ptr() as *mut JSValue,
            FrameBuffer::Empty => {
                unreachable!("data_ptr_unchecked called on Empty FrameBuffer")
            }
        }
    }

    /// Returns the data buffer capacity in JSValue units.
    #[inline(always)]
    pub(crate) fn capacity(&self) -> u32 {
        match self {
            FrameBuffer::Arena { buf_capacity, .. } => *buf_capacity,
            FrameBuffer::Heap(b) => b.len() as u32,
            FrameBuffer::Empty => 0,
        }
    }
}

/// One entry in the trampoline's virtual call stack.
#[derive(Debug)]
pub(crate) struct VirtualFrame {
    pub(crate) frame_idx: FrameIndex,
    pub(crate) caller_bc: Bytecode,
    /// Saved caller GC scope. `None` means the callee was `GcScopeKind::None` —
    /// no scope was swapped, so no scope needs to be restored on return/unwind.
    /// `Some` means the callee was `GcScopeKind::Light` or `Full` — a new scope
    /// was pushed and the caller's scope was saved here for later restoration.
    pub(crate) caller_scope: Option<ManuallyDrop<ScopedContext<'static>>>,
    pub(crate) return_sp: usize,
    /// `Some(instance)` when the callee is a flattened legacy (non-derived)
    /// constructor call: on return, a non-object result is replaced with the
    /// instance (mirrors `do_call_internal`'s legacy-constructor behavior).
    pub(crate) ctor_this: Option<JSValue>,
}

/// Complete snapshot of a suspended async frame.
#[derive(Debug)]
pub(crate) struct AsyncStackFrame {
    pub(crate) frame: StackFrame,
    /// Variable references for closure capture, migrated from the arena's
    /// var_ref_map when the frame is taken.
    pub(crate) var_refs: Vec<VarRef>,
}

impl AsyncStackFrame {
    #[inline(always)]
    pub(crate) fn data(&self) -> &[JSValue] {
        match &self.frame.buf {
            FrameBuffer::Heap(b) => b,
            _ => unreachable!("AsyncStackFrame must have Heap buf"),
        }
    }
}
