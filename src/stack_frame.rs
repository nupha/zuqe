// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    marker::{PhantomData, PhantomPinned},
    mem::ManuallyDrop,
    pin::Pin,
    ptr::NonNull,
};

use gc_lite::{GcTrace, GcTraceCtx, GcWeak};

use crate::{
    Object,
    builtins::iterator::ObjectIter,
    context::ScopedContext,
    error::{Exception, is_backtrace_needed},
    function::{AsyncStateImpl, VarRef},
    runtime::{RuntimeImpl, RuntimePtr},
    value::JSValue,
};

// ============================================================================
// Stackless Interpreter: Bump-Arena Virtual Frame Stack
// ============================================================================
// The new frame model replaces per-call heap-allocated StackFrame with a
// fixed-capacity bump arena. All active frames' metadata (FrameHead) and
// data regions (args + vars + operand stack) reside in contiguous memory.
//
// Key design decisions:
// - Arena data capacity is fixed at construction (never reallocated) to
//   guarantee raw pointer stability for VarRef pvalue fields.
// - Sync frames use bump allocation in arena.data; async frames always
//   use heap-allocated buffers (FrameBufLocation::Heap).
// - heap_buf_cache recycles oversized heap buffers for future allocations.
// ============================================================================

/// Index into StackFrameArena::heads, equivalent to a pointer but not
/// constrained by Rust's borrow rules. Multiple frames can be accessed
/// simultaneously through their indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameIndex(u32);

impl FrameIndex {
    pub const NULL: Self = Self(u32::MAX);

    #[inline(always)]
    pub const fn is_null(self) -> bool {
        self.0 == u32::MAX
    }
}

/// Frame lifecycle state within the arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameState {
    /// Frame is actively executing or on the virtual call stack.
    Active,
    /// Async frame has been suspended at a yield/await point;
    /// its data has been transferred to an AsyncStackFrame snapshot.
    Suspended,
}

/// Location of a frame's data buffer (args + vars + operand stack).
/// The same FrameHead type can represent arena-resident or heap-resident
/// frames, eliminating the need for separate snapshot types.
#[derive(Debug)]
pub(crate) enum FrameBufLocation {
    /// Data stored in StackFrameArena's shared fixed-capacity data slice
    /// (bump-allocated). The buf_offset is relative to arena.data start.
    Arena {
        /// Start offset within arena.data, in JSValue units.
        buf_offset: u32,
    },
    /// Data stored in an independent heap allocation (async frames,
    /// arena-overflow sync frames).
    Heap(Box<[JSValue]>),
    /// Slot has been suspended/freed; data region is invalid.
    /// Accessing this variant is a logic bug.
    None,
}

/// Fixed-size header for every stack frame in the arena.
///
/// FrameHead is stored in StackFrameArena::heads (a Vec). It can be safely
/// moved during Vec reallocation because:
/// - arg/var/stack raw pointers point into arena.data or a Heap Box,
///   never into FrameHead itself.
/// - FrameIndex is a pure index; the prev chain survives Vec moves.
/// - All external references use FrameIndex, not &FrameHead.
#[derive(Debug)]
pub(crate) struct FrameHead {
    /// Previous frame in the call chain (FrameIndex::NULL for root).
    pub(crate) prev: FrameIndex,
    /// Frame lifecycle state.
    pub(crate) state: FrameState,
    /// Associated function object (Bytecode or Native).
    pub(crate) func_obj: Object,
    /// Execution mode flags (STRICT / STRIP / ASYNC).
    pub(crate) mode: ExecMode,

    /// Program counter offset within the bytecode stream.
    pub(crate) cur_pc_offset: usize,
    /// Stack pointer offset relative to stack_base_ptr.
    pub(crate) cur_sp_offset: i32,

    /// Protected values: this_obj, new_target (deprecated but needed for compat).
    pub(crate) protected_values: [JSValue; 2],

    /// Variable references for closure capture.
    /// Each VarRef holds a raw pointer (pvalue) into the data buffer.
    /// After take/put operations, all pvalues must be updated to point
    /// to the correct data buffer location.
    pub(crate) var_ref_list: Vec<VarRef>,

    // ── Data region descriptor ──
    /// Where the frame's data buffer lives.
    pub(crate) buf: FrameBufLocation,
    /// Total capacity of the data buffer, in JSValue units.
    pub(crate) buf_capacity: u32,

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

/// Bump-allocator frame storage.
///
/// All active FrameHead entries are stored contiguously in `heads`.
/// `data` is a fixed-capacity Box<[JSValue]> (allocated once, never resized)
/// to guarantee pointer stability for Arena-variant frames.
///
/// When the arena is full, new sync frame data is allocated on the heap
/// and cached in heap_buf_cache for reuse. Async frames always use Heap.
pub(crate) struct StackFrameArena {
    /// Frame headers array. May contain Suspended tombstone slots
    /// (never swap_remove'd, preserving prev chain integrity).
    pub(crate) heads: Vec<FrameHead>,
    /// Fixed-capacity data buffer for Arena-variant frames.
    /// Allocated once at construction, never resized.
    pub(crate) data: Box<[JSValue]>,
    /// Bump pointer within data (next free slot).
    pub(crate) data_used: usize,

    /// Count of Suspended tombstone slots (triggers compact when threshold exceeded).
    pub(crate) suspended_count: usize,

    /// Cache of heap-allocated frame data buffers recycled from freed frames.
    /// Uses linear first-fit scan (max 8 entries; sorting overhead > scan benefit).
    pub(crate) heap_buf_cache: Vec<Box<[JSValue]>>,
}

impl StackFrameArena {
    /// Fixed arena data capacity in JSValue slots (8192 ≈ 64KB).
    pub(crate) const ARENA_DATA_CAP: usize = 8192;

