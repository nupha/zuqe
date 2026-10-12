// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{mem, ptr::NonNull};

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    ScopedContext,
    bytecode::exec_stackless::execute_trampoline,
    context::ContextPtr,
    error::JSResult,
    function::BCWrapper,
    gcref_wrapper,
    runtime::js_check_thread_stack_overflow,
    value::JSValue,
    vm_stack::{
        AsyncStackFrame, ExecMode, FrameAllocError, FrameBuffer, FrameIndex, StackFrame, VmStack,
    },
};

/// Async frame state: either actively executing in the arena, or suspended as a snapshot.
/// Uses an enum to statically prevent the invalid states of both-some or both-none.
#[derive(Debug)]
pub(crate) enum AsyncFrameState {
    /// Frame is alive in VmStack, referenced by FrameIndex.
    Active(FrameIndex),
    /// Frame has been suspended; its data lives in a heap-allocated snapshot.
    Suspended(AsyncStackFrame),
}

impl AsyncFrameState {
    /// Get the FrameHead regardless of active/suspended state.
    /// For Active, this requires the arena; for Suspended, it's self-contained.
    #[inline(always)]
    pub(crate) fn frame<'a>(&'a self, arena: &'a VmStack) -> &'a StackFrame {
        match self {
            AsyncFrameState::Active(idx) => arena.frame(*idx),
            AsyncFrameState::Suspended(snapshot) => &snapshot.frame,
        }
    }
}

/// Async execution state for bytecode running.
/// Used to save execution state of an async function or generator.
#[derive(Debug)]
pub struct AsyncStateImpl {
    /// 'this' argument
    pub(crate) this_val: JSValue,

    /// indicates there is an exception on runtime (used by  JS_CallInternal())
    #[deprecated(
        note = "this method is useless, you may detect exception by checking runtime current exception != Exception::No"
    )]
    pub(crate) throw_flag: bool,

    /// TRUE if the function has returned. The stack frame is no longer valid
    pub(crate) is_completed: bool,
    /// for async function only
    pub(crate) resolving_funcs: [JSValue; 2],

    /// Async frame state: Active (in arena) or Suspended (standalone snapshot).
    pub(crate) frame: AsyncFrameState,

    /// The bytecode function object, kept alive for resume to access bytecode.
    pub(crate) bytecode_func: BCWrapper,
}

impl GcTrace for AsyncStateImpl {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.this_val.trace(gcx);
        self.resolving_funcs[0].trace(gcx);
        self.resolving_funcs[1].trace(gcx);

        // Trace the frame data: when Active, the arena traces it separately;
        // when Suspended, the snapshot owns its data and must be traced here.
        if let AsyncFrameState::Suspended(ref snapshot) = self.frame {
            let head = &snapshot.frame;

            head.func_obj.trace(gcx);

            // Trace data buffer (args + vars + stack) via snapshot's data_slice.
            let data = snapshot.data();
            let arg_cnt = std::cmp::max(head.arg_real_count, head.arg_count) as usize;
            for v in data.iter().take(arg_cnt.min(data.len())) {
                v.trace(gcx);
            }
            let var_start = head.var_base_offset as usize;
            let var_end = var_start + head.var_count as usize + head.cur_sp_offset.max(0) as usize;
            for v in &data[var_start..var_end.min(data.len())] {
                v.trace(gcx);
            }

            for vr in &snapshot.var_refs {
                vr.trace(gcx);
            }
        }
        // Active frames are traced by VmStack::trace() in gc_collect_in.
    }
}

