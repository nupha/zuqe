// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{mem::ManuallyDrop, ptr::NonNull};

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{Object, context::ScopedContext, function::VarRef, value::JSValue};

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

/// Index into StackFrameArena::frames, equivalent to a pointer but not
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
    /// Async frame has been suspended at a yield/await point;
    /// its data has been transferred to an AsyncStackFrame snapshot.
    Suspended,
}

/// Location of a frame's data buffer (args + vars + operand stack).
/// The same FrameHead type can represent arena-resident or heap-resident
/// frames, eliminating the need for separate snapshot types.
#[derive(Debug)]
pub(crate) enum FrameBuffer {
    /// Data stored in StackFrameArena's shared fixed-capacity data slice
    /// (bump-allocated). `data_base` is the arena's data pointer (stable for
    /// the lifetime of the arena — `Box<[JSValue]>` never reallocates);
    /// `buf_offset` is the offset within that slice.
    Arena {
        /// Raw pointer to the arena's data slice base.
        data_base: *mut JSValue,
        /// Start offset within the data slice, in JSValue units.
        buf_offset: u32,
    },
    /// Data stored in an independent heap allocation (async frames,
    /// arena-overflow sync frames).
    Heap(Box<[JSValue]>),
    /// Slot has been suspended/freed; data region is invalid.
    /// Accessing this variant is a logic bug.
    Empty,
}

impl FrameBuffer {
    /// Resolve to a raw pointer into the data buffer.
    /// For the `Arena` variant the base address was stored at allocation time.
    #[inline(always)]
    pub(crate) fn data_ptr(&self) -> *mut JSValue {
        match self {
            FrameBuffer::Arena {
                data_base,
                buf_offset,
            } => unsafe { data_base.add(*buf_offset as usize) },
            FrameBuffer::Heap(b) => b.as_ptr() as *mut JSValue,
            FrameBuffer::Empty => std::ptr::null_mut(),
        }
    }
}

/// Fixed-size header for every stack frame in the arena.
///
/// FrameHead is stored in StackFrameArena::frames (a Vec). It can be safely
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

    /// Variable references for closure capture.
    /// Each VarRef holds a raw pointer (pvalue) into the data buffer.
    /// After take/put operations, all pvalues must be updated to point
    /// to the correct data buffer location.
    pub(crate) var_ref_list: Vec<VarRef>,

    /// Where the frame's data buffer lives.
    pub(crate) buf: FrameBuffer,
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

    /// Protected values: this_obj, new_target (deprecated but needed for compat).
    #[deprecated(note = "unused")]
    pub(crate) protected_values: [JSValue; 2],
}

/// Bump-allocator frame storage.
///
/// All active FrameHead entries are stored contiguously in `frames`.
/// `data` is a fixed-capacity Box<[JSValue]> (allocated once, never resized)
/// to guarantee pointer stability for Arena-variant frames.
///
/// When the arena is full, new sync frame data is allocated on the heap
/// and cached in heap_buf_cache for reuse. Async frames always use Heap.
pub(crate) struct StackFrameArena {
    /// Frame headers array, logically equivalent to the JS call stack.
    ///
    /// **Invariant**: `alloc_frame` always appends at `frames.len()` and
    /// `free_frame` hot-LIFO-pops the last element when `idx == len-1`.
    /// For synchronous execution the `frames` order is identical to call-stack
    /// order: deeper calls have larger indices.  LIFO return keeps `frames`
    /// compact; no holes, no gaps.
    ///
    /// **Exception**: async frames (generator/async/async-generator) that are
    /// `take_frame`'d (Suspended) while outer synchronous frames continue create
    /// Suspended tombstone slots that are NOT hot-LIFO-removable (the outer
    /// frame sits above them).  These are reclaimed by `compact()` at GC
    /// safe points.
    pub(crate) frames: Vec<FrameHead>,

    /// Fixed-capacity buffer space for arena.
    pub(crate) data: Box<[JSValue]>,
    /// Bump pointer within data (next free slot).
    pub(crate) data_used: usize,

    /// Count of Suspended tombstone slots (triggers compact when threshold exceeded).
    pub(crate) suspended_count: usize,

    /// Cache of heap-allocated frame data buffers recycled from freed frames.
    pub(crate) heap_buf_cache: Vec<Box<[JSValue]>>,
}