    /// Maximum number of cached heap buffers before eviction.
    const HEAP_BUF_CACHE_MAX: usize = 8;

    pub(crate) fn new() -> Self {
        Self {
            heads: Vec::with_capacity(256),
            data: vec![JSValue::UNDEFINED; Self::ARENA_DATA_CAP].into_boxed_slice(),
            data_used: 0,
            suspended_count: 0,
            heap_buf_cache: Vec::new(),
        }
    }

    /// Allocate a new frame header and data buffer, returning its index.
    ///
    /// Allocation strategy:
    /// - Async frames (mode contains ASYNC): always Heap (lifetime ≠ arena LIFO).
    /// - Sync frames: bump-allocate in arena.data when space permits;
    ///   otherwise take from heap_buf_cache or Box::new.
    pub(crate) fn alloc_frame(
        &mut self,
        func_obj: Object,
        mode: ExecMode,
        buf_capacity: usize,
    ) -> FrameIndex {
        let idx = FrameIndex(self.heads.len() as u32);

        let buf = if mode.contains(ExecMode::ASYNC) {
            // Async frames always use heap-allocated buffers because their
            // lifetime spans multiple yield/await cycles, incompatible with
            // the arena's LIFO bump-allocator model.
            FrameBufLocation::Heap(self.take_from_cache_or_alloc(buf_capacity))
        } else if self.data_used + buf_capacity <= self.data.len() {
            // Sync frame fits in arena: bump-allocate.
            let buf_offset = self.data_used as u32;
            self.data_used += buf_capacity;
            FrameBufLocation::Arena { buf_offset }
        } else {
            // Arena full for this frame: use heap (cached or new).
            FrameBufLocation::Heap(self.take_from_cache_or_alloc(buf_capacity))
        };

        self.heads.push(FrameHead {
            prev: FrameIndex::NULL,
            state: FrameState::Active,
            func_obj,
            mode,
            cur_pc_offset: 0,
            cur_sp_offset: 0,
            protected_values: [JSValue::UNDEFINED, JSValue::UNDEFINED],
            var_ref_list: Vec::new(),
            buf,
            buf_capacity: buf_capacity as u32,
            arg_base_offset: 0,
            arg_real_count: 0,
            arg_count: 0,
            var_base_offset: 0,
            var_count: 0,
        });

        idx
    }

    /// Find a cached heap buffer with capacity >= required, or allocate new.
    /// Uses linear first-fit scan; cache is small (max 8 entries) so the
    /// O(n) scan overhead is negligible compared to maintaining sort order.
    fn take_from_cache_or_alloc(&mut self, required: usize) -> Box<[JSValue]> {
        if let Some(pos) = self.heap_buf_cache.iter().position(|b| b.len() >= required) {
            self.heap_buf_cache.remove(pos)
        } else {
            vec![JSValue::UNDEFINED; required].into_boxed_slice()
        }
    }

    /// Logically free a frame.
    ///
    /// LIFO path (frame is last in heads): pop FrameHead + rewind data_used.
    /// Non-LIFO path: mark Suspended, wait for compact.
    ///
    /// ⚠️ Caller MUST ensure var_ref_list has been closed (close_var_refs)
    /// before calling free_frame on sync frames. VarRef raw pointers into
    /// arena.data become dangling after the region is reused.
    pub(crate) fn free_frame(&mut self, idx: FrameIndex) {
        debug_assert_eq!(self.heads[idx.0 as usize].state, FrameState::Active);

        if idx.0 as usize == self.heads.len() - 1 {
            // Hot LIFO path: pop FrameHead and rewind arena bump pointer.
            let head = self.heads.pop().unwrap();
            match head.buf {
                FrameBufLocation::Arena { .. } => {
                    self.data_used -= head.buf_capacity as usize;
                    // Arena slots are NOT zeroed — subsequent bump allocations
                    // will naturally overwrite them.
                }
                FrameBufLocation::Heap(heap_buf) => {
                    // Cache the heap buffer if it's too large to fit in arena
                    // (same reason it was allocated on heap originally).
                    let remaining = self.data.len() - self.data_used;
                    if heap_buf.len() > remaining
                        && self.heap_buf_cache.len() < Self::HEAP_BUF_CACHE_MAX
                    {
                        self.heap_buf_cache.push(heap_buf);
                    }
                    // Otherwise arena has enough space now; drop the heap buf.
                }
                FrameBufLocation::None => {
                    // Suspended frame data already migrated; only FrameHead freed.
                }
            }
        } else {
            // Cold path: async frame not at stack top (blocked by sibling frames).
            // Mark Suspended to preserve prev chain; heap buffer is recycled.
            let head = &mut self.heads[idx.0 as usize];
            if let FrameBufLocation::Heap(heap_buf) =
                std::mem::replace(&mut head.buf, FrameBufLocation::None)
            {
                let remaining = self.data.len() - self.data_used;
                if heap_buf.len() > remaining
                    && self.heap_buf_cache.len() < Self::HEAP_BUF_CACHE_MAX
                {
                    self.heap_buf_cache.push(heap_buf);
                }
            }
            head.buf_capacity = 0;
            head.state = FrameState::Suspended;
            self.suspended_count += 1;
        }
    }

    /// Get immutable reference to a frame header by index.
    #[inline(always)]
    pub(crate) fn frame(&self, idx: FrameIndex) -> &FrameHead {
        &self.heads[idx.0 as usize]
    }

