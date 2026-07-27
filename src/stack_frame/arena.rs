// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{collections::HashMap, ptr::NonNull};

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    Object,
    context::ScopedContext,
    function::VarRef,
    stack_frame::{AsyncStackFrame, ExecMode, FrameBuffer, FrameIndex, FrameState, StackFrame},
    value::JSValue,
};

/// Bump-allocator frame storage.
pub(crate) struct StackFrameArena {
    pub(crate) frames: Vec<StackFrame>,
    pub(crate) data: Box<[JSValue]>,
    pub(crate) data_used: usize,
    pub(crate) suspended_count: usize,
    pub(crate) heap_buf_cache: Vec<Box<[JSValue]>>,

    /// Live bytes currently held by heap-allocated frames (async + spilled).
    /// Combined with `data_used` this is the total live virtual stack size in
    /// bytes, checked against `max_stack_bytes` on every frame allocation.
    live_heap_bytes: usize,
    /// Maximum total live virtual stack size in bytes (bump `data_used` +
    /// `live_heap_bytes`). Exceeding it raises a stack-depth error instead of
    /// running out of host memory.
    max_stack_bytes: usize,

    /// Closure variable references keyed by frame index.
    /// Only frames with closure captures have entries here;
    /// most frames never pay the cost.
    /// Access via `var_refs_or_default` / `try_var_refs_mut` / `has_var_refs`.
    var_ref_map: HashMap<FrameIndex, Vec<VarRef>>,

    #[cfg(debug_assertions)]
    pub(crate) stats_bump: usize,
    #[cfg(debug_assertions)]
    pub(crate) stats_heap_async: usize,
    #[cfg(debug_assertions)]
    pub(crate) stats_heap_capacity: usize,
}

/// Why a frame allocation failed.
pub(crate) enum FrameAllocError {
    /// Host could not allocate the backing buffer (out of memory).
    Oom,
    /// Combined bump+heap virtual stack exceeded `max_stack_bytes`.
    Exceeded,
}

impl StackFrameArena {
    /// Default maximum total live virtual stack size in bytes (bump + heap).
    const STACK_LIMIT_DEFAULT: usize = 512 * 1024;
    const HEAP_BUF_CACHE_MAX: usize = 8;

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            frames: Vec::with_capacity(256),
            data: vec![JSValue::UNDEFINED; capacity].into_boxed_slice(),
            data_used: 0,
            suspended_count: 0,
            heap_buf_cache: Vec::new(),
            live_heap_bytes: 0,
            max_stack_bytes: Self::STACK_LIMIT_DEFAULT,
            var_ref_map: HashMap::new(),