impl AsyncStateImpl {
    fn new(
        mut ctx: ContextPtr,
        resolving_funcs: [JSValue; 2],
        bytecode_func: BCWrapper,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> Result<Self, i32> {
        let (arg_count, var_count, local_buf_size, mode) = {
            let bc = bytecode_func.payload().bytecode();
            (bc.arg_count, bc.var_count, bc.local_buf_size, bc.mode())
        };

        // Async frames always use Heap buffers (lifetime spans multiple
        // yield/await cycles, incompatible with arena LIFO model).
        // alloc_frame with ASYNC flag automatically selects Heap path.
        let arg_count = std::cmp::max(arg_count as usize, args.len());
        let buf_capacity = arg_count + local_buf_size as usize;

        let arena = ctx.rt_mut().vm_stack_mut();
        let frame_idx =
            match arena.alloc_frame(bytecode_func.into(), mode | ExecMode::ASYNC, buf_capacity) {
                Ok(idx) => idx,
                Err(e) => {
                    ctx.with_current_scope(|mut s| match e {
                        FrameAllocError::Oom => s.throw_stack_fail(),
                        FrameAllocError::Exceeded => s.throw_stack_exceeded(),
                    });
                    return Err(-1);
                }
            };

        // Initialize frame data: copy args, zero-fill vars, set up offsets.
        {
            arena.init_frame_args(frame_idx, arg_count as u16, var_count, args);

            // Async frames are Heap-backed (not bump-allocated), so the local
            // variable region is NOT implicitly zero-initialised – fill it here.
            let data_ptr = {
                let head = arena.frame(frame_idx);
                head.buf.data_ptr()
            };
            let var_start = arg_count;
            let var_end = var_start + var_count as usize;
            for i in var_start..var_end {
                unsafe {
                    *data_ptr.add(i) = JSValue::UNDEFINED;
                }
            }
        }

        Ok(Self {
            this_val: this_obj,
            resolving_funcs,
            is_completed: false,
            frame: AsyncFrameState::Active(frame_idx),
            bytecode_func,
            throw_flag: false,
        })
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    /// Get raw pointer to arguments region in the frame data buffer.
    /// Only valid when frame is Active; caller must ensure this.
    #[inline(always)]
    pub fn arg_ptr(&self) -> NonNull<JSValue> {
        match &self.frame {
            AsyncFrameState::Active(_) => {
                // The caller (resume) supplies the arena; for compat we need the rt.
                // This method is called from resume() which has ctx access.
                // We use a raw pointer approach — see frame_data_ptr().
                unimplemented!("arg_ptr: use frame_data_ptr with arena instead")
            }
            AsyncFrameState::Suspended(snapshot) => match &snapshot.frame.buf {
                FrameBuffer::Heap(b) => unsafe {
                    NonNull::new_unchecked(b.as_ptr() as *mut JSValue)
                },
                _ => unreachable!(),
            },
        }
    }

    #[inline(always)]
    pub fn arg_count(&self) -> usize {
        match &self.frame {
            AsyncFrameState::Active(_) => {
                // Same as arg_ptr: needs arena. Kept for compat via the
                // frame_head method below. Caller should use frame_head().
                unimplemented!("arg_count: use frame_head with arena instead")
            }
            AsyncFrameState::Suspended(snapshot) => snapshot.frame.arg_real_count as usize,
        }
    }

    /// Note: is_reject=true indicates runtime must be an effective Exception
    #[deprecated(
        note = "this method is useless, you may detect exception by checking runtime current exception != Exception::No"
    )]
    #[inline(always)]
    pub(crate) const fn set_throw_flag(&mut self, is_reject: bool) {
        self.throw_flag = is_reject;
    }

    /// Get immutable reference to the FrameHead via the arena.
    #[inline(always)]
    pub(crate) fn frame<'a>(&'a self, arena: &'a VmStack) -> &'a StackFrame {
        self.frame.frame(arena)
    }

    /// put given value to stack at position current stack pointer -1.
    /// Uses &VmStack (not &mut) since only raw pointer writes are performed.
    pub(super) fn set_return_value(&mut self, arena: &VmStack, val: JSValue) {
        let head = self.frame(arena);
        let sp_base = head.stack_base_ptr();
        unsafe {
            *sp_base.add(head.cur_sp_offset as usize - 1) = val;
        }
    }

    /// Take the top value from the async frame stack and protect it
    /// in the current GC scope. After this call the stack slot is UNDEFINED
    /// and the returned value is guaranteed to survive GC within this scope.
    ///
    /// Unlike `pop`, this does NOT adjust SP — it only reads and clears SP[-1].
    /// This is the safe alternative to raw `ptr::replace` on the async stack.
    #[inline(always)]
    pub(crate) fn take_top_value(&mut self, arena: &VmStack, ctx: &ScopedContext) -> JSValue {
        // Use frame_head() which works for both Active and Suspended frames.
        // For Suspended frames, the Heap buffer pointer is obtained via
        // arena.data_ptr(head) which handles FrameBufLocation::Heap correctly.
        let head = self.frame(arena);
        let ptr = unsafe { head.stack_cur_ptr().offset(-1) };
        let val = unsafe { std::ptr::replace(ptr, JSValue::UNDEFINED) };
        ctx.gc_protect_transient(&val);
        val
    }

    /// Store a value at SP[offset] in the async frame without adjusting SP.
    /// Wraps VmStack::stack_store for the current frame.
    /// Handles both Active (arena) and Suspended (snapshot) frames.
    #[inline]
    pub(crate) fn stack_store_value(&self, arena: &VmStack, offset: isize, val: JSValue) {
        match &self.frame {
            AsyncFrameState::Active(idx) => arena.frame_store_value(*idx, offset, val),
            AsyncFrameState::Suspended(snapshot) => {
                let head = &snapshot.frame;

                // Compute stack pointer directly from snapshot's Heap buffer.
                let ptr = match &head.buf {
                    FrameBuffer::Heap(b) => unsafe {
                        (b.as_ptr() as *mut JSValue)
                            .add((head.var_base_offset + head.var_count as u32) as usize)
                            .add(head.cur_sp_offset as usize)
                            .offset(offset)
                    },
                    _ => unreachable!("Async frame must be Heap"),
                };

                // SAFETY: ptr::write through raw pointer follows the same
                // unsafe pattern as arena.stack_store which writes through
                // an immutable &self reference.
                unsafe {
                    *ptr = val;
                }
            }
        }
    }