    /// Get mutable reference to a frame header by index.
    ///
    /// SAFETY: Only the currently executing frame may be mutably accessed.
    /// The trampoline design guarantees this: only cur_frame_idx is ever
    /// passed to frame_mut.
    #[inline(always)]
    pub(crate) fn frame_mut(&mut self, idx: FrameIndex) -> &mut FrameHead {
        &mut self.heads[idx.0 as usize]
    }

    /// Get raw pointer to a frame's data buffer (works for both Arena and Heap).
    #[inline(always)]
    pub(crate) fn data_ptr(&self, head: &FrameHead) -> *mut JSValue {
        match &head.buf {
            FrameBufLocation::Arena { buf_offset } => unsafe {
                self.data.as_ptr().add(*buf_offset as usize) as *mut JSValue
            },
            FrameBufLocation::Heap(b) => b.as_ptr() as *mut JSValue,
            FrameBufLocation::None => std::ptr::null_mut(),
        }
    }

    /// Get stack base pointer: the start of the operand stack region,
    /// immediately after the local variables area.
    #[inline(always)]
    pub(crate) fn stack_base_ptr(&self, head: &FrameHead) -> *mut JSValue {
        unsafe {
            self.data_ptr(head)
                .add((head.var_base_offset + head.var_count as u32) as usize)
        }
    }

    /// Get current stack top pointer (stack_base_ptr + cur_sp_offset).
    #[inline(always)]
    pub(crate) fn stack_cur_ptr(&self, head: &FrameHead) -> *mut JSValue {
        debug_assert!(head.cur_sp_offset >= 0);
        unsafe { self.stack_base_ptr(head).add(head.cur_sp_offset as usize) }
    }

    /// Initialize a freshly allocated frame's argument region.
    ///
    /// Copies `args` into the arg slots and fills any remaining formal-parameter
    /// slots with `UNDEFINED`.  Sets `arg_real_count`, `arg_count`,
    /// `var_base_offset`, `var_count` and `cur_sp_offset` on the frame head.
    ///
    /// This is the single canonical path for frame argument setup – all call
    /// sites (bfun.call, inline trampoline, AsyncStateImpl::new) MUST use this
    /// to ensure consistent treatment of missing arguments and head fields.
    #[inline(always)]
    pub(crate) fn init_frame_args(
        &mut self,
        idx: FrameIndex,
        bc_arg_count: u16,
        bc_var_count: u16,
        args: &[JSValue],
    ) {
        let arg_count = std::cmp::max(bc_arg_count as usize, args.len());
        let data_ptr = {
            let head = self.frame(idx);
            self.data_ptr(head)
        };
        let head = self.frame_mut(idx);

        // Copy actual arguments into the args region.
        for (i, arg) in args.iter().enumerate() {
            unsafe { *data_ptr.add(i) = *arg; }
        }
        // Fill unsupplied formal parameter slots with UNDEFINED.
        for i in args.len()..arg_count {
            unsafe { *data_ptr.add(i) = JSValue::UNDEFINED; }
        }

        head.arg_base_offset = 0;
        head.arg_real_count = args.len() as u16;
        head.arg_count = arg_count as u16;
        head.var_base_offset = arg_count as u32;
        head.var_count = bc_var_count;
        head.cur_sp_offset = 0;
    }

    // ── GC-safe stack access API ──

    /// Push a value onto the frame's operand stack (SP += 1).
    #[inline(always)]
    pub(crate) fn stack_push(&mut self, idx: FrameIndex, val: JSValue) {
        // Compute the stack pointer first using immutable borrow, then
        // get mutable access to advance SP. This avoids a borrow conflict
        // between frame_mut (&mut self) and stack_cur_ptr (&self).
        let ptr = {
            let head = self.frame(idx);
            self.stack_cur_ptr(head)
        };
        let head = self.frame_mut(idx);
        unsafe {
            *ptr = val;
        }
        head.cur_sp_offset += 1;
    }

    /// Safely take a value from the stack at the given offset (relative to SP)
    /// with GC transient protection. Replaces the slot with UNDEFINED.
    pub(crate) fn stack_take(
        &self,
        idx: FrameIndex,
        offset: isize,
        ctx: &ScopedContext,
    ) -> JSValue {
        let head = self.frame(idx);
        let ptr = unsafe { self.stack_cur_ptr(head).offset(offset) };
        let val = unsafe { std::ptr::replace(ptr, JSValue::UNDEFINED) };
        ctx.gc_protect_transient(&val);
        val
    }

    /// Store a value at the given stack offset without moving SP.
    #[inline(always)]
    pub(crate) fn stack_store(&self, idx: FrameIndex, offset: isize, val: JSValue) {
        let head = self.frame(idx);
        unsafe {
            *self.stack_cur_ptr(head).offset(offset) = val;
        }
    }

