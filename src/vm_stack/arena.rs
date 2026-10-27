// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::UnsafeCell;

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    Object,
    config::VM_STACK_LIMIT_DEFAULT,
    context::ScopedContext,
    function::VarRef,
    value::JSValue,
    vm_stack::{AsyncStackFrame, ExecMode, FrameBuffer, FrameIndex, FrameState, StackFrame},
};

/// Bump-allocator frame storage.
pub(crate) struct VmStack {
    /// MUST stay `Vec<Box<StackFrame>>` — never downgrade to `Vec<StackFrame>`.
    ///
    /// `execute_with_arena` (exec_stackless.rs) caches `&mut frame.cur_sp_offset`
    /// / `cur_pc_offset` as raw pointers across its whole interpretation loop,
    /// and that loop makes native calls (CALL → NativeFunction::class_call →
    /// alloc_frame → push). A plain `Vec<StackFrame>` reallocs on growth,
    /// moving every element and leaving those cached pointers — plus any
    /// `frame()/frame_mut()` reference held across a call — dangling. The
    /// stale writes then corrupt freed memory reused by GC nodes and frames,
    /// surfacing far away as "bad node" panics, recycled-callback TypeErrors
    /// ("not a function") and finalize_partition index aborts. ASAN pinned
    /// this exact chain: UAF read in execute_with_arena, freed by Vec::push →
    /// grow_one → realloc.
    ///
    /// Box indirection gives per-frame address stability regardless of Vec
    /// growth; the extra dereference is negligible next to bytecode dispatch.
    #[allow(clippy::vec_box)]
    pub(crate) frames: Vec<Box<StackFrame>>,

    /// Bump region backing every `FrameBuffer::Arena`.
    ///
    /// Stored as a raw slice (`data_base` + `data_len`) rather than
    /// `Box<[JSValue]>`: moving a `Box` retags its pointee as `Unique`
    /// (Stacked Borrows), which would invalidate every frame's cached
    /// `data_base`, `sp`, `arg`/`var` and `VarRef` pointer on every move of
    /// `VmStack`. The allocation is created once with `Box::into_raw` and
    /// freed only in `Drop` / `set_arena_capacity` (the latter requires an
    /// unused arena).
    data_base: *mut JSValue,
    data_len: usize,
    pub(crate) data_used: usize,
    pub(crate) suspended_count: usize,
    pub(crate) heap_buf_cache: Vec<Box<[UnsafeCell<JSValue>]>>,

    /// Live bytes currently held by heap-allocated frames (async + spilled).
    /// Combined with `data_used` this is the total live virtual stack size in
    /// bytes, checked against `vm_stack_limit` on every frame allocation.
    live_heap_bytes: usize,
    /// Maximum total live virtual stack size in bytes (bump `data_used` +
    /// `live_heap_bytes`). Exceeding it raises a stack-depth error instead of
    /// running out of host memory.
    vm_stack_limit: usize,

    /// Closure variable references, indexed by frame index.
    /// Only frames with closure captures have a non-empty entry; most frames
    /// never pay the cost.
    ///
    /// A vector, not a hash map: the frame index *is* the slot, and the map
    /// was looked up on **every** frame allocation and release (a hash per
    /// call, on the JIT's call path too).
    /// Access via `var_refs_or_default` / `try_var_refs_mut` / `has_var_refs`.
    var_refs: Vec<Vec<VarRef>>,

    /// Freed frame headers, ready for reuse.
    ///
    /// A call allocates one header (`Box<StackFrame>`) and frees it when the
    /// frame returns; that malloc/free was on every call — the JIT's call
    /// path too. The headers are parked here instead and the next
    /// `alloc_frame` takes one, so a steady-state call allocates nothing.
    /// The `Box` indirection stays (see `frames`): the header's address must
    /// not move.
    #[allow(clippy::vec_box)]
    frame_pool: Vec<Box<StackFrame>>,

    #[cfg(debug_assertions)]
    pub(crate) stats_bump: usize,
    #[cfg(debug_assertions)]
    pub(crate) stats_heap_async: usize,
    #[cfg(debug_assertions)]
    pub(crate) stats_heap_capacity: usize,
}

/// Why a frame allocation failed.
#[derive(Debug)]
pub(crate) enum FrameAllocError {
    /// Host could not allocate the backing buffer (out of memory).
    Oom,
    /// Combined bump+heap virtual stack exceeded `vm_stack_limit`.
    Exceeded,
}

