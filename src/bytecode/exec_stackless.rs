// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

use fallible_collections::TryCollect;
use smallvec::SmallVec;

use crate::{
    Array, JS_CLASS_OBJECT, ScopeResult,
    array::alloc_fast_array_slots,
    atom::Atom,
    builtins::{
        generator::{FUNC_RET_AWAIT, FUNC_RET_INITIAL_YIELD, FUNC_RET_YIELD, FUNC_RET_YIELD_STAR},
        iterator::ObjectIter,
    },
    bytecode::{
        OpCode, SpecialObject,
        arithmatic::{UnaryOp, op_binary_logic},
        class_ops, engine_ops,
        operators::{
            js_append_enumerate, js_for_in_next, js_for_in_start, js_for_of_next,
            js_iterator_get_value_done, js_op_define_class_for_arena, js_operator_delete,
            js_operator_in, js_operator_instanceof,
        },
        ref_ops, with_ops,
    },
    context::{ContextPtr, GlobalVarSetMode, ScopedContext},
    error::{Exception, is_backtrace_needed},
    eval::{EvalFlags, EvalType},
    function::{
        BCWrapper, Bytecode, CallContext, FunctionKind, GcScopeKind, JS_CALL_FLAG_CONSTRUCTOR,
        VarRef, build_arg_list, js_build_arguments, js_build_mapped_arguments, js_function_apply,
    },
    module::{js_dynamic_import, js_import_meta},
    object::{JS_PROP_CONFIGURABLE, JSProperty, Object, PayloadPtr, ThrowPolicy},
    utils::{read_from_ptr, unlikely},
    value::JSValue,
    vm_stack::{ExecMode, FrameAllocError, FrameIndex, VM_MAX_CALL_DEPTH, VirtualFrame, VmStack},
};

/// Action returned by the inner dispatch loop when it must switch frames or
/// propagate an exception through the trampoline.
#[derive(Debug)]
pub(crate) enum ExecAction {
    /// Call a bytecode function: the trampoline parks the caller and switches
    /// to the callee frame. Carries `this_obj`/`new_target` for the handler.
    CallBytecode {
        callee_bc: Bytecode,
        callee_frame_idx: FrameIndex,
        /// SP offset in caller frame where return value should be stored.
        return_sp: usize,
        this_obj: JSValue,
        new_target: JSValue,
        /// Instance created for a non-derived constructor call; a
        /// non-object return value is replaced by it.
        ctor_this: Option<JSValue>,
    },
    /// Cross-realm call: the trampoline switches to the callee realm's scope.
    CallBytecodeCrossRealm {
        callee_bc: Bytecode,
        callee_frame_idx: FrameIndex,
        callee_ctx_ptr: ContextPtr,
        return_sp: usize,
        this_obj: JSValue,
        new_target: JSValue,
        /// Same as `CallBytecode::ctor_this`.
        ctor_this: Option<JSValue>,
    },
    /// Tail call: recycle the current frame instead of pushing a `VirtualFrame`.
    TailCall {
        callee_bc: Bytecode,
        callee_func_obj: Object,
        args: Vec<JSValue>,
        /// `this` for the callee (TAIL_CALL_METHOD); UNDEFINED for TAIL_CALL.
        this_obj: JSValue,
    },
    /// Frame completed or suspended (`true` = yield/await).
    Return(JSValue, bool),
    /// Uncaught exception; propagate through the trampoline's virtual frames.
    Unwind,
    /// Exception raised in the inner loop. The outer loop scans for a handler
    /// and either continues there or propagates `Unwind`.
    Exception,
}

/// GC scope wrapper for the trampoline.
///
/// `inner` is `Option` (not `ManuallyDrop`) so a panic still pops the scope;
/// normal paths take it out via `into_inner()` or move it into a
/// `VirtualFrame`. A borrowed scope owns nothing.
///
/// # Safety of the `'static` transmute
/// `ContextPtr` is `Copy` and `GcScope`'s lifetime is `PhantomData`; every
/// scope is drained before the heap is destroyed.
#[derive(Debug)]
pub(crate) struct TrampolineScope {
    inner: Option<ScopedContext<'static>>,
}

impl TrampolineScope {
    /// Push a new scope.
    #[inline(always)]
    pub(crate) unsafe fn new(ctx: &mut ContextPtr) -> Self {
        Self {
            inner: Some(unsafe {
                std::mem::transmute::<ScopedContext<'_>, ScopedContext<'static>>(ctx.new_scope())
            }),
        }
    }

    /// Wrap the current scope without owning it.
    #[inline(always)]
    fn borrowed(scope: &ScopedContext<'_>) -> Self {
        Self {
            inner: Some(scope.borrow_current_scope()),
        }
    }

    /// Extract the scope; the caller must drop it (popping it if owned).
    pub(crate) fn into_inner(mut self) -> ScopedContext<'static> {
        self.inner.take().expect("trampoline scope already taken")
    }

    /// Drop the current scope and install `saved` in its place.
    ///
    /// # Safety
    ///
    /// `saved` must be the scope directly below the current one on the GC
    /// scope stack.
    #[inline(always)]
    unsafe fn swap_in_saved(&mut self, saved: ScopedContext<'static>) {
        drop(self.inner.take());
        self.inner = Some(saved);
    }

    /// Borrow the inner scope for `execute_with_arena`.
    #[inline(always)]
    pub(crate) const fn scope_mut(&mut self) -> &mut ScopedContext<'static> {
        self.inner.as_mut().expect("trampoline scope already taken")
    }
}

impl std::ops::Deref for TrampolineScope {
    type Target = ScopedContext<'static>;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().expect("trampoline scope already taken")
    }
}

impl std::ops::DerefMut for TrampolineScope {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().expect("trampoline scope already taken")
    }
}

/// Virtual frames of one `execute_trampoline` invocation.
///
/// Owns the parked caller scopes. `Drop` drains newest-first so a panic still
/// unwinds the GC scope stack in LIFO order.
#[derive(Debug)]
struct VirtualFrames(Vec<VirtualFrame>);

impl VirtualFrames {
    const fn new() -> Self {
        Self(Vec::new())
    }
}