    /// Get an immutable slice of the frame's data buffer.
    fn data_slice<'a>(&'a self, head: &'a FrameHead) -> &'a [JSValue] {
        match &head.buf {
            FrameBufLocation::Arena { buf_offset } => {
                let start = *buf_offset as usize;
                let end = start + head.buf_capacity as usize;
                &self.data[start..end]
            }
            FrameBufLocation::Heap(b) => b,
            FrameBufLocation::None => &[],
        }
    }
}

// ============================================================================
// AsyncStackFrame: Suspended Frame Snapshot
// ============================================================================

/// Complete snapshot of a suspended async frame, containing all metadata
/// and data needed to resume execution later.
///
/// The inner FrameHead always has buf = FrameBufLocation::Heap, making the
/// snapshot self-contained and independent of arena state.
#[derive(Debug)]
pub(crate) struct AsyncStackFrame {
    pub(crate) head: FrameHead,
}

impl StackFrameArena {
    /// Extract an active async frame from the arena, returning a suspended snapshot.
    ///
    /// Because async frames always use Heap buffers (see alloc_frame), take_frame
    /// requires NO data copy — it simply transfers ownership of the Heap Box.
    /// VarRef pvalues are updated to point into the Heap Box before transfer.
    ///
    /// Release strategy:
    /// - If frame is last in heads: pop directly (hot path, O(1), no tombstone).
    /// - Otherwise: mark Suspended, leave tombstone for later compact.
    pub(crate) fn take_frame(&mut self, idx: FrameIndex) -> AsyncStackFrame {
        let idx_usize = idx.0 as usize;
        let is_last = idx_usize == self.heads.len() - 1;

        let head = &mut self.heads[idx_usize];
        debug_assert_eq!(head.state, FrameState::Active);
        debug_assert!(
            head.mode.contains(ExecMode::ASYNC),
            "take_frame: only async frames can be suspended"
        );
        debug_assert!(
            matches!(head.buf, FrameBufLocation::Heap(_)),
            "take_frame: async frames must be Heap-variant"
        );

        // Take ownership of the Heap buffer, replace with None.
        let heap_buf = match std::mem::replace(&mut head.buf, FrameBufLocation::None) {
            FrameBufLocation::Heap(b) => b,
            _ => unreachable!("Async frame must be Heap"),
        };

        // Update VarRef pvalues to point into the Heap Box.
        // Must be done before pop/mark because pop removes FrameHead from Vec.
        // Extract offsets before iterating to avoid borrowing head both
        // mutably (var_ref_list) and immutably (as fn arg) simultaneously.
        let arg_base = head.arg_base_offset;
        let var_base = head.var_base_offset;
        for vr in &mut head.var_ref_list {
            vr.update_pvalue_for_heap_buf_offsets(&heap_buf, arg_base, var_base);
        }

        // Build snapshot with full ownership of the Heap data.
        let snapshot_head = FrameHead {
            prev: FrameIndex::NULL, // prev is rebuilt by trampoline on resume
            state: FrameState::Active,
            func_obj: head.func_obj,
            mode: head.mode,
            cur_pc_offset: head.cur_pc_offset,
            cur_sp_offset: head.cur_sp_offset,
            protected_values: head.protected_values,
            var_ref_list: std::mem::take(&mut head.var_ref_list),
            buf: FrameBufLocation::Heap(heap_buf),
            buf_capacity: head.buf_capacity,
            arg_base_offset: head.arg_base_offset,
            arg_real_count: head.arg_real_count,
            arg_count: head.arg_count,
            var_base_offset: head.var_base_offset,
            var_count: head.var_count,
        };

        if is_last {
            // Hot path: frame is at top of heads, pop without leaving tombstone.
            // No other frame's prev points to this index.
            self.heads.pop();
        } else {
            // Cold path: other frames' prev may point here; leave tombstone.
            head.buf_capacity = 0;
            head.state = FrameState::Suspended;
            self.suspended_count += 1;
        }

        AsyncStackFrame {
            head: snapshot_head,
        }
    }

    /// Place a suspended async frame back into the arena for resumption.
    ///
    /// The snapshot's FrameHead (with Heap buffer) is pushed onto heads.
    /// No arena bump allocation, no data copy, no VarRef pvalue update needed
    /// (pvalues already point into the Heap Box, which remains stable).
    pub(crate) fn put_frame(&mut self, mut snapshot: AsyncStackFrame) -> FrameIndex {
        let new_idx = FrameIndex(self.heads.len() as u32);

        // prev will be set by trampoline based on the call stack at resume time.
        snapshot.head.prev = FrameIndex::NULL;
        snapshot.head.state = FrameState::Active;

        self.heads.push(snapshot.head);
        new_idx
    }

    /// Compact heads: remove all Suspended tombstone slots and remap prev indices.
    ///
    /// Must be called at a GC safe point when:
    /// - Trampoline is not running (VirtualFrames are cleared).
    /// - All AsyncStateImpl frames are Suspended (detached from arena).
    /// - Active frames are only referenced via FrameHead.prev chain.
    pub(crate) fn compact(&mut self) {
        if self.suspended_count == 0 {
            return;
        }

        let old_len = self.heads.len();
        let mut mapping: Vec<Option<u32>> = Vec::with_capacity(old_len);
        let mut new_heads: Vec<FrameHead> = Vec::with_capacity(old_len);

        // Pass 1: filter out Suspended slots, build index mapping.
        for head in self.heads.drain(..) {
            if head.state == FrameState::Suspended {
                mapping.push(None);
            } else {
                mapping.push(Some(new_heads.len() as u32));
                new_heads.push(head);
            }
        }

        // Pass 2: remap prev indices in surviving heads.
        for head in &mut new_heads {
            if !head.prev.is_null() {
                let old_idx = head.prev.0 as usize;
                if let Some(&Some(new_idx)) = mapping.get(old_idx) {
                    head.prev = FrameIndex(new_idx);
                }
            }
        }

        self.heads = new_heads;
        self.suspended_count = 0;
    }