impl VmStack {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        let data = vec![JSValue::UNDEFINED; capacity].into_boxed_slice();
        let data_len = data.len();
        let data_base = Box::into_raw(data) as *mut JSValue;
        Self {
            frames: Vec::with_capacity(256),
            data_base,
            data_len,
            data_used: 0,
            suspended_count: 0,
            heap_buf_cache: Vec::new(),
            live_heap_bytes: 0,
            vm_stack_limit: VM_STACK_LIMIT_DEFAULT,
            var_refs: Vec::new(),
            frame_pool: Vec::new(),

            #[cfg(debug_assertions)]
            stats_bump: 0,
            #[cfg(debug_assertions)]
            stats_heap_async: 0,
            #[cfg(debug_assertions)]
            stats_heap_capacity: 0,
        }
    }

    /// Allocate a boxed arena region of `count` UNDEFINED slots.
    /// Returns `None` on allocation failure so callers can raise
    /// `throw_stack_fail` instead of aborting on host OOM.
    fn alloc_arena_data(count: usize) -> Option<Box<[JSValue]>> {
        let mut v: Vec<JSValue> = Vec::new();
        v.try_reserve_exact(count).ok()?;
        v.resize(count, JSValue::UNDEFINED);
        Some(v.into_boxed_slice())
    }

    /// Allocate a boxed heap frame buffer of `count` UNDEFINED slots.
    ///
    /// Elements are wrapped in `UnsafeCell` because frame data is mutated
    /// through raw pointers derived from shared references (`&self` /
    /// `&StackFrame`). Without the cell those writes would be UB under
    /// Stacked Borrows.
    fn alloc_heap_buf(count: usize) -> Option<Box<[UnsafeCell<JSValue>]>> {
        let mut v: Vec<UnsafeCell<JSValue>> = Vec::new();
        v.try_reserve_exact(count).ok()?;
        v.extend((0..count).map(|_| UnsafeCell::new(JSValue::UNDEFINED)));
        Some(v.into_boxed_slice())
    }

    #[inline(always)]
    fn reuse_or_alloc_heap_buf(&mut self, count: usize) -> Option<Box<[UnsafeCell<JSValue>]>> {
        if let Some(pos) = self.heap_buf_cache.iter().position(|b| b.len() >= count) {
            Some(self.heap_buf_cache.remove(pos))
        } else {
            Self::alloc_heap_buf(count)
        }
    }

    /// Resize the bump-arena data region. Returns `Err(())` (which the caller
    /// turns into `throw_stack_fail`) when the host cannot allocate, or when
    /// the arena has already been used (frames allocated) and cannot be safely
    /// resized.
    pub(crate) fn set_arena_capacity(&mut self, capacity: usize) -> Result<(), ()> {
        if self.data_used > 0 {
            #[cfg(debug_assertions)]
            eprintln!("can't resize stack arena: arena is in use");
            return Err(());
        }

        let data = Self::alloc_arena_data(capacity).ok_or(())?;
        let data_len = data.len();
        let data_base = Box::into_raw(data) as *mut JSValue;
        let old_base = std::mem::replace(&mut self.data_base, data_base);
        let old_len = std::mem::replace(&mut self.data_len, data_len);
        // SAFETY: the arena is unused (`data_used == 0`), so no frame buffer or
        // VarRef can still point into the old allocation.
        unsafe { Self::free_data(old_base, old_len) };
        self.data_used = 0;
        Ok(())
    }

    /// Release a bump region previously created by `Box::into_raw`.
    ///
    /// # Safety
    /// `base` must come from `Box::into_raw(Box<[JSValue]>)` with the same
    /// length and must not be referenced by any live frame.
    unsafe fn free_data(base: *mut JSValue, len: usize) {
        if !base.is_null() {
            drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(base, len)) });
        }
    }

    /// Set the maximum total live virtual stack size in bytes (bump + heap).
    pub(crate) const fn set_vm_stack_limit(&mut self, bytes: usize) {
        self.vm_stack_limit = bytes;
    }

    #[inline]
    pub(crate) fn alloc_frame(
        &mut self,
        func_obj: Object,
        mode: ExecMode,
        capacity: usize,
    ) -> Result<FrameIndex, FrameAllocError> {
        // Guard the combined virtual stack (bump + heap) before allocating.
        if self.data_used + self.live_heap_bytes + capacity > self.vm_stack_limit {
            return Err(FrameAllocError::Exceeded);
        }

        let idx = FrameIndex(self.frames.len() as u32);

        let buf = if mode.contains(ExecMode::ASYNC) {
            #[cfg(debug_assertions)]
            {
                self.stats_heap_async += 1;
            }
            let boxed = self
                .reuse_or_alloc_heap_buf(capacity)
                .ok_or(FrameAllocError::Oom)?;
            self.live_heap_bytes += boxed.len();
            FrameBuffer::Heap(boxed)
        } else if self.data_used + capacity > self.data_len {
            #[cfg(debug_assertions)]
            {
                self.stats_heap_capacity += 1;
            }
            let boxed = self
                .reuse_or_alloc_heap_buf(capacity)
                .ok_or(FrameAllocError::Oom)?;
            self.live_heap_bytes += boxed.len();
            FrameBuffer::Heap(boxed)
        } else {
            #[cfg(debug_assertions)]
            {
                self.stats_bump += 1;
            }
            // arena bump
            let buf_offset = self.data_used as u32;
            self.data_used += capacity;
            FrameBuffer::Arena {
                data_base: self.data_base,
                buf_offset,
                buf_capacity: capacity as u32,
            }
        };

        let header = self.new_frame_header(func_obj, mode, buf);
        self.frames.push(header);

        Ok(idx)
    }

    /// A frame header for a new activation: a pooled one when there is one,
    /// a fresh box otherwise.
    ///
    /// The recycled header is prepared in place (see [`StackFrame::reset`]):
    /// assigning a whole header would copy it through a stack temporary,
    /// which cost more than the malloc/free the pool avoids.
    #[inline]
    fn new_frame_header(
        &mut self,
        func_obj: Object,
        mode: ExecMode,
        buf: FrameBuffer,
    ) -> Box<StackFrame> {
        match self.frame_pool.pop() {
            Some(mut pooled) => {
                // The pooled header's buffer was taken out when it was freed,
                // so the reset below drops an empty buffer.
                pooled.reset(func_obj, mode, buf);
                pooled
            }
            None => Box::new(StackFrame {
                prev: FrameIndex::NULL,
                depth: 0,
                state: FrameState::Active,
                mode,
                func_obj,
                this_obj: JSValue::UNDEFINED,
                cur_pc_offset: 0,
                cur_sp_offset: 0,
                buf,
                arg_base_offset: 0,
                arg_real_count: 0,
                arg_count: 0,
                var_base_offset: 0,
                var_count: 0,
            }),
        }
    }

    /// Park a freed frame header for the next `alloc_frame`. The buffer must
    /// already be `Empty` — the caller owns the buffer's accounting.
    #[inline]
    fn park_frame(&mut self, frame: Box<StackFrame>) {
        debug_assert!(matches!(frame.buf, FrameBuffer::Empty));
        if self.frame_pool.len() < crate::config::FRAME_POOL_MAX {
            self.frame_pool.push(frame);
        }
    }

    #[inline]
    pub(crate) fn free_frame(&mut self, idx: FrameIndex) {
        debug_assert_eq!(self.frames[idx.0 as usize].state, FrameState::Active);

        // Clean up any closure var refs associated with this frame.
        self.clear_var_refs(idx);

        if idx.0 as usize == self.frames.len() - 1 {
            let mut head = self.frames.pop().unwrap();
            match std::mem::replace(&mut head.buf, FrameBuffer::Empty) {
                FrameBuffer::Arena { buf_capacity, .. } => {
                    self.data_used -= buf_capacity as usize;
                }
                FrameBuffer::Heap(heap_buf) => {
                    self.live_heap_bytes = self.live_heap_bytes.saturating_sub(heap_buf.len());
                    let remaining = self.data_len - self.data_used;
                    if heap_buf.len() > remaining
                        && self.heap_buf_cache.len() < crate::config::HEAP_BUF_CACHE_MAX
                    {
                        self.heap_buf_cache.push(heap_buf);
                    }
                }
                FrameBuffer::Empty => {}
            }
            while let Some(f) = self.frames.last() {
                if f.state == FrameState::Suspended {
                    self.frames.pop();
                    self.suspended_count -= 1;
                } else {
                    break;
                }
            }
            // The header is empty now — park it for the next allocation.
            self.park_frame(head);
        } else {
            let head = &mut self.frames[idx.0 as usize];
            if let FrameBuffer::Heap(heap_buf) =
                std::mem::replace(&mut head.buf, FrameBuffer::Empty)
            {
                self.live_heap_bytes = self.live_heap_bytes.saturating_sub(heap_buf.len());
                let remaining = self.data_len - self.data_used;
                if heap_buf.len() > remaining
                    && self.heap_buf_cache.len() < crate::config::HEAP_BUF_CACHE_MAX
                {
                    self.heap_buf_cache.push(heap_buf);
                }
            }
            head.state = FrameState::Suspended;
            head.prev = FrameIndex::NULL;
            self.suspended_count += 1;
        }
    }

    /// Detach every closure VarRef pointing into frame `idx`, then free the
    /// frame. Detach must happen before the free: VarRef raw pointers into
    /// the frame buffer become dangling once the region is recycled.
    /// Replaces the manually paired `try_var_refs_mut` + `detach` loop +
    /// `free_frame` pattern in the trampoline's frame-recycling paths.
    #[inline]
    pub(crate) fn detach_and_free(&mut self, idx: FrameIndex) {
        if let Some(var_refs) = self.try_var_refs_mut(idx) {
            for vr in var_refs {
                vr.detach();
            }
        }
        self.free_frame(idx);
    }

    #[inline(always)]
    pub(crate) fn frame(&self, idx: FrameIndex) -> &StackFrame {
        &self.frames[idx.0 as usize]
    }

    /// Non-panicking `frame`: returns `None` if `idx` no longer refers to a
    /// live slot (e.g. a tail call popped the frame and the replacement
    /// allocation failed).
    #[inline(always)]
    pub(crate) fn try_frame(&self, idx: FrameIndex) -> Option<&StackFrame> {
        self.frames.get(idx.0 as usize).map(|b| &**b)
    }

    #[inline(always)]
    pub(crate) fn frame_mut(&mut self, idx: FrameIndex) -> &mut StackFrame {
        &mut self.frames[idx.0 as usize]
    }

    #[inline]
    pub(crate) fn init_frame_args(
        &mut self,
        idx: FrameIndex,
        bc_arg_count: u16,
        bc_var_count: u16,
        args: &[JSValue],
    ) {
        let frame = self.frame_mut(idx);
        let arg_count = std::cmp::max(bc_arg_count as usize, args.len());

        let mut data_ptr = frame.buf.data_ptr();
        for val in args.iter().copied().chain(std::iter::repeat_n(
            JSValue::UNDEFINED,
            (arg_count + bc_var_count as usize) - args.len(),
        )) {
            unsafe {
                *data_ptr = val;
                data_ptr = data_ptr.add(1);
            }
        }

        frame.arg_base_offset = 0;
        frame.arg_real_count = args.len() as u16;
        frame.arg_count = arg_count as u16;
        frame.var_base_offset = arg_count as u32;
        frame.var_count = bc_var_count;
        frame.cur_sp_offset = 0;
    }

    #[inline(always)]
    pub(crate) fn frame_store_value(&self, idx: FrameIndex, offset: isize, val: JSValue) {
        let f = self.frame(idx);
        unsafe { *f.stack_cur_ptr().offset(offset) = val }
    }

    fn data<'a>(&'a self, head: &'a StackFrame) -> &'a [JSValue] {
        match &head.buf {
            FrameBuffer::Arena {
                buf_offset,
                buf_capacity,
                ..
            } => {
                let start = *buf_offset as usize;
                let end = start + *buf_capacity as usize;
                // SAFETY: the bump region is fixed-size and `start..end` is a
                // live frame slice; no `&mut` to the region exists while the
                // arena is being traced.
                unsafe { std::slice::from_raw_parts(self.data_base.add(start), end - start) }
            }
            FrameBuffer::Heap(b) => unsafe {
                std::slice::from_raw_parts(b.as_ptr() as *const JSValue, b.len())
            },
            FrameBuffer::Empty => &[],
        }
    }

    pub(crate) fn take_frame(&mut self, idx: FrameIndex) -> AsyncStackFrame {
        let idx_usize = idx.0 as usize;
        let is_last = idx_usize == self.frames.len() - 1;

        // Migrate the closure VarRefs out of the arena's slot (before the
        // frame borrow: the two live in the same struct).
        let mut var_refs = self.take_var_refs(idx);

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

        let heap_buf = match std::mem::replace(&mut head.buf, FrameBuffer::Empty) {
            FrameBuffer::Heap(b) => b,
            _ => unreachable!("Async frame must be Heap"),
        };

        let arg_base = head.arg_base_offset;
        let var_base = head.var_base_offset;

        for vr in &mut var_refs {
            vr.update_pvalue_for_heap_buf_offsets(&heap_buf, arg_base, var_base);
        }

        let frame = StackFrame {
            prev: FrameIndex::NULL,
            depth: head.depth,
            state: FrameState::Active,
            func_obj: head.func_obj,
            this_obj: head.this_obj,
            mode: head.mode,
            cur_pc_offset: head.cur_pc_offset,
            cur_sp_offset: head.cur_sp_offset,
            buf: FrameBuffer::Heap(heap_buf),
            arg_base_offset: head.arg_base_offset,
            arg_real_count: head.arg_real_count,
            arg_count: head.arg_count,
            var_base_offset: head.var_base_offset,
            var_count: head.var_count,
        };

        if is_last {
            // The header's buffer was moved out above, so it can be recycled.
            let head = self.frames.pop().unwrap();
            self.park_frame(head);
        } else {
            head.state = FrameState::Suspended;
            self.suspended_count += 1;
        }

        AsyncStackFrame { frame, var_refs }
    }

    pub(crate) fn put_frame(&mut self, mut frame: AsyncStackFrame) -> FrameIndex {
        let new_idx = FrameIndex(self.frames.len() as u32);
        frame.frame.prev = FrameIndex::NULL;
        frame.frame.state = FrameState::Active;

        // Re-insert closure VarRefs into the arena's map.
        if !frame.var_refs.is_empty() {
            self.set_var_refs(new_idx, frame.var_refs);
        }

        // An async frame comes back with its own header; take a pooled one
        // when there is one.
        let header = match self.frame_pool.pop() {
            Some(mut pooled) => {
                *pooled = frame.frame;
                pooled
            }
            None => Box::new(frame.frame),
        };
        self.frames.push(header);
        new_idx
    }

    pub(crate) fn find_catch_in_frame(
        &self,
        idx: FrameIndex,
        ctx: &mut ScopedContext,
    ) -> Option<(usize, usize)> {
        let head = self.frame(idx);
        let sp_base = head.stack_base_ptr();
        let mut sp = head.cur_sp_offset as usize;

        while sp > 0 {
            sp -= 1;

            if let Some(pos) =
                unsafe { std::ptr::replace(sp_base.add(sp), JSValue::UNDEFINED) }.as_catch_offset()
            {
                if pos == 0 {
                    unsafe { std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED) };
                    sp -= 1;

                    unsafe {
                        if let Some(o) =
                            std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED).as_object()
                        {
                            let _ =
                                crate::builtins::iterator::ObjectIter::close_iter_internal(ctx, o);
                        }
                    }
                } else {
                    let err_val = ctx.take_exception().to_value(ctx);
                    unsafe { std::ptr::write(sp_base.add(sp), err_val) }
                    sp += 1;
                    return Some((pos as usize, sp));
                }
            }
        }
        None
    }

    pub(crate) fn find_finally_in_frame(
        &self,
        idx: FrameIndex,
        bc_len: usize,
    ) -> Option<(usize, usize)> {
        let head = self.frame(idx);
        let sp_base = head.stack_base_ptr();
        let mut sp = head.cur_sp_offset as usize;

        while sp > 0 {
            sp -= 1;

            if let Some(pos) =
                unsafe { std::ptr::replace(sp_base.add(sp), JSValue::UNDEFINED) }.as_int()
                && pos > 0
                // A GOSUB marker encodes the (in-bounds) finally entry pc.
                // Plain integer arguments (e.g. a positive `n` in a recursive
                // call) must NOT be mistaken for a finally marker, otherwise an
                // unrelated value on the stack would hijack unwinding.
                && (pos as usize) < bc_len
            {
                unsafe { std::ptr::write(sp_base.add(sp), JSValue::from(pos)) }
                sp += 1;
                return Some((pos as usize, sp));
            }
        }
        None
    }

    // ── VarRef map accessors ────────────────────────────────────
    // These are the ONLY external API for accessing `var_ref_map`.
    // Calling code should never touch the vector directly — it must
    // go through these methods to keep the ownership / lifecycle
    // invariants centralised (free_frame cleanup, take/put migration).

    /// Get-or-create the VarRef vec for `idx`.
    /// Used by closure creation paths (get_var_ref, make_closure).
    #[inline(always)]
    pub(crate) fn var_refs_or_default(&mut self, idx: FrameIndex) -> &mut Vec<VarRef> {
        self.var_refs_mut(idx)
    }

    /// The slot for `idx`, growing the vector when the frame is new.
    #[inline(always)]
    fn var_refs_mut(&mut self, idx: FrameIndex) -> &mut Vec<VarRef> {
        let slot = idx.0 as usize;
        if self.var_refs.len() <= slot {
            self.var_refs.resize_with(slot + 1, Vec::new);
        }
        &mut self.var_refs[slot]
    }

    /// Take the frame's VarRefs out (the frame is going away).
    #[inline(always)]
    fn take_var_refs(&mut self, idx: FrameIndex) -> Vec<VarRef> {
        match self.var_refs.get_mut(idx.0 as usize) {
            Some(slot) => std::mem::take(slot),
            None => Vec::new(),
        }
    }

    /// Drop the frame's VarRefs (the frame is going away).
    #[inline(always)]
    fn clear_var_refs(&mut self, idx: FrameIndex) {
        if let Some(slot) = self.var_refs.get_mut(idx.0 as usize) {
            slot.clear();
        }
    }

    /// Store the frame's VarRefs (an async frame came back).
    #[inline(always)]
    fn set_var_refs(&mut self, idx: FrameIndex, var_refs: Vec<VarRef>) {
        *self.var_refs_mut(idx) = var_refs;
    }

    /// Get mutable access to existing VarRefs without inserting.
    /// Returns `None` when the frame has no closure variables.
    /// Used by detach / CLOSE_LOC paths that only need to act when refs exist.
    #[inline(always)]
    pub(crate) fn try_var_refs_mut(&mut self, idx: FrameIndex) -> Option<&mut Vec<VarRef>> {
        self.var_refs.get_mut(idx.0 as usize)
    }

    /// Check whether `idx` has at least one live closure variable reference.
    /// Used by debug assertions.
    #[inline(always)]
    pub(crate) fn has_var_refs(&self, idx: FrameIndex) -> bool {
        self.var_refs
            .get(idx.0 as usize)
            .is_some_and(|v| !v.is_empty())
    }
}