impl StackFrameArena {
    /// Arena data capacity in JSValue slots
    pub(crate) const STACK_FRAME_ARENA_CAP: usize = 512 * 1024 / std::mem::size_of::<JSValue>();

    /// Maximum number of cached heap buffers before eviction.
    const HEAP_BUF_CACHE_MAX: usize = 8;

    pub(crate) fn new() -> Self {
        Self {
            frames: Vec::with_capacity(256),
            data: vec![JSValue::UNDEFINED; Self::STACK_FRAME_ARENA_CAP].into_boxed_slice(),
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
        let idx = FrameIndex(self.frames.len() as u32);

        let buf = if mode.contains(ExecMode::ASYNC) {
            // Async frames are always use heap-allocated
            FrameBuffer::Heap(self.reuse_or_alloc_heap_buf(buf_capacity))
        } else if self.data_used + buf_capacity <= self.data.len() {
            // Sync frame fits in arena: bump-allocate.
            let buf_offset = self.data_used as u32;
            self.data_used += buf_capacity;
            FrameBuffer::Arena {
                data_base: self.data.as_mut_ptr(),
                buf_offset,
            }
        } else {
            // Arena full for this frame: use heap (cached or new).
            FrameBuffer::Heap(self.reuse_or_alloc_heap_buf(buf_capacity))
        };

        self.frames.push(FrameHead {
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
    fn reuse_or_alloc_heap_buf(&mut self, required: usize) -> Box<[JSValue]> {
        if let Some(pos) = self.heap_buf_cache.iter().position(|b| b.len() >= required) {
            self.heap_buf_cache.remove(pos)
        } else {
            vec![JSValue::UNDEFINED; required].into_boxed_slice()
        }
    }

    /// Logically free a frame.
    ///
    /// LIFO path (frame is last in frames): pop FrameHead + rewind data_used,
    /// then cascade-pop any Suspended tombstone slots that are now exposed at
    /// the new end of the Vec (they were blocked by this frame above them).
    /// This keeps `frames` compact for the common LIFO-return case without
    /// waiting for an explicit `compact()` pass.
    ///
    /// Non-LIFO path: mark Suspended, wait for compact.
    ///
    /// ⚠️ Caller MUST ensure var_ref_list has been closed (close_var_refs)
    /// before calling free_frame on sync frames. VarRef raw pointers into
    /// arena.data become dangling after the region is reused.
    pub(crate) fn free_frame(&mut self, idx: FrameIndex) {
        debug_assert_eq!(self.frames[idx.0 as usize].state, FrameState::Active);

        if idx.0 as usize == self.frames.len() - 1 {
            // Hot LIFO path: pop FrameHead and rewind arena bump pointer.
            let head = self.frames.pop().unwrap();
            match head.buf {
                FrameBuffer::Arena { .. } => {
                    self.data_used -= head.buf_capacity as usize;
                    // Arena slots are NOT zeroed — subsequent bump allocations
                    // will naturally overwrite them.
                }
                FrameBuffer::Heap(heap_buf) => {
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
                FrameBuffer::Empty => {
                    // Suspended frame data already migrated; only FrameHead freed.
                }
            }

            // Cascade-pop: after removing the active frame, any Suspended
            // tombstones now exposed at the new end can also be popped
            // safely — no remaining active frame's prev points to them.
            //
            // Safety arguments:
            // - Suspended frames own no live data (buf=None after take_frame).
            // - No external reference permanently pins a Suspended frame index.
            //   AsyncStateImpl.frame_idx is overwritten by put_frame on resume.
            // - GC trace skips Suspended frames entirely.
            // - prev pointers from remaining active frames never target a
            //   Suspended frame that is above them in the Vec (prev always
            //   points to a lower index).
            while let Some(f) = self.frames.last() {
                if f.state == FrameState::Suspended {
                    self.frames.pop();
                    self.suspended_count -= 1;
                } else {
                    break;
                }
            }
        } else {
            // Cold path: async frame not at stack top (blocked by sibling frames).
            // Mark Suspended to preserve prev chain; heap buffer is recycled.
            let head = &mut self.frames[idx.0 as usize];

            if let FrameBuffer::Heap(heap_buf) =
                std::mem::replace(&mut head.buf, FrameBuffer::Empty)
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
            head.prev = FrameIndex::NULL;

            self.suspended_count += 1;
        }
    }

    /// Get immutable reference to a frame header by index.
    #[inline(always)]
    pub(crate) fn frame(&self, idx: FrameIndex) -> &FrameHead {
        &self.frames[idx.0 as usize]
    }

    /// Get mutable reference to a frame header by index.
    ///
    /// SAFETY: Only the currently executing frame may be mutably accessed.
    /// The trampoline design guarantees this: only cur_frame_idx is ever
    /// passed to frame_mut.
    #[inline(always)]
    pub(crate) fn frame_mut(&mut self, idx: FrameIndex) -> &mut FrameHead {
        &mut self.frames[idx.0 as usize]
    }

    /// Get stack base pointer: the start of the operand stack region,
    /// immediately after the local variables area.
    #[inline(always)]
    pub(crate) fn stack_base_ptr(&self, head: &FrameHead) -> *mut JSValue {
        unsafe {
            head.buf
                .data_ptr()
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
            head.buf.data_ptr()
        };
        let head = self.frame_mut(idx);

        // Fill all slots: copied args → remaining slots UNDEFINED (arg holes + var region).
        // The var region is NOT implicitly zero-initialized: although arena data
        // starts as vec![UNDEFINED], after frames are freed and reallocated,
        // stale data from previous frames can leak into the var slots.
        for (i, val) in args
            .iter()
            .copied()
            .chain(std::iter::repeat_n(
                JSValue::UNDEFINED,
                (arg_count + bc_var_count as usize) - args.len(),
            ))
            .enumerate()
        {
            unsafe {
                *data_ptr.add(i) = val;
            }
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
            FrameBuffer::Arena { buf_offset, .. } => {
                let start = *buf_offset as usize;
                let end = start + head.buf_capacity as usize;
                &self.data[start..end]
            }
            FrameBuffer::Heap(b) => b,
            FrameBuffer::Empty => &[],
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
    /// - If frame is last in frames: pop directly (hot path, O(1), no tombstone).
    /// - Otherwise: mark Suspended, leave tombstone for later compact.
    pub(crate) fn take_frame(&mut self, idx: FrameIndex) -> AsyncStackFrame {
        let idx_usize = idx.0 as usize;
        let is_last = idx_usize == self.frames.len() - 1;

        let head = &mut self.frames[idx_usize];
        debug_assert_eq!(head.state, FrameState::Active);
        debug_assert!(
            head.mode.contains(ExecMode::ASYNC),
            "take_frame: only async frames can be suspended"
        );
        debug_assert!(
            matches!(head.buf, FrameBuffer::Heap(_)),
            "take_frame: async frames must be Heap-variant"
        );

        // Take ownership of the Heap buffer, replace with None.
        let heap_buf = match std::mem::replace(&mut head.buf, FrameBuffer::Empty) {
            FrameBuffer::Heap(b) => b,
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
            buf: FrameBuffer::Heap(heap_buf),
            buf_capacity: head.buf_capacity,
            arg_base_offset: head.arg_base_offset,
            arg_real_count: head.arg_real_count,
            arg_count: head.arg_count,
            var_base_offset: head.var_base_offset,
            var_count: head.var_count,
        };

        if is_last {
            // Hot path: frame is at top of frames, pop without leaving tombstone.
            // No other frame's prev points to this index.
            self.frames.pop();
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
    /// The snapshot's FrameHead (with Heap buffer) is pushed onto frames.
    /// No arena bump allocation, no data copy, no VarRef pvalue update needed
    /// (pvalues already point into the Heap Box, which remains stable).
    pub(crate) fn put_frame(&mut self, mut snapshot: AsyncStackFrame) -> FrameIndex {
        let new_idx = FrameIndex(self.frames.len() as u32);

        // prev will be set by trampoline based on the call stack at resume time.
        snapshot.head.prev = FrameIndex::NULL;
        snapshot.head.state = FrameState::Active;

        self.frames.push(snapshot.head);
        new_idx
    }

    /// Compact frames: remove all remaining Suspended tombstone slots and remap
    /// prev indices.  The common LIFO-return case has already been handled by
    /// `free_frame`'s cascade-pop; this pass only needs to clean up the
    /// occasional non-tail Suspended slots that could not be cascade-popped.
    ///
    /// Must be called at a GC safe point when:
    /// - Trampoline is not running (VirtualFrames are cleared).
    /// - All AsyncStateImpl frames are Suspended (detached from arena).
    /// - Active frames are only referenced via FrameHead.prev chain.
    pub(crate) fn compact(&mut self) {
        if self.suspended_count == 0 {
            return;
        }

        let old_len = self.frames.len();
        let mut mapping: Vec<Option<u32>> = Vec::with_capacity(old_len);
        let mut new_frames: Vec<FrameHead> = Vec::with_capacity(old_len);

        // Pass 1: filter out Suspended slots, build index mapping.
        for head in self.frames.drain(..) {
            if head.state == FrameState::Suspended {
                mapping.push(None);
            } else {
                mapping.push(Some(new_frames.len() as u32));
                new_frames.push(head);
            }
        }

        // Pass 2: remap prev indices in surviving frames.
        for head in &mut new_frames {
            if !head.prev.is_null() {
                let old_idx = head.prev.0 as usize;
                if let Some(&Some(new_idx)) = mapping.get(old_idx) {
                    head.prev = FrameIndex(new_idx);
                }
            }
        }

        self.frames = new_frames;
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

    /// Search the frame's operand stack for a GOSUB return address (finally block marker).
    ///
    /// When a try-finally block is entered, the compiler emits a `GOSUB` instruction
    /// that pushes `JSValue::Int(return_pc)` onto the operand stack. On exception
    /// propagation, this marker must be found so the finally block executes before
    /// the exception propagates upward.
    ///
    /// Returns `Some((finally_pc, finally_sp))` if a GOSUB marker is found,
    /// or `None` if no finally block needs to be executed in this frame.
    ///
    /// ## Implementation Notes
    ///
    /// The scanning logic is structurally identical to `find_catch_in_frame`, except:
    /// - It looks for `JSValue::Int` (GOSUB return PC) instead of `JSValue::CatchOffset`
    /// - The found marker is left at the stack top (for `RET` to consume it)
    /// - Values between the old top and the marker are cleared to `UNDEFINED`
    ///
    /// ## Known Limitation
    ///
    /// GOSUB uses `JSValue::Int` without a dedicated tag — the same "security flaw"
    /// noted in `exec.rs`'s GOSUB handler (`XXX: should have a different tag to avoid
    /// security flaw`). Regular integer values on the operand stack could be
    /// mistaken for GOSUB markers. In practice this is extremely rare because:
    /// 1. GOSUB markers are small positive values (PC offsets within bytecode)
    /// 2. They are placed at deterministic stack depths by the compiler
    /// 3. The scanning clears consumed intermediate values
    pub(crate) fn find_finally_in_frame(&self, idx: FrameIndex) -> Option<(usize, usize)> {
        let head = self.frame(idx);
        let sp_base = self.stack_base_ptr(head);
        let mut sp = head.cur_sp_offset as usize;

        // Scan from top of stack downward, looking for JSValue::Int (GOSUB marker).
        // GOSUB return addresses are always positive PC offsets.
        while sp > 0 {
            sp -= 1;
            if let JSValue::Int(pos) =
                unsafe { std::ptr::replace(sp_base.add(sp), JSValue::UNDEFINED) }
            {
                if pos > 0 {
                    // Found potential GOSUB marker. Replace it to keep it on
                    // the cleaned stack (RET reads SP[-1]).
                    unsafe {
                        std::ptr::write(sp_base.add(sp), JSValue::Int(pos));
                    }
                    // sp is now at the GOSUB marker position; increment so
                    // the marker is at SP[-1] (stack top).
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
        let data_ptr = head.buf.data_ptr();
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

        let vref = VarRef::create_bind(ctx, var_idx, is_arg, pvalue)?;
        var_ref_list.push(vref);
        Ok(vref)
    }
}

impl AsyncStackFrame {
    /// Get immutable slice of the snapshot's data buffer.
    pub(crate) fn data_slice(&self) -> &[JSValue] {
        match &self.head.buf {
            FrameBuffer::Heap(b) => b,
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
        for head in &self.frames {
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