    /// Search the frame's operand stack for a catch offset (exception handler).
    /// Extracted from the legacy StackFrame::handle_exception for arena use.
    ///
    /// Returns Some((catch_pc, catch_sp)) if a catch block is found,
    /// or None if the exception must propagate upward.
    pub(crate) fn find_catch_in_frame(
        &self,
        idx: FrameIndex,
        ctx: &mut ScopedContext,
    ) -> Option<(usize, usize)> {
        let head = self.frame(idx);
        let sp_base = self.stack_base_ptr(head);
        let mut sp = head.cur_sp_offset as usize;

        while sp > 0 {
            sp -= 1;
            if let JSValue::CatchOffset(pos) =
                unsafe { std::ptr::replace(sp_base.add(sp), JSValue::UNDEFINED) }
            {
                if pos == 0 {
                    // Enumerator anchor: close iterator with throw.
                    // Drop the next method slot.
                    unsafe {
                        std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED);
                    }
                    sp -= 1;
                    // Close iterator if SP[-1] is an object.
                    if let JSValue::Object(o) =
                        unsafe { std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED) }
                    {
                        let _ = crate::builtins::iterator::ObjectIter::close_iter_internal(ctx, o);
                    }
                } else {
                    // Found catch: write exception value to stack
                    // and return PC position with incremented SP.
                    let err_val = ctx.take_exception().to_value(ctx);
                    unsafe {
                        std::ptr::write(sp_base.add(sp), err_val);
                    }
                    sp += 1;
                    return Some((pos as usize, sp));
                }
            }
        }
        None
    }

    /// Create a VarRef pointing to a local variable in the given arena frame.
    /// Arena-equivalent of `StackFrame::get_var_ref()`. Used by direct eval
    /// in the stackless path where no old-style StackFrame exists.
    pub(crate) fn get_var_ref_frame(
        &mut self,
        ctx: &mut ScopedContext,
        frame_idx: FrameIndex,
        var_idx: u16,
        is_arg: bool,
        var_ref_list: &mut Vec<VarRef>,
    ) -> Result<VarRef, i32> {
        debug_assert!(ctx.is_arena_mode());

        // search in existing var refs
        if let Some(vr) = var_ref_list
            .iter()
            .find(|x| x.is_match(var_idx, is_arg))
            .copied()
        {
            return Ok(vr);
        }

        let head = self.frame(frame_idx);
        let data_ptr = self.data_ptr(head);
        let arg_base = data_ptr;
        let var_base = unsafe { data_ptr.add(head.var_base_offset as usize) };
        let pvalue = unsafe {
            NonNull::new_unchecked(if is_arg {
                debug_assert!(var_idx < head.arg_count);
                arg_base.add(var_idx as usize)
            } else {
                debug_assert!(var_idx < head.var_count);
                var_base.add(var_idx as usize)
            })
        };

        let vref = VarRef::create_bind(
            ctx,
            var_idx,
            is_arg,
            pvalue,
            if head.mode.contains(ExecMode::ASYNC) {
                // Async frames can be detached; hold a weak ref so that
                // the VarRef stays valid even when the GC runs between
                // yield/await and resumption.
                None
            } else {
                None
            },
        )?;
        var_ref_list.push(vref);
        Ok(vref)
    }
}

impl AsyncStackFrame {
    /// Get immutable slice of the snapshot's data buffer.
    pub(crate) fn data_slice(&self) -> &[JSValue] {
        match &self.head.buf {
            FrameBufLocation::Heap(b) => b,
            _ => unreachable!("AsyncStackFrame must have Heap buf"),
        }
    }
}

// ============================================================================
// VirtualFrame: Trampoline Call Stack Entry
// ============================================================================

/// One entry in the trampoline's virtual call stack.
/// Pushed on CallBytecode, popped on Return/Unwind.
/// Analogous to a C stack frame's saved return address.
#[derive(Debug)]
pub(crate) struct VirtualFrame {
    /// Caller's frame index in the arena.
    pub(crate) frame_idx: FrameIndex,
    /// Caller's bytecode (GC-managed reference, must be traced during GC).
    pub(crate) caller_bc: crate::function::Bytecode,
    /// Stack offset in caller frame where the return value should be written.
    pub(crate) return_sp: usize,
    /// Caller's GC scope, wrapped in ManuallyDrop to prevent automatic
    /// drop when switching to callee scope. Restored via into_inner on return.
    pub(crate) caller_scope: ManuallyDrop<ScopedContext<'static>>,
    /// Caller's context pointer (only Some for cross-realm calls).
    pub(crate) caller_ctx_ptr: Option<crate::context::ContextPtr>,
}

// ============================================================================
// GC Trace Integration for StackFrameArena
// ============================================================================

impl GcTrace for StackFrameArena {
    fn trace(&self, tr: &mut GcTraceCtx) {
        for head in &self.heads {
            // Skip suspended/freed slots; their data is traced by AsyncStateImpl.
            if head.state != FrameState::Active {
                continue;
            }

            head.func_obj.trace(tr);
            head.protected_values[0].trace(tr);
            head.protected_values[1].trace(tr);

            let data = self.data_slice(head);
            let data_end = head.buf_capacity as usize;

            // Trace arguments region.
            let cnt = std::cmp::max(head.arg_real_count, head.arg_count) as usize;
            for v in data.iter().take(cnt.min(data_end)) {
                v.trace(tr);
            }

            // Trace local variables and operand stack.
            let var_start = head.var_base_offset as usize;
            let total_active =
                var_start + head.var_count as usize + head.cur_sp_offset.max(0) as usize;
            for v in &data[var_start..std::cmp::min(total_active, data_end)] {
                v.trace(tr);
            }

            // Trace closure variable references.
            for vr in &head.var_ref_list {
                vr.trace(tr);
            }
        }
    }
}