    /// Get current SP offset for the active frame.
    #[inline(always)]
    pub(crate) fn cur_sp_offset(&self, arena: &VmStack) -> i32 {
        self.frame(arena).cur_sp_offset
    }

    /// Bump SP offset by the given delta.
    /// Handles both Active (arena) and Suspended (snapshot) frames.
    #[inline(always)]
    pub(crate) fn bump_sp(&mut self, arena: &mut VmStack, delta: i32) {
        match &mut self.frame {
            AsyncFrameState::Active(idx) => {
                arena.frame_mut(*idx).cur_sp_offset += delta;
            }
            AsyncFrameState::Suspended(snapshot) => {
                snapshot.frame.cur_sp_offset += delta;
            }
        }
    }

    /// Resume execution of the async function using the trampoline-based
    /// stackless interpreter.
    ///
    /// This replaces the legacy `resume_call()` which used a temporary
    /// Pin<Box<StackFrame>> + Bytecode::execute(). Now it directly calls
    /// execute_trampoline() which runs in a single C-stack loop,
    /// eliminating C-stack recursion for JS-to-JS calls.
    pub(crate) fn resume(&mut self, ctx: &mut ScopedContext) -> JSResult {
        debug_assert!(!self.is_completed);

        if js_check_thread_stack_overflow(ctx.rt(), 0) {
            return Err(ctx.throw_stack_overflow());
        }

        // Put suspended frame back into arena if needed.
        // After this, frame is guaranteed to be Active in arena.
        // IMPORTANT: use matches! guard BEFORE mem::replace — if the frame
        // is already Active, mem::replace would set it to Active(NULL)
        // even though the if-let pattern doesn't match (destructive replace).
        if matches!(&self.frame, AsyncFrameState::Suspended(_)) {
            let old = mem::replace(&mut self.frame, AsyncFrameState::Active(FrameIndex::NULL));
            if let AsyncFrameState::Suspended(snapshot) = old {
                let new_idx = ctx.rt_mut().vm_stack_mut().put_frame(snapshot);
                self.frame = AsyncFrameState::Active(new_idx);
            }
        }

        // Get the active frame index (must be Active now).
        let frame_idx = match self.frame {
            AsyncFrameState::Active(idx) => idx,
            _ => unreachable!("frame should be Active after put_frame"),
        };

        // Capture Copy values before mutable borrow for closure_var_refs.
        let this_val = self.this_val;
        let bc = self.bytecode_func.payload().bytecode();

        // Execute via trampoline.
        // closure_vars comes from BytecodeFunction's closure_var_refs,
        // which serves the same role as var_ref_list in the legacy path.
        //
        // The guard restores active_frame_index (cleared by execute_trampoline
        // on exit) — resume can be reached through fast paths that bypass
        // call_internal's protection (e.g. ObjectIter::apply_next's
        // IteratorNext shortcut for generator .next() in for-of).
        let (ret_val, is_completed) = ctx.with_active_frame_guard(|cx| {
            execute_trampoline(
                bc,
                frame_idx,
                cx,
                this_val,           // this_obj
                JSValue::UNDEFINED, // new_target
            )
        });

        if is_completed {
            // Function completed (normal return or exception).
            // The frame was freed by execute_trampoline.
            self.is_completed = true;
            self.frame = AsyncFrameState::Active(FrameIndex::NULL);
            // We don't need to close VarRefs here because
            // execute_trampoline's free_frame already handles cleanup.
        } else {
            // Function suspended (yield/await).
            // Take the frame snapshot from arena.
            let arena = ctx.rt_mut().vm_stack_mut();
            let snapshot = arena.take_frame(frame_idx);
            self.frame = AsyncFrameState::Suspended(snapshot);
        }

        if ret_val.is_exception() {
            Err(-1)
        } else {
            Ok(ret_val)
        }
    }
}

/// async execution state.
/// used to save execution state of an async function or generator.
#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(transparent)]
pub struct AsyncState(GcRef<AsyncStateImpl>);

gcref_wrapper!(AsyncState, AsyncStateImpl);

impl AsyncState {
    pub(crate) fn new(
        ctx: &mut ScopedContext,
        resolvings: [JSValue; 2],
        bytecode_func: BCWrapper,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> Result<Self, i32> {
        let inner = AsyncStateImpl::new(ctx.into(), resolvings, bytecode_func, this_obj, args)?;

        let a_state: AsyncState = ctx
            .alloc_local(inner)
            .map_err(|_| ctx.throw_out_of_memory())?
            .into();

        // Backref link is no longer needed: in the arena model, GC tracing
        // handles async frames through AsyncStateImpl.trace() (for Suspended)
        // and VmStack.trace() (for Active), independently.
        //
        // The state escapes its allocating scope at the first suspend, so
        // scope-local protection ends there. Root it on the runtime until
        // completion — promise-reaction trace edges alone left a GC-timing
        // window where a still-resumable state was collected (UAF).
        ctx.rt_mut().gc_pending_async_states.push(a_state);

        Ok(a_state)
    }
}