            #[cfg(debug_assertions)]
            stats_bump: 0,
            #[cfg(debug_assertions)]
            stats_heap_async: 0,
            #[cfg(debug_assertions)]
            stats_heap_capacity: 0,
        }
    }

    /// Allocate a boxed `[JSValue]` buffer of `count` UNDEFINED slots.
    /// Returns `None` on allocation failure so callers can raise
    /// `throw_stack_fail` instead of aborting on host OOM.
    fn alloc_boxed_slice(count: usize) -> Option<Box<[JSValue]>> {
        let mut v: Vec<JSValue> = Vec::new();
        v.try_reserve_exact(count).ok()?;
        v.resize(count, JSValue::UNDEFINED);
        Some(v.into_boxed_slice())
    }

    #[inline(always)]
    fn reuse_or_alloc_heap_buf(&mut self, count: usize) -> Option<Box<[JSValue]>> {
        if let Some(pos) = self.heap_buf_cache.iter().position(|b| b.len() >= count) {
            Some(self.heap_buf_cache.remove(pos))
        } else {
            Self::alloc_boxed_slice(count)
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

        self.data = Self::alloc_boxed_slice(capacity).ok_or(())?;
        self.data_used = 0;
        Ok(())
    }

    /// Set the maximum total live virtual stack size in bytes (bump + heap).
    pub(crate) const fn set_stack_limit(&mut self, bytes: usize) {
        self.max_stack_bytes = bytes;
    }

    pub(crate) fn alloc_frame(
        &mut self,
        func_obj: Object,
        mode: ExecMode,
        capacity: usize,
    ) -> Result<FrameIndex, FrameAllocError> {
        // Guard the combined virtual stack (bump + heap) before allocating.
        if self.data_used + self.live_heap_bytes + capacity > self.max_stack_bytes {
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
        } else if self.data_used + capacity > self.data.len() {
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
                data_base: self.data.as_mut_ptr(),
                buf_offset,
                buf_capacity: capacity as u32,
            }
        };

        self.frames.push(StackFrame {
            prev: FrameIndex::NULL,
            depth: 0,
            state: FrameState::Active,
            mode,
            func_obj,
            cur_pc_offset: 0,
            cur_sp_offset: 0,
            buf,
            arg_base_offset: 0,
            arg_real_count: 0,
            arg_count: 0,
            var_base_offset: 0,
            var_count: 0,
        });

        Ok(idx)
    }

    pub(crate) fn free_frame(&mut self, idx: FrameIndex) {
        debug_assert_eq!(self.frames[idx.0 as usize].state, FrameState::Active);

        // Clean up any closure var refs associated with this frame.
        self.var_ref_map.remove(&idx);

        if idx.0 as usize == self.frames.len() - 1 {
            let head = self.frames.pop().unwrap();
            match head.buf {
                FrameBuffer::Arena { buf_capacity, .. } => {
                    self.data_used -= buf_capacity as usize;
                }
                FrameBuffer::Heap(heap_buf) => {
                    self.live_heap_bytes = self.live_heap_bytes.saturating_sub(heap_buf.len());
                    let remaining = self.data.len() - self.data_used;
                    if heap_buf.len() > remaining
                        && self.heap_buf_cache.len() < Self::HEAP_BUF_CACHE_MAX
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
        } else {
            let head = &mut self.frames[idx.0 as usize];
            if let FrameBuffer::Heap(heap_buf) =
                std::mem::replace(&mut head.buf, FrameBuffer::Empty)
            {
                self.live_heap_bytes = self.live_heap_bytes.saturating_sub(heap_buf.len());
                let remaining = self.data.len() - self.data_used;
                if heap_buf.len() > remaining
                    && self.heap_buf_cache.len() < Self::HEAP_BUF_CACHE_MAX
                {
                    self.heap_buf_cache.push(heap_buf);
                }
            }
            head.state = FrameState::Suspended;
            head.prev = FrameIndex::NULL;
            self.suspended_count += 1;
        }
    }

    #[inline(always)]
    pub(crate) fn frame(&self, idx: FrameIndex) -> &StackFrame {
        &self.frames[idx.0 as usize]
    }

    #[inline(always)]
    pub(crate) fn frame_mut(&mut self, idx: FrameIndex) -> &mut StackFrame {
        &mut self.frames[idx.0 as usize]
    }

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
                &self.data[start..end]
            }
            FrameBuffer::Heap(b) => b,
            FrameBuffer::Empty => &[],
        }
    }

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

        let heap_buf = match std::mem::replace(&mut head.buf, FrameBuffer::Empty) {
            FrameBuffer::Heap(b) => b,
            _ => unreachable!("Async frame must be Heap"),
        };

        // Migrate closure VarRefs out of the arena's map.
        let mut var_refs = self.var_ref_map.remove(&idx).unwrap_or_default();

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
            self.frames.pop();
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
            self.var_ref_map.insert(new_idx, frame.var_refs);
        }

        self.frames.push(frame.frame);
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

    pub(crate) fn get_var_ref_frame(
        &mut self,
        ctx: &mut ScopedContext,
        frame_idx: FrameIndex,
        var_idx: u16,
        is_arg: bool,
    ) -> Result<VarRef, i32> {
        debug_assert!(ctx.is_arena_mode());

        // Look up existing VarRef in the map first.
        {
            let var_refs = self.var_ref_map.entry(frame_idx).or_default();
            if let Some(vr) = var_refs
                .iter()
                .find(|x| x.is_match(var_idx, is_arg))
                .copied()
            {
                return Ok(vr);
            }
        }

        // Read frame data (immutable borrow ends before var_ref_map access below).
        let (data_ptr, arg_count, var_count, var_base_offset) = {
            let head = self.frame(frame_idx);
            (
                head.buf.data_ptr(),
                head.arg_count,
                head.var_count,
                head.var_base_offset,
            )
        };

        let arg_base = data_ptr;
        let var_base = unsafe { data_ptr.add(var_base_offset as usize) };
        let pvalue = unsafe {
            NonNull::new_unchecked(if is_arg {
                debug_assert!(var_idx < arg_count);
                arg_base.add(var_idx as usize)
            } else {
                debug_assert!(var_idx < var_count);
                var_base.add(var_idx as usize)
            })
        };

        let vref = VarRef::create_bind(ctx, var_idx, is_arg, pvalue)?;

        #[cfg(debug_assertions)]
        {
            let head = self.frame(frame_idx);
            let buf_start: *const JSValue = head.buf.data_ptr() as *const JSValue;
            let buf_end = unsafe { buf_start.add(head.buf.capacity() as usize) };
            let ptr: *const JSValue = pvalue.as_ptr() as *const JSValue;
            debug_assert!(
                ptr >= buf_start && ptr < buf_end,
                "get_var_ref_frame: VarRef::Bind pvalue {:p} out of frame buf [{:p}, {:p})",
                ptr,
                buf_start,
                buf_end
            );
        }

        self.var_ref_map.entry(frame_idx).or_default().push(vref);
        Ok(vref)
    }

    // ── VarRef map accessors ────────────────────────────────────
    // These are the ONLY external API for accessing `var_ref_map`.
    // Calling code should never touch the HashMap directly — it must
    // go through these methods to keep the ownership / lifecycle
    // invariants centralised (free_frame cleanup, take/put migration).

    /// Get-or-create the VarRef vec for `idx`.
    /// Used by closure creation paths (get_var_ref, make_closure).
    #[inline(always)]
    pub(crate) fn var_refs_or_default(&mut self, idx: FrameIndex) -> &mut Vec<VarRef> {
        self.var_ref_map.entry(idx).or_default()
    }

    /// Get mutable access to existing VarRefs without inserting.
    /// Returns `None` when the frame has no closure variables.
    /// Used by detach / CLOSE_LOC paths that only need to act when refs exist.
    #[inline(always)]
    pub(crate) fn try_var_refs_mut(&mut self, idx: FrameIndex) -> Option<&mut Vec<VarRef>> {
        self.var_ref_map.get_mut(&idx)
    }

    /// Check whether `idx` has at least one live closure variable reference.
    /// Used by debug assertions.
    #[inline(always)]
    pub(crate) fn has_var_refs(&self, idx: FrameIndex) -> bool {
        self.var_ref_map.get(&idx).is_some_and(|v| !v.is_empty())
    }
}

#[cfg(debug_assertions)]
impl Drop for StackFrameArena {
    fn drop(&mut self) {
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
                    "[WARN] StackFrameArena bump_rate={:.1}% (bump={}, non_bump: async={}, cap={}, total={}, data_cap={})",
                    cap_bump_pct,
                    self.stats_bump,
                    self.stats_heap_async,
                    self.stats_heap_capacity,
                    total,
                    self.data.len(),
                );
            }
        }
    }
}

impl GcTrace for StackFrameArena {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        for (i, frame) in self.frames.iter().enumerate() {
            if frame.state != FrameState::Active {
                continue;
            }

            frame.func_obj.trace(gcx);

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
            if let Some(var_refs) = self.var_ref_map.get(&idx) {
                for vr in var_refs {
                    vr.trace(gcx);
                }
            }
        }
    }
}