// ============================================================================
// Legacy: Original StackFrame model (preserved for backward compatibility)
// ============================================================================

/// Inline capacity for var + stack slots in `local_buf` (zero-copy args excluded).
pub(crate) const FRAME_INLINE_CAP: usize = 16;

/// Fixed-capacity buffer for stack frame local data (args, vars, operand stack).
///
/// Unlike `SmallVec`, this type does **not** support growth after construction.
/// Once allocated via `new_alloc(cap)`, the backing memory is immutable in size,
/// guaranteeing that raw pointers derived from `buf_ptr()` remain valid for the
/// buffer's entire lifetime.
///
/// # Variants
/// - `Inline`: data stored directly in the enum (≤ `FRAME_INLINE_CAP` slots)
/// - `Spill`: heap-allocated `Box<[JSValue]>` for larger frames
pub(crate) enum FrameBuf {
    Inline([JSValue; FRAME_INLINE_CAP]),
    Spill(Box<[JSValue]>),
}

impl Default for FrameBuf {
    #[inline(always)]
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for FrameBuf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameBuf")
            .field("capacity", &self.capacity())
            .field("is_spill", &self.is_spill())
            .finish()
    }
}

impl FrameBuf {
    /// Create an empty inline buffer (no heap allocation).
    #[inline(always)]
    pub const fn new() -> Self {
        Self::Inline([JSValue::UNDEFINED; FRAME_INLINE_CAP])
    }

    /// Create a buffer with the given capacity, all slots initialized to `UNDEFINED`.
    /// Uses inline storage for small frames, heap allocation for larger ones.
    pub fn new_alloc(cap: usize) -> Self {
        if cap <= FRAME_INLINE_CAP {
            Self::Inline([JSValue::UNDEFINED; FRAME_INLINE_CAP])
        } else {
            Self::Spill(vec![JSValue::UNDEFINED; cap].into_boxed_slice())
        }
    }

    /// Capacity in number of `JSValue` slots.
    #[inline(always)]
    pub fn capacity(&self) -> usize {
        match self {
            Self::Inline(_) => FRAME_INLINE_CAP,
            Self::Spill(b) => b.len(),
        }
    }

    /// Whether this buffer is heap-allocated (for frame pool recycling decisions).
    #[inline(always)]
    pub const fn is_spill(&self) -> bool {
        matches!(self, Self::Spill(_))
    }

    /// Raw pointer to the backing storage. Stable after construction.
    #[inline(always)]
    pub fn as_mut_ptr(&mut self) -> *mut JSValue {
        match self {
            Self::Inline(a) => a.as_mut_ptr(),
            Self::Spill(b) => b.as_mut_ptr(),
        }
    }

    #[inline(always)]
    fn as_ptr(&self) -> *const JSValue {
        match self {
            Self::Inline(a) => a.as_ptr(),
            Self::Spill(b) => b.as_ptr(),
        }
    }

    /// Copy `src` into the buffer starting at `offset`.
    #[inline]
    pub fn write_slice(&mut self, offset: usize, src: &[JSValue]) {
        if src.is_empty() {
            return;
        }
        let cap = self.capacity();
        assert!(
            offset + src.len() <= cap,
            "FrameBuf::write_slice out of bounds"
        );
        unsafe {
            std::ptr::copy_nonoverlapping(src.as_ptr(), self.as_mut_ptr().add(offset), src.len());
        }
    }

    /// Fill slots in `[offset, offset+len)` with `value`.
    #[inline]
    pub fn fill_range(&mut self, offset: usize, len: usize, value: JSValue) {
        if len == 0 {
            return;
        }
        let cap = self.capacity();
        assert!(offset + len <= cap, "FrameBuf::fill_range out of bounds");
        unsafe {
            let ptr = self.as_mut_ptr().add(offset);
            for i in 0..len {
                ptr.add(i).write(value);
            }
        }
    }
}

impl std::ops::Index<usize> for FrameBuf {
    type Output = JSValue;
    #[inline(always)]
    fn index(&self, index: usize) -> &JSValue {
        assert!(index < self.capacity(), "FrameBuf index out of bounds");
        unsafe { &*self.as_ptr().add(index) }
    }
}

impl std::ops::IndexMut<usize> for FrameBuf {
    #[inline(always)]
    fn index_mut(&mut self, index: usize) -> &mut JSValue {
        assert!(index < self.capacity(), "FrameBuf index out of bounds");
        unsafe { &mut *self.as_mut_ptr().add(index) }
    }
}

bitflags::bitflags! {
    /// execution flag
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub(crate) struct ExecMode: u8 {
        /// strict mode
        const STRICT = 1 << 0;
        /// strip debug source
        const STRIP  = 1 << 1;
        /// use dedicated stack frame for async function execution
        const ASYNC  = 1 << 2;
    }
}

/// A stack frame to execute bytecode or native function,
/// holding function state, arguments, local variables and stack values.
#[derive(Debug)]
pub struct StackFrame {
    /// Raw pointer to parent stack frame (NULL means top)
    pub(super) prev: *mut Pin<Self>,

    /// store Runtime pointer, initialized to func_obj.rt() at first.
    /// used to keep access to Runtime in case has been func_obj disposed.
    #[deprecated(note = "no use")]
    pub(crate) rt: RuntimePtr,

    /// Associated function of this stack frame, could be:
    /// - Bytecode Function object
    /// - Native function object
    pub(crate) func_obj: Object,