impl Drop for VmStack {
    fn drop(&mut self) {
        // SAFETY: `data_base`/`data_len` come from `Box::into_raw` and the
        // arena is being destroyed, so no frame or VarRef can still use it.
        unsafe { Self::free_data(self.data_base, self.data_len) };

        #[cfg(debug_assertions)]
        {
            let total = self.stats_bump + self.stats_heap_async + self.stats_heap_capacity;
            if total == 0 {
                return;
            }

            // async 帧固定走 heap 是设计选择（需存活跨越 suspend/resume），
            // 不影响性能。仅检查容量不足导致的 heap fallback。
            //
            // 只警告不断言：深递归（如 test262 的无限递归用例，靠 stack-exceeded
            // 异常终止）会合法地占满 arena 数据区，之后每层调用都 fallback，
            // 命中率暴跌。Drop 处无法区分这种负载与容量配置过小。
            let cap_total = self.stats_bump + self.stats_heap_capacity;
            if cap_total > 0 {
                let cap_bump_pct = self.stats_bump as f64 / cap_total as f64 * 100.0;
                if cap_bump_pct < 99.5 {
                    eprintln!(
                        "[WARN] VmStack bump_rate={:.1}% (bump={}, non_bump: async={}, cap={}, total={}, data_cap={})",
                        cap_bump_pct,
                        self.stats_bump,
                        self.stats_heap_async,
                        self.stats_heap_capacity,
                        total,
                        self.data_len,
                    );
                }
            }
        }
    }
}

