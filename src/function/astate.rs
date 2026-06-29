// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{mem, ptr::NonNull};

use gc_lite::{GcNode, GcRef, GcTrace, GcTraceCtx, GcWeak};

use crate::{
    ScopedContext,
    context::ContextPtr,
    error::JSResult,
    function::BCWrapper,
    gcref_wrapper,
    runtime::js_check_stack_overflow,
    stack_frame::{
        AsyncStackFrame, ExecMode, FrameBufLocation, FrameHead, FrameIndex, StackFrameArena,
    },
    value::JSValue,
};

/// Async frame state: either actively executing in the arena, or suspended as a snapshot.
/// Uses an enum to statically prevent the invalid states of both-some or both-none.
#[derive(Debug)]
pub(crate) enum AsyncFrameState {
    /// Frame is alive in StackFrameArena, referenced by FrameIndex.
    Active(FrameIndex),
    /// Frame has been suspended; its data lives in a heap-allocated snapshot.
    Suspended(AsyncStackFrame),
}

impl AsyncFrameState {
    /// Get the FrameHead regardless of active/suspended state.
    /// For Active, this requires the arena; for Suspended, it's self-contained.
    #[inline(always)]
    pub(crate) fn frame_head<'a>(&'a self, arena: &'a StackFrameArena) -> &'a FrameHead {
        match self {
            AsyncFrameState::Active(idx) => arena.frame(*idx),
            AsyncFrameState::Suspended(snapshot) => &snapshot.head,
        }
    }