    /// extra values that should avoid been gc collected alone with this stack frame:
    /// e.g. this_obj, new_target. allows max 2 values.
    #[deprecated(
        note = "Use scope to protect gc nodes, filling proteced values in stack frame is not courraged"
    )]
    pub(crate) protected_values: [JSValue; 2],

    /// Execution mode
    pub(crate) mode: ExecMode,

    /// This stack frame is within AsyncState?
    pub(crate) is_async: bool,
    /// Weak backref to async state,
    /// only valid if stack frame is dedicated for an AsyncStateImpl
    pub(crate) async_state: GcWeak<AsyncStateImpl>,

    /// Buffer storing arguments, local variables, and stack space
    pub(crate) local_buf: FrameBuf,

    /// Argument values base pointer and count, points into local_buf (self-referencing)
    /// For native callback stack frame, this pointer should be null.
    pub(crate) arg_base_ptr: *mut JSValue,
    pub(crate) arg_real_count: u16,
    pub(crate) arg_count: u16, // at least has `length`

    /// Local variables base pointer and count, points into local_buf (self-referencing).
    /// For native callback stack frame, this pointer should be null.
    pub(crate) var_base_ptr: *mut JSValue,
    pub(crate) var_count: u16,

    /// List of variable references to local vardefs in stack frame
    var_ref_list: Vec<VarRef>,

    /// Program counter offset for bytecode functions
    /// Position in local_buf relative to pc_base (see Bytecode::bytecode_start)
    pub(crate) cur_pc_offset: usize,
    /// Stack pointer offset (used for generator functions)
    /// Position in local_buf relative to stack_base_ptr()
    /// Use stack_cur_ptr() to get raw pointer to this position
    pub(crate) cur_sp_offset: i32,

    _mark: PhantomPinned,
}

impl GcTrace for StackFrame {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.func_obj.trace(tr);
        self.protected_values[0].trace(tr);
        self.protected_values[1].trace(tr);

        // trace args
        let cnt = std::cmp::max(self.arg_real_count, self.arg_count);
        if cnt > 0 {
            let mut p = self.arg_base_ptr;
            for _ in 0..cnt {
                unsafe {
                    (&*p).trace(tr);
                    p = p.add(1);
                }
            }
        }

        for vr in self.var_refs() {
            vr.trace(tr)
        }
        for v in self.var_values() {
            v.trace(tr)
        }
        for v in self.stack_values() {
            v.trace(tr)
        }

        // trace parent stack frames
        if let Some(up) = self.parent() {
            up.trace(tr);
        }
    }
}

impl PartialEq for StackFrame {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        std::ptr::addr_eq(self, other)
    }
}

impl StackFrame {
    /// Allocate a `local_buf` with the given slot count.
    /// Uses inline storage for small frames; large frames reuse the runtime frame pool.
    pub(crate) fn alloc_local_buf(rt: &mut RuntimeImpl, buf_len: usize) -> FrameBuf {
        if buf_len <= FRAME_INLINE_CAP {
            FrameBuf::new()
        } else {
            rt.take_frame_buf(buf_len)
        }
    }

    pub(crate) fn new(func_obj: Object, mode: ExecMode, protected_values: [JSValue; 2]) -> Self {
        Self {
            prev: std::ptr::null_mut(),
            rt: func_obj.rt_ptr(),

            func_obj,
            mode,
            protected_values,
            is_async: false,
            async_state: GcWeak::default(),

            local_buf: FrameBuf::new(),
            arg_base_ptr: std::ptr::null_mut(),
            arg_real_count: 0,
            arg_count: 0,
            var_base_ptr: std::ptr::null_mut(),
            var_count: 0,

            var_ref_list: Vec::new(),
            cur_pc_offset: 0,
            cur_sp_offset: 0,

            _mark: PhantomPinned,
        }
    }

    /// get parent stack frame
    #[inline(always)]
    pub const fn parent(&self) -> Option<&StackFrame> {
        unsafe { self.prev.cast::<StackFrame>().as_ref() }
    }

    /// Returns the base pointer of the stack in this frame.
    /// Stack frame for native callback does not have stack, calling in this case will panic.
    #[inline(always)]
    pub const fn stack_base_ptr(&self) -> NonNull<JSValue> {
        unsafe { NonNull::new(self.var_base_ptr.add(self.var_count as usize)).unwrap() }
    }

    /// Returns the current stack pointer position (points one past top value)
    ///
    /// Note: The returned pointer cannot be dereferenced directly as it refers to
    /// an invalid position. To access the top stack value, use offset(-1):
    ///
    /// `let stack_top_val = frame.stack_cur_ptr().offset(-1).as_ref();`
    #[inline(always)]
    pub(crate) const fn stack_cur_ptr(&self) -> NonNull<JSValue> {
        debug_assert!(self.cur_sp_offset >= 0);
        unsafe { self.stack_base_ptr().add(self.cur_sp_offset as usize) }
    }