impl std::ops::Deref for VirtualFrames {
    type Target = Vec<VirtualFrame>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for VirtualFrames {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for VirtualFrames {
    fn drop(&mut self) {
        while let Some(mut vf) = self.0.pop() {
            if let Some(scope) = vf.caller_scope.take() {
                drop(scope);
            }
        }
    }
}

/// Caller state parked by [`Bytecode::execute_with_arena`] while a same-realm
/// callee runs inline (the in-function counterpart of [`VirtualFrame`]).
struct InlineCaller {
    bc: Bytecode,
    frame_idx: FrameIndex,
    /// Resume pc within `bc`, after the call's operands.
    pc_offset: usize,
    /// Operand stack offset at the call, used by the unwind path.
    sp_offset: i32,
    captured_vars: *mut [VarRef],
    arg_base_ptr: *mut JSValue,
    var_base_ptr: *mut JSValue,
    sp_base: *mut JSValue,
    mode: ExecMode,
    func_obj: Object,
    arg_real_count: u16,
    this_obj: JSValue,
    new_target: JSValue,
    /// SP offset where the callee stores its return value.
    return_sp: usize,
    /// Instance created for a non-derived constructor call.
    ctor_this: Option<JSValue>,
    /// Saved caller scope; `None` when the callee shared the caller's scope.
    caller_scope: Option<ScopedContext<'static>>,
}

/// Inline caller stack. `Drop` drains newest-first to keep GC scopes LIFO.
struct InlineFrames(Vec<InlineCaller>);

impl InlineFrames {
    const fn new() -> Self {
        Self(Vec::new())
    }
}

impl std::ops::Deref for InlineFrames {
    type Target = Vec<InlineCaller>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for InlineFrames {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for InlineFrames {
    fn drop(&mut self) {
        // `Vec` drops front-to-back (oldest first), which would pop GC scopes
        // out of LIFO order. Drain from the back (panic paths only; normal
        // paths pop every caller explicitly).
        while let Some(caller) = self.0.pop() {
            drop(caller.caller_scope);
        }
    }
}

/// Fast `+` for the four Int/Float64 tag pairs. Other tags take `arith_add`,
/// so coercion order and exceptions are unchanged.
#[inline(always)]
fn fast_numeric_add(a: JSValue, b: JSValue) -> Option<JSValue> {
    use crate::JSValueTag::{Float64, Int};
    match (a.tag(), b.tag()) {
        (Int, Int) => Some(JSValue::from(
            a.payload_i32() as i64 + b.payload_i32() as i64,
        )),
        (Float64, Int) => Some(JSValue::make_float(unsafe {
            a.as_float_unchecked() + b.payload_i32() as f64
        })),
        (Int, Float64) => Some(JSValue::make_float(unsafe {
            a.payload_i32() as f64 + b.as_float_unchecked()
        })),
        (Float64, Float64) => Some(JSValue::make_float(unsafe {
            a.as_float_unchecked() + b.as_float_unchecked()
        })),
        _ => None,
    }
}

/// Fast `-` for the same four Int/Float64 tag pairs as `fast_numeric_add`.
/// The Int arm widens to i64: i32 subtraction can leave the i32 range
/// (`0 - -2^31`), where the old i32 op wrapped in release builds.
#[inline(always)]
fn fast_numeric_sub(a: JSValue, b: JSValue) -> Option<JSValue> {
    use crate::JSValueTag::{Float64, Int};
    match (a.tag(), b.tag()) {
        (Int, Int) => Some(JSValue::from(
            a.payload_i32() as i64 - b.payload_i32() as i64,
        )),
        (Float64, Int) => Some(JSValue::make_float(unsafe {
            a.as_float_unchecked() - b.payload_i32() as f64
        })),
        (Int, Float64) => Some(JSValue::make_float(unsafe {
            a.payload_i32() as f64 - b.as_float_unchecked()
        })),
        (Float64, Float64) => Some(JSValue::make_float(unsafe {
            a.as_float_unchecked() - b.as_float_unchecked()
        })),
        _ => None,
    }
}

/// Fast `*` for the same four Int/Float64 tag pairs. The Int arm keeps
/// `TwoNumbers::mul`'s `-0.0` rule (zero product with a negative operand).
#[inline(always)]
fn fast_numeric_mul(a: JSValue, b: JSValue) -> Option<JSValue> {
    use crate::JSValueTag::{Float64, Int};
    match (a.tag(), b.tag()) {
        (Int, Int) => {
            // i32 x i32 always fits i64.
            let n = (a.payload_i32() as i64) * (b.payload_i32() as i64);
            Some(if n == 0 && (a.payload_i32() | b.payload_i32()) < 0 {
                JSValue::make_float(-0.0)
            } else {
                JSValue::from(n)
            })
        }
        (Float64, Int) => Some(JSValue::make_float(unsafe {
            a.as_float_unchecked() * b.payload_i32() as f64
        })),
        (Int, Float64) => Some(JSValue::make_float(unsafe {
            a.payload_i32() as f64 * b.as_float_unchecked()
        })),
        (Float64, Float64) => Some(JSValue::make_float(unsafe {
            a.as_float_unchecked() * b.as_float_unchecked()
        })),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
/// Get or create the `VarRef` for argument/local `var_idx` of the current
/// frame. Shared with the JIT's mapped-arguments helper.
pub(crate) fn get_var_ref(
    ctx: &mut ScopedContext,
    var_idx: u16,
    is_arg: bool,
    arg_base_ptr: *mut JSValue,
    var_base_ptr: *mut JSValue,
    arena: &mut VmStack,
    cur_frame_idx: FrameIndex,
    arg_count: u16,
    var_count: u16,
) -> Result<VarRef, i32> {
    debug_assert!(ctx.is_arena_mode());

    // Extract frame buffer range before any mutable borrow of arena.
    #[cfg(debug_assertions)]
    let frame_buf_range = {
        let head = arena.frame(cur_frame_idx);
        (head.buf.data_ptr(), head.buf.capacity())
    };

    let var_refs = arena.var_refs_or_default(cur_frame_idx);
    if let Some(vr) = var_refs
        .iter()
        .find(|x| x.is_match(var_idx, is_arg))
        .copied()
    {
        return Ok(vr);
    }
    let pvalue = unsafe {
        NonNull::new_unchecked(if is_arg {
            debug_assert!(var_idx < arg_count);
            arg_base_ptr.offset(var_idx as isize)
        } else {
            debug_assert!(var_idx < var_count);
            var_base_ptr.offset(var_idx as isize)
        })
    };
    let vref = VarRef::create_bind(ctx, var_idx, is_arg, pvalue)?;

    #[cfg(debug_assertions)]
    {
        let (buf_start, buf_cap) = frame_buf_range;
        let buf_start: *const JSValue = buf_start as *const JSValue;
        let buf_end = unsafe { buf_start.add(buf_cap as usize) };
        let ptr: *const JSValue = pvalue.as_ptr() as *const JSValue;
        debug_assert!(
            ptr >= buf_start && ptr < buf_end,
            "get_var_ref: VarRef::Bind pvalue {:p} out of frame buf [{:p}, {:p})",
            ptr,
            buf_start,
            buf_end
        );
    }

    var_refs.push(vref);
    Ok(vref)
}

/// Scan the current frame for a `catch` handler at `pc`.
///
/// `sp_offset` (relative to the stack base) is written to the frame header for
/// the scan and any GC that runs while handling.
fn catch_in_frame_at(
    rt: &crate::runtime::RuntimeImpl,
    realm_scope: &mut ScopedContext,
    arena: &mut VmStack,
    cur_frame_idx: FrameIndex,
    pc: usize,
    sp_offset: i32,
) -> Option<(usize, usize)> {
    arena.frame_mut(cur_frame_idx).cur_sp_offset = sp_offset;

    // Interrupt bypasses catch handling — it is not a JS exception.
    if rt.interrupt_code.is_some() {
        return None;
    }

    // Build the backtrace before the catch scan consumes the stack.
    if let Exception::Error(err_val) = rt.exception()
        && is_backtrace_needed(err_val)
    {
        arena.frame_mut(cur_frame_idx).cur_pc_offset = pc;
        realm_scope.build_backtrace_to(err_val, None, 0);
    }

    arena.find_catch_in_frame(cur_frame_idx, realm_scope)
}

impl Bytecode {
    /// Inline-cache site `ic_idx`, if present.
    ///
    /// Takes a raw pointer because the cache is interior-mutable.
    #[cfg(feature = "inline_cache")]
    #[inline]
    fn inline_cache_site<'a>(
        this: *const Bytecode,
        ic_idx: usize,
    ) -> Option<&'a mut crate::function::InlineCacheSite> {
        // SAFETY: single-threaded interpreter; the cache is interior-mutable.
        // Reborrow `&Bytecode` first to avoid autoref through `Deref`.
        let this = unsafe { &*this };
        let cache = unsafe { &mut *this.inline_cache.get() };
        cache.get_mut(ic_idx)
    }

    /// Execute this function on the arena stack and return the next action for
    /// the trampoline.
    #[allow(clippy::too_many_arguments)]
    fn execute_with_arena(
        &self,
        realm_scope: &mut ScopedContext,
        arena: &mut VmStack,
        cur_frame_idx: FrameIndex,
        gc_scope_base_depth: u32,
        mut this_obj: JSValue,
        mut new_target: JSValue,
        pc_offset: usize,
        sp_offset: usize,
        throwed: bool,
    ) -> ExecAction {
        debug_assert!(realm_scope.is_arena_mode());

        let mut rt = realm_scope.rt_ptr();

        // Feedback is recorded for every non-inert policy; tier-2 type feedback
        // only when the policy can reach tier 2.
        #[cfg(feature = "jit")]
        let jit_active = crate::jit::is_active(&rt);
        #[cfg(feature = "jit")]
        let jit_feedback = jit_active && crate::jit::records_feedback(&rt);
        // Inline same-realm calls unless the JIT owns the frame: its native
        // entry/exit reports assume the entered frame is the trampoline's.
        #[cfg(feature = "jit")]
        let inline_calls = !jit_active;
        #[cfg(not(feature = "jit"))]
        let inline_calls = true;

        // The frame's bytecode changes on inline calls, so the loop uses
        // `cur_bc`. The slices live as long as the GC payload (rooted by the
        // frame's `func_obj`), hence the `'static` widening.
        let mut cur_bc = *self;
        let mut bytecodes: &'static [u8] =
            unsafe { std::mem::transmute::<&[u8], &'static [u8]>(cur_bc.opcodes()) };
        debug_assert!(
            // A pending exception may resume at the end pc; that path never
            // fetches an opcode.
            pc_offset < bytecodes.len() || throwed,
            "pc_offset={}, bytecodes.len()={}",
            pc_offset,
            bytecodes.len()
        );

        // Frame data pointers. Arena data is fixed-capacity, so these stay
        // valid; header access always goes through `arena_ptr`.
        let (
            mut arg_base_ptr,
            mut var_base_ptr,
            mut sp_base,
            mut mode,
            mut func_obj,
            mut arg_real_count,
        ) = {
            let frame = arena.frame(cur_frame_idx);
            let arg_base = frame.buf.data_ptr_unchecked();
            let var_base = unsafe { arg_base.add(frame.var_base_offset as usize) };
            let sp = unsafe {
                frame
                    .buf
                    .data_ptr_unchecked()
                    .add((frame.var_base_offset + frame.var_count as u32) as usize)
            };
            (
                arg_base,
                var_base,
                sp,
                frame.mode,
                frame.func_obj,
                frame.arg_real_count,
            )
        };

        // Raw root for the arena. Re-derive every header access through it:
        // calls push frames and reborrow the arena, so a borrow held across
        // iterations is invalid under Stacked Borrows (and fat LTO may exploit
        // that to corrupt sp/pc).
        let arena_ptr: *mut VmStack = arena;

        // Accessors re-derive a fresh borrow from `arena_ptr`.
        macro_rules! frame {
            ($idx:expr) => {
                unsafe { (*arena_ptr).frame($idx) }
            };
        }
        macro_rules! frame_mut {
            ($idx:expr) => {
                unsafe { (*arena_ptr).frame_mut($idx) }
            };
        }
        macro_rules! arena_mut {
            () => {
                unsafe { &mut *arena_ptr }
            };
        }

        // `cur_frame_idx` tracks the executing frame; `start_frame_idx` is the
        // frame owned by the trampoline.
        let start_frame_idx = cur_frame_idx;
        let mut cur_frame_idx = cur_frame_idx;
        macro_rules! save_sp {
            ($val:expr) => {
                unsafe { (*arena_ptr).frame_mut(cur_frame_idx).cur_sp_offset = $val }
            };
        }
        macro_rules! save_pc {
            ($val:expr) => {
                unsafe { (*arena_ptr).frame_mut(cur_frame_idx).cur_pc_offset = $val }
            };
        }

        let mut cpools: &'static [JSValue] =
            unsafe { std::mem::transmute::<&[JSValue], &'static [JSValue]>(cur_bc.consts()) };

        let mut pc_base = bytecodes.as_ptr();
        let mut pc = unsafe { pc_base.add(pc_offset) };
        let mut sp = unsafe { sp_base.add(sp_offset) };

        save_sp!(sp_offset as i32);

        // Back-edge OSR: a taken back edge may switch this frame to its native
        // entry. `cur_sp_offset` is synced first because the prologue reads it.
        #[cfg(feature = "jit")]
        macro_rules! jit_note_backedge {
            ($target:expr) => {
                if jit_active && crate::jit::note_backedge(&rt, &cur_bc, $target) {
                    jit_retry_native_now!();
                }
            };
        }

        // Closure captures of the current function, held as a raw pointer: the
        // loop runs arbitrary code between uses, so a long-lived `&mut` would
        // alias the GC payload.
        let mut captured_vars: *mut [VarRef] = {
            if let Ok(mut bfun) = BCWrapper::try_from(JSValue::from(func_obj)) {
                // SAFETY: `bfun` is a Copy handle, so the payload outlives this
                // call; no GC runs during the lookup.
                let cv: *mut [VarRef] = bfun.payload_mut().captured_vars_mut();
                cv
            } else {
                // Non-bytecode functions have no captures; the trampoline must
                // never call this for them.
                std::ptr::slice_from_raw_parts_mut(NonNull::<VarRef>::dangling().as_ptr(), 0)
            }
        };
        macro_rules! captured_vars {
            () => {
                // SAFETY: see above; each borrow ends before the next runtime
                // call can touch the payload.
                unsafe { &mut *captured_vars }
            };
        }
        macro_rules! args {
            () => {
                // SAFETY: the region is `arg_base_ptr..+arg_real_count`; the
                // slice never outlives one use (the interpreter writes the
                // same slots through `ARG_REF!`).
                unsafe { std::slice::from_raw_parts(arg_base_ptr, arg_real_count as usize) }
            };
        }
        // [[HomeObject]] of the current function, used by `super`.
        macro_rules! func_home {
            () => {
                BCWrapper::try_from(JSValue::from(func_obj))
                    .ok()
                    .and_then(|bfun| bfun.payload().home_object())
            };
        }

        // ── Inlined same-realm calls ──
        // `callers` is declared before `cur_scope` so a panic drops the active
        // scope first, then the parked ones newest-first.
        let mut callers = InlineFrames::new();
        let mut cur_scope = TrampolineScope::borrowed(&*realm_scope);
        let mut scope_ptr = cur_scope.scope_mut() as *mut ScopedContext<'static>;
        // The loop swaps scopes; bind the current one before the macros, which
        // resolve identifiers at their definition site.
        let mut realm_scope: &mut ScopedContext<'static> = unsafe { &mut *scope_ptr };

        // Switch frame state to a freshly allocated callee frame.
        macro_rules! inline_switch_frame {
            ($callee_bc:expr, $callee_idx:expr, $captured:expr,
             $callee_this:expr, $callee_nt:expr) => {{
                let callee_bc = $callee_bc;
                let callee_idx = $callee_idx;
                let (new_sp_base, new_arg_base, new_var_base, new_mode, new_func, new_arg_real) = {
                    let head = frame!(callee_idx);
                    let arg_base = head.buf.data_ptr_unchecked();
                    (
                        head.stack_base_ptr(),
                        arg_base,
                        unsafe { arg_base.add(head.var_base_offset as usize) },
                        head.mode,
                        head.func_obj,
                        head.arg_real_count,
                    )
                };

                cur_bc = callee_bc;
                bytecodes =
                    unsafe { std::mem::transmute::<&[u8], &'static [u8]>(cur_bc.opcodes()) };
                cpools = unsafe {
                    std::mem::transmute::<&[JSValue], &'static [JSValue]>(cur_bc.consts())
                };
                pc_base = bytecodes.as_ptr();
                pc = pc_base;
                cur_frame_idx = callee_idx;
                // `is_strict_mode`, `current_function`, backtrace and GC read
                // this.
                rt.active_frame_index = callee_idx;
                sp_base = new_sp_base;
                arg_base_ptr = new_arg_base;
                var_base_ptr = new_var_base;
                sp = sp_base;
                mode = new_mode;
                func_obj = new_func;
                arg_real_count = new_arg_real;
                captured_vars = $captured;
                this_obj = $callee_this;
                new_target = $callee_nt;
            }};
        }

        // Park the current frame and switch to the callee (the trampoline's
        // `CallBytecode` handler, but keeping the caller in `callers`).
        macro_rules! inline_call_switch {
            ($callee_bc:expr, $callee_idx:expr, $captured:expr,
             $ret_sp:expr, $callee_this:expr, $callee_nt:expr, $ctor_this:expr) => {{
                let callee_bc = $callee_bc;
                let callee_idx = $callee_idx;
                // Sync the caller's header before the switch; GC/backtrace read
                // it while the callee runs.
                save_sp!(sp.offset_from(sp_base) as i32);
                let caller_sp_offset = unsafe { sp.offset_from(sp_base) as i32 };
                let run_in_caller_scope = match callee_bc.gc_scope_kind() {
                    GcScopeKind::None => true,
                    // Light allocations are rare and re-anchored before the
                    // next safepoint. If the enclosing scope is cleared from
                    // `gc_scope_base_depth` (every owned scope), share it and
                    // skip the push/promote/pop round trip.
                    GcScopeKind::Light => cur_scope.depth() >= gc_scope_base_depth,
                    GcScopeKind::Full => false,
                };
                let caller_scope = if run_in_caller_scope {
                    None
                } else {
                    let mut callee_realm_ctx = callee_bc.realm();
                    // SAFETY: same scope-swap contract as the trampoline.
                    let callee_scope = unsafe { TrampolineScope::new(&mut callee_realm_ctx) };
                    let saved = std::mem::replace(&mut cur_scope, callee_scope).into_inner();
                    scope_ptr = cur_scope.scope_mut() as *mut ScopedContext<'static>;
                    realm_scope = unsafe { &mut *scope_ptr };
                    Some(saved)
                };
                callers.push(InlineCaller {
                    bc: cur_bc,
                    frame_idx: cur_frame_idx,
                    pc_offset: unsafe { pc.offset_from(pc_base) as usize },
                    sp_offset: caller_sp_offset,
                    captured_vars,
                    arg_base_ptr,
                    var_base_ptr,
                    sp_base,
                    mode,
                    func_obj,
                    arg_real_count,
                    this_obj,
                    new_target,
                    return_sp: $ret_sp,
                    ctor_this: $ctor_this,
                    caller_scope,
                });
                inline_switch_frame!(callee_bc, callee_idx, $captured, $callee_this, $callee_nt);
            }};
        }

        // Free the current frame, restore its inline caller, and store the
        // return value in the caller's stack slot.
        macro_rules! inline_return_to_caller {
            ($ret_val:expr) => {{
                let ret_val = $ret_val;
                let caller = callers.pop().expect("inline return without caller");
                // Detach VarRefs before recycling the frame buffer.
                arena_mut!().detach_and_free(cur_frame_idx);
                if let Some(saved) = caller.caller_scope {
                    ret_val.promote(&cur_scope);
                    // SAFETY: `saved` was parked directly below `cur_scope`.
                    unsafe { cur_scope.swap_in_saved(saved) };
                    scope_ptr = cur_scope.scope_mut() as *mut ScopedContext<'static>;
                    realm_scope = unsafe { &mut *scope_ptr };
                }
                // Legacy constructor: a non-object return becomes the instance.
                let ret_val = match caller.ctor_this {
                    Some(inst) if !ret_val.is_object() => inst,
                    _ => ret_val,
                };
                unsafe {
                    *caller.sp_base.add(caller.return_sp) = ret_val;
                }

                cur_bc = caller.bc;
                bytecodes =
                    unsafe { std::mem::transmute::<&[u8], &'static [u8]>(cur_bc.opcodes()) };
                cpools = unsafe {
                    std::mem::transmute::<&[JSValue], &'static [JSValue]>(cur_bc.consts())
                };
                pc_base = bytecodes.as_ptr();
                cur_frame_idx = caller.frame_idx;
                rt.active_frame_index = caller.frame_idx;
                pc = unsafe { pc_base.add(caller.pc_offset) };
                sp_base = caller.sp_base;
                sp = unsafe { sp_base.add(caller.return_sp + 1) };
                save_sp!(caller.return_sp as i32 + 1);
                arg_base_ptr = caller.arg_base_ptr;
                var_base_ptr = caller.var_base_ptr;
                captured_vars = caller.captured_vars;
                mode = caller.mode;
                func_obj = caller.func_obj;
                arg_real_count = caller.arg_real_count;
                this_obj = caller.this_obj;
                new_target = caller.new_target;
            }};
        }

        // Propagate a pending exception through the inline callers after the
        // current frame has already been freed. Each restored caller is
        // scanned for a catch, then a finally, exactly like the trampoline's
        // Unwind handler; the trampoline's start frame is never freed here.
        macro_rules! inline_unwind_dead {
            ($label:lifetime) => {{
                loop {
                    let Some(caller) = callers.pop() else {
                        return ExecAction::Unwind;
                    };
                    let is_start = caller.frame_idx == start_frame_idx;
                    if let Some(saved) = caller.caller_scope {
                        unsafe { cur_scope.swap_in_saved(saved) };
                        scope_ptr = cur_scope.scope_mut() as *mut ScopedContext<'static>;
                    }
                    cur_bc = caller.bc;
                    bytecodes = unsafe {
                        std::mem::transmute::<&[u8], &'static [u8]>(cur_bc.opcodes())
                    };
                    cpools = unsafe {
                        std::mem::transmute::<&[JSValue], &'static [JSValue]>(cur_bc.consts())
                    };
                    pc_base = bytecodes.as_ptr();
                    cur_frame_idx = caller.frame_idx;
                    rt.active_frame_index = caller.frame_idx;
                    sp_base = caller.sp_base;
                    save_sp!(caller.sp_offset);
                    arg_base_ptr = caller.arg_base_ptr;
                    var_base_ptr = caller.var_base_ptr;
                    captured_vars = caller.captured_vars;
                    mode = caller.mode;
                    func_obj = caller.func_obj;
                    arg_real_count = caller.arg_real_count;
                    this_obj = caller.this_obj;
                    new_target = caller.new_target;
                    realm_scope = unsafe { &mut *scope_ptr };

                    // An interrupt bypasses catch and finally handling.
                    if rt.interrupt_code.is_some() {
                        if is_start {
                            return ExecAction::Unwind;
                        }
                        arena_mut!().detach_and_free(cur_frame_idx);
                        continue;
                    }

                    if let Some((new_pc, new_sp)) = catch_in_frame_at(
                        &rt,
                        realm_scope,
                        arena_mut!(),
                        cur_frame_idx,
                        caller.pc_offset,
                        caller.sp_offset,
                    ) {
                        save_sp!(new_sp as i32);
                        pc = unsafe { pc_base.add(new_pc) };
                        sp = unsafe { sp_base.add(new_sp) };
                        break;
                    }

                    if let Some((finally_pc, finally_sp)) = arena_mut!()
                        .find_finally_in_frame(cur_frame_idx, bytecodes.len())
                    {
                        let err_val = realm_scope
                            .take_exception()
                            .to_value(realm_scope);
                        let finally_sp_base = frame!(cur_frame_idx).stack_base_ptr();
                        unsafe {
                            std::ptr::write(finally_sp_base.add(finally_sp - 1), err_val);
                        }
                        pc = unsafe { pc_base.add(finally_pc) };
                        sp = unsafe { finally_sp_base.add(finally_sp + 1) };
                        save_sp!((finally_sp + 1) as i32);
                        break;
                    }

                    if is_start {
                        return ExecAction::Unwind;
                    }
                    arena_mut!().detach_and_free(cur_frame_idx);
                }
                continue $label;
            }};
        }

        // Inline TCO: recycle the current frame in place at the caller's depth,
        // keeping the `callers` chain untouched. The label is passed in because
        // `macro_rules!` resolves labels at the definition site.
        macro_rules! inline_tail_switch {
            ($label:lifetime, $callee_bc:expr, $callee_fun:expr, $args:expr,
             $callee_this:expr, $captured:expr) => {{
                let callee_bc = $callee_bc;
                // Copy the args first (the buffer is freed below); SmallVec
                // keeps the common ≤4-arg case stack-only.
                let tail_args: SmallVec<[JSValue; 4]> = $args.iter().copied().collect();
                let orig_prev = frame!(cur_frame_idx).prev;
                let tail_depth = frame!(cur_frame_idx).depth.saturating_sub(1);
                arena_mut!().detach_and_free(cur_frame_idx);
                let new_idx = match build_callee_frame(
                    arena_mut!(),
                    tail_depth,
                    $callee_fun.into(),
                    callee_bc,
                    &tail_args,
                    $callee_this,
                ) {
                    Ok(idx) => idx,
                    Err(e) => {
                        match e {
                            FrameAllocError::Oom => {
                                let _ = realm_scope.throw_stack_fail();
                            }
                            FrameAllocError::Exceeded => {
                                let _ = realm_scope.throw_stack_exceeded();
                            }
                        }
                        inline_unwind_dead!($label);
                    }
                };
                frame_mut!(new_idx).prev = orig_prev;
                inline_switch_frame!(
                    callee_bc,
                    new_idx,
                    $captured,
                    $callee_this,
                    JSValue::UNDEFINED
                );
                continue $label;
            }};
        }

        if throwed {
            // An interrupt is control flow, not a JS exception: it bypasses
            // catch handling entirely. Otherwise a caller's `catch` could
            // swallow an interrupt raised in a callee.
            if rt.interrupt_code.is_some() {
                return ExecAction::Unwind;
            }

            let cur_pc = unsafe { pc.offset_from(pc_base) as usize };

            // Build the backtrace before the catch scan; skip if present.
            if let Exception::Error(err_val) = rt.exception()
                && is_backtrace_needed(err_val)
            {
                save_pc!(cur_pc);
                realm_scope.build_backtrace_to(err_val, None, 0);
            }

            if let Some((new_pc, new_sp)) =
                unsafe { (*arena_ptr).find_catch_in_frame(cur_frame_idx, realm_scope) }
            {
                save_sp!(new_sp as i32);
                unsafe {
                    pc = pc_base.add(new_pc);
                    sp = sp_base.add(new_sp);
                }
            } else {
                // Not caught here; propagate.
                return ExecAction::Unwind;
            }
        }

        // ── JIT native entry / re-entry (OSR) ──
        // `pc_offset` is 0 on entry or an OSR resume point (after a call or at
        // a loop header). `Resume`/`Retry`/`Exception` rewind `pc`/`sp` to the
        // reported offsets before falling through to the interpreter loop.
        #[cfg(feature = "jit")]
        let mut jit_initial_action: Option<ExecAction> = None;
        #[cfg(feature = "jit")]
        let jit_scope_ptr = realm_scope as *mut ScopedContext as *mut u8;
        #[cfg(feature = "jit")]
        if !throwed && jit_active {
            let cur_sp_offset_ptr: *mut i32 =
                unsafe { &mut (*arena_ptr).frame_mut(cur_frame_idx).cur_sp_offset };
            if let Some((kind, jf)) = crate::jit::invoke_native(
                &rt,
                &cur_bc,
                jit_scope_ptr,
                arg_base_ptr,
                var_base_ptr,
                sp_base,
                cur_sp_offset_ptr,
                this_obj,
                pc_offset as u32,
                u32::from(arg_real_count),
                arena_ptr,
                cur_frame_idx.as_usize() as u32,
                new_target,
                func_home!(),
            ) {
                match kind {
                    crate::jit::JitExitKind::Done => {
                        save_sp!(jf.out_sp);
                        save_pc!(jf.out_pc as usize);
                        return ExecAction::Return(jf.out_value, false);
                    }
                    crate::jit::JitExitKind::Interrupt | crate::jit::JitExitKind::Unwind => {
                        return ExecAction::Unwind;
                    }
                    crate::jit::JitExitKind::Exception => {
                        save_sp!(jf.out_sp);
                        pc = unsafe { pc_base.add(jf.out_pc as usize) };
                        sp = unsafe { sp_base.add(jf.out_sp as usize) };
                        jit_initial_action = Some(ExecAction::Exception);
                    }
                    crate::jit::JitExitKind::Resume | crate::jit::JitExitKind::Retry => {
                        save_sp!(jf.out_sp);
                        pc = unsafe { pc_base.add(jf.out_pc as usize) };
                        sp = unsafe { sp_base.add(jf.out_sp as usize) };
                    }
                    _ => {}
                }
            }
        }

        // Native code reborrowed the context through a raw pointer, so the
        // original `&mut` is stale. Assign the binding declared before the
        // frame-switch macros; a new `let` would shadow it and leave the opcode
        // body on the stale caller scope after an inline swap.
        #[cfg(feature = "jit")]
        {
            realm_scope = unsafe { &mut *(jit_scope_ptr as *mut ScopedContext) };
        }

        // ── Stack access macros ──

        macro_rules! SP_VAL {
            (0) => {
                unsafe { *sp }
            };
            ($idx:expr) => {
                unsafe { *sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_REF {
            (0) => {
                unsafe { sp.as_ref().unwrap() }
            };
            ($idx:expr) => {
                unsafe { sp.offset($idx as isize).as_ref().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_REF {
            (0) => {
                unsafe { &*sp }
            };
            ($idx:expr) => {
                unsafe { &*sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_MUT {
            ($idx:expr) => {
                unsafe { sp.offset($idx as isize).as_mut().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_MUT {
            ($idx:expr) => {
                unsafe { &mut *sp.offset($idx as isize) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! SP_PUT {
            ($idx:expr, $val:expr) => {
                unsafe {
                    *sp.offset(($idx) as isize).as_mut().unwrap() = $val;
                }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! SP_PUT {
            ($idx:expr, $val:expr) => {
                unsafe {
                    *sp.offset(($idx) as isize) = $val;
                }
            };
        }

        // `sp` stays in a register. Sync the header only where code outside
        // this loop reads it: GC, catch/finally scans, JIT entry/OSR, and
        // runtime re-entry (`sync_sp!` call sites).
        macro_rules! SP_GO {
            ($step:expr) => {
                sp = unsafe { sp.offset(($step) as isize) }
            };
        }

        // Sync the local `sp` to the frame header.
        macro_rules! sync_sp {
            () => {
                save_sp!(unsafe { sp.offset_from(sp_base) as i32 })
            };
        }

        macro_rules! PC_GO {
            ($step:expr) => {
                pc = unsafe { pc.offset(($step) as isize) }
            };
        }
        macro_rules! PC_FETCH {
            (i32) => {
                unsafe {
                    let v = read_from_ptr::<i32>(pc);
                    pc = pc.add(4);
                    v
                }
            };
            (u32) => {
                unsafe {
                    let v = read_from_ptr::<u32>(pc);
                    pc = pc.add(4);
                    v
                }
            };
            (i16) => {
                unsafe {
                    let v = read_from_ptr::<i16>(pc);
                    pc = pc.add(2);
                    v
                }
            };
            (u16) => {
                unsafe {
                    let v = read_from_ptr::<u16>(pc);
                    pc = pc.add(2);
                    v
                }
            };
            (u8) => {
                unsafe {
                    let v = read_from_ptr::<u8>(pc);
                    pc = pc.add(1);
                    v
                }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! ARG_REF {
            ($idx:expr) => {
                unsafe { arg_base_ptr.offset(($idx) as _).as_mut().unwrap() }
            };
        }
        #[cfg(not(debug_assertions))]
        macro_rules! ARG_REF {
            ($idx:expr) => {
                unsafe { &mut *arg_base_ptr.offset(($idx) as _) }
            };
        }

        #[cfg(debug_assertions)]
        macro_rules! VAR_REF {
        [$idx:expr] => {
            unsafe { (var_base_ptr.offset(($idx) as isize).as_mut().unwrap()) }
        };
    }
        #[cfg(not(debug_assertions))]
        macro_rules! VAR_REF {
        [$idx:expr] => {
            unsafe { (&mut *var_base_ptr.offset(($idx) as isize)) }
        };
    }

        #[cfg(debug_assertions)]
        macro_rules! CPOOL {
        [$idx:expr] => {
            cpools.as_ref()[($idx) as usize]
        };
    }
        #[cfg(not(debug_assertions))]
        macro_rules! CPOOL {
        [$idx:expr] => {
            unsafe {
                *cpools.as_ref().as_ptr().add(($idx) as usize)
            }
        };
    }

        // ── JIT hooks ──
        // Try the frame's native entry at the current pc (after a native callee
        // or an OSR back edge). Called at those sites, not per opcode.
        #[cfg(feature = "jit")]
        macro_rules! jit_retry_native_now {
            () => {
                if jit_active {
                    // The OSR prologue reads `cur_sp_offset`; sync `sp` first.
                    sync_sp!();
                    let attempt_pc = unsafe { pc.offset_from(pc_base) as u32 };
                    let cur_sp_offset_ptr: *mut i32 =
                        unsafe { &mut (*arena_ptr).frame_mut(cur_frame_idx).cur_sp_offset };
                    let entered = crate::jit::invoke_native(
                        &rt,
                        &cur_bc,
                        jit_scope_ptr,
                        arg_base_ptr,
                        var_base_ptr,
                        sp_base,
                        cur_sp_offset_ptr,
                        this_obj,
                        attempt_pc,
                        u32::from(arg_real_count),
                        arena_ptr,
                        cur_frame_idx.as_usize() as u32,
                        new_target,
                        func_home!(),
                    );
                    // Native code reborrowed the context; the `&mut` is stale.
                    realm_scope = unsafe { &mut *(jit_scope_ptr as *mut ScopedContext) };
                    if let Some((kind, jf)) = entered {
                        match kind {
                            crate::jit::JitExitKind::Done => {
                                save_sp!(jf.out_sp);
                                save_pc!(jf.out_pc as usize);
                                return ExecAction::Return(jf.out_value, false);
                            }
                            crate::jit::JitExitKind::Interrupt
                            | crate::jit::JitExitKind::Unwind => {
                                return ExecAction::Unwind;
                            }
                            crate::jit::JitExitKind::Exception => {
                                save_sp!(jf.out_sp);
                                pc = unsafe { pc_base.add(jf.out_pc as usize) };
                                sp = unsafe { sp_base.add(jf.out_sp as usize) };
                                break ExecAction::Exception;
                            }
                            crate::jit::JitExitKind::Resume | crate::jit::JitExitKind::Retry => {
                                save_sp!(jf.out_sp);
                                pc = unsafe { pc_base.add(jf.out_pc as usize) };
                                sp = unsafe { sp_base.add(jf.out_sp as usize) };
                            }
                            _ => {}
                        }
                    }
                }
            };
        }
        #[cfg(not(feature = "jit"))]
        macro_rules! jit_retry_native_now {
            () => {};
        }

        // The function id is the node address, stable for the runtime.
        #[cfg(feature = "jit")]
        if jit_active {
            crate::jit::note_enter(&rt, &cur_bc);
        }

        // Native entry ended in an exception: scan once before the loop.
        #[cfg(feature = "jit")]
        if jit_initial_action.is_some() {
            let cur_pc = unsafe { pc.offset_from(pc_base) as usize };
            let sp_offset = unsafe { sp.offset_from(sp_base) } as i32;
            let Some((new_pc, new_sp)) = catch_in_frame_at(
                &rt,
                realm_scope,
                arena_mut!(),
                cur_frame_idx,
                cur_pc,
                sp_offset,
            ) else {
                return ExecAction::Unwind;
            };
            save_sp!(new_sp as i32);
            pc = unsafe { pc_base.add(new_pc) };
            sp = unsafe { sp_base.add(new_sp) };
        }

        // ── Opcode dispatch loop ──
        // The outer loop handles catch re-entry. Same-realm calls stay in this
        // loop; other actions return to the trampoline.
        'arena_loop: loop {
            let action: ExecAction = loop {
                let opcode = unsafe { std::mem::transmute::<u8, OpCode>(PC_FETCH!(u8)) };

                match opcode {
                    // ── PUSH / CONST / VALUE ops ──
                    OpCode::PUSH_I32 => {
                        // [^] -> [int_value, ^]
                        let a = PC_FETCH!(i32);
                        SP_PUT!(0, JSValue::from(a));
                        SP_GO!(1);
                    }
                    OpCode::PUSH_CONST => {
                        // [^] -> [const_val, ^]
                        let id = PC_FETCH!(u32);
                        SP_PUT!(0, CPOOL![id]);
                        SP_GO!(1);
                    }
                    OpCode::PUSH_ATOM_VALUE => {
                        let a = PC_FETCH!(u32);
                        let Ok(val) = Atom::from_id(a).to_value(realm_scope) else {
                            break ExecAction::Exception;
                        };
                        SP_PUT!(0, val);
                        SP_GO!(1);
                    }
                    OpCode::UNDEFINED => {
                        SP_PUT!(0, JSValue::UNDEFINED);
                        SP_GO!(1);
                    }
                    OpCode::NULL => {
                        SP_PUT!(0, JSValue::NULL);
                        SP_GO!(1);
                    }
                    OpCode::PUSH_THIS => {
                        let val: JSValue = if !mode.contains(ExecMode::STRICT) {
                            if this_obj.is_undefined_or_null() {
                                JSValue::from(realm_scope.global_obj())
                            } else if this_obj.is_object() {
                                this_obj
                            } else {
                                if let Ok(v) = this_obj.objectify(realm_scope) {
                                    v.into()
                                } else {
                                    break ExecAction::Exception;
                                }
                            }
                        } else {
                            this_obj
                        };
                        SP_PUT!(0, val);
                        SP_GO!(1);
                    }
                    OpCode::PUSH_FALSE => {
                        SP_PUT!(0, JSValue::from(false));
                        SP_GO!(1);
                    }
                    OpCode::PUSH_TRUE => {
                        SP_PUT!(0, JSValue::from(true));
                        SP_GO!(1);
                    }

                    // ── OBJECT / SPECIAL_OBJECT ──
                    OpCode::OBJECT => {
                        // [^] -> [obj, ^]
                        let obj = realm_scope
                            .create_base_object()
                            .map_or(JSValue::EXCEPTION, JSValue::from);
                        SP_PUT!(0, obj);
                        SP_GO!(1);
                        if unlikely(SP_REF!(-1).is_exception()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::SPECIAL_OBJECT => {
                        let id = PC_FETCH!(u8);
                        debug_assert!(
                            (SpecialObject::Arguments as u8..=SpecialObject::ImportMeta as u8)
                                .contains(&id)
                        );
                        match unsafe { std::mem::transmute::<u8, SpecialObject>(id) } {
                            SpecialObject::Arguments => {
                                SP_PUT!(0, js_build_arguments(realm_scope, args!()));
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break ExecAction::Exception;
                                }
                            }
                            SpecialObject::MappedArguments => {
                                // Shared with the JIT.
                                match js_build_mapped_arguments(
                                    realm_scope,
                                    args!(),
                                    arg_base_ptr,
                                    var_base_ptr,
                                    arena_mut!(),
                                    cur_frame_idx,
                                    cur_bc.arg_count,
                                    cur_bc.var_count,
                                    func_obj,
                                ) {
                                    Some(obj) => {
                                        SP_PUT!(0, JSValue::from(obj));
                                        SP_GO!(1);
                                    }
                                    None => break ExecAction::Exception,
                                }
                            }
                            SpecialObject::ThisFunc => {
                                SP_PUT!(0, JSValue::from(func_obj));
                                SP_GO!(1);
                            }
                            SpecialObject::NewTarget => {
                                SP_PUT!(0, new_target);
                                SP_GO!(1);
                            }
                            SpecialObject::HomeObject => {
                                SP_PUT!(0, func_home!().map_or(JSValue::UNDEFINED, JSValue::from));
                                SP_GO!(1);
                            }
                            SpecialObject::VarObject => {
                                SP_PUT!(
                                    0,
                                    realm_scope
                                        .create_base_object_with_proto(None)
                                        .map_or(JSValue::EXCEPTION, JSValue::from)
                                );
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break ExecAction::Exception;
                                }
                            }
                            SpecialObject::ImportMeta => {
                                SP_PUT!(
                                    0,
                                    js_import_meta(realm_scope)
                                        .map_or(JSValue::EXCEPTION, JSValue::from)
                                );
                                SP_GO!(1);
                                if unlikely(SP_REF!(-1).is_exception()) {
                                    break ExecAction::Exception;
                                }
                            }
                        }
                    }

                    // ── REST params ──
                    OpCode::REST => {
                        let start = PC_FETCH!(u16);
                        if let Ok(arr) = args!()
                            .iter()
                            .skip(start as usize)
                            .copied()
                            .try_collect::<Vec<_>>()
                            .map_err(|_| realm_scope.throw_out_of_memory())
                            .and_then(|vv| Array::from_values(realm_scope, vv))
                        {
                            SP_PUT!(0, JSValue::from(arr));
                            SP_GO!(1);
                        } else {
                            SP_PUT!(0, JSValue::EXCEPTION);
                            SP_GO!(1);
                            break ExecAction::Exception;
                        }
                    }

                    // ── Stack manipulation ops ──
                    OpCode::DROP => {
                        SP_GO!(-1);
                    }
                    OpCode::NIP => {
                        SP_PUT!(-2, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::NIP1 => {
                        SP_PUT!(-3, SP_VAL!(-2));
                        SP_PUT!(-2, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::DUP => {
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_GO!(1);
                    }
                    OpCode::DUP1 => {
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_PUT!(-1, SP_VAL!(-2));
                        SP_GO!(1);
                    }
                    OpCode::DUP2 => {
                        SP_PUT!(0, SP_VAL!(-2));
                        SP_PUT!(1, SP_VAL!(-1));
                        SP_GO!(2);
                    }
                    OpCode::SWAP => unsafe {
                        std::ptr::swap(sp.offset(-1), sp.offset(-2));
                    },
                    OpCode::SWAP2 => unsafe {
                        std::ptr::swap(sp.offset(-1), sp.offset(-3));
                        std::ptr::swap(sp.offset(-2), sp.offset(-4));
                    },

                    // ── Local variable / argument access ──
                    OpCode::GET_LOC => {
                        // [^] -> [local_val, ^]
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(0, *VAR_REF![idx]);
                        SP_GO!(1);
                    }
                    OpCode::PUT_LOC => {
                        // [val, ^] -> [^]
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = SP_VAL!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::GET_ARG => {
                        // [^] -> [arg_val, ^]
                        let idx = PC_FETCH!(u16);
                        SP_PUT!(0, *ARG_REF!(idx));
                        SP_GO!(1);
                    }
                    OpCode::PUT_ARG => {
                        let idx = PC_FETCH!(u16);
                        *ARG_REF!(idx) = SP_VAL!(-1);
                        SP_GO!(-1);
                    }
                    OpCode::SET_ARG => {
                        let idx = PC_FETCH!(u16);
                        *ARG_REF![idx] = SP_VAL!(-1);
                    }
                    OpCode::SET_LOC => {
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = SP_VAL!(-1);
                    }
                    OpCode::CLOSE_LOC => {
                        let idx = PC_FETCH!(u16);
                        arena_mut!().close_loc_ref(cur_frame_idx, idx);
                    }

                    // ── Closure variable refs ──
                    OpCode::GET_VAR_REF => {
                        let idx = PC_FETCH!(u16);
                        let val = captured_vars!()[idx as usize].value();
                        SP_PUT!(0, val);
                        SP_GO!(1);
                    }
                    OpCode::PUT_VAR_REF => {
                        let idx = PC_FETCH!(u16);
                        captured_vars!()[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                        SP_GO!(-1);
                    }
                    OpCode::SET_VAR_REF => {
                        // [value, ^] -> [value, ^]
                        // store stack top value to var_ref
                        let idx = PC_FETCH!(u16);
                        captured_vars!()[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                    }
                    OpCode::MAKE_LOC_REF | OpCode::MAKE_ARG_REF | OpCode::MAKE_VAR_REF_REF => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let idx = PC_FETCH!(u16);
                        let kind = match opcode {
                            OpCode::MAKE_ARG_REF => ref_ops::RefKind::Arg,
                            OpCode::MAKE_VAR_REF_REF => ref_ops::RefKind::VarRefRef,
                            _ => ref_ops::RefKind::Loc,
                        };
                        match ref_ops::make_ref(
                            realm_scope,
                            kind,
                            atom,
                            idx,
                            arg_base_ptr,
                            var_base_ptr,
                            arena_mut!(),
                            cur_frame_idx,
                            cur_bc.arg_count,
                            cur_bc.var_count,
                            captured_vars!(),
                        ) {
                            Ok((obj, prop)) => {
                                SP_PUT!(0, obj);
                                SP_PUT!(1, prop);
                                SP_GO!(2);
                            }
                            Err(_) => break ExecAction::Exception,
                        }
                    }

                    // ── Type conversion / unary operations ──
                    OpCode::PLUS | OpCode::NEG | OpCode::INC | OpCode::DEC => {
                        let op1 = SP_VAL!(-1);
                        if opcode == OpCode::PLUS && op1.is_number() {
                            // + before an int or float: do nothing
                            SP_PUT!(-1, op1);
                        } else if opcode == OpCode::PLUS && op1.is_bigint() {
                            // unary `+` on a BigInt throws (ToNumber)
                            let _ = realm_scope
                                .throw_type_error("Cannot convert a BigInt value to a number");
                            break ExecAction::Exception;
                        } else if let Ok(res) =
                            UnaryOp::new(realm_scope, op1, true).and_then(|unary| match opcode {
                                OpCode::PLUS => unary.plus(realm_scope),
                                OpCode::NEG => unary.neg(realm_scope),
                                OpCode::INC => unary.inc(realm_scope),
                                OpCode::DEC => unary.dec(realm_scope),
                                _ => unreachable!(),
                            })
                        {
                            SP_PUT!(-1, res);
                        } else {
                            SP_PUT!(-1, JSValue::UNDEFINED);
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::TO_PROPKEY => {
                        let key = SP_REF!(-1);
                        if !key.is_string_or_symbol() && !key.is_int() {
                            if let Ok(pk) = key.to_propkey(realm_scope) {
                                SP_PUT!(-1, pk);
                            } else {
                                break ExecAction::Exception;
                            }
                        }
                    }
                    OpCode::TO_OBJECT => {
                        if !SP_REF!(-1).is_object() {
                            if let Ok(o) = SP_VAL!(-1).objectify(realm_scope) {
                                SP_PUT!(-1, JSValue::from(o));
                            } else {
                                break ExecAction::Exception;
                            }
                        }
                    }

                    // ── CALL / RETURN ──
                    OpCode::CALL | OpCode::TAIL_CALL => {
                        // [func_obj, arg1, ..., argN, ^] -> [ret_val, ^]
                        let call_argc = PC_FETCH!(u16);
                        let call_argv = unsafe { sp.offset(-(call_argc as isize)) };

                        save_pc!(pc.offset_from(pc_base) as _);

                        let func = unsafe { *call_argv.offset(-1) };

                        // Record the callee so the compiler can tell a
                        // monomorphic site from a polymorphic one.
                        #[cfg(feature = "jit")]
                        if jit_active {
                            crate::jit::note_call(
                                &rt,
                                &cur_bc,
                                (unsafe { pc.offset_from(pc_base) } as u32) - 3,
                                func,
                            );
                        }

                        // ── Inline bytecode dispatch ──
                        if let Ok(mut bfun) = BCWrapper::try_from(func) {
                            let bc = bfun.payload().bytecode();
                            let func_kind = bc.func_kind();
                            let callee_captured: *mut [VarRef] =
                                bfun.payload_mut().captured_vars_mut();

                            // Generators/async need `class_call` (AGenPayload or
                            // AsyncState + INITIAL_YIELD); only Normal functions
                            // use the inline frame.
                            if func_kind == FunctionKind::Normal {
                                // Copy the stack args into the callee frame.
                                let args_slice = unsafe {
                                    std::slice::from_raw_parts(call_argv, call_argc as usize)
                                };

                                if unlikely(opcode == OpCode::TAIL_CALL) {
                                    // Cross-realm tail calls skip TCO and fall
                                    // through to `call_internal`.
                                    if bc.realm() == realm_scope.ctx_ptr {
                                        // No inline chain: the trampoline
                                        // recycles its own start frame.
                                        if !inline_calls || callers.is_empty() {
                                            break ExecAction::TailCall {
                                                callee_bc: bc,
                                                callee_func_obj: bfun.into(),
                                                args: args_slice.to_vec(),
                                                this_obj: JSValue::UNDEFINED,
                                            };
                                        }
                                        // Inline TCO: recycle at the same depth.
                                        inline_tail_switch!(
                                            'arena_loop,
                                            bc,
                                            bfun,
                                            args_slice,
                                            JSValue::UNDEFINED,
                                            callee_captured
                                        );
                                    }
                                } else {
                                    // Cross-realm non-tail calls fall through to
                                    // `call_internal` (nested trampoline).
                                    if inline_calls
                                        && !callers.is_empty()
                                        && bc.realm() != realm_scope.ctx_ptr
                                    {
                                        // fall through
                                    } else {
                                        // Allocate and initialize the callee frame.
                                        let caller_depth = frame!(cur_frame_idx).depth;
                                        let callee_frame_idx = match build_callee_frame(
                                            arena_mut!(),
                                            caller_depth,
                                            bfun.into(),
                                            bc,
                                            args_slice,
                                            JSValue::UNDEFINED,
                                        ) {
                                            Ok(idx) => idx,
                                            Err(e) => {
                                                match e {
                                                    FrameAllocError::Oom => {
                                                        let _ = realm_scope.throw_stack_fail();
                                                    }
                                                    FrameAllocError::Exceeded => {
                                                        let _ = realm_scope.throw_stack_exceeded();
                                                    }
                                                }
                                                // Route through Exception: the catch
                                                // scan must cleanse the stack first.
                                                break ExecAction::Exception;
                                            }
                                        };
                                        frame_mut!(callee_frame_idx).prev = cur_frame_idx;

                                        let return_sp = unsafe {
                                            sp.offset(-(call_argc as isize + 1))
                                                .offset_from(sp_base)
                                        }
                                            as usize;

                                        if inline_calls && bc.realm() == realm_scope.ctx_ptr {
                                            inline_call_switch!(
                                                bc,
                                                callee_frame_idx,
                                                callee_captured,
                                                return_sp,
                                                JSValue::UNDEFINED,
                                                JSValue::UNDEFINED,
                                                None
                                            );
                                            continue 'arena_loop;
                                        }

                                        // Cross-realm: let the trampoline switch
                                        // GC scopes.
                                        let callee_ctx = bc.realm();
                                        break if callee_ctx != realm_scope.ctx_ptr {
                                            ExecAction::CallBytecodeCrossRealm {
                                                callee_bc: bc,
                                                callee_frame_idx,
                                                callee_ctx_ptr: callee_ctx,
                                                return_sp,
                                                this_obj: JSValue::UNDEFINED,
                                                new_target: JSValue::UNDEFINED,
                                                ctor_this: None,
                                            }
                                        } else {
                                            ExecAction::CallBytecode {
                                                callee_bc: bc,
                                                callee_frame_idx,
                                                return_sp,
                                                this_obj: JSValue::UNDEFINED,
                                                new_target: JSValue::UNDEFINED,
                                                ctor_this: None,
                                            }
                                        };
                                    }
                                }
                            }

                            // Generators/async fall through to `call_internal`.
                        }

                        // Native or non-Normal: `call_internal`. Sync `sp` first:
                        // native code can run GC, and the frame trace must see
                        // the live operands.
                        sync_sp!();
                        let ret_val = func.call_internal(
                            realm_scope,
                            JSValue::UNDEFINED,
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if let Ok(r) = ret_val {
                            if unlikely(opcode == OpCode::TAIL_CALL) {
                                break ExecAction::Return(r, false);
                            } else {
                                SP_GO!(-(call_argc as i32 + 1));
                                SP_PUT!(0, r);
                                SP_GO!(1);
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                        jit_retry_native_now!();
                    }
                    OpCode::CALL_CONSTRUCTOR => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        save_pc!(pc.offset_from(pc_base) as _);

                        let func = unsafe { *call_argv.offset(-2) };
                        let ctor_new_target = unsafe { *call_argv.offset(-1) };

                        // ── Inline bytecode dispatch (mirrors CALL) ──
                        // `super()` chains must not recurse on the C stack;
                        // non-constructors fall through to `call_internal`.
                        if let Ok(mut bfun) = BCWrapper::try_from(func)
                            && bfun.flags().is_constructor()
                            && bfun.payload().bytecode().func_kind() == FunctionKind::Normal
                        {
                            let bc = bfun.payload().bytecode();
                            let callee_captured: *mut [VarRef] =
                                bfun.payload_mut().captured_vars_mut();

                            // Cross-realm constructors fall through to
                            // `call_internal`; creating the instance here would
                            // duplicate it.
                            if inline_calls
                                && !callers.is_empty()
                                && bc.realm() != realm_scope.ctx_ptr
                            {
                                // fall through
                            } else {
                                // Non-derived constructor: create the instance
                                // at the call site and use it for a non-object
                                // return. Derived constructors start in TDZ.
                                let (callee_this, ctor_this) =
                                    if !bc.attrs.is_derived_class_constructor {
                                        match realm_scope.ordinary_create_from_constructor_payload(
                                            ctor_new_target,
                                            PayloadPtr::none(JS_CLASS_OBJECT),
                                        ) {
                                            Ok(o) => {
                                                let v = JSValue::from(o);
                                                (v, Some(v))
                                            }
                                            Err(_) => {
                                                let _ = realm_scope.throw_type_error(
                                                    "constructor must return an object",
                                                );
                                                break ExecAction::Exception;
                                            }
                                        }
                                    } else {
                                        (JSValue::UNDEFINED, None)
                                    };

                                let args_slice = unsafe {
                                    std::slice::from_raw_parts(call_argv, call_argc as usize)
                                };
                                let caller_depth = frame!(cur_frame_idx).depth;
                                let callee_frame_idx = match build_callee_frame(
                                    arena_mut!(),
                                    caller_depth,
                                    bfun.into(),
                                    bc,
                                    args_slice,
                                    callee_this,
                                ) {
                                    Ok(idx) => idx,
                                    Err(e) => {
                                        match e {
                                            FrameAllocError::Oom => {
                                                let _ = realm_scope.throw_stack_fail();
                                            }
                                            FrameAllocError::Exceeded => {
                                                let _ = realm_scope.throw_stack_exceeded();
                                            }
                                        }
                                        // Propagate through the Exception handler
                                        // (see CALL).
                                        break ExecAction::Exception;
                                    }
                                };
                                frame_mut!(callee_frame_idx).prev = cur_frame_idx;

                                // The instance is traced through the callee frame's
                                // `this_obj`; drop its LOCAL protection so it does
                                // not accumulate in the caller scope.
                                if let Some(node) = callee_this.as_gc_node() {
                                    let _b = realm_scope.remove_local(node);
                                    debug_assert!(
                                        _b,
                                        "fresh constructor instance was not LOCAL in the caller's scope"
                                    );
                                }

                                let return_sp = unsafe {
                                    sp.offset(-(call_argc as isize + 2)).offset_from(sp_base)
                                } as usize;

                                if inline_calls && bc.realm() == realm_scope.ctx_ptr {
                                    inline_call_switch!(
                                        bc,
                                        callee_frame_idx,
                                        callee_captured,
                                        return_sp,
                                        callee_this,
                                        ctor_new_target,
                                        ctor_this
                                    );
                                    continue 'arena_loop;
                                }

                                let callee_ctx = bc.realm();

                                break if callee_ctx != realm_scope.ctx_ptr {
                                    ExecAction::CallBytecodeCrossRealm {
                                        callee_bc: bc,
                                        callee_frame_idx,
                                        callee_ctx_ptr: callee_ctx,
                                        return_sp,
                                        this_obj: callee_this,
                                        new_target: ctor_new_target,
                                        ctor_this,
                                    }
                                } else {
                                    ExecAction::CallBytecode {
                                        callee_bc: bc,
                                        callee_frame_idx,
                                        return_sp,
                                        this_obj: callee_this,
                                        new_target: ctor_new_target,
                                        ctor_this,
                                    }
                                };
                            }
                        }

                        // Native, non-Normal or non-constructor: `call_internal`.
                        sync_sp!();
                        let ret_val = func.call_internal(
                            realm_scope,
                            JSValue::UNDEFINED,
                            ctor_new_target,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            JS_CALL_FLAG_CONSTRUCTOR,
                        );
                        if let Ok(val) = ret_val {
                            SP_GO!(-(call_argc + 2));
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                        jit_retry_native_now!();
                    }
                    OpCode::CALL_METHOD | OpCode::TAIL_CALL_METHOD => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        save_pc!(pc.offset_from(pc_base) as _);

                        let func = unsafe { *call_argv.offset(-1) };
                        let method_this = unsafe { *call_argv.offset(-2) };

                        // Record the callee (see CALL).
                        #[cfg(feature = "jit")]
                        if jit_active {
                            crate::jit::note_call(
                                &rt,
                                &cur_bc,
                                (unsafe { pc.offset_from(pc_base) } as u32) - 3,
                                func,
                            );
                        }

                        // ── Inline bytecode dispatch (mirrors CALL) ──
                        // Method calls must not recurse on the C stack either
                        // (deep `o.m()` or `super.m()` chains).
                        if let Ok(mut bfun) = BCWrapper::try_from(func) {
                            let bc = bfun.payload().bytecode();
                            let callee_captured: *mut [VarRef] =
                                bfun.payload_mut().captured_vars_mut();

                            if bc.func_kind() == FunctionKind::Normal {
                                let args_slice = unsafe {
                                    std::slice::from_raw_parts(call_argv, call_argc as usize)
                                };

                                if unlikely(opcode == OpCode::TAIL_CALL_METHOD) {
                                    // Cross-realm tail calls skip TCO and fall
                                    // through to `call_internal`.
                                    if bc.realm() == realm_scope.ctx_ptr {
                                        if !inline_calls || callers.is_empty() {
                                            break ExecAction::TailCall {
                                                callee_bc: bc,
                                                callee_func_obj: bfun.into(),
                                                args: args_slice.to_vec(),
                                                this_obj: method_this,
                                            };
                                        }
                                        // Inline TCO: recycle at the same depth.
                                        inline_tail_switch!(
                                            'arena_loop,
                                            bc,
                                            bfun,
                                            args_slice,
                                            method_this,
                                            callee_captured
                                        );
                                    }
                                } else if inline_calls
                                    && !callers.is_empty()
                                    && bc.realm() != realm_scope.ctx_ptr
                                {
                                    // Cross-realm: fall through to call_internal.
                                } else {
                                    let caller_depth = frame!(cur_frame_idx).depth;
                                    let callee_frame_idx = match build_callee_frame(
                                        arena_mut!(),
                                        caller_depth,
                                        bfun.into(),
                                        bc,
                                        args_slice,
                                        method_this,
                                    ) {
                                        Ok(idx) => idx,
                                        Err(e) => {
                                            match e {
                                                FrameAllocError::Oom => {
                                                    let _ = realm_scope.throw_stack_fail();
                                                }
                                                FrameAllocError::Exceeded => {
                                                    let _ = realm_scope.throw_stack_exceeded();
                                                }
                                            }

                                            // Route through Exception: the catch
                                            // scan must cleanse the stack first.
                                            break ExecAction::Exception;
                                        }
                                    };
                                    frame_mut!(callee_frame_idx).prev = cur_frame_idx;

                                    let return_sp = unsafe {
                                        sp.offset(-(call_argc as isize + 2)).offset_from(sp_base)
                                    } as usize;

                                    if inline_calls && bc.realm() == realm_scope.ctx_ptr {
                                        inline_call_switch!(
                                            bc,
                                            callee_frame_idx,
                                            callee_captured,
                                            return_sp,
                                            method_this,
                                            JSValue::UNDEFINED,
                                            None
                                        );
                                        continue 'arena_loop;
                                    }

                                    let callee_ctx = bc.realm();
                                    break if callee_ctx != realm_scope.ctx_ptr {
                                        ExecAction::CallBytecodeCrossRealm {
                                            callee_bc: bc,
                                            callee_frame_idx,
                                            callee_ctx_ptr: callee_ctx,
                                            return_sp,
                                            this_obj: method_this,
                                            new_target: JSValue::UNDEFINED,
                                            ctor_this: None,
                                        }
                                    } else {
                                        ExecAction::CallBytecode {
                                            callee_bc: bc,
                                            callee_frame_idx,
                                            return_sp,
                                            this_obj: method_this,
                                            new_target: JSValue::UNDEFINED,
                                            ctor_this: None,
                                        }
                                    };
                                }
                            }

                            // Generators/async fall through to `call_internal`.
                        }

                        // Native or non-Normal: `call_internal`.
                        sync_sp!();
                        let ret_val = func.call_internal(
                            realm_scope,
                            method_this,
                            JSValue::UNDEFINED,
                            unsafe { std::slice::from_raw_parts(call_argv, call_argc as usize) },
                            0,
                        );
                        if let Ok(r) = ret_val {
                            if unlikely(opcode == OpCode::TAIL_CALL_METHOD) {
                                break ExecAction::Return(r, false);
                            } else {
                                SP_GO!(-(call_argc + 2));
                                SP_PUT!(0, r);
                                SP_GO!(1);
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                        jit_retry_native_now!();
                    }
                    OpCode::RETURN => {
                        SP_GO!(-1);
                        let ret = SP_VAL!(0);
                        break ExecAction::Return(ret, false);
                    }
                    OpCode::RETURN_UNDEF => {
                        break ExecAction::Return(JSValue::UNDEFINED, false);
                    }

                    // ── Unary / comparison / binary logic ──
                    OpCode::POST_INC | OpCode::POST_DEC => {
                        let op1 = SP_VAL!(-1);
                        if op1.is_bigint() {
                            // BigInt: ToNumeric is the value itself; return the
                            // old value and store the new one.
                            let res = UnaryOp::new(realm_scope, op1, true).and_then(|unary| {
                                if opcode == OpCode::POST_INC {
                                    unary.inc(realm_scope)
                                } else {
                                    unary.dec(realm_scope)
                                }
                            });
                            match res {
                                Ok(r) => {
                                    SP_PUT!(-1, op1);
                                    SP_PUT!(0, r);
                                    SP_GO!(1);
                                }
                                Err(_) => break ExecAction::Exception,
                            }
                        } else if let Ok(n1) = op1.to_number(realm_scope) {
                            SP_PUT!(-1, JSValue::from(n1));
                            if let Ok(res) = UnaryOp::new(realm_scope, JSValue::from(n1), true)
                                .and_then(|unary| {
                                    if opcode == OpCode::POST_INC {
                                        unary.inc(realm_scope)
                                    } else {
                                        unary.dec(realm_scope)
                                    }
                                })
                            {
                                SP_PUT!(0, res);
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::INC_LOC | OpCode::DEC_LOC => {
                        let idx = PC_FETCH!(u8) as i32;
                        let op1 = VAR_REF![idx];
                        let old = *op1;
                        // Hot path: Int locals. Crossing i32 still works here
                        // (the result becomes a Float64); only non-number tags
                        // take the generic path.
                        if let Some(a) = old.as_int() {
                            let res = match opcode {
                                OpCode::INC_LOC if a == i32::MAX => JSValue::from(a as u32 + 1),
                                OpCode::DEC_LOC if a == i32::MIN => JSValue::from(a as i64 - 1),
                                OpCode::INC_LOC => JSValue::from(a + 1),
                                OpCode::DEC_LOC => JSValue::from(a - 1),
                                _ => unreachable!(),
                            };
                            // Feedback: local before/after (`pc` is past the
                            // 1-byte operand; the site is the opcode offset).
                            #[cfg(feature = "jit")]
                            if jit_feedback {
                                let site = (unsafe { pc.offset_from(pc_base) } as u32) - 2;
                                crate::jit::note_local_update(&rt, &cur_bc, site, old, res);
                            }
                            *op1 = res;
                        } else if let Ok(res) =
                            UnaryOp::new(realm_scope, old, true).and_then(|unary| match opcode {
                                OpCode::INC_LOC => unary.inc(realm_scope),
                                OpCode::DEC_LOC => unary.dec(realm_scope),
                                _ => unreachable!(),
                            })
                        {
                            // Feedback: local before/after (the result class
                            // exposes overflow).
                            #[cfg(feature = "jit")]
                            if jit_feedback {
                                let site = (unsafe { pc.offset_from(pc_base) } as u32) - 2;
                                crate::jit::note_local_update(&rt, &cur_bc, site, old, res);
                            }
                            *op1 = res;
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::ADD_LOC => {
                        let idx = PC_FETCH!(u8) as i32;
                        let op1 = SP_VAL!(-1);
                        SP_GO!(-1);
                        let pv = VAR_REF![idx];
                        // Hot path: Int locals; other tags take the generic
                        // conversion path.
                        let res = if let Some(v) = fast_numeric_add(*pv, op1) {
                            Ok(v)
                        } else {
                            JSValue::arith_add(realm_scope, *pv, op1)
                        };
                        if let Ok(val) = res {
                            // Feedback: local (lhs), operand (rhs), result.
                            #[cfg(feature = "jit")]
                            if jit_feedback {
                                let site = (unsafe { pc.offset_from(pc_base) } as u32) - 2;
                                crate::jit::note_binary(&rt, &cur_bc, site, *pv, op1, val);
                            }
                            *pv = val;
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::NOT => {
                        if let Ok(res) = UnaryOp::new_int(realm_scope, SP_VAL!(-1))
                            .and_then(|unary| unary.not(realm_scope))
                        {
                            SP_PUT!(-1, res);
                        } else {
                            SP_PUT!(-1, JSValue::UNDEFINED);
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::SHL
                    | OpCode::SHR
                    | OpCode::SAR
                    | OpCode::AND
                    | OpCode::OR
                    | OpCode::XOR => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        // Hot path: Int/Bool operands. Same truncation and
                        // shift masking as the generic helper.
                        let fast = if op1.is_int_or_bool() && op2.is_int_or_bool() {
                            let a = op1.payload_i32();
                            let b = op2.payload_i32();
                            Some(match opcode {
                                OpCode::SHL => JSValue::from(a << (b & 0x1f)),
                                OpCode::SAR => JSValue::from(a >> (b & 0x1f)),
                                OpCode::SHR => JSValue::from((a as u32) >> (b as u32 & 0x1f)),
                                OpCode::AND => JSValue::from(a & b),
                                OpCode::OR => JSValue::from(a | b),
                                OpCode::XOR => JSValue::from(a ^ b),
                                _ => unreachable!(),
                            })
                        } else {
                            None
                        };
                        if let Some(val) = fast {
                            SP_PUT!(-2, val);
                            SP_GO!(-1);
                            #[cfg(feature = "jit")]
                            if jit_feedback {
                                crate::jit::note_binary(
                                    &rt,
                                    &cur_bc,
                                    (unsafe { pc.offset_from(pc_base) } as u32) - 1,
                                    op1,
                                    op2,
                                    val,
                                );
                            }
                        } else if let Ok(new_sp) = op_binary_logic(realm_scope, opcode, sp) {
                            sp = new_sp;
                            // Feedback: operands and result (may be Float64
                            // after the sign bit).
                            #[cfg(feature = "jit")]
                            if jit_feedback {
                                crate::jit::note_binary(
                                    &rt,
                                    &cur_bc,
                                    (unsafe { pc.offset_from(pc_base) } as u32) - 1,
                                    op1,
                                    op2,
                                    SP_VAL!(-1),
                                );
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::LT | OpCode::LTE | OpCode::GT | OpCode::GTE => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        #[cfg(feature = "jit")]
                        if jit_feedback {
                            crate::jit::note_binary(
                                &rt,
                                &cur_bc,
                                (unsafe { pc.offset_from(pc_base) } as u32) - 1,
                                op1,
                                op2,
                                // A comparison always yields a Bool.
                                JSValue::from(false),
                            );
                        }
                        let fast = match (op1.as_int(), op2.as_int()) {
                            (Some(a), Some(b)) => Some(match opcode {
                                OpCode::LT => a < b,
                                OpCode::LTE => a <= b,
                                OpCode::GT => a > b,
                                OpCode::GTE => a >= b,
                                _ => unreachable!(),
                            }),
                            _ => None,
                        };
                        if let Some(b) = fast {
                            SP_PUT!(-2, JSValue::from(b));
                            SP_GO!(-1);
                        } else if let Ok(rel) = op1.arith_cmp(realm_scope, op2) {
                            let b = matches!(
                                (rel, opcode),
                                (Some(std::cmp::Ordering::Less), OpCode::LT | OpCode::LTE)
                                    | (Some(std::cmp::Ordering::Greater), OpCode::GT | OpCode::GTE)
                                    | (Some(std::cmp::Ordering::Equal), OpCode::LTE | OpCode::GTE)
                            );
                            SP_PUT!(-2, JSValue::from(b));
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::EQ | OpCode::NEQ => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        if let Ok(b) = op1.arith_eq(realm_scope, op2) {
                            SP_PUT!(-2, JSValue::from(b ^ (opcode == OpCode::NEQ)));
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::STRICT_EQ | OpCode::STRICT_NEQ => {
                        let mut b = SP_VAL!(-2).strict_eq(SP_VAL!(-1));
                        if opcode == OpCode::STRICT_NEQ {
                            b = !b;
                        }
                        SP_PUT!(-2, JSValue::from(b));
                        SP_GO!(-1);
                    }
                    OpCode::IN => {
                        if let Ok(b) = js_operator_in(realm_scope, SP_REF!(-1), SP_REF!(-2)) {
                            SP_PUT!(-2, JSValue::from(b));
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PRIVATE_IN => {
                        if unlikely(
                            crate::bytecode::operators::js_operator_private_in(realm_scope, sp)
                                .is_err(),
                        ) {
                            break ExecAction::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::INSTANCEOF => {
                        if unlikely(js_operator_instanceof(realm_scope, sp).is_err()) {
                            break ExecAction::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }
                    OpCode::TYPEOF => {
                        let op1 = SP_VAL!(-1);
                        let atom = op1.type_of();
                        SP_PUT!(
                            -1,
                            atom.force_string(realm_scope)
                                .map_or(JSValue::EXCEPTION, JSValue::make_string)
                        );
                    }
                    OpCode::DELETE => {
                        if unlikely(js_operator_delete(realm_scope, sp) != 0) {
                            break ExecAction::Exception;
                        } else {
                            SP_GO!(-1);
                        }
                    }

                    // ── Global variable access (var_ref indirect via captured_vars) ──
                    OpCode::GET_VAR_UNDEF | OpCode::GET_VAR => {
                        let idx = PC_FETCH!(u16) as usize;
                        // Resolve the global by name on each access; no VarRef cell.
                        let var_name = cur_bc.global_var_refs()[idx].var_name;
                        if let Ok(v) =
                            realm_scope.get_global_var(var_name, opcode == OpCode::GET_VAR)
                        {
                            SP_PUT!(0, v);
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PUT_VAR | OpCode::PUT_VAR_INIT => {
                        let idx = PC_FETCH!(u16) as usize;
                        // Handles TDZ (ReferenceError) and read-only (TypeError).
                        let var_name = cur_bc.global_var_refs()[idx].var_name;
                        let res = realm_scope.set_global_var(
                            var_name,
                            SP_VAL!(-1),
                            if opcode == OpCode::PUT_VAR_INIT {
                                GlobalVarSetMode::Init
                            } else {
                                GlobalVarSetMode::Normal
                            },
                        );
                        SP_GO!(-1);

                        if res.is_err() {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::DELETE_VAR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        match realm_scope.global_obj().delete_property(realm_scope, atom) {
                            Ok(ret) => {
                                SP_PUT!(0, JSValue::from(ret));
                                SP_GO!(1);
                            }
                            Err(_) => {
                                break ExecAction::Exception;
                            }
                        }
                    }

                    // ── Local variable check / init ──
                    OpCode::SET_LOC_UNINITIALIZED => {
                        let idx = PC_FETCH!(u16);
                        *VAR_REF![idx] = JSValue::UNINITIALIZED;
                    }
                    OpCode::GET_LOC_CHECK | OpCode::GET_LOC_CHECKTHIS => {
                        let idx = PC_FETCH!(u16);
                        let value = VAR_REF![idx];
                        if unlikely(value.is_uninitialized()) {
                            // The error belongs to the caller's realm (as with
                            // CHECK_CTOR_RETURN).
                            let prev = frame!(cur_frame_idx).prev;
                            if !prev.is_null() {
                                let prev_func = frame!(prev).func_obj;
                                if let Ok(bfun) = BCWrapper::try_from(JSValue::from(prev_func)) {
                                    let mut caller_ctx_ptr = bfun.payload().bytecode().realm();
                                    let mut caller_scope = caller_ctx_ptr.new_scope();
                                    let _ = caller_scope
                                        .throw_reference_error_uninitialized2(&cur_bc, idx, false);
                                    // caller_scope dropped → GC scope popped
                                    break ExecAction::Exception;
                                }
                            }

                            let _ = realm_scope
                                .throw_reference_error_uninitialized2(&cur_bc, idx, false);
                            break ExecAction::Exception;
                        } else {
                            SP_PUT!(0, *value);
                            SP_GO!(1);
                        }
                    }
                    OpCode::PUT_LOC_CHECK => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(pval.is_uninitialized()) {
                            let _ = realm_scope
                                .throw_reference_error_uninitialized2(&cur_bc, idx, false);
                            break ExecAction::Exception;
                        } else {
                            *pval = SP_VAL!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PUT_LOC_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(!pval.is_uninitialized()) {
                            let _ = realm_scope
                                .throw_reference_error("'this' can be initialized only once");
                            break ExecAction::Exception;
                        } else {
                            *pval = SP_VAL!(-1);
                            SP_GO!(-1);
                        }
                    }
                    OpCode::SET_LOC_CHECK => {
                        // Keeps the value on the stack (unlike PUT_LOC_CHECK) so
                        // the assignment expression has a result.
                        let idx = PC_FETCH!(u16);
                        let pval = VAR_REF![idx];
                        if unlikely(pval.is_uninitialized()) {
                            let _ = realm_scope
                                .throw_reference_error_uninitialized2(&cur_bc, idx, false);
                            break ExecAction::Exception;
                        }
                        *pval = SP_VAL!(-1);
                        // Value stays on the stack for the assignment result.
                    }
                    OpCode::GET_VAR_REF_CHECK | OpCode::PUT_VAR_REF_CHECK => {
                        let idx = PC_FETCH!(u16);
                        let val = captured_vars!()[idx as usize].value();
                        if unlikely(val.is_uninitialized()) {
                            let _ = realm_scope
                                .throw_reference_error_uninitialized2(&cur_bc, idx, true);
                            break ExecAction::Exception;
                        } else if opcode == OpCode::GET_VAR_REF_CHECK {
                            SP_PUT!(0, val);
                            SP_GO!(1);
                        } else {
                            captured_vars!()[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                            SP_GO!(-1);
                        }
                    }
                    OpCode::PUT_VAR_REF_CHECK_INIT => {
                        let idx = PC_FETCH!(u16);
                        if unlikely(!captured_vars!()[idx as usize].value().is_uninitialized()) {
                            let _ = realm_scope
                                .throw_reference_error_uninitialized2(&cur_bc, idx, true);
                            break ExecAction::Exception;
                        } else {
                            captured_vars!()[idx as usize].set_value(&mut rt, SP_VAL!(-1));
                            SP_GO!(-1);
                        }
                    }

                    // ── Constructor / brand / throw ──
                    OpCode::CHECK_CTOR_RETURN => {
                        match class_ops::check_ctor_return(
                            realm_scope,
                            SP_VAL!(-1),
                            arena_ptr,
                            cur_frame_idx,
                        ) {
                            Ok(flag) => {
                                SP_PUT!(0, JSValue::from(flag));
                                SP_GO!(1);
                            }
                            Err(_) => break ExecAction::Exception,
                        }
                    }
                    OpCode::CHECK_CTOR => {
                        if class_ops::check_ctor(realm_scope, new_target).is_err() {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::INIT_CTOR => {
                        match class_ops::init_ctor(realm_scope, func_obj, new_target, args!()) {
                            Ok(val) => {
                                SP_PUT!(0, val);
                                SP_GO!(1);
                                jit_retry_native_now!();
                            }
                            Err(_) => break ExecAction::Exception,
                        }
                    }
                    OpCode::CHECK_BRAND => {
                        if class_ops::check_brand(realm_scope, SP_VAL!(-2), SP_VAL!(-1)).is_err() {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::ADD_BRAND => {
                        let r = class_ops::add_brand(realm_scope, SP_VAL!(-2), SP_VAL!(-1));
                        SP_GO!(-2);
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::THROW => {
                        SP_GO!(-1);
                        let _ = realm_scope.throw_value(SP_VAL!(0));
                        break ExecAction::Exception;
                    }
                    OpCode::THROW_ERROR => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let type_ = PC_FETCH!(u8);
                        engine_ops::throw_error(realm_scope, atom, type_);
                        break ExecAction::Exception;
                    }

                    // ── Array / Apply ──
                    OpCode::ARRAY_FROM => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        if let Ok(mut list) = alloc_fast_array_slots(realm_scope, call_argc as _) {
                            let call_argv = unsafe { sp.offset(-call_argc as isize) };
                            for i in 0..call_argc as usize {
                                let val = unsafe { *call_argv.add(i) };
                                list.push(val);
                            }
                            if let Ok(arr) = crate::array::Array::from_values(realm_scope, list) {
                                SP_GO!(-call_argc);
                                SP_PUT!(0, JSValue::from(arr));
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::APPLY => {
                        let magic = PC_FETCH!(u16) as i32;
                        let op_3 = SP_VAL!(-3);
                        let op_2 = SP_VAL!(-2);
                        let op_1 = SP_VAL!(-1);
                        SP_GO!(-3);
                        if let Ok(res) = js_function_apply(
                            realm_scope,
                            CallContext::new(op_3, &[op_2, op_1], magic, None),
                        ) {
                            SP_PUT!(0, res);
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    // ── Eval ──
                    OpCode::EVAL | OpCode::TAIL_EVAL => {
                        let call_argc = PC_FETCH!(u16) as i32;
                        let scope_idx = PC_FETCH!(u16) as i32 - 1;
                        let call_argv = unsafe { sp.offset(-call_argc as isize) };
                        save_pc!(pc.offset_from(pc_base) as _);
                        let func_val = unsafe { *call_argv.offset(-1) };
                        if func_val.is_same(realm_scope.eval_func) {
                            // ── Direct eval (intrinsic %eval%) ──
                            let ret_val = if call_argc >= 1 {
                                let obj = unsafe { &*call_argv };
                                obj.eval_object(
                                    realm_scope,
                                    cur_frame_idx,
                                    JSValue::UNDEFINED,
                                    EvalType::Direct,
                                    EvalFlags::empty(),
                                    scope_idx as _,
                                )
                            } else {
                                Ok(JSValue::UNDEFINED)
                            };
                            SP_GO!(-(call_argc + 1));
                            if let Ok(v) = ret_val {
                                SP_PUT!(0, v);
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            // ── eval was reassigned: run it as a normal call ──
                            // Use inline dispatch to avoid C-stack recursion. In
                            // `return eval(...)` position it is a tail call.
                            if let Ok(mut bfun) = BCWrapper::try_from(func_val) {
                                let bc = bfun.payload().bytecode();
                                let func_kind = bc.func_kind();
                                if func_kind == FunctionKind::Normal {
                                    let callee_captured: *mut [VarRef] =
                                        bfun.payload_mut().captured_vars_mut();
                                    let args_slice = unsafe {
                                        std::slice::from_raw_parts(call_argv, call_argc as usize)
                                    };

                                    // TAIL_EVAL: replace the current frame;
                                    // cross-realm calls skip TCO.
                                    if unlikely(opcode == OpCode::TAIL_EVAL)
                                        && bc.realm() == realm_scope.ctx_ptr
                                    {
                                        // Inside an inline chain the frame must
                                        // be recycled here; the trampoline only
                                        // owns its own frame stack.
                                        if !inline_calls || callers.is_empty() {
                                            break ExecAction::TailCall {
                                                callee_bc: bc,
                                                callee_func_obj: bfun.into(),
                                                args: args_slice.to_vec(),
                                                this_obj: JSValue::UNDEFINED,
                                            };
                                        }
                                        inline_tail_switch!(
                                            'arena_loop,
                                            bc,
                                            bfun,
                                            args_slice,
                                            JSValue::UNDEFINED,
                                            callee_captured
                                        );
                                    }

                                    // Non-tail: push a frame. `OP_eval` cannot tell
                                    // tail position, and recycling the script frame
                                    // would be fatal. Depth/byte limits still apply.
                                    //
                                    // A cross-realm call cannot join an inline
                                    // chain: the trampoline owns its own frame
                                    // stack, so such a call falls through to
                                    // `call_internal` (mirrors CALL).
                                    if inline_calls
                                        && !callers.is_empty()
                                        && bc.realm() != realm_scope.ctx_ptr
                                    {
                                        // fall through to `call_internal`
                                    } else {
                                        let caller_depth = frame!(cur_frame_idx).depth;
                                        let callee_frame_idx = match build_callee_frame(
                                            arena_mut!(),
                                            caller_depth,
                                            bfun.into(),
                                            bc,
                                            args_slice,
                                            JSValue::UNDEFINED,
                                        ) {
                                            Ok(idx) => idx,
                                            Err(e) => {
                                                match e {
                                                    FrameAllocError::Oom => {
                                                        let _ = realm_scope.throw_stack_fail();
                                                    }
                                                    FrameAllocError::Exceeded => {
                                                        let _ = realm_scope.throw_stack_exceeded();
                                                    }
                                                }

                                                // Route through Exception: the catch
                                                // scan must cleanse the stack first.
                                                break ExecAction::Exception;
                                            }
                                        };
                                        frame_mut!(callee_frame_idx).prev = cur_frame_idx;

                                        let return_sp = unsafe {
                                            sp.offset(-(call_argc as isize + 1))
                                                .offset_from(sp_base)
                                        }
                                            as usize;

                                        if inline_calls && bc.realm() == realm_scope.ctx_ptr {
                                            inline_call_switch!(
                                                bc,
                                                callee_frame_idx,
                                                callee_captured,
                                                return_sp,
                                                JSValue::UNDEFINED,
                                                JSValue::UNDEFINED,
                                                None
                                            );
                                            continue 'arena_loop;
                                        }

                                        break ExecAction::CallBytecode {
                                            callee_bc: bc,
                                            callee_frame_idx,
                                            return_sp,
                                            this_obj: JSValue::UNDEFINED,
                                            new_target: JSValue::UNDEFINED,
                                            ctor_this: None,
                                        };
                                    }
                                }
                            }

                            // Generator/async or native: `call_internal`.
                            sync_sp!();
                            let ret_val = func_val.call_internal(
                                realm_scope,
                                JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                                unsafe {
                                    std::slice::from_raw_parts(call_argv, call_argc as usize)
                                },
                                0,
                            );
                            SP_GO!(-(call_argc + 1));
                            if let Ok(v) = ret_val {
                                SP_PUT!(0, v);
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                            jit_retry_native_now!();
                        }
                    }
                    OpCode::APPLY_EVAL => {
                        let var_scope_idx = PC_FETCH!(u16) as i32 - 1;
                        let func_val = SP_REF!(-2);
                        if let Ok(arg_list) = build_arg_list(realm_scope, SP_VAL!(-1)) {
                            let ret_val = {
                                if func_val.is_same(realm_scope.eval_func) {
                                    if !arg_list.is_empty() {
                                        arg_list[0].eval_object(
                                            realm_scope,
                                            cur_frame_idx,
                                            JSValue::UNDEFINED,
                                            EvalType::Direct,
                                            EvalFlags::empty(),
                                            var_scope_idx,
                                        )
                                    } else {
                                        Ok(JSValue::UNDEFINED)
                                    }
                                } else {
                                    func_val.call(realm_scope, JSValue::UNDEFINED, &arg_list)
                                }
                            };
                            if let Ok(v) = ret_val {
                                SP_GO!(-2);
                                SP_PUT!(0, v);
                                SP_GO!(1);
                            } else {
                                break ExecAction::Exception;
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    // ── Property access ──
                    OpCode::LNOT => {
                        let val: JSValue = SP_VAL!(-1);
                        let b = if val.is_int_or_bool() {
                            val.payload_i32() != 0
                        } else if let Ok(b) = val.to_bool() {
                            b
                        } else {
                            break ExecAction::Exception;
                        };
                        SP_PUT!(-1, JSValue::from(!b));
                    }

                    OpCode::GET_FIELD | OpCode::GET_FIELD2 => {
                        // get_field: [target, ^] -> [value, ^]
                        // get_field2: [target, ^] -> [target, value, ^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        #[cfg(feature = "inline_cache")]
                        let ic_idx = PC_FETCH!(u16) as usize;

                        let target = SP_VAL!(-1);

                        #[cfg(feature = "inline_cache")]
                        {
                            // IC fast path
                            let ic_cache = unsafe { &*cur_bc.inline_cache.get() };

                            if ic_idx < ic_cache.len()
                                && let Some(o) = target.as_object()
                                && let Some(shape) = o.shape()
                            {
                                let shape_ptr = unsafe { shape.gc_ref().as_ptr().as_ptr() };

                                // The site holds up to IC_WAYS shapes.
                                if let Some((slot, _)) =
                                    ic_cache[ic_idx].find(shape_ptr, shape.uid, atom.id())
                                {
                                    // One slot read covers the tombstone check and
                                    // the value; accessor/varref slots use
                                    // `value()`, `Deleted` falls back.
                                    let val = match o.prop_slot(slot as usize) {
                                        JSProperty::Normal(v) => Some(*v),
                                        JSProperty::Deleted => None,
                                        p => match p.value(realm_scope, o.into()) {
                                            Ok(v) => Some(v),
                                            Err(_) => break ExecAction::Exception,
                                        },
                                    };
                                    if let Some(val) = val {
                                        if opcode == OpCode::GET_FIELD {
                                            SP_PUT!(-1, val);
                                        } else {
                                            SP_PUT!(0, val);
                                            SP_GO!(1);
                                        }
                                        continue;
                                    }
                                }
                            }
                        }

                        // Slow path: lookup property
                        if let Ok(val) = target.get_property(realm_scope, atom) {
                            #[cfg(feature = "inline_cache")]
                            if let Some(o) = target.as_object()
                                && let Some(shape) = o.shape()
                                && let Some((idx, flags)) = o.find_shape_property(atom)
                            {
                                // Update the inline cache.
                                let ic_cache = unsafe { &mut *cur_bc.inline_cache.get() }; // SAFETY: single-threaded.
                                if ic_idx < ic_cache.len() {
                                    ic_cache[ic_idx].insert(
                                        unsafe { shape.gc_ref().as_ptr().as_ptr() },
                                        shape.uid,
                                        atom.id(),
                                        idx as u32,
                                        flags as u8,
                                    );
                                }
                            }

                            if opcode == OpCode::GET_FIELD {
                                SP_PUT!(-1, val);
                            } else {
                                SP_PUT!(0, val);
                                SP_GO!(1)
                            }
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    OpCode::PUT_FIELD => {
                        // [target, value, ^] -> [^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        #[cfg(feature = "inline_cache")]
                        let ic_idx = PC_FETCH!(u16) as usize;

                        let target = SP_VAL!(-2);
                        let value = SP_VAL!(-1);

                        #[cfg(feature = "inline_cache")]
                        {
                            // IC fast path: direct slot write when shape and
                            // flags match.
                            let ic_cache = unsafe { &*cur_bc.inline_cache.get() };
                            if ic_idx < ic_cache.len()
                                && let Some(o) = target.as_object()
                                && let Some(shape) = o.shape()
                            {
                                let shape_ptr = unsafe { shape.gc_ref().as_ptr().as_ptr() };

                                if let Some((slot, cached_flags)) =
                                    ic_cache[ic_idx].find(shape_ptr, shape.uid, atom.id())
                                {
                                    let slot = slot as usize;

                                    // `delete` marks the slot Deleted without
                                    // changing the shape.
                                    if !matches!(o.prop_slot(slot), JSProperty::Deleted) {
                                        let prs_flags = cached_flags as u32;
                                        if prs_flags & crate::object::JS_PROP_TMASK
                                            == crate::object::JS_PROP_NORMAL
                                            && prs_flags & crate::object::JS_PROP_WRITABLE != 0
                                            && prs_flags & crate::object::JS_PROP_LENGTH == 0
                                        {
                                            let mut o =
                                                unsafe { SP_MUT!(-2).as_object_unchecked() };
                                            // Non-GC values need no write barrier,
                                            // so skip its owner/heap loads.
                                            if value.is_gc() {
                                                o.set_prop_slot(slot, JSProperty::Normal(value));
                                            } else {
                                                o.set_prop_slot_non_gc(
                                                    slot,
                                                    JSProperty::Normal(value),
                                                );
                                            }

                                            SP_GO!(-2);
                                            continue;
                                        }
                                    }
                                }
                            }
                        }

                        // Slow path: property lookup and set.
                        SP_GO!(-2);

                        let r = target.set_property_internal(
                            realm_scope,
                            atom,
                            value,
                            None,
                            ThrowPolicy::StrictOnly,
                            false,
                        );
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }

                        #[cfg(feature = "inline_cache")]
                        if let Some(o) = target.as_object()
                            && let Some(shape) = o.shape()
                            && let Some((idx, flags)) = o.find_shape_property(atom)
                        {
                            // Update the IC after a successful slow path.
                            let ic_cache = unsafe { &mut *cur_bc.inline_cache.get() };
                            if ic_idx < ic_cache.len() {
                                ic_cache[ic_idx].insert(
                                    unsafe { shape.gc_ref().as_ptr().as_ptr() },
                                    shape.uid,
                                    atom.id(),
                                    idx as u32,
                                    flags as u8,
                                );
                            }
                        }
                    }

                    // ── Binary arithmetic ──
                    OpCode::ADD
                    | OpCode::SUB
                    | OpCode::MUL
                    | OpCode::DIV
                    | OpCode::MOD
                    | OpCode::POW => {
                        let op1 = SP_VAL!(-2);
                        let op2 = SP_VAL!(-1);
                        let res = match opcode {
                            OpCode::ADD => {
                                if let Some(v) = fast_numeric_add(op1, op2) {
                                    Ok(v)
                                } else {
                                    JSValue::arith_add(realm_scope, op1, op2)
                                }
                            }
                            OpCode::SUB => {
                                if let Some(v) = fast_numeric_sub(op1, op2) {
                                    Ok(v)
                                } else {
                                    JSValue::arith_sub(realm_scope, op1, op2)
                                }
                            }
                            OpCode::MUL => {
                                if let Some(v) = fast_numeric_mul(op1, op2) {
                                    Ok(v)
                                } else {
                                    JSValue::arith_mul(realm_scope, op1, op2)
                                }
                            }
                            OpCode::DIV => JSValue::arith_div(realm_scope, op1, op2),
                            OpCode::MOD => JSValue::arith_mod(realm_scope, op1, op2),
                            OpCode::POW => JSValue::arith_pow(realm_scope, op1, op2),
                            _ => unreachable!(),
                        };
                        // Feedback: operands and result (the result class
                        // exposes int overflow).
                        #[cfg(feature = "jit")]
                        if jit_feedback && let Ok(val) = res {
                            crate::jit::note_binary(
                                &rt,
                                &cur_bc,
                                (unsafe { pc.offset_from(pc_base) } as u32) - 1,
                                op1,
                                op2,
                                val,
                            );
                        }

                        if let Ok(val) = res {
                            SP_PUT!(-2, val);
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    // ── Control flow ──
                    OpCode::GOTO => {
                        let n = unsafe { read_from_ptr::<i32>(pc) }; // could be minus
                        PC_GO!(n);

                        // Only back edges poll: a forward jump is bounded within
                        // the frame, so polling there would charge every
                        // `if`/`break` for interrupt latency.
                        if n < 0 {
                            #[cfg(feature = "jit")]
                            jit_note_backedge!(unsafe { pc.offset_from(pc_base) as usize });

                            if unlikely(realm_scope.poll_interrupts()) {
                                break ExecAction::Exception;
                            }
                        }
                    }
                    OpCode::IF_TRUE | OpCode::IF_FALSE => {
                        let b = opcode == OpCode::IF_TRUE;
                        let val = SP_VAL!(-1);
                        SP_GO!(-1);

                        // Feedback: the condition's type drives branch selection.
                        #[cfg(feature = "jit")]
                        if jit_feedback {
                            crate::jit::note_value(
                                &rt,
                                &cur_bc,
                                (unsafe { pc.offset_from(pc_base) } as u32) - 1,
                                val,
                            );
                        }

                        // Skip the jump offset.
                        PC_GO!(4);

                        let test = if val.is_int_or_bool() {
                            val.payload_i32() != 0
                        } else if val.is_undefined_or_null() {
                            false
                        } else if let Ok(x) = val.to_bool() {
                            x
                        } else {
                            break ExecAction::Exception;
                        };

                        if b == test {
                            let off = unsafe { read_from_ptr::<i32>(pc.offset(-4)) };

                            pc = unsafe { pc.offset((off - 4) as isize) };

                            // A taken backward branch is a loop back edge: `pc`
                            // is the target (loop header), not the fall-through.
                            if off < 0 {
                                #[cfg(feature = "jit")]
                                jit_note_backedge!(unsafe { pc.offset_from(pc_base) as usize });

                                if unlikely(realm_scope.poll_interrupts()) {
                                    break ExecAction::Exception;
                                }
                            }
                        }
                    }

                    // ── Array element access ──
                    OpCode::GET_ARRAY_EL => {
                        // [array, prop, ^] -> [value, ^]
                        #[cfg(feature = "inline_cache")]
                        let ic_idx = PC_FETCH!(u16) as usize;
                        let val = {
                            #[cfg(feature = "inline_cache")]
                            let r = if let Some(site) = Bytecode::inline_cache_site(&cur_bc, ic_idx)
                            {
                                SP_REF!(-2).get_property_value_ic(realm_scope, SP_VAL!(-1), site)
                            } else {
                                SP_REF!(-2).get_property_value(realm_scope, SP_VAL!(-1))
                            };
                            #[cfg(not(feature = "inline_cache"))]
                            let r = SP_REF!(-2).get_property_value(realm_scope, SP_VAL!(-1));
                            r.unwrap_or(JSValue::EXCEPTION)
                        };
                        let is_err = val.is_exception();
                        SP_PUT!(-2, val);
                        SP_GO!(-1);

                        if unlikely(is_err) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL2 => {
                        // [array, prop, ^] -> [array, value, ^]
                        #[cfg(feature = "inline_cache")]
                        let ic_idx = PC_FETCH!(u16) as usize;
                        let val = {
                            #[cfg(feature = "inline_cache")]
                            let r = if let Some(site) = Bytecode::inline_cache_site(&cur_bc, ic_idx)
                            {
                                SP_REF!(-2).get_property_value_ic(realm_scope, SP_VAL!(-1), site)
                            } else {
                                SP_REF!(-2).get_property_value(realm_scope, SP_VAL!(-1))
                            };
                            #[cfg(not(feature = "inline_cache"))]
                            let r = SP_REF!(-2).get_property_value(realm_scope, SP_VAL!(-1));
                            r.unwrap_or(JSValue::EXCEPTION)
                        };
                        let is_err = val.is_exception();
                        SP_PUT!(-1, val);

                        if unlikely(is_err) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::GET_ARRAY_EL3 => {
                        // [array, prop, ^] -> [array, prop1, value, ^]
                        #[cfg(feature = "inline_cache")]
                        let ic_idx = PC_FETCH!(u16) as usize;
                        let arr = SP_REF!(-2);

                        // A null/undefined target throws before `ToPropertyKey`
                        // can run user code (the key is already evaluated, but
                        // its conversion must not happen).
                        if arr.is_undefined_or_null() {
                            let _ = realm_scope.throw_type_error("value has no property ");
                            break ExecAction::Exception;
                        }

                        // Normalize the key once so read and write agree;
                        // otherwise `a[k]++` runs `toString`/`valueOf` twice.
                        let prop = SP_VAL!(-1);
                        if !prop.is_int() && !prop.is_string_or_symbol() {
                            let Ok(prop1) = prop.to_propkey(realm_scope) else {
                                break ExecAction::Exception;
                            };
                            SP_PUT!(-1, prop1);
                        }

                        let val = {
                            #[cfg(feature = "inline_cache")]
                            let r = if let Some(site) = Bytecode::inline_cache_site(&cur_bc, ic_idx)
                            {
                                arr.get_property_value_ic(realm_scope, SP_VAL!(-1), site)
                            } else {
                                arr.get_property_value(realm_scope, SP_VAL!(-1))
                            };
                            #[cfg(not(feature = "inline_cache"))]
                            let r = arr.get_property_value(realm_scope, SP_VAL!(-1));
                            r.unwrap_or(JSValue::EXCEPTION)
                        };
                        let is_err = val.is_exception();
                        SP_PUT!(0, val);
                        SP_GO!(1);

                        if unlikely(is_err) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PUT_ARRAY_EL => {
                        // [arr, prop, value, ^] -> [^]
                        #[cfg(feature = "inline_cache")]
                        let ic_idx = PC_FETCH!(u16) as usize;
                        let mut arr = SP_VAL!(-3);
                        #[cfg(feature = "inline_cache")]
                        let ret = if let Some(site) = Bytecode::inline_cache_site(&cur_bc, ic_idx) {
                            arr.set_property_value_ic(
                                realm_scope,
                                SP_VAL!(-2),
                                SP_VAL!(-1),
                                ThrowPolicy::StrictOnly,
                                false,
                                site,
                            )
                        } else {
                            arr.set_property_value(
                                realm_scope,
                                SP_VAL!(-2),
                                SP_VAL!(-1),
                                ThrowPolicy::StrictOnly,
                                false,
                            )
                        };
                        #[cfg(not(feature = "inline_cache"))]
                        let ret = arr.set_property_value(
                            realm_scope,
                            SP_VAL!(-2),
                            SP_VAL!(-1),
                            ThrowPolicy::StrictOnly,
                            false,
                        );
                        SP_GO!(-3);
                        if unlikely(ret.is_err()) {
                            break ExecAction::Exception;
                        }
                    }

                    // ── Misc ──
                    OpCode::GET_SUPER => {
                        if let Ok(proto) = class_ops::get_super(realm_scope, SP_VAL!(-1)) {
                            SP_PUT!(-1, proto);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::IMPORT => {
                        // `specifier options` -> Promise
                        let options = SP_VAL!(-1);
                        let specifier = SP_VAL!(-2);
                        if let Ok(res) = js_dynamic_import(realm_scope, specifier, options) {
                            SP_PUT!(-2, res);
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::REGEXP => {
                        // `[id, ^] -> [regexp, ^]`; move the stack only on success.
                        if let Some(re_id) = SP_VAL!(-1).as_int() {
                            let regexp = engine_ops::make_regexp(realm_scope, re_id)
                                .unwrap_or(JSValue::EXCEPTION);
                            SP_PUT!(-2, regexp);
                            SP_GO!(-1);
                            if unlikely(SP_REF!(-2).is_exception()) {
                                break ExecAction::Exception;
                            }
                        }
                    }
                    OpCode::PUSH_BIGINT_I32 => {
                        // [^] -> [bigint_value, ^]
                        // Reconstruct i32-range BigInts from the immediate (no
                        // const-pool lookup or parser allocation).
                        let a = PC_FETCH!(i32);
                        let big = engine_ops::bigint_from_i32(realm_scope, a)
                            .unwrap_or(JSValue::EXCEPTION);
                        SP_PUT!(0, big);
                        SP_GO!(1);
                        if unlikely(SP_REF!(-1).is_exception()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::NOP => {}

                    // ── Try-catch ──
                    OpCode::CATCH => {
                        let diff = unsafe { read_from_ptr::<i32>(pc) };
                        SP_PUT!(
                            0,
                            JSValue::make_catch_offset(pc.offset_from(pc_base) as i32 + diff)
                        );
                        SP_GO!(1);
                        PC_GO!(4);
                    }
                    OpCode::NIP_CATCH => {
                        // [catch_offset, ..., ret_val, ^] -> [ret_eval, ^]
                        SP_GO!(-1);
                        let ret_val = SP_VAL!(0);
                        while sp > sp_base && !SP_REF!(-1).is_catch_offset() {
                            SP_GO!(-1);
                        }
                        if unlikely(sp == sp_base) {
                            let _ = realm_scope.throw_internal_error("nip_catch");
                            break ExecAction::Exception;
                        } else {
                            SP_PUT!(-1, ret_val);
                        }
                    }

                    // ── For-in / For-of / Iterator ──
                    OpCode::FOR_OF_START | OpCode::FOR_AWAIT_OF_START => {
                        // [obj, ^] -> [iter_obj, next_method, catchoffset(0), ^]
                        let is_async = opcode == OpCode::FOR_AWAIT_OF_START;

                        if let Ok((iter_obj, next_method)) = SP_REF!(-1)
                            .get_iterator_must(realm_scope, is_async)
                            .and_then(|o| {
                                o.get_property(realm_scope, Atom::CONST_next)
                                    .map(|m| (o, m))
                            })
                        {
                            SP_PUT!(-1, JSValue::from(iter_obj));
                            SP_PUT!(0, next_method);
                            SP_GO!(1);
                            SP_PUT!(0, JSValue::make_catch_offset(0));
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::FOR_OF_NEXT => {
                        // [iter_obj, next, ^] -> [value, done, ^]
                        let offset = -3 - PC_FETCH!(u8) as i32;
                        let r = unsafe { js_for_of_next(realm_scope, sp, offset) };
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                        SP_GO!(2);
                    }
                    OpCode::FOR_AWAIT_OF_NEXT => {
                        // [iter_obj, next, catch_offset] -> the same + promise
                        // Disable the catch offset before `next()` so exceptions
                        // from it do not close the iterator.
                        let iter = SP_VAL!(-3);
                        let next = SP_VAL!(-2);
                        let saved_catch = SP_VAL!(-1);
                        SP_PUT!(-1, JSValue::UNDEFINED);

                        // `next` may be native and trigger GC; sync the operands
                        // for the frame trace.
                        sync_sp!();
                        let result =
                            next.call_internal(realm_scope, iter, JSValue::UNDEFINED, &[], 0);
                        if let Ok(obj) = result {
                            // Restore the catch offset for NIP_CATCH (`return`
                            // inside the loop body).
                            SP_PUT!(-1, saved_catch);
                            SP_PUT!(0, obj);
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                        jit_retry_native_now!();
                    }
                    OpCode::ITERATOR_GET_VALUE_DONE => {
                        if unsafe { js_iterator_get_value_done(realm_scope, sp) } != 0 {
                            break ExecAction::Exception;
                        } else {
                            SP_GO!(1);
                        }
                    }
                    OpCode::ITERATOR_CHECK_OBJECT => {
                        if !SP_REF!(-1).is_object() {
                            let _ = realm_scope.throw_type_error("iterator must return an object");
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::ITERATOR_CLOSE => {
                        // [iter_obj, next, catch_offset, ^] -> [^]
                        SP_GO!(-2);

                        // SP[-1] is iter_obj
                        if let Some(o) = SP_VAL!(-1).as_object()
                            && ObjectIter::close_iter_internal(realm_scope, o).is_err()
                        {
                            break ExecAction::Exception;
                        }
                        SP_GO!(-1);
                    }
                    OpCode::ITERATOR_NEXT => {
                        // [iter_obj, next, catch_offset, val, ^] -> [.., val2, ^]
                        if let Ok(ret) = SP_REF!(-3).call(
                            realm_scope,
                            SP_VAL!(-4),
                            std::slice::from_ref(SP_REF!(-1)),
                        ) {
                            SP_PUT!(-1, ret);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::ITERATOR_CALL => {
                        // [iter_obj, next, catch_offset, val, ^] -> [.., val2, bool, ^]
                        let flags = PC_FETCH!(u8);

                        if let Ok(method) = SP_REF!(-4).get_property(
                            realm_scope,
                            if flags & 1 != 0 {
                                Atom::CONST_throw
                            } else {
                                Atom::CONST_return
                            },
                        ) {
                            let ret_flag = if unlikely(method.is_undefined_or_null()) {
                                true
                            } else {
                                if let Ok(ret) = method.call(
                                    realm_scope,
                                    SP_VAL!(-4),
                                    if flags & 2 != 0 {
                                        &[]
                                    } else {
                                        std::slice::from_ref(SP_REF!(-1))
                                    },
                                ) {
                                    SP_PUT!(-1, ret);
                                } else {
                                    break ExecAction::Exception;
                                }
                                false
                            };

                            SP_PUT!(0, JSValue::from(ret_flag));
                            SP_GO!(1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }

                    // ── FCLOSURE ──
                    OpCode::FCLOSURE => {
                        // [^] -> [func_obj, ^]
                        let id = PC_FETCH!(u32);

                        let bc = unsafe { CPOOL![id].as_bytecode_unchecked() };
                        let r = bc
                            .make_closure_for_arena(
                                realm_scope,
                                None,
                                captured_vars!(),
                                arena_mut!(),
                                cur_frame_idx,
                            )
                            .map_or(JSValue::EXCEPTION, |f| f.into());

                        SP_PUT!(0, r);
                        SP_GO!(1);
                        if r.is_exception() {
                            break ExecAction::Exception;
                        }
                    }

                    // ── Generator/Async suspension and return ──
                    OpCode::AWAIT => {
                        break ExecAction::Return(JSValue::from(FUNC_RET_AWAIT), true);
                    }
                    OpCode::YIELD => {
                        break ExecAction::Return(JSValue::from(FUNC_RET_YIELD), true);
                    }
                    OpCode::YIELD_STAR | OpCode::ASYNC_YIELD_STAR => {
                        break ExecAction::Return(JSValue::from(FUNC_RET_YIELD_STAR), true);
                    }
                    OpCode::INITIAL_YIELD => {
                        break ExecAction::Return(JSValue::from(FUNC_RET_INITIAL_YIELD), true);
                    }
                    OpCode::RETURN_ASYNC => {
                        break ExecAction::Return(JSValue::UNDEFINED, true);
                    }

                    // ── Subroutine call (try-finally) ──
                    OpCode::GOSUB => {
                        let diff = unsafe { read_from_ptr::<i32>(pc) };
                        // Push return address as Int (offset to instruction after GOSUB)
                        SP_PUT!(0, JSValue::from(pc.offset_from(pc_base) as i32 + 4));
                        SP_GO!(1);
                        PC_GO!(diff);
                    }
                    OpCode::RET => {
                        if let Some(i) = SP_REF!(-1).as_int() {
                            let pos = i;
                            if unlikely(pos >= bytecodes.len() as i32) {
                                let _ = realm_scope.throw_internal_error("invalid ret value");
                                break ExecAction::Exception;
                            } else {
                                SP_GO!(-1);
                                pc = unsafe { pc_base.offset(pos as isize) };
                            }
                        } else {
                            let _ = realm_scope.throw_internal_error("invalid ret value");
                            break ExecAction::Exception;
                        }
                    }

                    // ── Stack manipulation ──
                    OpCode::DUP3 => {
                        // [a, b, c, ^] -> [a, b, c, a, b, c, ^]
                        SP_PUT!(0, SP_VAL!(-3));
                        SP_PUT!(1, SP_VAL!(-2));
                        SP_PUT!(2, SP_VAL!(-1));
                        SP_GO!(3);
                    }
                    OpCode::INSERT2 => {
                        // [obj, a, ^] -> [a, obj, a, ^]
                        SP_PUT!(0, SP_VAL!(-1));
                        SP_PUT!(-1, SP_VAL!(-2));
                        SP_PUT!(-2, SP_VAL!(0));
                        SP_GO!(1);
                    }
                    OpCode::INSERT3 => {
                        // [obj, prop, a, ^] -> [a, obj, prop, a, ^]
                        unsafe {
                            std::slice::from_raw_parts_mut(sp.offset(-3), 4).rotate_right(1);
                        };
                        SP_PUT!(-3, SP_VAL!(0));
                        SP_GO!(1);
                    }
                    OpCode::INSERT4 => {
                        // [this, obj, prop, a, ^] -> [a, this, obj, prop, a, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 5) };
                        lst.rotate_right(1);
                        SP_PUT!(-4, SP_VAL!(0));
                        SP_GO!(1);
                    }
                    OpCode::ROT3L => {
                        // [x, a, b, ^] -> [a, b, x, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-3), 3) };
                        lst.rotate_left(1);
                    }
                    OpCode::ROT4L => {
                        // [x, a, b, c, ^] -> [a, b, c, x, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 4) };
                        lst.rotate_left(1);
                    }
                    OpCode::ROT5L => {
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-5), 5) };
                        lst.rotate_left(1);
                    }
                    OpCode::ROT3R => {
                        // [a, b, x, ^] -> [x, a, b, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-3), 3) };
                        lst.rotate_right(1);
                    }
                    OpCode::PERM3 => {
                        // [obj, a, fixed, ^] -> [a, obj, fixed, ^]
                        unsafe {
                            std::ptr::swap(sp.offset(-2), sp.offset(-3));
                        }
                    }
                    OpCode::PERM4 => {
                        // [obj, prop, a, fixed, ^] -> [a, obj, prop, fixed, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-4), 3) };
                        lst.rotate_right(1);
                    }
                    OpCode::PERM5 => {
                        // [this, obj, prop, a, fixed, ^] -> [a, this, obj, prop, fixed, ^]
                        let lst = unsafe { std::slice::from_raw_parts_mut(sp.offset(-5), 4) };
                        lst.rotate_right(1);
                    }
                    OpCode::IS_UNDEFINED_OR_NULL => {
                        SP_PUT!(-1, JSValue::from(SP_REF!(-1).is_undefined_or_null()));
                    }

                    // ── For-in ──
                    OpCode::FOR_IN_START => {
                        if js_for_in_start(realm_scope, sp).is_err() {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::FOR_IN_NEXT => {
                        if js_for_in_next(realm_scope, sp).is_err() {
                            break ExecAction::Exception;
                        }
                        SP_GO!(2);
                    }

                    // ── Global varref / Ref operations ──
                    OpCode::MAKE_VAR_REF => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        match ref_ops::make_global_ref(realm_scope, atom) {
                            Ok((v0, v1)) => {
                                SP_PUT!(0, v0);
                                SP_PUT!(1, v1);
                                SP_GO!(2);
                            }
                            Err(_) => break ExecAction::Exception,
                        }
                    }
                    OpCode::GET_REF_VALUE => {
                        // [obj, prop, ^] -> [obj, value, ^]
                        match ref_ops::get_ref_value(realm_scope, SP_VAL!(-2), SP_VAL!(-1)) {
                            Ok(val) => {
                                SP_PUT!(0, val);
                                SP_GO!(1);
                            }
                            Err(_) => break ExecAction::Exception,
                        }
                    }
                    OpCode::GET_SUPER_VALUE => {
                        match class_ops::get_super_value(
                            realm_scope,
                            SP_VAL!(-3),
                            SP_VAL!(-2),
                            SP_VAL!(-1),
                        ) {
                            Ok(val) => {
                                SP_PUT!(-3, val);
                                SP_GO!(-2);
                            }
                            Err(_) => break ExecAction::Exception,
                        }
                    }
                    OpCode::PUT_REF_VALUE => {
                        // [obj, prop, val, ^] -> [^]
                        let r = ref_ops::put_ref_value(
                            realm_scope,
                            SP_VAL!(-3),
                            SP_VAL!(-2),
                            SP_VAL!(-1),
                        );
                        SP_GO!(-3);
                        if r.is_err() {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PUT_SUPER_VALUE => {
                        // [this_obj, obj, prop, val, ^] -> [^]
                        let r = class_ops::put_super_value(
                            realm_scope,
                            SP_VAL!(-4),
                            SP_VAL!(-3),
                            SP_VAL!(-2),
                            SP_VAL!(-1),
                        );
                        SP_GO!(-4);
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::DEFINE_ARRAY_EL => {
                        // [obj, prop, value, ^] -> [obj, prop, ^]
                        let ret = class_ops::define_array_el(
                            realm_scope,
                            SP_MUT!(-3),
                            SP_VAL!(-2),
                            SP_VAL!(-1),
                        );
                        SP_GO!(-1);
                        if unlikely(ret.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::APPEND => {
                        // [dest, start_pos, src, ^] -> [dest, end_pos, ^]
                        if unsafe { js_append_enumerate(realm_scope, sp).is_ok() } {
                            SP_GO!(-1);
                        } else {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::COPY_DATA_PROPERTIES => {
                        // target source excludeList
                        let mask = PC_FETCH!(u8) as i32;
                        let r = class_ops::copy_data_properties(
                            realm_scope,
                            SP_MUT!(-1 - (mask & 3)),
                            SP_VAL!(-1 - ((mask >> 2) & 7)),
                            SP_VAL!(-1 - ((mask >> 5) & 7)),
                        );
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                    }

                    // ── Private Symbol ──
                    OpCode::PRIVATE_SYMBOL => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        match class_ops::private_symbol(realm_scope, atom) {
                            Ok(val) => {
                                SP_PUT!(0, val);
                                SP_GO!(1);
                            }
                            Err(_) => break ExecAction::Exception,
                        }
                    }
                    OpCode::GET_PRIVATE_FIELD => {
                        let val =
                            class_ops::get_private_field(realm_scope, SP_VAL!(-2), SP_VAL!(-1))
                                .unwrap_or(JSValue::EXCEPTION);
                        SP_PUT!(-2, val);
                        SP_GO!(-1);
                        if unlikely(val.is_exception()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::PUT_PRIVATE_FIELD => {
                        // [this_obj, name, value, ^] -> [^]
                        let ret = class_ops::put_private_field(
                            realm_scope,
                            SP_MUT!(-3),
                            SP_VAL!(-2),
                            SP_VAL!(-1),
                        );
                        SP_GO!(-3);
                        if unlikely(ret.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::DEFINE_PRIVATE_FIELD => {
                        // [this_obj, name, value, ^] -> [this_obj, ^]
                        let ret = class_ops::define_private_field(
                            realm_scope,
                            SP_MUT!(-3),
                            SP_VAL!(-2),
                            SP_VAL!(-1),
                        );
                        SP_GO!(-2);
                        if unlikely(ret.is_err()) {
                            break ExecAction::Exception;
                        }
                    }

                    // ── Object / Method / Class definition ──
                    OpCode::DEFINE_FIELD => {
                        // [this_obj, value, ^] -> [this_obj, ^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let ret =
                            class_ops::define_field(realm_scope, SP_MUT!(-2), atom, SP_VAL!(-1));
                        SP_GO!(-1);
                        if unlikely(ret.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::SET_NAME => {
                        // [obj, ^] -> [obj, ^]
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let r = SP_REF!(-1).as_object().ok_or(-1).and_then(|mut o| {
                            o.define_name(realm_scope, atom, JS_PROP_CONFIGURABLE)
                        });
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::SET_NAME_COMPUTED => {
                        // [name, value, ^] -> [name, value, ^]
                        let r = class_ops::set_name_computed(realm_scope, SP_VAL!(-2), SP_MUT!(-1));
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::SET_PROTO => {
                        // [obj, proto, ^] -> [obj, ^]
                        if class_ops::set_proto(realm_scope, SP_MUT!(-2), SP_VAL!(-1)).is_err() {
                            break ExecAction::Exception;
                        }
                        SP_GO!(-1);
                    }
                    OpCode::SET_HOME_OBJECT => {
                        // [home_obj, func, ^] -> [home_obj, func, ^]
                        class_ops::set_home_object(SP_MUT!(-1), SP_VAL!(-2));
                    }
                    OpCode::DEFINE_METHOD | OpCode::DEFINE_METHOD_COMPUTED => {
                        let (is_computed, atom, op_flags) =
                            if opcode == OpCode::DEFINE_METHOD_COMPUTED {
                                (true, Atom::NULL, PC_FETCH!(u8))
                            } else {
                                (false, Atom::from_id(PC_FETCH!(u32)), PC_FETCH!(u8))
                            };

                        let ret = class_ops::define_method(
                            realm_scope,
                            SP_MUT!(if is_computed { -3 } else { -2 }),
                            SP_VAL!(-1),
                            atom,
                            SP_VAL!(-2),
                            op_flags,
                        );

                        if is_computed {
                            SP_GO!(-2);
                        } else {
                            SP_GO!(-1);
                        }

                        if ret.is_err() {
                            break ExecAction::Exception;
                        }
                    }
                    OpCode::DEFINE_CLASS | OpCode::DEFINE_CLASS_COMPUTED => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let class_flags = PC_FETCH!(u8) as u32;
                        let r = js_op_define_class_for_arena(
                            realm_scope,
                            sp,
                            atom,
                            class_flags as _,
                            captured_vars!(),
                            arena_mut!(),
                            cur_frame_idx,
                            opcode == OpCode::DEFINE_CLASS_COMPUTED,
                        );
                        if unlikely(r.is_err()) {
                            break ExecAction::Exception;
                        }
                    }

                    // ── With statement ──
                    OpCode::WITH_GET_VAR
                    | OpCode::WITH_PUT_VAR
                    | OpCode::WITH_DELETE_VAR
                    | OpCode::WITH_MAKE_REF
                    | OpCode::WITH_GET_REF => {
                        let atom = Atom::from_id(PC_FETCH!(u32));
                        let diff = PC_FETCH!(i32);
                        let is_with = PC_FETCH!(u8) != 0;

                        let mut env_obj = SP_VAL!(-1);

                        match with_ops::in_env(realm_scope, env_obj, atom, is_with) {
                            Ok(true) => {
                                match opcode {
                                    OpCode::WITH_GET_VAR => {
                                        match with_ops::get_var(realm_scope, env_obj, atom) {
                                            Ok(val) => SP_PUT!(-1, val),
                                            Err(_) => break ExecAction::Exception,
                                        }
                                    }
                                    OpCode::WITH_PUT_VAR => {
                                        let ret = with_ops::put_var(
                                            realm_scope,
                                            env_obj,
                                            atom,
                                            SP_VAL!(-2),
                                        );
                                        // Consume the operands before checking the error.
                                        SP_GO!(-2);
                                        if unlikely(ret.is_err()) {
                                            break ExecAction::Exception;
                                        }
                                    }
                                    OpCode::WITH_DELETE_VAR => {
                                        match with_ops::delete_var(realm_scope, &mut env_obj, atom)
                                        {
                                            Ok(b) => SP_PUT!(-1, JSValue::from(b)),
                                            Err(_) => break ExecAction::Exception,
                                        }
                                    }
                                    OpCode::WITH_MAKE_REF => {
                                        // Push the object/prop-name pair.
                                        match with_ops::prop_name(realm_scope, atom) {
                                            Ok(prop) => {
                                                SP_PUT!(0, prop);
                                                SP_GO!(1);
                                            }
                                            Err(_) => break ExecAction::Exception,
                                        }
                                    }
                                    OpCode::WITH_GET_REF => {
                                        match with_ops::get_ref(realm_scope, env_obj, atom) {
                                            Ok(val) => {
                                                SP_PUT!(0, val);
                                                SP_GO!(1);
                                            }
                                            Err(_) => break ExecAction::Exception,
                                        }
                                    }
                                    _ => {
                                        #[cfg(debug_assertions)]
                                        unreachable!();
                                        #[cfg(not(debug_assertions))]
                                        unsafe {
                                            std::hint::unreachable_unchecked();
                                        }
                                    }
                                }

                                PC_GO!(diff - 5);
                            }
                            Ok(false) => SP_GO!(-1),
                            Err(_) => break ExecAction::Exception,
                        }
                    }

                    _ => {
                        let err_pc = unsafe { pc.offset_from(pc_base) - 1 } as usize;
                        let _ = realm_scope.throw_internal_error(&format!(
                            "invalid opcode: 0x{:02x}, pc={err_pc}",
                            opcode as u8,
                        ));
                        break ExecAction::Exception;
                    }
                } // match opcode

                #[cfg(all(debug_assertions, feature = "debug_gc"))]
                {
                    sync_sp!();
                    realm_scope.clear_locals_from_depth(gc_scope_base_depth);
                    rt.gc_collect();
                }
                #[cfg(not(all(debug_assertions, feature = "debug_gc")))]
                if rt.gc_req {
                    // GC scans up to `cur_sp_offset`; sync the moved `sp` first.
                    sync_sp!();
                    realm_scope.clear_locals_from_depth(gc_scope_base_depth);
                    rt.gc_collect();
                }
            }; // opcodes inner loop

            match action {
                ExecAction::Return(ret_val, is_generator) => {
                    if !is_generator && !callers.is_empty() {
                        inline_return_to_caller!(ret_val);
                        continue 'arena_loop;
                    }
                    debug_assert!(
                        callers.is_empty(),
                        "generator/suspension return with inline callers"
                    );
                    save_pc!(pc.offset_from(pc_base) as usize);
                    save_sp!(sp.offset_from(sp_base) as i32);
                    return ExecAction::Return(ret_val, is_generator);
                }

                ExecAction::Exception => {
                    let cur_pc = unsafe { pc.offset_from(pc_base) as usize };
                    let sp_offset = unsafe { sp.offset_from(sp_base) } as i32;
                    let Some((new_pc, new_sp)) = catch_in_frame_at(
                        &rt,
                        realm_scope,
                        arena_mut!(),
                        cur_frame_idx,
                        cur_pc,
                        sp_offset,
                    ) else {
                        // Uncaught here. With no inline caller, let the
                        // trampoline run the finally scan and free the frame;
                        // otherwise unwind the local caller chain here.
                        if callers.is_empty() {
                            return ExecAction::Unwind;
                        }
                        if rt.interrupt_code.is_none()
                            && let Some((finally_pc, finally_sp)) =
                                arena_mut!().find_finally_in_frame(cur_frame_idx, bytecodes.len())
                        {
                            let err_val = realm_scope.take_exception().to_value(realm_scope);
                            let finally_sp_base = frame!(cur_frame_idx).stack_base_ptr();
                            unsafe {
                                std::ptr::write(finally_sp_base.add(finally_sp - 1), err_val);
                            }
                            pc = unsafe { pc_base.add(finally_pc) };
                            sp = unsafe { finally_sp_base.add(finally_sp + 1) };
                            save_sp!((finally_sp + 1) as i32);
                            continue 'arena_loop;
                        }
                        arena_mut!().detach_and_free(cur_frame_idx);
                        inline_unwind_dead!('arena_loop);
                    };
                    save_sp!(new_sp as i32);
                    pc = unsafe { pc_base.add(new_pc) };
                    sp = unsafe { sp_base.add(new_sp) };
                    continue 'arena_loop;
                }

                ExecAction::CallBytecode {
                    callee_bc,
                    callee_frame_idx,
                    return_sp,
                    this_obj,
                    new_target,
                    ctor_this,
                } => {
                    debug_assert!(callers.is_empty(), "trampoline call with inline callers");
                    save_pc!(pc.offset_from(pc_base) as usize);
                    save_sp!(sp.offset_from(sp_base) as i32);
                    return ExecAction::CallBytecode {
                        callee_bc,
                        callee_frame_idx,
                        return_sp,
                        this_obj,
                        new_target,
                        ctor_this,
                    };
                }

                ExecAction::CallBytecodeCrossRealm {
                    callee_bc,
                    callee_frame_idx,
                    callee_ctx_ptr,
                    return_sp,
                    this_obj,
                    new_target,
                    ctor_this,
                } => {
                    save_pc!(pc.offset_from(pc_base) as usize);
                    save_sp!(sp.offset_from(sp_base) as i32);

                    return ExecAction::CallBytecodeCrossRealm {
                        callee_bc,
                        callee_frame_idx,
                        callee_ctx_ptr,
                        return_sp,
                        this_obj,
                        new_target,
                        ctor_this,
                    };
                }

                ExecAction::Unwind => {
                    unreachable!(
                        "Unwind not emitted from inner loop — goes through Exception handler"
                    )
                }

                ExecAction::TailCall {
                    callee_bc,
                    callee_func_obj,
                    args,
                    this_obj,
                } => {
                    debug_assert!(
                        callers.is_empty(),
                        "trampoline tail call with inline callers"
                    );
                    sync_sp!();
                    return ExecAction::TailCall {
                        callee_bc,
                        callee_func_obj,
                        args,
                        this_obj,
                    };
                }
            }
        } // 'arena_loop
    }
}

/// Allocate and initialize a callee frame.
///
/// Capacity covers `max(declared args, actual args) + locals`. The callee's
/// depth is `caller_depth + 1`, bounding non-tail recursion at
/// `VM_MAX_CALL_DEPTH`; a tail call passes `depth - 1` so the replacement keeps
/// the caller's depth.
#[inline(always)]
fn build_callee_frame(
    arena: &mut VmStack,
    caller_depth: u32,
    func_obj: Object,
    bc: Bytecode,
    args: &[JSValue],
    this_obj: JSValue,
) -> Result<FrameIndex, FrameAllocError> {
    // Saturate: unlimited depth (0) would otherwise overflow `u32`.
    let callee_depth = caller_depth.saturating_add(1);
    if VM_MAX_CALL_DEPTH > 0 && callee_depth > VM_MAX_CALL_DEPTH {
        return Err(FrameAllocError::Exceeded);
    }

    let cap = std::cmp::max(bc.arg_count as usize, args.len()) + bc.local_buf_size as usize;
    let idx = arena.alloc_frame(func_obj, bc.mode(), cap)?;
    let head = arena.frame_mut(idx);
    head.depth = callee_depth;
    head.this_obj = this_obj;
    arena.init_frame_args(idx, bc.arg_count, bc.var_count, args);

    Ok(idx)
}

/// Drop the scopes parked in the virtual frames. Required before an early
/// return (yield/await); leaving them parked corrupts the GC scope stack.
#[inline(always)]
fn drain_saved_scopes(virtual_frames: &mut VirtualFrames) {
    while let Some(mut vf) = virtual_frames.pop() {
        if let Some(scope) = vf.caller_scope.take() {
            drop(scope);
        }
    }
}

/// Pop one virtual frame and restore the caller: swap its saved scope back in
/// (unless it was shared) and return its saved pc/sp.
///
/// The normal Return path promotes the value and uses `return_sp` instead.
#[inline(always)]
fn restore_caller_frame(
    caller_vf: VirtualFrame,
    scope: &mut TrampolineScope,
    arena: &VmStack,
) -> (Bytecode, FrameIndex, usize, usize) {
    if let Some(saved_caller_scope) = caller_vf.caller_scope {
        // Light/Full: drop the callee scope and restore the caller.
        unsafe { scope.swap_in_saved(saved_caller_scope) };
    }
    // None: nothing was swapped.

    let caller_head = arena.frame(caller_vf.frame_idx);
    (
        caller_vf.caller_bc,
        caller_vf.frame_idx,
        caller_head.cur_pc_offset,
        caller_head.cur_sp_offset as usize,
    )
}

/// Run frames in one loop without C-stack recursion for JS-to-JS calls.
///
/// Returns `(value, completed)`; `completed == false` means suspended
/// (yield/await).
pub(crate) fn execute_trampoline(
    start_bc: Bytecode,
    start_frame_idx: FrameIndex,
    start_scope: &mut ScopedContext,
    mut this_obj: JSValue,
    mut new_target: JSValue,
) -> (JSValue, bool) {
    let mut rt_ptr = start_scope.rt_ptr();
    let mut cur_bc = start_bc;
    let mut cur_frame_idx = start_frame_idx;

    // Stable arena pointer: avoids borrow conflicts with the scope, which
    // borrows the runtime.
    let arena_ptr: *mut VmStack = &raw mut rt_ptr.vm_stack;

    // Seed the start frame's `this` (a traced GC root).
    unsafe {
        (*arena_ptr).frame_mut(start_frame_idx).this_obj = this_obj;
    }

    let mut realm_ctx = start_bc.realm();
    // Declared before the scope: on panic, the scope drops first, then this
    // guard drains parked scopes newest-first.
    let mut virtual_frames = VirtualFrames::new();
    // A None-scope callee runs in the caller's scope, so a call pays no scope
    // push/pop/promote. Cross-realm callees always get their own scope (their
    // error objects need the callee realm's prototypes).
    let mut cur_trampoline_scope = if start_bc.gc_scope_kind() == GcScopeKind::None
        && start_bc.realm() == start_scope.ctx_ptr
    {
        TrampolineScope::borrowed(start_scope)
    } else {
        unsafe { TrampolineScope::new(&mut realm_ctx) }
    };
    // Lowest owned scope: safepoints clear LOCAL protection from here up, so
    // suspended callers cannot accumulate garbage. A borrowed start scope sits
    // below it.
    let gc_scope_base_depth =
        cur_trampoline_scope.depth() + u32::from(!cur_trampoline_scope.owns_scope());

    // A pending exception (e.g. `.throw()` on a suspended async function) must
    // be handled by the first `execute_with_arena` call.
    let mut throwed = rt_ptr.exception() != Exception::No;

    // PC/SP from the frame head: 0 on first run, saved on resume.
    let (mut pc_offset, mut sp_offset) = {
        let arena = unsafe { &*arena_ptr };
        let head = arena.frame(start_frame_idx);
        (head.cur_pc_offset, head.cur_sp_offset as usize)
    };

    'trampoline: loop {
        // Poll deferred JIT work between frames (a safe point).
        #[cfg(feature = "jit")]
        crate::jit::poll_deferred(&rt_ptr);

        let arena = unsafe { &mut *arena_ptr };

        rt_ptr.active_frame_index = cur_frame_idx;
        debug_assert!(cur_trampoline_scope.is_arena_mode());
        // Frames dispatched here must be bytecode carriers:
        // `is_bytecode_frame_active` drives the caller-scope GC policy.
        debug_assert!(
            unsafe { (*rt_ptr).is_bytecode_frame_active() },
            "trampoline frame is not classified as a bytecode frame"
        );

        // Args and [[HomeObject]] are derived inside the dispatch loop, which
        // also switches frames for inline calls.
        let action = cur_bc.execute_with_arena(
            cur_trampoline_scope.scope_mut(),
            arena,
            cur_frame_idx,
            gc_scope_base_depth,
            this_obj,
            new_target,
            pc_offset,
            sp_offset,
            throwed,
        );

        match action {
            ExecAction::Exception => {
                unreachable!(
                    "Exception from execute_with_arena should be handled inside — \
                     trampoline only receives Unwind for uncaught exceptions"
                )
            }
            ExecAction::CallBytecode {
                callee_bc,
                callee_frame_idx: callee_idx,
                return_sp,
                this_obj: callee_this,
                new_target: callee_new_target,
                ctor_this,
            } => {
                let callee_gc_kind = callee_bc.gc_scope_kind();

                // `None` for a shared scope; `Some` for Light/Full.
                let caller_scope = match callee_gc_kind {
                    GcScopeKind::None => {
                        // Run in the caller's scope; this callee allocates no
                        // LOCAL nodes.
                        None
                    }
                    GcScopeKind::Light | GcScopeKind::Full => {
                        let mut callee_realm_ctx = callee_bc.realm();
                        let callee_trampoline_scope =
                            unsafe { TrampolineScope::new(&mut callee_realm_ctx) };

                        Some(
                            std::mem::replace(&mut cur_trampoline_scope, callee_trampoline_scope)
                                .into_inner(),
                        )
                    }
                };

                virtual_frames.push(VirtualFrame {
                    frame_idx: cur_frame_idx,
                    caller_bc: cur_bc,
                    return_sp,
                    caller_scope,
                    ctor_this,
                });

                cur_bc = callee_bc;
                cur_frame_idx = callee_idx;
                pc_offset = 0;
                sp_offset = 0;
                throwed = false;

                this_obj = callee_this;
                new_target = callee_new_target;
            }

            ExecAction::TailCall {
                callee_bc,
                callee_func_obj,
                args,
                this_obj: callee_this,
            } => {
                let arena = unsafe { &mut *arena_ptr };

                // Keep the backtrace chain across TCO: the replacement's `prev`
                // points to the original caller, skipping the tail-calling
                // function.
                let orig_prev = {
                    let head = arena.frame(cur_frame_idx);
                    head.prev
                };

                // Pass `depth - 1` (`build_callee_frame` adds 1) so the
                // replacement keeps the caller's depth; TCO never deepens.
                let tail_depth = arena.frame(cur_frame_idx).depth.saturating_sub(1);

                // Detach the outgoing frame's VarRefs and recycle it before
                // allocating the callee, keeping the bump region bounded.
                arena.detach_and_free(cur_frame_idx);

                // On failure, propagate as a normal uncaught exception so
                // enclosing try/catch/finally still run.
                let new_idx = match build_callee_frame(
                    arena,
                    tail_depth,
                    callee_func_obj,
                    callee_bc,
                    &args,
                    callee_this,
                ) {
                    Ok(idx) => idx,
                    Err(e) => {
                        match e {
                            FrameAllocError::Oom => {
                                let _ = cur_trampoline_scope.scope_mut().throw_stack_fail();
                            }
                            FrameAllocError::Exceeded => {
                                let _ = cur_trampoline_scope.scope_mut().throw_stack_exceeded();
                            }
                        }

                        // The caller frame is already recycled, so unwind to its
                        // caller; the error stays catchable and enclosing
                        // handlers still run.
                        if let Some(caller_vf) = virtual_frames.pop() {
                            (cur_bc, cur_frame_idx, pc_offset, sp_offset) =
                                restore_caller_frame(caller_vf, &mut cur_trampoline_scope, arena);
                            throwed = true;

                            continue 'trampoline;
                        } else {
                            drop(cur_trampoline_scope.into_inner());
                            rt_ptr.active_frame_index = FrameIndex::NULL;
                            return (JSValue::EXCEPTION, true);
                        }
                    }
                };

                // Restore the backtrace chain: the tail-called function appears
                // to have been called directly by the original caller.
                arena.frame_mut(new_idx).prev = orig_prev;

                cur_bc = callee_bc;
                cur_frame_idx = new_idx;
                pc_offset = 0;
                sp_offset = 0;
                this_obj = callee_this;
                new_target = JSValue::UNDEFINED;
                throwed = false;
            }

            ExecAction::Return(ret_val, is_generator) => {
                // ── Frame return ──
                if is_generator {
                    if ret_val.is_undefined() {
                        // RETURN_ASYNC: async function completed normally.
                        // The actual return value is at SP[-1] in the frame.
                        let arena = unsafe { &mut *arena_ptr };
                        // Extract stack pointer and read return value.
                        let sp_offset = {
                            let head = arena.frame(cur_frame_idx);
                            head.cur_sp_offset
                        };
                        let sp_base = {
                            let head = arena.frame(cur_frame_idx);
                            head.stack_base_ptr()
                        };
                        let actual_ret =
                            unsafe { std::ptr::read(sp_base.offset(sp_offset as isize - 1)) };
                        // Detach VarRefs before freeing (an async arrow may have
                        // captured them, e.g. `new.target`).
                        arena.detach_and_free(cur_frame_idx);
                        // Promote before the scope pop so the value stays LOCAL
                        // in the caller's scope. A borrowed scope has no locals.
                        if cur_trampoline_scope.owns_scope() {
                            actual_ret.promote(&cur_trampoline_scope);
                        }
                        drop(cur_trampoline_scope.into_inner());

                        // Drop the parked caller scopes.
                        drain_saved_scopes(&mut virtual_frames);
                        rt_ptr.active_frame_index = FrameIndex::NULL;

                        return (actual_ret, true);
                    }

                    // YIELD/AWAIT/YIELD_STAR: suspended. The frame stays Active;
                    // the caller must `take_frame`.
                    //
                    // No sp fix-up for YIELD_STAR: the resume handler pushes the
                    // completion type and the following DUP/IF_TRUE/DROP sequence
                    // consumes it before ITERATOR_NEXT.
                    drop(cur_trampoline_scope.into_inner());

                    // Drop the parked caller scopes before the early return.
                    drain_saved_scopes(&mut virtual_frames);
                    rt_ptr.active_frame_index = FrameIndex::NULL;
                    return (ret_val, false);
                }

                let arena = unsafe { &mut *arena_ptr };

                // The outermost frame is owned by `BytecodeFunction::call`;
                // inner frames are freed here.
                let is_outermost = virtual_frames.is_empty();
                if !is_outermost {
                    // Detach VarRefs before freeing; they would dangle after the
                    // region is reused.
                    arena.detach_and_free(cur_frame_idx);
                }
                // Promote before the scope pop.

                if let Some(caller_vf) = virtual_frames.pop() {
                    if let Some(saved_caller_scope) = caller_vf.caller_scope {
                        // Light/Full: promote, pop the callee scope, restore.
                        ret_val.promote(&cur_trampoline_scope);
                        unsafe { cur_trampoline_scope.swap_in_saved(saved_caller_scope) };
                    }
                    // None: the scope was never swapped.

                    // Legacy constructor: a non-object return becomes the
                    // instance created at the call site (LOCAL in this scope).
                    let ret_val = match caller_vf.ctor_this {
                        Some(inst) if !ret_val.is_object() => inst,
                        _ => ret_val,
                    };

                    cur_bc = caller_vf.caller_bc;
                    cur_frame_idx = caller_vf.frame_idx;
                    // Restore the caller's `this` from its frame header.
                    this_obj = arena.frame(cur_frame_idx).this_obj;

                    let caller_head = arena.frame(caller_vf.frame_idx);
                    let sp_ptr = caller_head.stack_base_ptr();
                    unsafe {
                        *sp_ptr.add(caller_vf.return_sp) = ret_val;
                    }

                    // The call consumed `func + args`: resume at `return_sp + 1`.
                    pc_offset = caller_head.cur_pc_offset;
                    sp_offset = caller_vf.return_sp + 1;
                    throwed = false;
                } else {
                    // Outermost: promote and drop the trampoline scope. A
                    // borrowed scope has no locals.
                    if cur_trampoline_scope.owns_scope() {
                        ret_val.promote(&cur_trampoline_scope);
                    }
                    drop(cur_trampoline_scope.into_inner());
                    rt_ptr.active_frame_index = FrameIndex::NULL;
                    return (ret_val, true);
                }
            }

            // ── Exception propagation ──
            ExecAction::Unwind => {
                let arena = unsafe { &mut *arena_ptr };

                // Check for a finally (GOSUB marker) before freeing. An
                // interrupt bypasses handlers: the GOSUB scan would read its
                // leftover operand stack as a pc.
                let found_finally = if rt_ptr.interrupt_code.is_some() {
                    None
                } else {
                    arena.find_finally_in_frame(cur_frame_idx, cur_bc.opcodes().len())
                };
                if let Some((finally_pc, finally_sp)) = found_finally {
                    // Save the exception below the GOSUB marker so the THROW
                    // after the finally can rethrow it.
                    let err_val = cur_trampoline_scope
                        .take_exception()
                        .to_value(&mut cur_trampoline_scope);
                    let sp_base = arena.frame(cur_frame_idx).stack_base_ptr();
                    unsafe {
                        // After RET pops the marker, the exception is at TOS.
                        std::ptr::write(sp_base.add(finally_sp - 1), err_val);
                    }
                    pc_offset = finally_pc;
                    sp_offset = finally_sp + 1; // exception + GOSUB marker
                    throwed = false;
                    continue 'trampoline;
                }

                // As on Return: inner frames are freed here; the outermost is
                // owned by `BytecodeFunction::call`.
                let is_outermost = virtual_frames.is_empty();
                if !is_outermost {
                    // Detach VarRefs before freeing (as on Return).
                    arena.detach_and_free(cur_frame_idx);
                }
                if let Some(caller_vf) = virtual_frames.pop() {
                    (cur_bc, cur_frame_idx, pc_offset, sp_offset) =
                        restore_caller_frame(caller_vf, &mut cur_trampoline_scope, arena);
                    this_obj = arena.frame(cur_frame_idx).this_obj;
                    throwed = true;

                    continue 'trampoline;
                } else {
                    // Outermost Unwind: drop the trampoline scope.
                    drop(cur_trampoline_scope.into_inner());
                    rt_ptr.active_frame_index = FrameIndex::NULL;
                    return (JSValue::EXCEPTION, true);
                }
            }

            ExecAction::CallBytecodeCrossRealm {
                callee_bc,
                callee_frame_idx: callee_idx,
                callee_ctx_ptr,
                return_sp,
                this_obj: callee_this,
                new_target: callee_new_target,
                ctor_this,
            } => {
                // ── Cross-realm call ──
                // Always give the callee a realm-bound scope, even for None: a
                // thrown error needs that realm's prototypes.
                let caller_scope = {
                    let mut callee_realm_ctx = callee_ctx_ptr;
                    let callee_trampoline_scope =
                        unsafe { TrampolineScope::new(&mut callee_realm_ctx) };

                    Some(
                        std::mem::replace(&mut cur_trampoline_scope, callee_trampoline_scope)
                            .into_inner(),
                    )
                };

                virtual_frames.push(VirtualFrame {
                    frame_idx: cur_frame_idx,
                    caller_bc: cur_bc,
                    return_sp,
                    caller_scope,
                    ctor_this,
                });

                cur_bc = callee_bc;
                cur_frame_idx = callee_idx;
                pc_offset = 0;
                sp_offset = 0;
                throwed = false;
                this_obj = callee_this;
                new_target = callee_new_target;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use crate::{
        Context, EvalFlags, EvalName, EvalType, Runtime, ScopedContext,
        atom::Atom,
        error::JSResult,
        function::{CallContext, Callback, NativeFunction},
        object::JS_PROP_C_W_E,
        value::JSValue,
    };

    /// A None-scope callee runs in the caller's scope without promotion, so
    /// the value it hands back stays alive through the caller's scope.
    /// `Array.prototype.map` enters through `call_internal`, i.e. a nested
    /// trampoline (the borrowed-scope path).
    #[test]
    fn a_thin_callee_hands_back_a_live_value() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        let v = ctx
            .eval(
                b"function thin(x){ return x; } \
                  function run(){ \
                    var local = { tag: 'kept' }; \
                    var got = [local].map(thin)[0]; \
                    for (var i = 0; i < 20000; i++) { var junk = { i: i }; } \
                    return got.tag; \
                  } \
                  run();",
                &EvalName::Name("thin-callee.js".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .unwrap();

        assert_eq!(
            v.as_string(false).unwrap().content().encode_utf8(false),
            "kept",
            "the value the thin callee handed back did not survive"
        );
    }

    /// A throwing None-scope callee must keep the caller's object alive across
    /// the unwind.
    #[test]
    fn a_thin_callee_throws_into_the_callers_scope() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        let v = ctx
            .eval(
                b"function thin(x){ throw x; } \
                  function run(){ \
                    var local = { tag: 'kept' }; \
                    try { [local].map(thin); } catch (e) { return e.tag; } \
                    return 'no-throw'; \
                  } \
                  run();",
                &EvalName::Name("thin-callee-throw.js".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .unwrap();

        assert_eq!(
            v.as_string(false).unwrap().content().encode_utf8(false),
            "kept",
            "the thrown value did not reach the handler"
        );
    }

    /// A panic through `execute_trampoline` must pop every owned scope in LIFO
    /// order, or gc-lite's assertion masks the original panic.
    #[test]
    fn panic_in_trampoline_keeps_gc_scope_stack_balanced() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx: Context<'static> = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        ctx.with_root_scope(|mut root| -> Result<(), i32> {
            let proto = root.function_proto();
            let boom = NativeFunction::create(
                &mut root,
                Callback::Func(|_: &mut ScopedContext, _: CallContext| -> JSResult {
                    panic!("intentional panic from native callback")
                }),
                Atom::CONST_empty_string,
                0,
                0,
                proto,
                None,
            )?;
            root.global_obj().define_property_value_str(
                &mut root,
                "boom",
                JSValue::from(boom),
                JS_PROP_C_W_E,
            )?;
            Ok(())
        })
        .unwrap();

        let panicked = catch_unwind(AssertUnwindSafe(|| {
            let _ = ctx.eval(
                // Nested calls with object literals force Full GC scopes, so
                // the trampoline has several scopes parked in virtual frames
                // when the panic unwinds through them.
                b"function level2() { const x = { a: 2 }; boom(); return x; }\n\
                  function level1() { const x = { a: 1 }; level2(); return x; }\n\
                  level1();",
                &EvalName::Name("<panic-test>".into()),
                EvalType::Global,
                EvalFlags::empty(),
            );
        }));
        assert!(
            panicked.is_err(),
            "native callback must propagate its panic"
        );

        // If the trampoline had leaked a scope, this second eval's scope drop
        // would trip the LIFO assertion added by gc-lite.
        ctx.eval(
            b"40 + 2",
            &EvalName::Name("<after-panic>".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .expect("context must stay usable after a trampoline panic");
    }

    /// `return eval(...)` with `eval` rebound to a normal function is a
    /// `TAIL_EVAL`, i.e. a real tail call. Inside an inline chain it must be
    /// recycled in place: the trampoline only owns its own frame stack, so
    /// escaping to it would drop the parked callers.
    #[test]
    fn a_rebound_tail_eval_stays_in_the_inline_chain() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        let v = ctx
            .eval(
                b"(function () { \
                    var count = 0; \
                    function f(n) { \
                      'use strict'; \
                      if (n === 0) { count += 1; return 0; } \
                      return eval(n - 1); \
                    } \
                    eval('var eval = f;'); \
                    f(20000); \
                    return count; \
                  })();",
                &EvalName::Name("rebound-tail-eval.js".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .unwrap();

        assert_eq!(
            v.as_int(),
            Some(1),
            "the rebound tail eval did not reach its base case exactly once"
        );
    }

    /// The non-tail spelling (`var x = eval(...)`) must join the inline chain
    /// too; pushing a frame and escaping to the trampoline breaks the same
    /// contract.
    #[test]
    fn a_rebound_non_tail_eval_stays_in_the_inline_chain() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        let v = ctx
            .eval(
                b"(function () { \
                    function f(n) { \
                      'use strict'; \
                      if (n === 0) { return 0; } \
                      var x = eval(n - 1); \
                      return x + 1; \
                    } \
                    eval('var eval = f;'); \
                    return f(10); \
                  })();",
                &EvalName::Name("rebound-non-tail-eval.js".into()),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .unwrap();

        assert_eq!(v.as_int(), Some(10), "the rebound eval call returned wrong");
    }
}