    /// Get the data buffer slice for GC tracing and value access.
    fn data_slice<'a>(&'a self, arena: &'a StackFrameArena) -> &'a [JSValue] {
        match self {
            AsyncFrameState::Active(idx) => {
                let head = arena.frame(*idx);
                match &head.buf {
                    FrameBufLocation::Heap(b) => b,
                    FrameBufLocation::Arena { buf_offset } => {
                        let start = *buf_offset as usize;
                        let end = start + head.buf_capacity as usize;
                        &arena.data[start..end]
                    }
                    FrameBufLocation::None => &[],
                }
            }
            AsyncFrameState::Suspended(snapshot) => match &snapshot.head.buf {
                FrameBufLocation::Heap(b) => b,
                _ => unreachable!("Suspended frame must be Heap-variant"),
            },
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
            let head = &snapshot.head;

            head.func_obj.trace(gcx);
            head.protected_values[0].trace(gcx);
            head.protected_values[1].trace(gcx);

            // Trace data buffer (args + vars + stack) via snapshot's data_slice.
            let data = snapshot.data_slice();
            let arg_cnt = std::cmp::max(head.arg_real_count, head.arg_count) as usize;
            for v in data.iter().take(arg_cnt.min(data.len())) {
                v.trace(gcx);
            }
            let var_start = head.var_base_offset as usize;
            let var_end = var_start + head.var_count as usize + head.cur_sp_offset.max(0) as usize;
            for v in &data[var_start..var_end.min(data.len())] {
                v.trace(gcx);
            }

            for vr in &head.var_ref_list {
                vr.trace(gcx);
            }
        }
        // Active frames are traced by StackFrameArena::trace() in gc_collect_in.
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

        let arena = ctx.rt_mut().frame_arena_mut();
        let frame_idx =
            arena.alloc_frame(bytecode_func.into(), mode | ExecMode::ASYNC, buf_capacity);

        // Initialize frame data: copy args, zero-fill vars, set up offsets.
        {
            // Get data pointer first (immutable borrow), then mutable head.
            // Explicit block ensures the immutable borrow is released.
            let data_ptr = {
                let head = arena.frame(frame_idx);
                arena.data_ptr(head)
            };
            let head = arena.frame_mut(frame_idx);

            // Copy actual arguments into the args region.
            for (i, arg) in args.iter().enumerate() {
                unsafe {
                    *data_ptr.add(i) = *arg;
                }
            }
            // Fill remaining formal parameter slots with UNDEFINED.
            for i in args.len()..arg_count {
                unsafe {
                    *data_ptr.add(i) = JSValue::UNDEFINED;
                }
            }
            // Fill local variable region with UNDEFINED.
            let var_start = arg_count;
            let var_end = var_start + var_count as usize;
            for i in var_start..var_end {
                unsafe {
                    *data_ptr.add(i) = JSValue::UNDEFINED;
                }
            }

            // Set up frame layout offsets (all relative to data buffer start).
            head.arg_base_offset = 0;
            head.arg_real_count = args.len() as u16;
            head.arg_count = arg_count as u16;
            head.var_base_offset = arg_count as u32;
            head.var_count = var_count;
            head.cur_sp_offset = 0; // stack starts empty
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
            AsyncFrameState::Suspended(snapshot) => match &snapshot.head.buf {
                FrameBufLocation::Heap(b) => unsafe {
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
            AsyncFrameState::Suspended(snapshot) => snapshot.head.arg_real_count as usize,
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

    /// Get mutable reference to the FrameHead via the arena.
    /// Only valid when frame is Active; panics if Suspended.
    #[inline(always)]
    pub(crate) fn frame_head_mut<'a>(
        &'a mut self,
        arena: &'a mut StackFrameArena,
    ) -> &'a mut FrameHead {
        match self.frame {
            AsyncFrameState::Active(idx) => arena.frame_mut(idx),
            AsyncFrameState::Suspended(_) => unreachable!("frame_head_mut: frame is Suspended"),
        }
    }

    /// Get immutable reference to the FrameHead via the arena.
    pub(crate) fn frame_head<'a>(&'a self, arena: &'a StackFrameArena) -> &'a FrameHead {
        self.frame.frame_head(arena)
    }

    /// Get the current FrameIndex, if the frame is Active in the arena.
    #[inline(always)]
    pub(crate) fn frame_idx(&self) -> Option<FrameIndex> {
        match &self.frame {
            AsyncFrameState::Active(idx) => Some(*idx),
            AsyncFrameState::Suspended(_) => None,
        }
    }

    /// put given value to stack at position current stack pointer -1.
    /// Uses &StackFrameArena (not &mut) since only raw pointer writes are performed.
    pub(super) fn set_return_value(&mut self, arena: &StackFrameArena, val: JSValue) {
        let head = self.frame_head(arena);
        let sp_base = arena.stack_base_ptr(head);
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
    pub fn take_top(&mut self, arena: &StackFrameArena, ctx: &ScopedContext) -> JSValue {
        // Use frame_head() which works for both Active and Suspended frames.
        // For Suspended frames, the Heap buffer pointer is obtained via
        // arena.data_ptr(head) which handles FrameBufLocation::Heap correctly.
        let head = self.frame_head(arena);
        let ptr = unsafe { arena.stack_cur_ptr(head).offset(-1) };
        let val = unsafe { std::ptr::replace(ptr, JSValue::UNDEFINED) };
        ctx.gc_protect_transient(&val);
        val
    }

    /// Push a value onto the async frame's operand stack (SP += 1).
    /// Wraps StackFrameArena::stack_push for the current frame.
    /// Handles both Active (arena) and Suspended (snapshot) frames.
    #[inline]
    pub(crate) fn stack_push(&mut self, arena: &mut StackFrameArena, val: JSValue) {
        match &mut self.frame {
            AsyncFrameState::Active(idx) => arena.stack_push(*idx, val),
            AsyncFrameState::Suspended(snapshot) => {
                let head = &mut snapshot.head;
                head.cur_sp_offset += 1;
                // Compute stack pointer directly from snapshot's Heap buffer.
                // SAFETY: snapshot.head.buf is always Heap for async frames.
                let ptr = match &head.buf {
                    FrameBufLocation::Heap(b) => unsafe {
                        (b.as_ptr() as *mut JSValue)
                            .add((head.var_base_offset + head.var_count as u32) as usize)
                            .add(head.cur_sp_offset as usize)
                    },
                    _ => unreachable!("Async frame must be Heap"),
                };
                unsafe {
                    *ptr = val;
                }
            }
        }
    }

    /// Store a value at SP[offset] in the async frame without adjusting SP.
    /// Wraps StackFrameArena::stack_store for the current frame.
    /// Handles both Active (arena) and Suspended (snapshot) frames.
    #[inline]
    pub(crate) fn stack_store(&self, arena: &StackFrameArena, offset: isize, val: JSValue) {
        match &self.frame {
            AsyncFrameState::Active(idx) => arena.stack_store(*idx, offset, val),
            AsyncFrameState::Suspended(snapshot) => {
                let head = &snapshot.head;
                // Compute stack pointer directly from snapshot's Heap buffer.
                let ptr = match &head.buf {
                    FrameBufLocation::Heap(b) => unsafe {
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
    pub(crate) fn cur_sp_offset(&self, arena: &StackFrameArena) -> i32 {
        self.frame_head(arena).cur_sp_offset
    }

    /// Bump SP offset by the given delta.
    /// Handles both Active (arena) and Suspended (snapshot) frames.
    #[inline(always)]
    pub(crate) fn bump_sp(&mut self, arena: &mut StackFrameArena, delta: i32) {
        match &mut self.frame {
            AsyncFrameState::Active(idx) => {
                arena.frame_mut(*idx).cur_sp_offset += delta;
            }
            AsyncFrameState::Suspended(snapshot) => {
                snapshot.head.cur_sp_offset += delta;
            }
        }
    }

    /// Ensure the frame is Active in the arena, putting a Suspended frame back if needed.
    /// This is a helper for code paths that require arena access (e.g., frame_head_mut).
    fn ensure_active(&mut self, arena: &mut StackFrameArena) {
        if matches!(&self.frame, AsyncFrameState::Suspended(_)) {
            let old = mem::replace(&mut self.frame, AsyncFrameState::Active(FrameIndex::NULL));
            if let AsyncFrameState::Suspended(snapshot) = old {
                let new_idx = arena.put_frame(snapshot);
                self.frame = AsyncFrameState::Active(new_idx);
            }
        }
    }

    /// Resume execution of the async function.
    pub(crate) fn resume(&mut self, ctx: &mut ScopedContext) -> JSResult {
        debug_assert!(!self.is_completed);

        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        // Put suspended frame back into arena if needed.
        // After this, frame is guaranteed to be Active in arena.
        // IMPORTANT: use matches! guard BEFORE mem::replace — if the frame
        // is already Active, mem::replace would set it to Active(NULL)
        // even though the if-let pattern doesn't match (destructive replace).
        if matches!(&self.frame, AsyncFrameState::Suspended(_)) {
            let old = mem::replace(&mut self.frame, AsyncFrameState::Active(FrameIndex::NULL));
            if let AsyncFrameState::Suspended(snapshot) = old {
                let new_idx = ctx.rt_mut().frame_arena_mut().put_frame(snapshot);
                self.frame = AsyncFrameState::Active(new_idx);
            }
        }

        // Copy args to avoid borrow conflict: arena borrow (for data_ptr)
        // and ctx.with_new_scope (&mut ctx) cannot coexist.
        let args_vec: Vec<JSValue> = {
            let arena = ctx.rt().frame_arena();
            let head = self.frame_head(arena);
            let data_ptr = arena.data_ptr(head);
            let slice = unsafe {
                std::slice::from_raw_parts(
                    data_ptr.add(head.arg_base_offset as usize),
                    head.arg_real_count as usize,
                )
            };
            slice.iter().copied().collect()
        };

        let mut ret_val = ctx.with_new_scope(|scope| {
            let res = self.resume_call(scope, self.this_val, &args_vec);
            res
        });

        let completed = match ret_val {
            Ok(JSValue::UNDEFINED) => {
                // After resume_call, frame is Suspended. Put it back
                // in the arena so we can access the data buffer.
                self.ensure_active(ctx.rt_mut().frame_arena_mut());
                // Get the stack top value from the frame data buffer.
                // Must carefully order borrows to avoid double-borrowing arena.
                let sp_val = {
                    let arena = ctx.rt_mut().frame_arena_mut();
                    // Extract all needed info from FrameHead first, then drop
                    // the mutable borrow before accessing arena again.
                    let (sp_off, var_base, var_count, dp) = {
                        let head = self.frame_head_mut(arena);
                        debug_assert!(head.cur_sp_offset > 0);
                        let off = head.cur_sp_offset;
                        let vb = head.var_base_offset;
                        let vc = head.var_count;
                        // Get data pointer directly from head's buf field
                        // (avoids arena reborrow; async frames always Heap).
                        let dp = match &head.buf {
                            FrameBufLocation::Heap(b) => b.as_ptr() as *mut JSValue,
                            _ => std::ptr::null_mut(),
                        };
                        (off, vb, vc, dp)
                    };
                    // SAFETY: dp points to the frame's Heap buffer (async
                    // frames always use Heap). Pointer arithmetic yields the
                    // stack top slot (sp[-1] = base + var_count + sp_offset - 1).
                    let sp_slot = unsafe {
                        dp.add((var_base + var_count as u32) as usize + sp_off as usize - 1)
                    };
                    unsafe { std::ptr::replace(sp_slot, JSValue::UNDEFINED) }
                };
                ret_val = Ok(sp_val);
                true
            }
            Err(_) => true,
            _ => false,
        };

        if completed {
            self.is_completed = true;
            // After resume_call, frame may still be Suspended (e.g., Err path).
            // Put it back in the arena for cleanup.
            self.ensure_active(ctx.rt_mut().frame_arena_mut());
            // Close closure variables: VarRef raw pointers into the frame data
            // buffer must be detached before the buffer is recycled.
            let arena = ctx.rt_mut().frame_arena_mut();
            let head = self.frame_head_mut(arena);
            // VarRef cleanup: var_ref_list is moved to ClosedOver state
            // (data copied out of frame buffer, pointers become independent).
            // For now, we simply clear the list — the BCWrapper's closure_var_refs
            // already hold the captured values.
            head.var_ref_list.clear();
        }

        ret_val
    }

    /// Resume executing async function in given scope.
    ///
    /// Currently delegates to Bytecode::execute() which uses the legacy
    /// StackFrame-based path. When exec.rs is fully migrated, this will
    /// call execute_trampoline() instead.
    fn resume_call(
        &mut self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        args: &[JSValue],
    ) -> JSResult {
        if ctx.poll_interrupts() {
            return Err(-1);
        }

        // Get bytecode from the stored BCWrapper.
        // Note: do NOT deref (*self.bytecode_func), as that would convert
        // BCWrapper to Object, losing BCWrapper-specific methods.
        let mut func = self.bytecode_func;
        let func_node_ptr = func.gc_node_ptr().as_ptr();

        let bf = func.payload_mut();
        let bc = bf.bytecode();

        // Build a weak back-reference so closure VarRefs can track this
        // AsyncStateImpl through the temporary StackFrame.
        let async_backref: GcWeak<AsyncStateImpl> =
            unsafe { self.gc_ref().downgrade(ctx.rt_mut().gc_heap_mut()) };

        // Get current frame state from arena.
        let arena = ctx.rt_mut().frame_arena_mut();
        let head = self.frame_head(&*arena);
        let pc_offset = head.cur_pc_offset;
        let sp_offset = head.cur_sp_offset;
        debug_assert!(sp_offset >= 0);

        // Build legacy StackFrame, execute, and restore frame state.
        // The frame metadata (Heap buffer, VarRefs, offsets) is extracted
        // from the legacy StackFrame after execution and stored back into
        // self.frame as an AsyncStackFrame snapshot.
        let (ret_val, frame_snapshot) = {
            use crate::stack_frame::{FrameBuf, FrameState, StackFrame};

            let mut sf = Box::pin(StackFrame::new(
                self.bytecode_func.into(),
                head.mode,
                head.protected_values,
            ));

            let sf_mut = unsafe { sf.as_mut().get_unchecked_mut() };
            sf_mut.is_async = true;
            sf_mut.async_state = async_backref;

            // Transfer Heap buffer ownership to the temporary StackFrame.
            // IMPORTANT: this consumes self.frame (sets it to Active(NULL)).
            // After bc.execute(), we extract the modified state and restore
            // self.frame to a valid Suspended snapshot.
            match mem::replace(&mut self.frame, AsyncFrameState::Active(FrameIndex::NULL)) {
                AsyncFrameState::Active(frame_idx) => {
                    let snapshot = arena.take_frame(frame_idx);
                    match snapshot.head.buf {
                        FrameBufLocation::Heap(heap_buf) => {
                            sf_mut.local_buf = FrameBuf::Spill(heap_buf);
                        }
                        _ => unreachable!("Async frame must be Heap"),
                    }
                    sf_mut.arg_base_ptr = sf_mut.local_buf.as_mut_ptr();
                    unsafe {
                        sf_mut.var_base_ptr = sf_mut
                            .local_buf
                            .as_mut_ptr()
                            .add(snapshot.head.arg_count as usize);
                    }
                    sf_mut.arg_real_count = snapshot.head.arg_real_count;
                    sf_mut.arg_count = snapshot.head.arg_count;
                    sf_mut.var_count = snapshot.head.var_count;
                    sf_mut.cur_pc_offset = snapshot.head.cur_pc_offset;
                    sf_mut.cur_sp_offset = snapshot.head.cur_sp_offset;
                    // Preserve VarRefs from the snapshot.
                    *sf_mut.var_refs_mut() = snapshot.head.var_ref_list;
                }
                AsyncFrameState::Suspended(snapshot) => {
                    match snapshot.head.buf {
                        FrameBufLocation::Heap(heap_buf) => {
                            sf_mut.local_buf = FrameBuf::Spill(heap_buf);
                        }
                        _ => unreachable!("Suspended frame must be Heap"),
                    }
                    sf_mut.arg_base_ptr = sf_mut.local_buf.as_mut_ptr();
                    unsafe {
                        sf_mut.var_base_ptr = sf_mut
                            .local_buf
                            .as_mut_ptr()
                            .add(snapshot.head.arg_count as usize);
                    }
                    sf_mut.arg_real_count = snapshot.head.arg_real_count;
                    sf_mut.arg_count = snapshot.head.arg_count;
                    sf_mut.var_count = snapshot.head.var_count;
                    sf_mut.cur_pc_offset = snapshot.head.cur_pc_offset;
                    sf_mut.cur_sp_offset = snapshot.head.cur_sp_offset;
                    // Preserve VarRefs from the snapshot.
                    *sf_mut.var_refs_mut() = snapshot.head.var_ref_list;
                }
            }

            let rv = bc.execute(
                ctx,
                bf.home_object(),
                sf_mut,
                this_obj,
                JSValue::UNDEFINED,
                pc_offset,
                sp_offset as usize,
                bf.closure_var_refs_mut(),
                args,
                self.throw_flag,
            );

            // Extract modified frame state from the legacy StackFrame
            // before sf is dropped (which would free the Heap buffer).
            // IMPORTANT: this must happen INSIDE the block, because sf
            // is dropped at the closing brace and sf_mut becomes dangling.
            // We use mem::replace to take ownership of the Heap buffer
            // without dropping it, and mem::take for Vec fields.
            let buf_ptr = sf_mut.local_buf.as_mut_ptr();
            let heap_buf = match std::mem::replace(&mut sf_mut.local_buf, FrameBuf::new()) {
                FrameBuf::Spill(b) => b,
                _ => unreachable!("Async frame must use Heap buffer after execute"),
            };
            let buf_cap = heap_buf.len() as u32;
            // Compute offsets from base pointer differences.
            // arg_base_ptr and var_base_ptr point into the Heap buffer;
            // offset_from yields the JSValue-index offset.
            let arg_base_offset = unsafe { sf_mut.arg_base_ptr.offset_from(buf_ptr) } as u32;
            let var_base_offset = unsafe { sf_mut.var_base_ptr.offset_from(buf_ptr) } as u32;

            let snapshot = AsyncStackFrame {
                head: FrameHead {
                    prev: FrameIndex::NULL,
                    state: FrameState::Active,
                    func_obj: self.bytecode_func.into(),
                    mode: sf_mut.mode,
                    cur_pc_offset: sf_mut.cur_pc_offset,
                    cur_sp_offset: sf_mut.cur_sp_offset,
                    protected_values: sf_mut.protected_values,
                    var_ref_list: std::mem::take(sf_mut.var_refs_mut()),
                    buf: FrameBufLocation::Heap(heap_buf),
                    buf_capacity: buf_cap,
                    arg_base_offset,
                    arg_real_count: sf_mut.arg_real_count,
                    arg_count: sf_mut.arg_count,
                    var_base_offset,
                    var_count: sf_mut.var_count,
                },
            };

            (rv, snapshot)
        };

        // Restore the frame state from the legacy execution.
        // The frame is always Suspended after resume_call — the caller
        // (resume) decides whether to put it back in the arena.
        self.frame = AsyncFrameState::Suspended(frame_snapshot);

        // Note: closure values could be allocated in stack-frame's gc scope during execution,
        // before stack-frame is dropping, promote closure values up to func's gc scope.
        let func_node = unsafe { std::ptr::NonNull::new_unchecked(func_node_ptr) };
        let mut rt = ctx.rt_ptr();
        for vr in bf.closure_var_refs() {
            if let Some(n) = vr.value().gc_node_ptr() {
                rt.gc_heap_mut().bind(func_node, n);
            }
        }

        ret_val.err()
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
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?
            .into();

        // Backref link is no longer needed: in the arena model, GC tracing
        // handles async frames through AsyncStateImpl.trace() (for Suspended)
        // and StackFrameArena.trace() (for Active), independently.

        Ok(a_state)
    }
}