    pub const fn stack_values(&self) -> &[JSValue] {
        debug_assert!(self.cur_sp_offset >= 0);
        if self.cur_sp_offset > 0 {
            unsafe {
                std::slice::from_raw_parts(
                    self.stack_base_ptr().as_ptr(),
                    self.cur_sp_offset as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// Write a value at `SP[offset]` without advancing SP.
    /// The slot must already be allocated (within the stack buffer).
    #[inline(always)]
    pub fn store(&self, offset: isize, val: JSValue) {
        unsafe {
            *self.stack_cur_ptr().offset(offset).as_ptr() = val;
        }
    }

    /// Push a value onto the stack: write at SP[0] and advance SP by 1.
    #[inline(always)]
    pub fn push(&mut self, val: JSValue) {
        unsafe {
            *self.stack_cur_ptr().as_ptr() = val;
        }
        self.cur_sp_offset += 1;
    }

    /// Take a value from the stack at `offset` (relative to SP) and protect it
    /// in the current GC scope. This is the ONLY safe way to consume a value
    /// from the bytecode stack — replaces the slot with UNDEFINED and protects
    /// any GC node held by the value.
    ///
    /// After calling this, the value at `SP[offset]` is UNDEFINED and should
    /// not be accessed again.
    ///
    /// Takes `&self` because the mutation is on the raw stack buffer, not on
    /// the StackFrame struct fields. This allows calling through `Pin<Box<Self>>`.
    pub fn take(&self, offset: isize, ctx: &ScopedContext) -> JSValue {
        let val = unsafe {
            std::ptr::replace(
                self.stack_cur_ptr().as_ptr().offset(offset),
                JSValue::UNDEFINED,
            )
        };
        ctx.gc_protect_transient(&val);
        val
    }

    /// Returns *Actual* argument values
    pub const fn arg_values(&self) -> &[JSValue] {
        if self.arg_real_count > 0 {
            unsafe { std::slice::from_raw_parts(self.arg_base_ptr, self.arg_real_count as usize) }
        } else {
            &[]
        }
    }

    /// Returns local variable values
    pub const fn var_values(&self) -> &[JSValue] {
        if self.var_count > 0 {
            unsafe { std::slice::from_raw_parts(self.var_base_ptr, self.var_count as usize) }
        } else {
            &[]
        }
    }

    #[inline(always)]
    pub const fn var_refs(&self) -> &Vec<VarRef> {
        &self.var_ref_list
    }

    #[inline(always)]
    pub const fn var_refs_mut(&mut self) -> &mut Vec<VarRef> {
        &mut self.var_ref_list
    }

    /// Handles current exception by adding backtrace information if needed
    /// Returns:
    /// - Some((pc, sp)) if exception was caught (continue execution at new position)
    /// - None if exception is uncaught (abort execution)
    #[cold]
    pub(crate) fn handle_exception(
        &mut self,
        ctx: &mut ScopedContext,
        pc: usize,
        mut sp: usize,
    ) -> Option<(usize, usize)> {
        let exc = ctx.rt().exception();
        debug_assert!(
            exc != Exception::No,
            "handle_exception called without an exception"
        );

        if let Exception::Error(err_val) = exc
            && is_backtrace_needed(err_val)
        {
            // add the backtrace information now:
            // it is not done before if the exception happens in a bytecode operation
            self.cur_pc_offset = pc;
            ctx.build_backtrace_to(err_val, None, 0);
        }

        if !exc.is_uncatchable() {
            let sp_base = self.stack_base_ptr().as_ptr();

            // find catch offset
            while sp > 0 {
                sp -= 1;
                if let JSValue::CatchOffset(pos) =
                    unsafe { std::ptr::replace(sp_base.add(sp), JSValue::UNDEFINED) }
                {
                    if pos == 0 {
                        // enumerator: close it with a throw

                        // drop the next method
                        unsafe { std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED) };
                        sp -= 1;

                        // close iterator if SP[-1] is obj
                        if let JSValue::Object(o) =
                            unsafe { std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED) }
                        {
                            let _ = ObjectIter::close_iter_internal(ctx, o);
                        }
                    } else {
                        let err_val = ctx.take_exception().to_value(ctx);
                        unsafe {
                            std::ptr::replace(sp_base.add(sp), err_val);
                        }
                        sp += 1;
                        return Some((pos as usize, sp));
                    }
                }
            }
        }

        None // exception is uncatched, abort with sp=0, pc unchanged
    }

    /// Prepare to exit bytecode execution.
    pub(crate) fn handle_exit(
        &mut self,
        ctx: &mut ScopedContext,
        pc: usize,
        sp: usize,
        is_async: bool,
    ) {
        debug_assert_eq!(
            ctx.rt().current_stack_frame(),
            Some(&*self),
            "StackFrame::handle_exit() must be called on current stack frame"
        );

        if is_async {
            // async execution, local variables are freed by the caller.
            // Hence the label 'done' should never be reached in async execution.
            self.cur_pc_offset = pc;
            self.cur_sp_offset = sp as i32;
        } else {
            // var_refs may still referencing the stack,
            // before stack frame drop, detatch all var_refs.
            self.close_var_refs();
        }

        // switch back to previous stack frame
        ctx.rt_mut().pop_stack_frame();
    }

    /// get stack frame parent iterator
    pub const fn parent_iter(&self, include_self: bool) -> StackFrameParentIter<'_> {
        StackFrameParentIter {
            current: if include_self {
                Some(NonNull::from_ref(self))
            } else {
                NonNull::new(self.prev.cast())
            },
            _mark: PhantomData,
        }
    }
}

impl Drop for StackFrame {
    fn drop(&mut self) {
        if self.local_buf.is_spill() {
            self.rt
                .recycle_frame_buf(std::mem::take(&mut self.local_buf));
        }
    }
}

pub struct StackFrameParentIter<'a> {
    current: Option<NonNull<StackFrame>>,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Iterator for StackFrameParentIter<'a> {
    type Item = &'a StackFrame;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(sf) = self.current.take() {
            unsafe {
                self.current = NonNull::new(sf.as_ref().prev.cast::<StackFrame>());
                Some(sf.as_ref())
            }
        } else {
            None
        }
    }
}

impl StackFrameParentIter<'static> {
    pub const EMPTY: Self = Self {
        current: None,
        _mark: PhantomData,
    };
}
