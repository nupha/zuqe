// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

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

    #[cfg(debug_assertions)]
    pub(crate) stats_bump: usize,
    #[cfg(debug_assertions)]
    pub(crate) stats_heap_async: usize,
    #[cfg(debug_assertions)]
    pub(crate) stats_heap_capacity: usize,
}

impl StackFrameArena {
    pub(crate) const STACK_FRAME_ARENA_CAP: usize = 512 * 1024 / std::mem::size_of::<JSValue>();
    const HEAP_BUF_CACHE_MAX: usize = 8;

    pub(crate) fn new() -> Self {
        Self {
            frames: Vec::with_capacity(256),
            data: vec![JSValue::UNDEFINED; Self::STACK_FRAME_ARENA_CAP].into_boxed_slice(),
            data_used: 0,
            suspended_count: 0,
            heap_buf_cache: Vec::new(),

            #[cfg(debug_assertions)]
            stats_bump: 0,
            #[cfg(debug_assertions)]
            stats_heap_async: 0,
            #[cfg(debug_assertions)]
            stats_heap_capacity: 0,
        }
    }

    #[inline(always)]
    fn reuse_or_alloc_heap_buf(&mut self, count: usize) -> Box<[JSValue]> {
        if let Some(pos) = self.heap_buf_cache.iter().position(|b| b.len() >= count) {
            self.heap_buf_cache.remove(pos)
        } else {
            vec![JSValue::UNDEFINED; count].into_boxed_slice()
        }
    }

    pub(crate) fn alloc_frame(
        &mut self,
        func_obj: Object,
        mode: ExecMode,
        capacity: usize,
    ) -> FrameIndex {
        let idx = FrameIndex(self.frames.len() as u32);

        let buf = if mode.contains(ExecMode::ASYNC) {
            #[cfg(debug_assertions)]
            {
                self.stats_heap_async += 1;
            }
            FrameBuffer::Heap(self.reuse_or_alloc_heap_buf(capacity))
        } else if self.data_used + capacity > self.data.len() {
            #[cfg(debug_assertions)]
            {
                self.stats_heap_capacity += 1;
            }
            FrameBuffer::Heap(self.reuse_or_alloc_heap_buf(capacity))
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
            }
        };

        self.frames.push(StackFrame {
            prev: FrameIndex::NULL,
            state: FrameState::Active,
            mode,
            func_obj,
            cur_pc_offset: 0,
            cur_sp_offset: 0,
            protected_values: [JSValue::UNDEFINED, JSValue::UNDEFINED],
            var_ref_list: Vec::new(),
            buf,
            buf_capacity: capacity as u32,
            arg_base_offset: 0,
            arg_real_count: 0,
            arg_count: 0,
            var_base_offset: 0,
            var_count: 0,
        });

        idx
    }

    pub(crate) fn free_frame(&mut self, idx: FrameIndex) {
        debug_assert_eq!(self.frames[idx.0 as usize].state, FrameState::Active);

        if idx.0 as usize == self.frames.len() - 1 {
            let head = self.frames.pop().unwrap();
            match head.buf {
                FrameBuffer::Arena { .. } => {
                    self.data_used -= head.buf_capacity as usize;
                }
                FrameBuffer::Heap(heap_buf) => {
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
            FrameBuffer::Arena { buf_offset, .. } => {
                let start = *buf_offset as usize;
                let end = start + head.buf_capacity as usize;
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

        let arg_base = head.arg_base_offset;
        let var_base = head.var_base_offset;

        for vr in &mut head.var_ref_list {
            vr.update_pvalue_for_heap_buf_offsets(&heap_buf, arg_base, var_base);
        }

        let frame = StackFrame {
            prev: FrameIndex::NULL,
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
            self.frames.pop();
        } else {
            head.buf_capacity = 0;
            head.state = FrameState::Suspended;
            self.suspended_count += 1;
        }

        AsyncStackFrame { frame }
    }

    pub(crate) fn put_frame(&mut self, mut frame: AsyncStackFrame) -> FrameIndex {
        let new_idx = FrameIndex(self.frames.len() as u32);
        frame.frame.prev = FrameIndex::NULL;
        frame.frame.state = FrameState::Active;
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
            if let JSValue::CatchOffset(pos) =
                unsafe { std::ptr::replace(sp_base.add(sp), JSValue::UNDEFINED) }
            {
                if pos == 0 {
                    unsafe { std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED) };
                    sp -= 1;
                    if let JSValue::Object(o) =
                        unsafe { std::ptr::replace(sp_base.add(sp - 1), JSValue::UNDEFINED) }
                    {
                        let _ = crate::builtins::iterator::ObjectIter::close_iter_internal(ctx, o);
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

    pub(crate) fn find_finally_in_frame(&self, idx: FrameIndex) -> Option<(usize, usize)> {
        let head = self.frame(idx);
        let sp_base = head.stack_base_ptr();
        let mut sp = head.cur_sp_offset as usize;

        while sp > 0 {
            sp -= 1;
            if let JSValue::Int(pos) =
                unsafe { std::ptr::replace(sp_base.add(sp), JSValue::UNDEFINED) }
                && pos > 0
            {
                unsafe { std::ptr::write(sp_base.add(sp), JSValue::Int(pos)) }
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
        var_ref_list: &mut Vec<VarRef>,
    ) -> Result<VarRef, i32> {
        debug_assert!(ctx.is_arena_mode());

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

#[cfg(debug_assertions)]
impl Drop for StackFrameArena {
    fn drop(&mut self) {
        let total = self.stats_bump + self.stats_heap_async + self.stats_heap_capacity;
        if total == 0 {
            return;
        }

        let bump_pct = self.stats_bump as f64 / total as f64 * 100.0;

        // 非 bump alloc（async + 容量不足）超过 0.5% 则警告
        if bump_pct < 99.5 {
            eprintln!(
                "[WARN] StackFrameArena bump_rate={:.1}% (bump={}, non_bump: async={}, cap={}, total={})",
                bump_pct, self.stats_bump, self.stats_heap_async, self.stats_heap_capacity, total,
            );
        }

        // 非 bump alloc 超过 5% — 严重影响 call 效率，debug 下直接 panic
        debug_assert!(
            bump_pct >= 95.0,
            "StackFrameArena bump_rate too low: {:.1}% (bump={}, async={}, cap={}, total={}). \
             Non-bump path adds heap alloc overhead per function call.",
            bump_pct, self.stats_bump, self.stats_heap_async, self.stats_heap_capacity, total,
        );
    }
}

impl GcTrace for StackFrameArena {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        for frame in &self.frames {
            if frame.state != FrameState::Active {
                continue;
            }

            frame.func_obj.trace(gcx);
            frame.protected_values[0].trace(gcx);
            frame.protected_values[1].trace(gcx);

            let data = self.data(frame);
            let data_end = frame.buf_capacity as usize;

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

            for vr in &frame.var_ref_list {
                vr.trace(gcx);
            }
        }
    }
}