impl GcTrace for VmStack {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        for (i, frame) in self.frames.iter().enumerate() {
            if frame.state != FrameState::Active {
                continue;
            }

            frame.func_obj.trace(gcx);
            frame.this_obj.trace(gcx);

            let data = self.data(frame);
            let data_end = frame.buf.capacity() as usize;

            let cnt = std::cmp::max(frame.arg_real_count, frame.arg_count) as usize;
            for v in data.iter().take(cnt.min(data_end)) {
                v.trace(gcx);
            }

            let var_start = frame.var_base_offset as usize;
            let total_active =
                var_start + frame.var_count as usize + frame.cur_sp_offset.max(0) as usize;
            for v in &data[var_start..std::cmp::min(total_active, data_end)] {
                v.trace(gcx);
            }

            let idx = FrameIndex(i as u32);
            if let Some(var_refs) = self.var_refs.get(idx.0 as usize) {
                for vr in var_refs {
                    vr.trace(gcx);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EvalFlags, EvalName, EvalType, Runtime, ScopedContext};
    use gc_lite::GcHead;
    use std::ptr::NonNull;

    /// A call must not allocate its frame header: the freed header is parked
    /// and the next call takes it (the malloc/free was on every call).
    ///
    /// The recycled header must also carry nothing from its previous
    /// activation — a stale argument count would show up as a wrong
    /// `arguments.length`.
    #[test]
    fn the_frame_headers_are_recycled() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        let name = EvalName::Name("frame-pool.js".into());

        ctx.eval(
            b"function three(a, b, c){ return arguments.length; } \
              function none(){ return arguments.length; } \
              function many(){ var v0 = 0, v1 = 0; return arguments.length; } \
              three(1, 2, 3) * 100 + none() * 10 + many(1, 2, 3, 4, 5);",
            &name,
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();

        // Every call freed its frame, so the headers are parked for reuse.
        let parked = rt.vm_stack.frame_pool.len();
        assert!(parked >= 1, "no frame header was recycled");

        // The next call takes a parked header and parks it again: the pool
        // must not grow.
        ctx.eval(b"none();", &name, EvalType::Global, EvalFlags::empty())
            .unwrap();
        assert_eq!(
            rt.vm_stack.frame_pool.len(),
            parked,
            "the frame headers are not reused"
        );
    }

    /// The recycled header must not leak the previous activation's shape.
    #[test]
    fn a_recycled_header_carries_no_previous_state() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        let v = ctx
            .eval(
                b"function three(a, b, c){ return arguments.length; } \
                  function none(){ return arguments.length; } \
                  three(1, 2, 3) * 100 + none() * 10;",
                &EvalName::Name("frame-recycle.js".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .unwrap();
        // 300: `none()` saw its own zero arguments, not `three`'s three.
        assert_eq!(v.as_int(), Some(300), "the argument count leaked");
    }

    /// The arena layer only stores `func_obj`; it never dereferences it.
    /// A dangling non-null handle is therefore enough for frame tests and
    /// lets them run under Miri without a `RuntimeImpl` (whose linkme-based
    /// class table Miri cannot execute).
    fn fake_object() -> Object {
        Object(unsafe { gc_lite::GcRef::from_node_ptr(NonNull::<GcHead>::dangling()) })
    }

    fn alloc(arena: &mut VmStack, cap: usize) -> FrameIndex {
        arena
            .alloc_frame(fake_object(), ExecMode::empty(), cap)
            .unwrap()
    }

    /// Frame data pointers must stay valid across later frame allocations and
    /// frees: `frames` is a `Vec<Box<_>>` and the bump region never reallocs.
    /// This is the Miri-checkable core of the interpreter's pointer discipline.
    #[test]
    fn frame_data_ptr_survives_alloc_free() {
        let mut arena = VmStack::with_capacity(256);
        let first = alloc(&mut arena, 8);
        arena.init_frame_args(first, 1, 2, &[JSValue::from(7)]);

        let base = arena.frame(first).buf.data_ptr();
        assert_eq!(unsafe { *base }, JSValue::from(7));

        // Grow `frames` (Box keeps headers stable) and advance the bump region.
        let mut later = Vec::new();
        for _ in 0..16 {
            later.push(alloc(&mut arena, 8));
        }
        assert_eq!(arena.frame(first).buf.data_ptr(), base);

        // Write through the pointer captured before the growth.
        unsafe { *base = JSValue::from(42) };
        assert_eq!(arena.frame(first).buf.data_ptr(), base);
        assert_eq!(unsafe { *base }, JSValue::from(42));

        // Free the later frames (LIFO) and reuse the bump region.
        for idx in later.into_iter().rev() {
            arena.free_frame(idx);
        }
        assert_eq!(arena.frame(first).buf.data_ptr(), base);
        assert_eq!(unsafe { *base }, JSValue::from(42));
    }

    /// `frame_store_value` must observe the same data region as the pointer
    /// derived at allocation time, including after the region is recycled.
    #[test]
    fn frame_store_value_after_recycle() {
        let mut arena = VmStack::with_capacity(64);
        let first = alloc(&mut arena, 4);
        let base = arena.frame(first).buf.data_ptr();

        for _ in 0..8 {
            let tmp = alloc(&mut arena, 4);
            arena.frame_store_value(tmp, 0, JSValue::from(1));
            arena.free_frame(tmp);
        }

        arena.frame_store_value(first, 0, JSValue::from(9));
        assert_eq!(arena.frame(first).buf.data_ptr(), base);
        assert_eq!(unsafe { *base }, JSValue::from(9));
    }

    /// Frame headers live behind `Box`, so `frames` growth must not move them.
    /// The interpreter keeps `FrameIndex` plus raw data pointers across calls;
    /// this pins the address-stability half of that contract.
    #[test]
    fn frame_header_address_is_stable_across_growth() {
        let mut arena = VmStack::with_capacity(256);
        let first = alloc(&mut arena, 4);
        arena.frame_mut(first).depth = 7;

        let head: *const StackFrame = arena.frame(first);
        let mut later = Vec::new();
        for _ in 0..32 {
            later.push(alloc(&mut arena, 4));
        }

        assert_eq!(arena.frame(first) as *const StackFrame, head);
        assert_eq!(unsafe { (*head).depth }, 7);
    }

    /// Async frames are Heap-backed: `take_frame`/`put_frame` move the header
    /// (and its buffer `Box`) out and back in. Values and frame accounting must
    /// survive the round-trip, which is the CALL/RETURN-adjacent path Miri can
    /// exercise without a runtime.
    #[test]
    fn async_frame_take_put_roundtrip() {
        let mut arena = VmStack::with_capacity(64);
        let idx = arena
            .alloc_frame(fake_object(), ExecMode::ASYNC, 8)
            .unwrap();
        arena.init_frame_args(idx, 2, 2, &[JSValue::from(11), JSValue::from(22)]);
        arena.frame_store_value(idx, 0, JSValue::from(33));

        let snapshot = arena.take_frame(idx);
        assert_eq!(snapshot.frame.arg_real_count, 2);
        assert_eq!(snapshot.frame.arg_count, 2);
        assert_eq!(snapshot.frame.var_count, 2);
        assert_eq!(snapshot.frame.this_obj, JSValue::UNDEFINED);

        let new_idx = arena.put_frame(snapshot);
        assert_eq!(arena.frame(new_idx).arg_real_count, 2);
        let head = arena.frame(new_idx);
        assert_eq!(unsafe { *head.buf.data_ptr().add(4) }, JSValue::from(33));
        arena.frame_store_value(new_idx, 0, JSValue::from(44));
        assert_eq!(unsafe { *head.buf.data_ptr().add(4) }, JSValue::from(44));
        arena.free_frame(new_idx);
    }

    /// `set_arena_capacity` must refresh the cached base pointer; allocations
    /// after a resize have to target the new buffer.
    #[test]
    fn resize_refreshes_data_base() {
        let mut arena = VmStack::with_capacity(8);
        arena.set_arena_capacity(128).unwrap();

        let idx = alloc(&mut arena, 16);
        arena.frame_store_value(idx, 0, JSValue::from(5));
        let base = arena.frame(idx).buf.data_ptr();
        assert_eq!(unsafe { *base }, JSValue::from(5));
    }

    /// A freed Heap buffer is parked in `heap_buf_cache` (when larger than the
    /// free arena region) and handed to the next allocation. The reused buffer
    /// must keep the `UnsafeCell` write permission.
    #[test]
    fn heap_buf_cache_reused_after_free() {
        let mut arena = VmStack::with_capacity(4);
        let idx = arena
            .alloc_frame(fake_object(), ExecMode::ASYNC, 8)
            .unwrap();
        let cap = arena.frame(idx).buf.capacity();
        assert_eq!(cap, 8);
        arena.init_frame_args(idx, 1, 1, &[JSValue::from(1)]);
        assert_eq!(
            unsafe { *arena.frame(idx).buf.data_ptr() },
            JSValue::from(1)
        );

        let base = arena.frame(idx).buf.data_ptr();
        arena.free_frame(idx);
        assert_eq!(arena.heap_buf_cache.len(), 1);
        assert_eq!(arena.live_heap_bytes, 0);

        // A smaller request reuses the cached (larger) buffer as-is.
        let idx = arena
            .alloc_frame(fake_object(), ExecMode::ASYNC, 4)
            .unwrap();
        assert_eq!(arena.frame(idx).buf.capacity(), cap);
        assert_eq!(arena.frame(idx).buf.data_ptr(), base);

        arena.init_frame_args(idx, 1, 0, &[JSValue::from(2)]);
        assert_eq!(
            unsafe { *arena.frame(idx).buf.data_ptr() },
            JSValue::from(2)
        );
        arena.frame_store_value(idx, 0, JSValue::from(3));
        let sp = unsafe {
            arena
                .frame(idx)
                .buf
                .data_ptr()
                .add(arena.frame(idx).arg_count as usize)
        };
        assert_eq!(unsafe { *sp }, JSValue::from(3));
        arena.free_frame(idx);
    }

    /// When the bump region is exhausted, frames spill to Heap. Their data must
    /// stay stable across later allocations and frees, same as Arena frames.
    #[test]
    fn spilled_frame_uses_heap_and_survives_growth() {
        let mut arena = VmStack::with_capacity(16);
        let first = alloc(&mut arena, 16);
        assert!(matches!(arena.frame(first).buf, FrameBuffer::Arena { .. }));

        let spilled = alloc(&mut arena, 8);
        assert!(matches!(arena.frame(spilled).buf, FrameBuffer::Heap(_)));
        arena.init_frame_args(spilled, 1, 0, &[JSValue::from(5)]);
        let base = arena.frame(spilled).buf.data_ptr();
        assert_eq!(unsafe { *base }, JSValue::from(5));

        let mut later = Vec::new();
        for _ in 0..4 {
            later.push(alloc(&mut arena, 4));
        }
        assert_eq!(arena.frame(spilled).buf.data_ptr(), base);

        arena.frame_store_value(spilled, 0, JSValue::from(6));
        let sp = unsafe { base.add(arena.frame(spilled).arg_count as usize) };
        assert_eq!(unsafe { *sp }, JSValue::from(6));

        for idx in later.into_iter().rev() {
            arena.free_frame(idx);
        }
        arena.free_frame(spilled);
    }

    /// Repeated resizes must free each previous region exactly once. The test
    /// body only exercises the paths; Miri's leak/double-free checks are the
    /// actual assertion.
    #[test]
    fn resize_twice_then_drop() {
        let mut arena = VmStack::with_capacity(8);
        arena.set_arena_capacity(32).unwrap();
        arena.set_arena_capacity(64).unwrap();

        let idx = alloc(&mut arena, 4);
        arena.frame_store_value(idx, 0, JSValue::from(1));
        assert_eq!(
            unsafe { *arena.frame(idx).buf.data_ptr() },
            JSValue::from(1)
        );
        drop(arena);
    }
}
