// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The native entry ABI and the helper trampolines.
//!
//! Compiled code never re-implements JS semantics. A helper reads the operands
//! straight out of the frame buffer (exactly like the interpreter's own `sp`),
//! calls the interpreter's existing Rust function, writes the result back, and
//! reports failure through its return value. The frame buffer is the only place
//! operands live, so a GC triggered inside a helper sees them.

use crate::{
    JSValue, ScopedContext,
    bytecode::{
        OpCode, SpecialObject,
        arithmatic::{UnaryOp, op_binary_logic},
        operators::{js_operator_delete, js_operator_in, js_operator_instanceof},
    },
    context::GlobalVarSetMode,
    function::{BCWrapper, VarRef, js_build_arguments, js_build_mapped_arguments},
    object::{JSProperty, Object, ThrowPolicy},
    vm_stack::{FrameIndex, VmStack},
};

/// Status returned by the stack helpers: `0` is success, `1`/`2` exit the
/// native frame with `JitExitKind::Exception`/`Interrupt`, and any other value
/// is a status the caller interprets itself (see [`HELPER_WITH_MISS`]).
pub(crate) const HELPER_OK: u32 = 0;
pub(crate) const HELPER_EXCEPTION: u32 = 1;
pub(crate) const HELPER_INTERRUPT: u32 = 2;
/// `with_*`: the environment object did not have the name, so the helper
/// dropped it and the generated code falls through to the normal variable
/// access. `0` is the hit — the access was performed and the stack reshaped for
/// the instruction's branch target.
pub(crate) const HELPER_WITH_MISS: u32 = 3;

/// What the native entry returns. Stored as a `u32` so the ABI is a single
/// register (a big `#[repr(C)]` struct return would need an sret pointer).
/// `CallBytecode`/`TailCall` are emitted by epic E4; the baseline never uses
/// them yet.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum JitExitKind {
    Done = 0,
    Exception = 1,
    Interrupt = 2,
    Unwind = 3,
    CallBytecode = 4,
    TailCall = 5,
    Resume = 6,
    Retry = 7,
}

impl JitExitKind {
    pub(crate) const fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Done,
            1 => Self::Exception,
            2 => Self::Interrupt,
            3 => Self::Unwind,
            6 => Self::Resume,
            _ => Self::Retry,
        }
    }
}

/// Everything a compiled function needs. Reconstructed by the interpreter at
/// the call site; helpers re-derive `ScopedContext`/`RuntimeImpl` from the raw
/// pointers. Field offsets are read by generated code, so the layout is frozen.
#[repr(C)]
pub(crate) struct JitExecFrame {
    /// `*mut ScopedContext`.
    pub scope: *mut u8,
    /// Points at the frame's argument slots.
    pub arg_base: *mut JSValue,
    /// Points at the frame's local slots.
    pub var_base: *mut JSValue,
    /// Points at the frame's operand-stack base.
    pub sp_base: *mut JSValue,
    /// Current operand-stack top (absolute). Updated by the native code and by
    /// stack helpers.
    pub sp: *mut JSValue,
    /// `Bytecode::consts().as_ptr()`.
    pub cpools: *const JSValue,
    /// `&mut StackFrame::cur_sp_offset` (GC scans up to this many slots).
    pub cur_sp_offset: *mut i32,
    /// `HELPERS.as_ptr()`.
    pub helpers: *const u8,
    pub this: JSValue,
    /// `GcScopeKind` as a `u32` (0 = Full, 2 = None).
    pub gc_scope_kind: u32,
    /// Immediate for the few helpers that need one (`push_atom_value`, global
    /// variable index, property atom).
    pub imm: u32,
    /// Second immediate (inline-cache index for `get_field`/`put_field`).
    pub imm2: u32,
    /// Entry pc: 0 is the function entry; any other value is an OSR resume
    /// point (a loop header for a hot loop, or the pc after a call the
    /// trampoline performed on our behalf).
    pub pc: u32,
    /// The running function's bytecode, for helpers that need more than a
    /// scalar (global-variable names; the inline cache). Valid for the whole
    /// call: the frame's `func_obj` keeps it reachable.
    pub bytecode: *const crate::function::BytecodeImpl,
    /// Return value for `Done`.
    pub out_value: JSValue,
    /// Byte offset into the opcode stream for `Resume`/`Exception`.
    pub out_pc: u32,
    /// Slot offset from `sp_base` for `Resume`/`Exception`.
    pub out_sp: i32,
    /// Actual argument count of the current call (`StackFrame::arg_real_count`,
    /// i.e. before the `undefined` padding up to `arg_count`). `rest` collects
    /// `arg_base[start..arg_real_count]`, exactly like the interpreter's
    /// `args` slice.
    pub arg_real_count: u32,
    /// `*mut VmStack`: the running interpreter's arena. Frame-entry helpers
    /// (`special_object`, `fclosure`) reach the current `StackFrame` through
    /// `arena` + `cur_frame_idx`, the same way the interpreter does.
    pub arena: *mut VmStack,
    /// Index of the running frame inside `arena` (`FrameIndex::as_usize`).
    pub cur_frame_idx: u32,
    /// The activation's `new.target` (the interpreter's per-call value).
    pub new_target: JSValue,
    /// The activation's `[[HomeObject]]`, when the callee is a method.
    pub home_obj: Option<Object>,
    /// P7 7.2 instrumentation: speculative int fast paths that bailed to their
    /// helper during this activation. `invoke_native` folds it into
    /// `JitMetrics::specialized_guard_misses` after the call returns; counting
    /// per activation (not per function) keeps recursive calls from
    /// double-counting.
    pub guard_misses: u32,
}

/// The compiled entry. Returns a [`JitExitKind`] as a `u32`.
pub(crate) type NativeEntry = unsafe extern "C" fn(*mut JitExecFrame) -> u32;

/// `imm2` value the speculative `add_loc`/`inc_loc`/`dec_loc` slow paths write
/// before calling their helper, so the helper knows this call is a guard miss
/// (P7 7.2) and not the ordinary helper path. Chosen out of range of the other
/// `imm2` users (`get_field`/`put_field` inline-cache indices).
pub(crate) const SPEC_MISS_IMM2: u32 = u32::MAX;

/// Per-function storage for the installed native entry, living on
/// `BytecodeImpl`. `None` until a compile installs one. It holds a strong `Rc`
/// to its [`CodeSlot`], so the mapping lives exactly as long as a function that
/// can call it exists and is freed when the last `BytecodeImpl` is collected
/// (D-16). Written through `&Bytecode` (interior mutability); all access happens
/// on the single runtime thread.
///
/// [`CodeSlot`]: crate::jit::code_cache::CodeSlot
pub(crate) struct FunctionSlot {
    slot: std::cell::UnsafeCell<Option<std::rc::Rc<crate::jit::code_cache::CodeSlot>>>,
}

impl Default for FunctionSlot {
    fn default() -> Self {
        Self {
            slot: std::cell::UnsafeCell::new(None),
        }
    }
}

impl FunctionSlot {
    /// The installed entry, or `None` while the function is interpreted (or
    /// after the cache evicted its mapping). The returned pointer stays valid
    /// for as long as this slot holds the code.
    #[inline]
    pub(crate) fn entry(&self) -> Option<NativeEntry> {
        // SAFETY: single-threaded; a short read with no aliasing reference held
        // across it (the caller copies the entry pointer out immediately).
        let slot = unsafe { &*self.slot.get() }.as_ref()?;
        let code = slot.entry()?;
        // SAFETY: `code` is the start of code produced by `compiler::compile`
        // for the same `NativeEntry` ABI.
        Some(unsafe { std::mem::transmute::<*const u8, NativeEntry>(code) })
    }

    /// Publish `slot` (whose code is already executable). `poll` is the only
    /// caller and runs between native activations, so no in-flight call
    /// observes the swap.
    pub(crate) fn install(&self, slot: std::rc::Rc<crate::jit::code_cache::CodeSlot>) {
        // SAFETY: single-threaded; see the type's note.
        unsafe { *self.slot.get() = Some(slot) };
    }
}

#[inline]
unsafe fn scope_of<'a>(f: &JitExecFrame) -> &'a mut ScopedContext<'static> {
    unsafe { &mut *(f.scope as *mut ScopedContext<'static>) }
}

/// Binary arithmetic: `sp[-2], sp[-1] -> sp[-2]`, `sp -= 1`.
unsafe fn binary(
    f: *mut JitExecFrame,
    op: fn(&mut ScopedContext, JSValue, JSValue) -> Result<JSValue, i32>,
) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        let a = *sp.offset(-2);
        let b = *sp.offset(-1);
        match op(scope, a, b) {
            Ok(value) => {
                *sp.offset(-2) = value;
                f.sp = sp.offset(-1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// Bitwise/shift operators, reusing the interpreter's `op_binary_logic` (which
/// also handles BigInt and mixed-type throws).
unsafe fn logic(f: *mut JitExecFrame, opcode: OpCode) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        match op_binary_logic(scope, opcode, f.sp) {
            Ok(new_sp) => {
                f.sp = new_sp;
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

macro_rules! binary_wrapper {
    ($name:ident, $op:path) => {
        unsafe extern "C" fn $name(f: *mut JitExecFrame) -> u32 {
            unsafe { binary(f, $op) }
        }
    };
}

binary_wrapper!(h_add, JSValue::arith_add);
binary_wrapper!(h_sub, JSValue::arith_sub);
binary_wrapper!(h_mul, JSValue::arith_mul);
binary_wrapper!(h_div, JSValue::arith_div);
binary_wrapper!(h_mod, JSValue::arith_mod);

unsafe extern "C" fn h_bit_shl(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::SHL) }
}
unsafe extern "C" fn h_bit_shr(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::SHR) }
}
unsafe extern "C" fn h_bit_sar(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::SAR) }
}
unsafe extern "C" fn h_bit_and(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::AND) }
}
unsafe extern "C" fn h_bit_or(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::OR) }
}
unsafe extern "C" fn h_bit_xor(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::XOR) }
}

/// Relational compare, preserving the interpreter's `(rel, opcode)` table.
unsafe fn compare(f: *mut JitExecFrame, kind: u8) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        let a = *sp.offset(-2);
        let b = *sp.offset(-1);
        match a.arith_cmp(scope, b) {
            Ok(rel) => {
                use std::cmp::Ordering;
                let result = matches!(
                    (rel, kind),
                    (Some(Ordering::Less), 0 | 1)
                        | (Some(Ordering::Greater), 2 | 3)
                        | (Some(Ordering::Equal), 1 | 3)
                );
                *sp.offset(-2) = JSValue::from(result);
                f.sp = sp.offset(-1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

macro_rules! cmp_wrapper {
    ($name:ident, $kind:expr) => {
        unsafe extern "C" fn $name(f: *mut JitExecFrame) -> u32 {
            unsafe { compare(f, $kind) }
        }
    };
}
// 0 = lt, 1 = lte, 2 = gt, 3 = gte
cmp_wrapper!(h_lt, 0);
cmp_wrapper!(h_lte, 1);
cmp_wrapper!(h_gt, 2);
cmp_wrapper!(h_gte, 3);

unsafe fn loose_eq(f: *mut JitExecFrame, negate: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        let a = *sp.offset(-2);
        let b = *sp.offset(-1);
        match a.arith_eq(scope, b) {
            Ok(value) => {
                *sp.offset(-2) = JSValue::from(value ^ negate);
                f.sp = sp.offset(-1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}
unsafe extern "C" fn h_eq(f: *mut JitExecFrame) -> u32 {
    unsafe { loose_eq(f, false) }
}
unsafe extern "C" fn h_neq(f: *mut JitExecFrame) -> u32 {
    unsafe { loose_eq(f, true) }
}

unsafe fn strict_eq(f: *mut JitExecFrame, negate: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let result = (*sp.offset(-2)).strict_eq(*sp.offset(-1)) ^ negate;
        *sp.offset(-2) = JSValue::from(result);
        f.sp = sp.offset(-1);
        HELPER_OK
    }
}
unsafe extern "C" fn h_strict_eq(f: *mut JitExecFrame) -> u32 {
    unsafe { strict_eq(f, false) }
}
unsafe extern "C" fn h_strict_neq(f: *mut JitExecFrame) -> u32 {
    unsafe { strict_eq(f, true) }
}

/// Unary `+ - ++ --`, `sp[-1] -> sp[-1]`.
unsafe fn unary(f: *mut JitExecFrame, kind: u8) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        let value = *sp.offset(-1);
        // Mirrors the interpreter's `PLUS` fast paths before `UnaryOp::new`.
        let result = match kind {
            0 if value.is_number() => Ok(value),
            _ => UnaryOp::new(scope, value, true).and_then(|unary| match kind {
                0 => unary.plus(scope),
                1 => unary.neg(scope),
                2 => unary.inc(scope),
                _ => unary.dec(scope),
            }),
        };
        match result {
            Ok(value) => {
                *sp.offset(-1) = value;
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}
macro_rules! unary_wrapper {
    ($name:ident, $kind:expr) => {
        unsafe extern "C" fn $name(f: *mut JitExecFrame) -> u32 {
            unsafe { unary(f, $kind) }
        }
    };
}
// 0 = plus, 1 = neg, 2 = inc, 3 = dec
unary_wrapper!(h_plus, 0);
unary_wrapper!(h_neg, 1);
unary_wrapper!(h_inc, 2);
unary_wrapper!(h_dec, 3);

unsafe extern "C" fn h_lnot(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let value = *sp.offset(-1);
        let truthy = if value.is_int_or_bool() {
            value.payload_i32() != 0
        } else {
            match value.to_bool() {
                Ok(b) => b,
                Err(_) => return HELPER_EXCEPTION,
            }
        };
        *sp.offset(-1) = JSValue::from(!truthy);
        HELPER_OK
    }
}

unsafe extern "C" fn h_not(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        match UnaryOp::new_int(scope, *sp.offset(-1)).and_then(|unary| unary.not(scope)) {
            Ok(value) => {
                *sp.offset(-1) = value;
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

unsafe extern "C" fn h_typeof(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        let atom = (*sp.offset(-1)).type_of();
        match atom.force_string(scope) {
            Ok(sref) => {
                *sp.offset(-1) = JSValue::make_string(sref);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `if_true`/`if_false` slow path: `to_bool(value)`. Returns 0/1, or -1 when
/// ToBoolean fails (which only happens on a corrupt value).
pub(crate) unsafe extern "C" fn h_to_bool(_f: *mut JitExecFrame, value: JSValue) -> i64 {
    if value.is_int_or_bool() {
        return (value.payload_i32() != 0) as i64;
    }
    if value.is_undefined_or_null() {
        return 0;
    }
    match value.to_bool() {
        Ok(b) => b as i64,
        Err(_) => -1,
    }
}

/// `push_atom_value`: push `Atom::from_id(f.imm).to_value()`, matching the
/// interpreter (which pushes the `EXCEPTION` sentinel and continues on failure).
unsafe extern "C" fn h_push_atom(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let value = crate::atom::Atom::from_id(f.imm)
            .to_value(scope)
            .unwrap_or(JSValue::EXCEPTION);
        *f.sp = value;
        f.sp = f.sp.add(1);
        HELPER_OK
    }
}

/// P7 7.2 instrumentation: count one speculative guard miss when the frame
/// marks this helper call as the bail of an `int` fast path. The count lives in
/// the activation, so `invoke_native` can fold it without double-counting
/// recursive calls.
unsafe fn note_spec_miss(f: &mut JitExecFrame) {
    if f.imm2 == SPEC_MISS_IMM2 {
        f.guard_misses = f.guard_misses.saturating_add(1);
    }
}

/// `inc_loc` / `dec_loc`: unary update of local `f.imm`, no operand-stack change
/// (so an exception exit leaves `sp` exactly as the interpreter would).
unsafe fn local_unary(f: *mut JitExecFrame, inc: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        note_spec_miss(f);
        let scope = scope_of(f);
        let slot = f.var_base.add(f.imm as usize);
        let result = UnaryOp::new(scope, *slot, true).and_then(|unary| {
            if inc {
                unary.inc(scope)
            } else {
                unary.dec(scope)
            }
        });
        match result {
            Ok(value) => {
                *slot = value;
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

unsafe extern "C" fn h_inc_local(f: *mut JitExecFrame) -> u32 {
    unsafe { local_unary(f, true) }
}
unsafe extern "C" fn h_dec_local(f: *mut JitExecFrame) -> u32 {
    unsafe { local_unary(f, false) }
}

// `pow`: the interpreter's `arith_pow` (Int fast paths, float, BigInt).
binary_wrapper!(h_pow, JSValue::arith_pow);

/// `to_object`: objectify a primitive, no-op for an object.
unsafe extern "C" fn h_to_object(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let value = *f.sp.offset(-1);
        if value.is_object() {
            return HELPER_OK;
        }
        match value.objectify(scope) {
            Ok(o) => {
                *f.sp.offset(-1) = JSValue::from(o);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `to_propkey`: ToPropertyKey unless already a string/symbol/int.
unsafe extern "C" fn h_to_propkey(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let key = *f.sp.offset(-1);
        if key.is_string_or_symbol() || key.is_int() {
            return HELPER_OK;
        }
        match key.to_propkey(scope) {
            Ok(pk) => {
                *f.sp.offset(-1) = pk;
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `in`: `[prop, obj, ^] -> [bool, ^]`.
unsafe extern "C" fn h_in(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        match js_operator_in(scope, &*sp.offset(-1), &*sp.offset(-2)) {
            Ok(b) => {
                *sp.offset(-2) = JSValue::from(b);
                f.sp = sp.offset(-1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `instanceof`: `[ctor, val, ^] -> [bool, ^]`.
unsafe extern "C" fn h_instanceof(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        match js_operator_instanceof(scope, sp) {
            Ok(()) => {
                f.sp = sp.offset(-1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `delete`: `[obj, prop, ^] -> [bool, ^]`.
unsafe extern "C" fn h_delete(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        if js_operator_delete(scope, sp) != 0 {
            HELPER_EXCEPTION
        } else {
            f.sp = sp.offset(-1);
            HELPER_OK
        }
    }
}

/// `add_loc`: `[v, ^] -> [^]`, `loc += v` (the operand is popped first, so an
/// error exit leaves `sp` exactly as the interpreter would).
unsafe extern "C" fn h_add_loc(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        note_spec_miss(f);
        let scope = scope_of(f);
        let operand = *f.sp.offset(-1);
        f.sp = f.sp.offset(-1);
        let slot = f.var_base.add(f.imm as usize);
        match JSValue::arith_add(scope, *slot, operand) {
            Ok(value) => {
                *slot = value;
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `post_inc` / `post_dec`: `[v, ^] -> [ToNumber(v), v ± 1, ^]`.
unsafe fn post_inc_dec(f: *mut JitExecFrame, inc: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let operand = *f.sp.offset(-1);
        if operand.is_bigint() {
            let result = UnaryOp::new(scope, operand, true).and_then(|unary| {
                if inc {
                    unary.inc(scope)
                } else {
                    unary.dec(scope)
                }
            });
            return match result {
                Ok(next) => {
                    *f.sp.offset(-1) = operand;
                    *f.sp = next;
                    f.sp = f.sp.add(1);
                    HELPER_OK
                }
                Err(_) => HELPER_EXCEPTION,
            };
        }
        let Ok(number) = operand.to_number(scope) else {
            return HELPER_EXCEPTION;
        };
        *f.sp.offset(-1) = JSValue::from(number);
        match UnaryOp::new(scope, JSValue::from(number), true).and_then(|unary| {
            if inc {
                unary.inc(scope)
            } else {
                unary.dec(scope)
            }
        }) {
            Ok(next) => {
                *f.sp = next;
                f.sp = f.sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

unsafe extern "C" fn h_post_inc(f: *mut JitExecFrame) -> u32 {
    unsafe { post_inc_dec(f, true) }
}
unsafe extern "C" fn h_post_dec(f: *mut JitExecFrame) -> u32 {
    unsafe { post_inc_dec(f, false) }
}

/// `get_var` / `get_var_undef`: push a global binding, reusing the interpreter's
/// `get_global_var`. `throw` is true only for `get_var`.
unsafe fn global_get(f: *mut JitExecFrame, throw: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let name = (*f.bytecode).global_var_refs()[f.imm as usize].var_name;
        match scope.get_global_var(name, throw) {
            Ok(value) => {
                *f.sp = value;
                f.sp = f.sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

unsafe extern "C" fn h_get_var(f: *mut JitExecFrame) -> u32 {
    unsafe { global_get(f, true) }
}
unsafe extern "C" fn h_get_var_undef(f: *mut JitExecFrame) -> u32 {
    unsafe { global_get(f, false) }
}

/// `put_var` / `put_var_init`: `set_global_var` handles TDZ and read-only checks.
unsafe fn global_put(f: *mut JitExecFrame, init: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let name = (*f.bytecode).global_var_refs()[f.imm as usize].var_name;
        let value = *f.sp.offset(-1);
        let mode = if init {
            GlobalVarSetMode::Init
        } else {
            GlobalVarSetMode::Normal
        };
        let result = scope.set_global_var(name, value, mode);
        f.sp = f.sp.offset(-1);
        if result.is_err() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

unsafe extern "C" fn h_put_var(f: *mut JitExecFrame) -> u32 {
    unsafe { global_put(f, false) }
}
unsafe extern "C" fn h_put_var_init(f: *mut JitExecFrame) -> u32 {
    unsafe { global_put(f, true) }
}

/// `get_array_el`: `[array, prop, ^] -> [value, ^]`.
///
/// shortcut: element accesses carry no inline-cache index, so they lean on
/// `get_property_index`'s class/bounds fast path instead of a shape IC;
/// upgrade to a real `InlineCacheSite` when non-fast (dictionary) arrays show
/// up hot — that needs an ic-index operand on GET_ARRAY_EL/PUT_ARRAY_EL.
unsafe extern "C" fn h_get_array_el(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let value = (*f.sp.offset(-2))
            .get_property_value(scope, *f.sp.offset(-1))
            .unwrap_or(JSValue::EXCEPTION);
        let is_err = value.is_exception();
        *f.sp.offset(-2) = value;
        f.sp = f.sp.offset(-1);
        if is_err { HELPER_EXCEPTION } else { HELPER_OK }
    }
}

/// `get_array_el2`: `[array, prop, ^] -> [array, value, ^]`.
unsafe extern "C" fn h_get_array_el2(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let value = (*f.sp.offset(-2))
            .get_property_value(scope, *f.sp.offset(-1))
            .unwrap_or(JSValue::EXCEPTION);
        let is_err = value.is_exception();
        *f.sp.offset(-1) = value;
        if is_err { HELPER_EXCEPTION } else { HELPER_OK }
    }
}

/// `get_array_el3`: `[array, prop, ^] -> [array, prop1, value, ^]`.
unsafe extern "C" fn h_get_array_el3(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let array = *f.sp.offset(-2);
        if !array.is_int() && !array.is_string_or_symbol() {
            if array.is_undefined_or_null() {
                let _ = scope.throw_type_error("value has no property ");
                return HELPER_EXCEPTION;
            }
            let Ok(prop1) = (*f.sp.offset(-1)).to_propkey(scope) else {
                return HELPER_EXCEPTION;
            };
            *f.sp.offset(-1) = prop1;
        }
        let value = array
            .get_property_value(scope, *f.sp.offset(-1))
            .unwrap_or(JSValue::EXCEPTION);
        let is_err = value.is_exception();
        *f.sp = value;
        f.sp = f.sp.add(1);
        if is_err { HELPER_EXCEPTION } else { HELPER_OK }
    }
}

/// `put_array_el`: `[arr, prop, value, ^] -> [^]`.
unsafe extern "C" fn h_put_array_el(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let mut array = *f.sp.offset(-3);
        let prop = *f.sp.offset(-2);
        let value = *f.sp.offset(-1);
        let result = array.set_property_value(scope, prop, value, ThrowPolicy::StrictOnly, false);
        f.sp = f.sp.offset(-3);
        if result.is_err() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// Write a `get_field`/`get_field2` result: replace the target, or push (for
/// `get_field2`, which keeps the target).
fn finish_field_get(f: &mut JitExecFrame, value: JSValue, push: bool) -> u32 {
    if push {
        unsafe { *f.sp = value };
        f.sp = unsafe { f.sp.add(1) };
    } else {
        unsafe { *f.sp.offset(-1) = value };
    }
    HELPER_OK
}

/// `get_field` / `get_field2`: the interpreter's inline-cache fast path, then
/// the full lookup that also refreshes the cache. Kept out of generated code so
/// the guard sequence can never drift from `exec_stackless`.
unsafe fn field_get(f: *mut JitExecFrame, push: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let target = *f.sp.offset(-1);
        let atom = crate::atom::Atom::from_id(f.imm);

        #[cfg(feature = "inline_cache")]
        {
            let ic_idx = f.imm2 as usize;
            let cache = &*(*f.bytecode).inline_cache.get();
            if ic_idx < cache.len()
                && let Some(o) = target.as_object()
                && let Some(shape) = o.shape()
            {
                let shape_ptr = shape.gc_ref().as_ptr().as_ptr();
                // The site holds up to IC_WAYS shapes (epic F3).
                if let Some((slot, _)) = cache[ic_idx].find(shape_ptr, shape.uid) {
                    let slot = slot as usize;
                    if !matches!(o.prop_slot(slot), JSProperty::Deleted) {
                        return match o.prop_slot(slot).value(scope, o.into()) {
                            Ok(value) => finish_field_get(f, value, push),
                            Err(_) => HELPER_EXCEPTION,
                        };
                    }
                }
            }
        }

        match target.get_property(scope, atom) {
            Ok(value) => {
                #[cfg(feature = "inline_cache")]
                {
                    let ic_idx = f.imm2 as usize;
                    if let Some(o) = target.as_object()
                        && let Some(shape) = o.shape()
                        && let Some((idx, flags)) = o.find_shape_property(atom)
                    {
                        let cache = &mut *(*f.bytecode).inline_cache.get();
                        if ic_idx < cache.len() {
                            cache[ic_idx].insert(
                                shape.gc_ref().as_ptr().as_ptr(),
                                shape.uid,
                                idx as u32,
                                flags as u8,
                            );
                        }
                    }
                }
                finish_field_get(f, value, push)
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

unsafe extern "C" fn h_get_field(f: *mut JitExecFrame) -> u32 {
    unsafe { field_get(f, false) }
}
unsafe extern "C" fn h_get_field2(f: *mut JitExecFrame) -> u32 {
    unsafe { field_get(f, true) }
}

/// `put_field`: cached direct slot write when the shape and flags allow it,
/// otherwise the full property set (which refreshes the cache).
unsafe extern "C" fn h_put_field(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let target = *f.sp.offset(-2);
        let value = *f.sp.offset(-1);
        let atom = crate::atom::Atom::from_id(f.imm);

        #[cfg(feature = "inline_cache")]
        {
            let ic_idx = f.imm2 as usize;
            let cache = &*(*f.bytecode).inline_cache.get();
            if ic_idx < cache.len()
                && let Some(o) = target.as_object()
                && let Some(shape) = o.shape()
            {
                let shape_ptr = shape.gc_ref().as_ptr().as_ptr();
                if let Some((slot, cached_flags)) = cache[ic_idx].find(shape_ptr, shape.uid) {
                    let slot = slot as usize;
                    if !matches!(o.prop_slot(slot), JSProperty::Deleted) {
                        let flags = cached_flags as u32;
                        if flags & crate::object::JS_PROP_TMASK == crate::object::JS_PROP_NORMAL
                            && flags & crate::object::JS_PROP_WRITABLE != 0
                            && flags & crate::object::JS_PROP_LENGTH == 0
                        {
                            let mut o = target.as_object_unchecked();
                            o.set_prop_slot(slot, JSProperty::Normal(value));
                            f.sp = f.sp.offset(-2);
                            return HELPER_OK;
                        }
                    }
                }
            }
        }

        let result =
            target.set_property_internal(scope, atom, value, None, ThrowPolicy::StrictOnly, false);
        f.sp = f.sp.offset(-2);
        if result.is_err() {
            return HELPER_EXCEPTION;
        }

        #[cfg(feature = "inline_cache")]
        if let Some(o) = target.as_object()
            && let Some(shape) = o.shape()
            && let Some((idx, flags)) = o.find_shape_property(atom)
        {
            let ic_idx = f.imm2 as usize;
            let cache = &mut *(*f.bytecode).inline_cache.get();
            if ic_idx < cache.len() {
                cache[ic_idx].insert(
                    shape.gc_ref().as_ptr().as_ptr(),
                    shape.uid,
                    idx as u32,
                    flags as u8,
                );
            }
        }

        HELPER_OK
    }
}

/// `nip_catch`: `[catch_offset, …, value, ^] -> […, value, ^]`; unwinds the
/// operand stack down to the innermost `CatchOffset` and writes `value` there.
unsafe extern "C" fn h_nip_catch(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp_base = f.sp_base;
        let mut sp = f.sp.offset(-1);
        let value = *sp;
        while sp > sp_base && !(*sp.offset(-1)).is_catch_offset() {
            sp = sp.offset(-1);
        }
        if sp == sp_base {
            let _ = scope.throw_internal_error("nip_catch");
            HELPER_EXCEPTION
        } else {
            *sp.offset(-1) = value;
            f.sp = sp;
            HELPER_OK
        }
    }
}

/// `rest`: build the rest-parameter array from the *actual* arguments, exactly
/// like the interpreter's `REST` (`args[start..]`, `f.imm` is `start`). The
/// frame's `arg_base` is the buffer start the interpreter's own `args` slice
/// points at, so a short call yields an empty array and never the `undefined`
/// padding up to the declared count.
unsafe extern "C" fn h_rest(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let start = f.imm as usize;
        let real = f.arg_real_count as usize;
        let values = if start < real {
            std::slice::from_raw_parts(f.arg_base, real)[start..].to_vec()
        } else {
            Vec::new()
        };
        let value = match crate::array::Array::from_values(scope, values) {
            Ok(arr) => JSValue::from(arr),
            Err(_) => JSValue::EXCEPTION,
        };
        *f.sp = value;
        f.sp = f.sp.add(1);
        if value.is_exception() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `special_object`: the frame-entry special objects the parser emits. Mirrors
/// the interpreter's arm for every kind except `ImportMeta` (module state,
/// which the verifier keeps rejecting). `imm` is the `SpecialObject` id.
unsafe extern "C" fn h_special_object(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let args = std::slice::from_raw_parts(f.arg_base, f.arg_real_count as usize);
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        let func_obj = (*f.arena).frame(frame_idx).func_obj;
        let value = match std::mem::transmute::<u8, SpecialObject>(f.imm as u8) {
            SpecialObject::Arguments => js_build_arguments(scope, args),
            SpecialObject::MappedArguments => {
                match js_build_mapped_arguments(
                    scope,
                    args,
                    f.arg_base,
                    f.var_base,
                    &mut *f.arena,
                    frame_idx,
                    (*f.bytecode).arg_count,
                    (*f.bytecode).var_count,
                    func_obj,
                ) {
                    Some(obj) => JSValue::from(obj),
                    // Like the interpreter's arm: the failure pushes nothing.
                    None => return HELPER_EXCEPTION,
                }
            }
            SpecialObject::ThisFunc => JSValue::from(func_obj),
            SpecialObject::NewTarget => f.new_target,
            SpecialObject::HomeObject => f.home_obj.map_or(JSValue::UNDEFINED, JSValue::from),
            SpecialObject::VarObject => scope
                .create_base_object_with_proto(None)
                .map_or(JSValue::EXCEPTION, JSValue::from),
            SpecialObject::ImportMeta => {
                let _ = scope.throw_internal_error("special_object");
                JSValue::EXCEPTION
            }
        };
        *f.sp = value;
        f.sp = f.sp.add(1);
        if value.is_exception() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `push_this` for a sloppy function: `undefined`/`null` becomes the global
/// object and any other primitive is objectified (the interpreter's arm).
/// Strict functions never reach this — their `this` is the frame's raw value,
/// which the compiler pushes inline.
unsafe extern "C" fn h_push_this(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let this_obj = f.this;
        let value = if this_obj.is_undefined_or_null() {
            JSValue::from(scope.global_obj())
        } else if this_obj.is_object() {
            this_obj
        } else {
            let Ok(objectified) = this_obj.objectify(scope) else {
                // The interpreter breaks before `SP_PUT!` on this path too.
                return HELPER_EXCEPTION;
            };
            objectified.into()
        };
        *f.sp = value;
        f.sp = f.sp.add(1);
        HELPER_OK
    }
}

/// `fclosure`: close over the constant pool's nested bytecode. The captured
/// var refs come from the *current* frame's function object, exactly as the
/// interpreter resolves them (the nested bytecode's own free variables bind to
/// the enclosing activation). `imm` is the constant index.
unsafe extern "C" fn h_fclosure(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let nested = (*f.cpools.add(f.imm as usize)).as_bytecode_unchecked();
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        let func_obj = (*f.arena).frame(frame_idx).func_obj;
        let mut bfun = BCWrapper::try_from(JSValue::from(func_obj)).ok();
        let captured: &mut [VarRef] = match bfun.as_mut() {
            Some(bfun) => &mut *bfun.payload_mut().captured_vars_mut(),
            None => &mut [],
        };
        let value = nested
            .make_closure_for_arena(scope, None, captured, &mut *f.arena, frame_idx)
            .map_or(JSValue::EXCEPTION, Into::into);
        *f.sp = value;
        f.sp = f.sp.add(1);
        if value.is_exception() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// The current frame's captured var refs, resolved from its func_obj exactly
/// as the interpreter does (each function in the call chain owns its own
/// `captured_vars`).
unsafe fn captured_vars_of<'a>(f: &'a JitExecFrame) -> &'a mut [VarRef] {
    unsafe {
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        let func_obj = (*f.arena).frame(frame_idx).func_obj;
        match BCWrapper::try_from(JSValue::from(func_obj)) {
            // `bfun` is a Copy `GcRef`, so the payload borrows GC memory that
            // outlives this call; launder it to reuse the caller's lifetime.
            Ok(mut bfun) => {
                let cv: *mut [VarRef] = bfun.payload_mut().captured_vars_mut();
                &mut *cv
            }
            Err(_) => &mut [],
        }
    }
}

/// The running function's bytecode, for the rare helper that must name a
/// binding (the uninitialized-var_ref error message).
unsafe fn current_bytecode(f: &JitExecFrame) -> Option<crate::function::Bytecode> {
    let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
    let func_obj = unsafe { (*f.arena).frame(frame_idx).func_obj };
    BCWrapper::try_from(JSValue::from(func_obj))
        .ok()
        .map(|bfun| bfun.payload().bytecode())
}

/// `get_var_ref`: push a captured binding. `imm` is the var_ref index.
unsafe extern "C" fn h_get_var_ref(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let value = captured_vars_of(f)[f.imm as usize].value();
        *f.sp = value;
        f.sp = f.sp.add(1);
        HELPER_OK
    }
}

/// `put_var_ref`: write the top of stack back and pop it.
unsafe extern "C" fn h_put_var_ref(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let value = *f.sp.offset(-1);
        let mut rt = scope_of(f).rt_ptr();
        captured_vars_of(f)[f.imm as usize].set_value(&mut rt, value);
        f.sp = f.sp.offset(-1);
        HELPER_OK
    }
}

/// `set_var_ref`: write the top of stack back, keeping it (assignment result).
unsafe extern "C" fn h_set_var_ref(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let value = *f.sp.offset(-1);
        let mut rt = scope_of(f).rt_ptr();
        captured_vars_of(f)[f.imm as usize].set_value(&mut rt, value);
        HELPER_OK
    }
}

/// `get_var_ref_check`: push a captured binding unless it is still in the TDZ.
/// The error is raised here so the message names the binding, exactly like the
/// interpreter's `throw_reference_error_uninitialized2(_, _, true)`.
unsafe extern "C" fn h_get_var_ref_check(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let idx = f.imm as u16;
        let value = captured_vars_of(f)[idx as usize].value();
        if value.is_uninitialized() {
            if let Some(b) = current_bytecode(f) {
                let _ = scope_of(f).throw_reference_error_uninitialized2(&b, idx, true);
            }
            return HELPER_EXCEPTION;
        }
        *f.sp = value;
        f.sp = f.sp.add(1);
        HELPER_OK
    }
}

/// `put_var_ref_check`: TDZ-checked `put_var_ref`.
unsafe extern "C" fn h_put_var_ref_check(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let idx = f.imm as u16;
        if captured_vars_of(f)[idx as usize].value().is_uninitialized() {
            if let Some(b) = current_bytecode(f) {
                let _ = scope_of(f).throw_reference_error_uninitialized2(&b, idx, true);
            }
            return HELPER_EXCEPTION;
        }
        let value = *f.sp.offset(-1);
        let mut rt = scope_of(f).rt_ptr();
        captured_vars_of(f)[idx as usize].set_value(&mut rt, value);
        f.sp = f.sp.offset(-1);
        HELPER_OK
    }
}

/// `put_var_ref_check_init`: initialize a captured `this` binding, which must
/// still be in the TDZ (re-initialization is a ReferenceError).
unsafe extern "C" fn h_put_var_ref_check_init(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let idx = f.imm as u16;
        if !captured_vars_of(f)[idx as usize].value().is_uninitialized() {
            if let Some(b) = current_bytecode(f) {
                let _ = scope_of(f).throw_reference_error_uninitialized2(&b, idx, true);
            }
            return HELPER_EXCEPTION;
        }
        let value = *f.sp.offset(-1);
        let mut rt = scope_of(f).rt_ptr();
        captured_vars_of(f)[idx as usize].set_value(&mut rt, value);
        f.sp = f.sp.offset(-1);
        HELPER_OK
    }
}

/// Loop back edge: deliver interrupts and run a deferred collection. Mirrors
/// the interpreter's per-opcode safepoint.
unsafe extern "C" fn h_safepoint(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        if scope.poll_interrupts() {
            return HELPER_INTERRUPT;
        }
        let mut rt = scope.rt_ptr();
        if rt.gc_req {
            // GcScopeKind::None == 2.
            if f.gc_scope_kind != 2 {
                scope.clear_locals();
            }
            rt.gc_collect();
        }
        HELPER_OK
    }
}

/// `set_name`: name the function on top of the stack (`[obj, ^] -> [obj, ^]`,
/// a peek — so an exception leaves `sp` exactly as the interpreter would).
/// `imm` is the atom id.
unsafe extern "C" fn h_set_name(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let atom = crate::atom::Atom::from_id(f.imm);
        let name = (*f.sp.offset(-1))
            .as_object()
            .ok_or(-1)
            .and_then(|mut o| o.define_name(scope, atom, crate::object::JS_PROP_CONFIGURABLE));
        if name.is_err() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `for_of_start`: `[obj, ^] -> [iter_obj, next, catch_offset(0), ^]`. The zero
/// catch offset is the sentinel `find_catch_in_frame` later uses to close the
/// iterator on an abrupt exit, so it must be pushed exactly like the
/// interpreter pushes it.
unsafe extern "C" fn h_for_of_start(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        let opened = (*sp.offset(-1))
            .get_iterator_must(scope, false)
            .and_then(|o| {
                o.get_property(scope, crate::atom::Atom::CONST_next)
                    .map(|next| (o, next))
            });
        match opened {
            Ok((iter, next)) => {
                *sp.offset(-1) = JSValue::from(iter);
                *sp = next;
                *sp.offset(1) = JSValue::make_catch_offset(0);
                f.sp = sp.offset(2);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `for_of_next`: calls `next()` on the iterator at `sp[imm]`, pushes
/// `[value, done]`. Mirrors `js_for_of_next` (which also clears the iterator
/// slot on failure so the unwinder does not close it twice).
unsafe extern "C" fn h_for_of_next(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let sp = f.sp;
        if crate::bytecode::operators::js_for_of_next(scope, sp, -3 - f.imm as i32).is_err() {
            return HELPER_EXCEPTION;
        }
        f.sp = sp.add(2);
        HELPER_OK
    }
}

/// `for_in_start`: `[obj, ^] -> [enum_obj, ^]`.
unsafe extern "C" fn h_for_in_start(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        if crate::bytecode::operators::js_for_in_start(scope_of(f), f.sp).is_err() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `for_in_next`: `[enum_obj, ^] -> [enum_obj, value, done, ^]`.
unsafe extern "C" fn h_for_in_next(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        if crate::bytecode::operators::js_for_in_next(scope_of(f), f.sp).is_err() {
            HELPER_EXCEPTION
        } else {
            f.sp = f.sp.add(2);
            HELPER_OK
        }
    }
}

/// `iterator_check_object`: the value on top must be an object.
unsafe extern "C" fn h_iterator_check_object(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        if (*f.sp.offset(-1)).is_object() {
            HELPER_OK
        } else {
            let _ = scope_of(f).throw_type_error("iterator must return an object");
            HELPER_EXCEPTION
        }
    }
}

/// `iterator_get_value_done`: `[iter_result, ^] -> [value, done, ^]`.
unsafe extern "C" fn h_iterator_get_value_done(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        if crate::bytecode::operators::js_iterator_get_value_done(scope_of(f), f.sp) != 0 {
            HELPER_EXCEPTION
        } else {
            f.sp = f.sp.add(1);
            HELPER_OK
        }
    }
}

/// `iterator_close`: the lowering already dropped `next` and the catch offset,
/// so `sp[-1]` is the iterator. Calls its `return` method like the interpreter.
unsafe extern "C" fn h_iterator_close(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let target = *f.sp.offset(-1);
        if let Some(o) = target.as_object()
            && crate::builtins::iterator::ObjectIter::close_iter_internal(scope_of(f), o).is_err()
        {
            return HELPER_EXCEPTION;
        }
        f.sp = f.sp.offset(-1);
        HELPER_OK
    }
}

/// `iterator_next`: `[iter_obj, next, catch_offset, value, ^] -> [.., result, ^]`.
unsafe extern "C" fn h_iterator_next(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let result = (*sp.offset(-3)).call(scope_of(f), *sp.offset(-4), &[*sp.offset(-1)]);
        match result {
            Ok(value) => {
                *sp.offset(-1) = value;
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `iterator_call`: close (`return`) or forward the throw of the iterator at
/// `sp[-4]`. `imm & 1` selects `throw` instead of `return`, `imm & 2` means no
/// argument. Pushes the returned flag.
unsafe extern "C" fn h_iterator_call(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let flags = f.imm as u8;
        let scope = scope_of(f);
        let atom = if flags & 1 != 0 {
            crate::atom::Atom::CONST_throw
        } else {
            crate::atom::Atom::CONST_return
        };
        let Ok(method) = (*sp.offset(-4)).get_property(scope, atom) else {
            return HELPER_EXCEPTION;
        };
        let ret_flag = if method.is_undefined_or_null() {
            true
        } else {
            let args: &[JSValue] = if flags & 2 != 0 {
                &[]
            } else {
                &[*sp.offset(-1)]
            };
            match method.call(scope, *sp.offset(-4), args) {
                Ok(value) => {
                    *sp.offset(-1) = value;
                    false
                }
                Err(_) => return HELPER_EXCEPTION,
            }
        };
        *sp = JSValue::from(ret_flag);
        f.sp = sp.add(1);
        HELPER_OK
    }
}

/// Result of a `class_ops` call: the helper only needs OK/EXCEPTION.
#[inline]
fn status(result: Result<(), i32>) -> u32 {
    if result.is_ok() {
        HELPER_OK
    } else {
        HELPER_EXCEPTION
    }
}

/// `define_field`: `[obj, value, ^] -> [obj, ^]` (`imm` = atom id).
unsafe extern "C" fn h_define_field(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let atom = crate::atom::Atom::from_id(f.imm);
        let r = crate::bytecode::class_ops::define_field(
            scope_of(f),
            &mut *sp.offset(-2),
            atom,
            *sp.offset(-1),
        );
        f.sp = sp.offset(-1);
        status(r)
    }
}

/// `set_name_computed`: `[name, value, ^] -> [name, value, ^]` (peek).
unsafe extern "C" fn h_set_name_computed(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        status(crate::bytecode::class_ops::set_name_computed(
            scope_of(f),
            *sp.offset(-2),
            &mut *sp.offset(-1),
        ))
    }
}

/// `set_proto`: `[obj, proto, ^] -> [obj, ^]`.
unsafe extern "C" fn h_set_proto(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r =
            crate::bytecode::class_ops::set_proto(scope_of(f), &mut *sp.offset(-2), *sp.offset(-1));
        if r.is_err() {
            return HELPER_EXCEPTION;
        }
        f.sp = sp.offset(-1);
        HELPER_OK
    }
}

/// `set_home_object`: `[home_obj, func, ^] -> [home_obj, func, ^]` (peek).
unsafe extern "C" fn h_set_home_object(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        crate::bytecode::class_ops::set_home_object(&mut *sp.offset(-1), *sp.offset(-2));
        HELPER_OK
    }
}

/// `define_array_el`: `[obj, prop, value, ^] -> [obj, prop, ^]`.
unsafe extern "C" fn h_define_array_el(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::define_array_el(
            scope_of(f),
            &mut *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        );
        f.sp = sp.offset(-1);
        status(r)
    }
}

/// `copy_data_properties`: three operands in place (`imm` = mask).
unsafe extern "C" fn h_copy_data_properties(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let mask = f.imm as i32;
        let target = -1 - (mask & 3);
        let source = -1 - ((mask >> 2) & 7);
        let exclude = -1 - ((mask >> 5) & 7);
        let r = crate::bytecode::class_ops::copy_data_properties(
            scope_of(f),
            &mut *sp.offset(target as isize),
            *sp.offset(source as isize),
            *sp.offset(exclude as isize),
        );
        status(r)
    }
}

/// `define_method`: `[obj, func, ^] -> [obj, ^]` (`imm` = atom, `imm2` = flags).
unsafe extern "C" fn h_define_method(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::define_method(
            scope_of(f),
            &mut *sp.offset(-2),
            *sp.offset(-1),
            crate::atom::Atom::from_id(f.imm),
            JSValue::UNDEFINED,
            f.imm2 as u8,
        );
        f.sp = sp.offset(-1);
        status(r)
    }
}

/// `define_method_computed`: `[obj, name, func, ^] -> [obj, ^]`.
unsafe extern "C" fn h_define_method_computed(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::define_method(
            scope_of(f),
            &mut *sp.offset(-3),
            *sp.offset(-1),
            crate::atom::Atom::NULL,
            *sp.offset(-2),
            f.imm as u8,
        );
        f.sp = sp.offset(-2);
        status(r)
    }
}

/// `define_class` / `define_class_computed` (`imm` = atom, `imm2` = flags).
unsafe fn define_class(f: *mut JitExecFrame, is_computed: bool) -> u32 {
    unsafe {
        let f = &mut *f;
        let arena = f.arena;
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        let sp = f.sp;
        let captured = captured_vars_of(f);
        let r = crate::bytecode::operators::js_op_define_class_for_arena(
            scope_of(f),
            sp,
            crate::atom::Atom::from_id(f.imm),
            f.imm2 as i32,
            captured,
            &mut *arena,
            frame_idx,
            is_computed,
        );
        status(r)
    }
}

/// `define_class`.
unsafe extern "C" fn h_define_class(f: *mut JitExecFrame) -> u32 {
    unsafe { define_class(f, false) }
}

/// `define_class_computed`.
unsafe extern "C" fn h_define_class_computed(f: *mut JitExecFrame) -> u32 {
    unsafe { define_class(f, true) }
}

/// `check_ctor_return`: `[ret_val, ^] -> [ret_val, bool, ^]`.
unsafe extern "C" fn h_check_ctor_return(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        match crate::bytecode::class_ops::check_ctor_return(
            scope_of(f),
            *sp.offset(-1),
            f.arena,
            FrameIndex::from_u32(f.cur_frame_idx),
        ) {
            Ok(flag) => {
                *sp = JSValue::from(flag);
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `check_ctor`.
unsafe extern "C" fn h_check_ctor(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        status(crate::bytecode::class_ops::check_ctor(
            scope_of(f),
            f.new_target,
        ))
    }
}

/// `init_ctor`: `[^] -> [this, ^]`.
unsafe extern "C" fn h_init_ctor(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        let func_obj = (*f.arena).frame(frame_idx).func_obj;
        let args = std::slice::from_raw_parts(f.arg_base, f.arg_real_count as usize);
        let sp = f.sp;
        match crate::bytecode::class_ops::init_ctor(scope_of(f), func_obj, f.new_target, args) {
            Ok(val) => {
                *sp = val;
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `check_brand`: `[obj, brand, ^] -> [obj, brand, ^]` (peek + throw).
unsafe extern "C" fn h_check_brand(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        status(crate::bytecode::class_ops::check_brand(
            scope_of(f),
            *sp.offset(-2),
            *sp.offset(-1),
        ))
    }
}

/// `add_brand`: `[func, home_obj, ^] -> [^]`.
unsafe extern "C" fn h_add_brand(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::add_brand(scope_of(f), *sp.offset(-2), *sp.offset(-1));
        f.sp = sp.offset(-2);
        status(r)
    }
}

/// `private_symbol`: `[^] -> [symbol, ^]` (`imm` = atom id).
unsafe extern "C" fn h_private_symbol(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let atom = crate::atom::Atom::from_id(f.imm);
        match crate::bytecode::class_ops::private_symbol(scope_of(f), atom) {
            Ok(val) => {
                *sp = val;
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `get_private_field`: `[obj, name, ^] -> [value, ^]`.
unsafe extern "C" fn h_get_private_field(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let val = crate::bytecode::class_ops::get_private_field(
            scope_of(f),
            *sp.offset(-2),
            *sp.offset(-1),
        )
        .unwrap_or(JSValue::EXCEPTION);
        *sp.offset(-2) = val;
        f.sp = sp.offset(-1);
        if val.is_exception() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `put_private_field`: `[obj, name, value, ^] -> [^]`.
unsafe extern "C" fn h_put_private_field(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::put_private_field(
            scope_of(f),
            &mut *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        );
        f.sp = sp.offset(-3);
        status(r)
    }
}

/// `define_private_field`: `[obj, name, value, ^] -> [obj, ^]`.
unsafe extern "C" fn h_define_private_field(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::define_private_field(
            scope_of(f),
            &mut *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        );
        f.sp = sp.offset(-2);
        status(r)
    }
}

/// `append`: `[dest, start, src, ^] -> [dest, end, ^]`.
unsafe extern "C" fn h_append(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        if crate::bytecode::operators::js_append_enumerate(scope_of(f), sp).is_err() {
            return HELPER_EXCEPTION;
        }
        f.sp = sp.offset(-1);
        HELPER_OK
    }
}

/// `private_in`: `[obj, field, ^] -> [bool, ^]`.
unsafe extern "C" fn h_private_in(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        if crate::bytecode::operators::js_operator_private_in(scope_of(f), sp).is_err() {
            return HELPER_EXCEPTION;
        }
        f.sp = sp.offset(-1);
        HELPER_OK
    }
}

/// `get_super`: `[obj, ^] -> [obj.prototype, ^]`.
unsafe extern "C" fn h_get_super(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        match crate::bytecode::class_ops::get_super(scope_of(f), *sp.offset(-1)) {
            Ok(proto) => {
                *sp.offset(-1) = proto;
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `get_super_value`: `[this, obj, prop, ^] -> [value, ^]`.
unsafe extern "C" fn h_get_super_value(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        match crate::bytecode::class_ops::get_super_value(
            scope_of(f),
            *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        ) {
            Ok(val) => {
                *sp.offset(-3) = val;
                f.sp = sp.offset(-2);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `put_super_value`: `[this, obj, prop, val, ^] -> [^]`.
unsafe extern "C" fn h_put_super_value(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::class_ops::put_super_value(
            scope_of(f),
            *sp.offset(-4),
            *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        );
        f.sp = sp.offset(-4);
        status(r)
    }
}

/// `object`: `[^] -> [obj, ^]`, an empty base object (the seed for an object
/// literal, then filled by `define_field`/`define_array_el`/...).
unsafe extern "C" fn h_object(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let value = scope_of(f)
            .create_base_object()
            .map_or(JSValue::EXCEPTION, JSValue::from);
        *f.sp = value;
        f.sp = f.sp.add(1);
        if value.is_exception() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `array_from`: `[v0, …, vN-1, ^] -> [array, ^]`; `imm` is `N`. Mirrors the
/// interpreter's `ARRAY_FROM` (the same fast-array slot allocation).
unsafe extern "C" fn h_array_from(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let scope = scope_of(f);
        let count = f.imm as i32;
        let base = f.sp.offset(-(count as isize));
        let Ok(mut list) = crate::array::alloc_fast_array_slots(scope, count as usize) else {
            return HELPER_EXCEPTION;
        };
        for i in 0..count as usize {
            list.push(*base.add(i));
        }
        let Ok(array) = crate::array::Array::from_values(scope, list) else {
            return HELPER_EXCEPTION;
        };
        f.sp = base;
        *f.sp = JSValue::from(array);
        f.sp = f.sp.add(1);
        HELPER_OK
    }
}

/// `close_loc`: detaches the frame's `var_ref` for local `imm`, so a later
/// write goes through the box instead of the (now dead) frame slot.
unsafe extern "C" fn h_close_loc(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let idx = f.imm as u16;
        let frame_idx = FrameIndex::from_u32(f.cur_frame_idx);
        if let Some(vrs) = (*f.arena).try_var_refs_mut(frame_idx)
            && let Some(vr) = vrs.iter_mut().find(|v| v.is_match(idx, false))
        {
            vr.detach();
        }
        HELPER_OK
    }
}

/// `make_loc_ref` / `make_arg_ref` / `make_var_ref_ref`: build the ref object
/// and push `[obj, prop]` (`imm` = atom id, `imm2` = slot index). Shared with
/// the interpreter's arm (`ref_ops::make_ref`).
unsafe fn make_ref(f: *mut JitExecFrame, kind: crate::bytecode::ref_ops::RefKind) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::ref_ops::make_ref(
            scope_of(f),
            kind,
            crate::atom::Atom::from_id(f.imm),
            f.imm2 as u16,
            f.arg_base,
            f.var_base,
            &mut *f.arena,
            FrameIndex::from_u32(f.cur_frame_idx),
            (*f.bytecode).arg_count,
            (*f.bytecode).var_count,
            captured_vars_of(f),
        );
        match r {
            Ok((obj, prop)) => {
                *sp = obj;
                *sp.add(1) = prop;
                f.sp = sp.add(2);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `make_loc_ref`.
unsafe extern "C" fn h_make_loc_ref(f: *mut JitExecFrame) -> u32 {
    unsafe { make_ref(f, crate::bytecode::ref_ops::RefKind::Loc) }
}

/// `make_arg_ref`.
unsafe extern "C" fn h_make_arg_ref(f: *mut JitExecFrame) -> u32 {
    unsafe { make_ref(f, crate::bytecode::ref_ops::RefKind::Arg) }
}

/// `make_var_ref_ref`.
unsafe extern "C" fn h_make_var_ref_ref(f: *mut JitExecFrame) -> u32 {
    unsafe { make_ref(f, crate::bytecode::ref_ops::RefKind::VarRefRef) }
}

/// `make_var_ref`: `[^] -> [base, prop, ^]` (`imm` = atom id).
unsafe extern "C" fn h_make_var_ref(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let atom = crate::atom::Atom::from_id(f.imm);
        match crate::bytecode::ref_ops::make_global_ref(scope_of(f), atom) {
            Ok((base, prop)) => {
                *sp = base;
                *sp.add(1) = prop;
                f.sp = sp.add(2);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `get_ref_value`: `[obj, prop, ^] -> [obj, value, ^]`.
unsafe extern "C" fn h_get_ref_value(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        match crate::bytecode::ref_ops::get_ref_value(
            scope_of(f),
            *sp.offset(-2),
            *sp.offset(-1),
        ) {
            Ok(value) => {
                *sp = value;
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `put_ref_value`: `[obj, prop, val, ^] -> [^]`.
unsafe extern "C" fn h_put_ref_value(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let r = crate::bytecode::ref_ops::put_ref_value(
            scope_of(f),
            *sp.offset(-3),
            *sp.offset(-2),
            *sp.offset(-1),
        );
        f.sp = sp.offset(-3);
        if r.is_err() {
            HELPER_EXCEPTION
        } else {
            HELPER_OK
        }
    }
}

/// `delete_var`: `[^] -> [bool, ^]` — `delete name` resolves against the global
/// object; it is the fall-through of `with_delete_var` (which only reaches the
/// global object when the with object does not have the name).
unsafe extern "C" fn h_delete_var(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let atom = crate::atom::Atom::from_id(f.imm);
        let scope = scope_of(f);
        let mut global = scope.global_obj();
        match global.delete_property(scope, atom) {
            Ok(deleted) => {
                *sp = JSValue::from(deleted);
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `with_get_var` / `with_put_var` / `with_delete_var` / `with_make_ref` /
/// `with_get_ref`: resolve `imm` (atom) on the environment object on top of the
/// stack, with `imm2` as the `is_with` flag (`@@unscopables` applies). Shared
/// with the interpreter (`bytecode::with_ops`).
unsafe fn with_var(f: *mut JitExecFrame, op: WithOp) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let atom = crate::atom::Atom::from_id(f.imm);
        let mut env_obj = *sp.offset(-1);
        let scope = scope_of(f);
        let handled = match crate::bytecode::with_ops::in_env(scope, env_obj, atom, f.imm2 != 0) {
            Ok(handled) => handled,
            Err(_) => return HELPER_EXCEPTION,
        };
        if !handled {
            // Fall through to the ordinary variable access below this op.
            f.sp = sp.offset(-1);
            return HELPER_WITH_MISS;
        }
        match op {
            WithOp::GetVar => match crate::bytecode::with_ops::get_var(scope, env_obj, atom) {
                Ok(value) => {
                    *sp.offset(-1) = value;
                    HELPER_OK
                }
                Err(_) => HELPER_EXCEPTION,
            },
            WithOp::PutVar => {
                let r = crate::bytecode::with_ops::put_var(scope, env_obj, atom, *sp.offset(-2));
                // The window is consumed before the error is examined (the
                // interpreter's `SP_GO!(-2)` runs before its `is_err` check).
                f.sp = sp.offset(-2);
                if r.is_err() {
                    HELPER_EXCEPTION
                } else {
                    HELPER_OK
                }
            }
            WithOp::DeleteVar => {
                match crate::bytecode::with_ops::delete_var(scope, &mut env_obj, atom) {
                    Ok(deleted) => {
                        *sp.offset(-1) = JSValue::from(deleted);
                        HELPER_OK
                    }
                    Err(_) => HELPER_EXCEPTION,
                }
            }
            WithOp::MakeRef => {
                // The object stays; its property name is appended above it.
                *sp = crate::bytecode::with_ops::prop_name(scope, atom);
                f.sp = sp.add(1);
                HELPER_OK
            }
            WithOp::GetRef => {
                *sp = crate::bytecode::with_ops::get_ref(scope, env_obj, atom);
                f.sp = sp.add(1);
                HELPER_OK
            }
        }
    }
}

/// `with_get_var`: `[env_obj, ^] -> [value, ^]` on a hit.
unsafe extern "C" fn h_with_get_var(f: *mut JitExecFrame) -> u32 {
    unsafe { with_var(f, WithOp::GetVar) }
}

/// `with_put_var`: `[value, env_obj, ^] -> [^]` on a hit.
unsafe extern "C" fn h_with_put_var(f: *mut JitExecFrame) -> u32 {
    unsafe { with_var(f, WithOp::PutVar) }
}

/// `with_delete_var`: `[env_obj, ^] -> [bool, ^]` on a hit.
unsafe extern "C" fn h_with_delete_var(f: *mut JitExecFrame) -> u32 {
    unsafe { with_var(f, WithOp::DeleteVar) }
}

/// `with_make_ref`: `[env_obj, ^] -> [env_obj, prop, ^]` on a hit.
unsafe extern "C" fn h_with_make_ref(f: *mut JitExecFrame) -> u32 {
    unsafe { with_var(f, WithOp::MakeRef) }
}

/// `with_get_ref`: `[env_obj, ^] -> [env_obj, value, ^]` on a hit.
unsafe extern "C" fn h_with_get_ref(f: *mut JitExecFrame) -> u32 {
    unsafe { with_var(f, WithOp::GetRef) }
}

/// Which `with_*` access [`with_var`] performs.
#[derive(Clone, Copy)]
enum WithOp {
    GetVar,
    PutVar,
    DeleteVar,
    MakeRef,
    GetRef,
}

/// `throw`: `[value, ^] -> [^]`, raising `value`. Always `HELPER_EXCEPTION`, so
/// the native frame leaves with the pc the interpreter would have reported for
/// this instruction: `cur_pc_offset` is *post*-instruction in both engines (the
/// interpreter's `PC_FETCH!` already stepped over the opcode) and
/// `do_build_backtrace` subtracts one byte to find the line, so the exit passes
/// `pc + size` like every other helper failure.
///
/// The value is read before the pop (the frame's `cur_sp_offset` still covers
/// its slot, so a GC inside the raise sees it) and the pop happens before the
/// raise, mirroring the interpreter's `SP_GO!(-1)`.
unsafe extern "C" fn h_throw(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let value = *sp.offset(-1);
        f.sp = sp.offset(-1);
        let _ = scope_of(f).throw_value(value);
        HELPER_EXCEPTION
    }
}

/// `throw_error <atom> <type>`: the engine-generated error named by `imm2`.
/// `0 -> 0`; always raises.
unsafe extern "C" fn h_throw_error(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let atom = crate::atom::Atom::from_id(f.imm);
        let type_ = f.imm2 as u8;
        crate::bytecode::engine_ops::throw_error(scope_of(f), atom, type_);
        HELPER_EXCEPTION
    }
}

/// `regexp`: `[id, ^] -> [regexp, ^]`. A non-integer operand leaves the stack
/// untouched, exactly like the interpreter's `if let Some` with no else arm.
///
/// On failure this reports the error instead of parking the interpreter's
/// `EXCEPTION` sentinel on the stack and continuing; the two differ only when
/// allocating the object fails.
unsafe extern "C" fn h_regexp(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        let Some(re_id) = (*sp.offset(-1)).as_int() else {
            return HELPER_OK;
        };
        match crate::bytecode::engine_ops::make_regexp(scope_of(f), re_id) {
            Ok(value) => {
                *sp.offset(-2) = value;
                f.sp = sp.offset(-1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// `push_bigint_i32 <value>`: `[^] -> [bigint, ^]`.
unsafe extern "C" fn h_push_bigint_i32(f: *mut JitExecFrame) -> u32 {
    unsafe {
        let f = &mut *f;
        let sp = f.sp;
        match crate::bytecode::engine_ops::bigint_from_i32(scope_of(f), f.imm as i32) {
            Ok(value) => {
                *sp = value;
                f.sp = sp.add(1);
                HELPER_OK
            }
            Err(_) => HELPER_EXCEPTION,
        }
    }
}

/// Stack helpers in call order; the compiler loads entry `id` and calls it.
pub(crate) const HELPER_COUNT: usize = 115;
pub(crate) const HELPER_ADD: usize = 0;
pub(crate) const HELPER_SUB: usize = 1;
pub(crate) const HELPER_MUL: usize = 2;
pub(crate) const HELPER_DIV: usize = 3;
pub(crate) const HELPER_MOD: usize = 4;
pub(crate) const HELPER_LT: usize = 5;
pub(crate) const HELPER_LTE: usize = 6;
pub(crate) const HELPER_GT: usize = 7;
pub(crate) const HELPER_GTE: usize = 8;
pub(crate) const HELPER_EQ: usize = 9;
pub(crate) const HELPER_NEQ: usize = 10;
pub(crate) const HELPER_STRICT_EQ: usize = 11;
pub(crate) const HELPER_STRICT_NEQ: usize = 12;
pub(crate) const HELPER_AND: usize = 13;
pub(crate) const HELPER_OR: usize = 14;
pub(crate) const HELPER_XOR: usize = 15;
pub(crate) const HELPER_SHL: usize = 16;
pub(crate) const HELPER_SAR: usize = 17;
pub(crate) const HELPER_SHR: usize = 18;
pub(crate) const HELPER_PLUS: usize = 19;
pub(crate) const HELPER_NEG: usize = 20;
pub(crate) const HELPER_INC: usize = 21;
pub(crate) const HELPER_DEC: usize = 22;
pub(crate) const HELPER_LNOT: usize = 23;
pub(crate) const HELPER_NOT: usize = 24;
pub(crate) const HELPER_TYPEOF: usize = 25;
/// Index of [`h_to_bool`], called with [`TruthyFn`]'s signature.
pub(crate) const HELPER_TO_BOOL: usize = 26;
/// Index of [`h_safepoint`], called at loop back edges.
pub(crate) const HELPER_SAFEPOINT: usize = 27;
/// Index of [`h_push_atom`].
pub(crate) const HELPER_PUSH_ATOM: usize = 28;
/// Index of [`h_inc_local`].
pub(crate) const HELPER_INC_LOCAL: usize = 29;
/// Index of [`h_dec_local`].
pub(crate) const HELPER_DEC_LOCAL: usize = 30;
/// Index of [`h_get_var`].
pub(crate) const HELPER_GET_VAR: usize = 31;
/// Index of [`h_get_var_undef`].
pub(crate) const HELPER_GET_VAR_UNDEF: usize = 32;
/// Index of [`h_put_var`].
pub(crate) const HELPER_PUT_VAR: usize = 33;
/// Index of [`h_put_var_init`].
pub(crate) const HELPER_PUT_VAR_INIT: usize = 34;
/// Index of [`h_get_array_el`].
pub(crate) const HELPER_GET_ARRAY_EL: usize = 35;
/// Index of [`h_get_array_el2`].
pub(crate) const HELPER_GET_ARRAY_EL2: usize = 36;
/// Index of [`h_get_array_el3`].
pub(crate) const HELPER_GET_ARRAY_EL3: usize = 37;
/// Index of [`h_put_array_el`].
pub(crate) const HELPER_PUT_ARRAY_EL: usize = 38;
/// Index of [`h_get_field`].
pub(crate) const HELPER_GET_FIELD: usize = 39;
/// Index of [`h_get_field2`].
pub(crate) const HELPER_GET_FIELD2: usize = 40;
/// Index of [`h_put_field`].
pub(crate) const HELPER_PUT_FIELD: usize = 41;
/// Index of [`h_pow`].
pub(crate) const HELPER_POW: usize = 42;
/// Index of [`h_to_object`].
pub(crate) const HELPER_TO_OBJECT: usize = 43;
/// Index of [`h_to_propkey`].
pub(crate) const HELPER_TO_PROPKEY: usize = 44;
/// Index of [`h_in`].
pub(crate) const HELPER_IN: usize = 45;
/// Index of [`h_instanceof`].
pub(crate) const HELPER_INSTANCEOF: usize = 46;
/// Index of [`h_delete`].
pub(crate) const HELPER_DELETE: usize = 47;
/// Index of [`h_add_loc`].
pub(crate) const HELPER_ADD_LOC: usize = 48;
/// Index of [`h_post_inc`].
pub(crate) const HELPER_POST_INC: usize = 49;
/// Index of [`h_post_dec`].
pub(crate) const HELPER_POST_DEC: usize = 50;
/// Index of [`h_nip_catch`].
pub(crate) const HELPER_NIP_CATCH: usize = 51;
/// Index of [`h_rest`].
pub(crate) const HELPER_REST: usize = 52;
/// Index of [`h_special_object`].
pub(crate) const HELPER_SPECIAL_OBJECT: usize = 53;
/// Index of [`h_push_this`].
pub(crate) const HELPER_PUSH_THIS: usize = 54;
/// Index of [`h_fclosure`].
pub(crate) const HELPER_FCLOSURE: usize = 55;
/// Index of [`h_set_name`].
pub(crate) const HELPER_SET_NAME: usize = 56;
/// Index of [`h_get_var_ref`].
pub(crate) const HELPER_GET_VAR_REF: usize = 57;
/// Index of [`h_put_var_ref`].
pub(crate) const HELPER_PUT_VAR_REF: usize = 58;
/// Index of [`h_set_var_ref`].
pub(crate) const HELPER_SET_VAR_REF: usize = 59;
/// Index of [`h_get_var_ref_check`].
pub(crate) const HELPER_GET_VAR_REF_CHECK: usize = 60;
/// Index of [`h_put_var_ref_check`].
pub(crate) const HELPER_PUT_VAR_REF_CHECK: usize = 61;
/// Index of [`h_put_var_ref_check_init`].
pub(crate) const HELPER_PUT_VAR_REF_CHECK_INIT: usize = 62;
/// Index of [`h_for_in_start`].
pub(crate) const HELPER_FOR_IN_START: usize = 63;
/// Index of [`h_for_in_next`].
pub(crate) const HELPER_FOR_IN_NEXT: usize = 64;
/// Index of [`h_for_of_start`].
pub(crate) const HELPER_FOR_OF_START: usize = 65;
/// Index of [`h_for_of_next`].
pub(crate) const HELPER_FOR_OF_NEXT: usize = 66;
/// Index of [`h_iterator_check_object`].
pub(crate) const HELPER_ITERATOR_CHECK_OBJECT: usize = 67;
/// Index of [`h_iterator_get_value_done`].
pub(crate) const HELPER_ITERATOR_GET_VALUE_DONE: usize = 68;
/// Index of [`h_iterator_close`].
pub(crate) const HELPER_ITERATOR_CLOSE: usize = 69;
/// Index of [`h_iterator_next`].
pub(crate) const HELPER_ITERATOR_NEXT: usize = 70;
/// Index of [`h_iterator_call`].
pub(crate) const HELPER_ITERATOR_CALL: usize = 71;
/// Index of [`h_define_field`].
pub(crate) const HELPER_DEFINE_FIELD: usize = 72;
/// Index of [`h_set_name_computed`].
pub(crate) const HELPER_SET_NAME_COMPUTED: usize = 73;
/// Index of [`h_set_proto`].
pub(crate) const HELPER_SET_PROTO: usize = 74;
/// Index of [`h_set_home_object`].
pub(crate) const HELPER_SET_HOME_OBJECT: usize = 75;
/// Index of [`h_define_array_el`].
pub(crate) const HELPER_DEFINE_ARRAY_EL: usize = 76;
/// Index of [`h_copy_data_properties`].
pub(crate) const HELPER_COPY_DATA_PROPERTIES: usize = 77;
/// Index of [`h_define_method`].
pub(crate) const HELPER_DEFINE_METHOD: usize = 78;
/// Index of [`h_define_method_computed`].
pub(crate) const HELPER_DEFINE_METHOD_COMPUTED: usize = 79;
/// Index of [`h_define_class`].
pub(crate) const HELPER_DEFINE_CLASS: usize = 80;
/// Index of [`h_define_class_computed`].
pub(crate) const HELPER_DEFINE_CLASS_COMPUTED: usize = 81;
/// Index of [`h_check_ctor_return`].
pub(crate) const HELPER_CHECK_CTOR_RETURN: usize = 82;
/// Index of [`h_check_ctor`].
pub(crate) const HELPER_CHECK_CTOR: usize = 83;
/// Index of [`h_init_ctor`].
pub(crate) const HELPER_INIT_CTOR: usize = 84;
/// Index of [`h_check_brand`].
pub(crate) const HELPER_CHECK_BRAND: usize = 85;
/// Index of [`h_add_brand`].
pub(crate) const HELPER_ADD_BRAND: usize = 86;
/// Index of [`h_private_symbol`].
pub(crate) const HELPER_PRIVATE_SYMBOL: usize = 87;
/// Index of [`h_get_private_field`].
pub(crate) const HELPER_GET_PRIVATE_FIELD: usize = 88;
/// Index of [`h_put_private_field`].
pub(crate) const HELPER_PUT_PRIVATE_FIELD: usize = 89;
/// Index of [`h_define_private_field`].
pub(crate) const HELPER_DEFINE_PRIVATE_FIELD: usize = 90;
/// Index of [`h_append`].
pub(crate) const HELPER_APPEND: usize = 91;
/// Index of [`h_private_in`].
pub(crate) const HELPER_PRIVATE_IN: usize = 92;
/// Index of [`h_get_super`].
pub(crate) const HELPER_GET_SUPER: usize = 93;
/// Index of [`h_get_super_value`].
pub(crate) const HELPER_GET_SUPER_VALUE: usize = 94;
/// Index of [`h_put_super_value`].
pub(crate) const HELPER_PUT_SUPER_VALUE: usize = 95;
/// Index of [`h_close_loc`].
pub(crate) const HELPER_CLOSE_LOC: usize = 96;
/// Index of [`h_object`].
pub(crate) const HELPER_OBJECT: usize = 97;
/// Index of [`h_array_from`].
pub(crate) const HELPER_ARRAY_FROM: usize = 98;
/// Index of [`h_make_loc_ref`].
pub(crate) const HELPER_MAKE_LOC_REF: usize = 99;
/// Index of [`h_make_arg_ref`].
pub(crate) const HELPER_MAKE_ARG_REF: usize = 100;
/// Index of [`h_make_var_ref_ref`].
pub(crate) const HELPER_MAKE_VAR_REF_REF: usize = 101;
/// Index of [`h_make_var_ref`].
pub(crate) const HELPER_MAKE_VAR_REF: usize = 102;
/// Index of [`h_get_ref_value`].
pub(crate) const HELPER_GET_REF_VALUE: usize = 103;
/// Index of [`h_put_ref_value`].
pub(crate) const HELPER_PUT_REF_VALUE: usize = 104;
/// Index of [`h_with_get_var`].
pub(crate) const HELPER_WITH_GET_VAR: usize = 105;
/// Index of [`h_with_put_var`].
pub(crate) const HELPER_WITH_PUT_VAR: usize = 106;
/// Index of [`h_with_delete_var`].
pub(crate) const HELPER_WITH_DELETE_VAR: usize = 107;
/// Index of [`h_with_make_ref`].
pub(crate) const HELPER_WITH_MAKE_REF: usize = 108;
/// Index of [`h_with_get_ref`].
pub(crate) const HELPER_WITH_GET_REF: usize = 109;
/// Index of [`h_delete_var`].
pub(crate) const HELPER_DELETE_VAR: usize = 110;
/// Index of [`h_throw`].
pub(crate) const HELPER_THROW: usize = 111;
/// Index of [`h_throw_error`].
pub(crate) const HELPER_THROW_ERROR: usize = 112;
/// Index of [`h_regexp`].
pub(crate) const HELPER_REGEXP: usize = 113;
/// Index of [`h_push_bigint_i32`].
pub(crate) const HELPER_PUSH_BIGINT_I32: usize = 114;

/// The helper dispatch table. Function pointers are immutable and never
/// interchanged, so sharing the table across threads is sound.
#[repr(transparent)]
pub(crate) struct HelperTable(pub(crate) [*const (); HELPER_COUNT]);
unsafe impl Sync for HelperTable {}

pub(crate) static HELPERS: HelperTable = HelperTable([
    h_add as *const (),
    h_sub as *const (),
    h_mul as *const (),
    h_div as *const (),
    h_mod as *const (),
    h_lt as *const (),
    h_lte as *const (),
    h_gt as *const (),
    h_gte as *const (),
    h_eq as *const (),
    h_neq as *const (),
    h_strict_eq as *const (),
    h_strict_neq as *const (),
    h_bit_and as *const (),
    h_bit_or as *const (),
    h_bit_xor as *const (),
    h_bit_shl as *const (),
    h_bit_sar as *const (),
    h_bit_shr as *const (),
    h_plus as *const (),
    h_neg as *const (),
    h_inc as *const (),
    h_dec as *const (),
    h_lnot as *const (),
    h_not as *const (),
    h_typeof as *const (),
    h_to_bool as *const (),
    h_safepoint as *const (),
    h_push_atom as *const (),
    h_inc_local as *const (),
    h_dec_local as *const (),
    h_get_var as *const (),
    h_get_var_undef as *const (),
    h_put_var as *const (),
    h_put_var_init as *const (),
    h_get_array_el as *const (),
    h_get_array_el2 as *const (),
    h_get_array_el3 as *const (),
    h_put_array_el as *const (),
    h_get_field as *const (),
    h_get_field2 as *const (),
    h_put_field as *const (),
    h_pow as *const (),
    h_to_object as *const (),
    h_to_propkey as *const (),
    h_in as *const (),
    h_instanceof as *const (),
    h_delete as *const (),
    h_add_loc as *const (),
    h_post_inc as *const (),
    h_post_dec as *const (),
    h_nip_catch as *const (),
    h_rest as *const (),
    h_special_object as *const (),
    h_push_this as *const (),
    h_fclosure as *const (),
    h_set_name as *const (),
    h_get_var_ref as *const (),
    h_put_var_ref as *const (),
    h_set_var_ref as *const (),
    h_get_var_ref_check as *const (),
    h_put_var_ref_check as *const (),
    h_put_var_ref_check_init as *const (),
    h_for_in_start as *const (),
    h_for_in_next as *const (),
    h_for_of_start as *const (),
    h_for_of_next as *const (),
    h_iterator_check_object as *const (),
    h_iterator_get_value_done as *const (),
    h_iterator_close as *const (),
    h_iterator_next as *const (),
    h_iterator_call as *const (),
    h_define_field as *const (),
    h_set_name_computed as *const (),
    h_set_proto as *const (),
    h_set_home_object as *const (),
    h_define_array_el as *const (),
    h_copy_data_properties as *const (),
    h_define_method as *const (),
    h_define_method_computed as *const (),
    h_define_class as *const (),
    h_define_class_computed as *const (),
    h_check_ctor_return as *const (),
    h_check_ctor as *const (),
    h_init_ctor as *const (),
    h_check_brand as *const (),
    h_add_brand as *const (),
    h_private_symbol as *const (),
    h_get_private_field as *const (),
    h_put_private_field as *const (),
    h_define_private_field as *const (),
    h_append as *const (),
    h_private_in as *const (),
    h_get_super as *const (),
    h_get_super_value as *const (),
    h_put_super_value as *const (),
    h_close_loc as *const (),
    h_object as *const (),
    h_array_from as *const (),
    h_make_loc_ref as *const (),
    h_make_arg_ref as *const (),
    h_make_var_ref_ref as *const (),
    h_make_var_ref as *const (),
    h_get_ref_value as *const (),
    h_put_ref_value as *const (),
    h_with_get_var as *const (),
    h_with_put_var as *const (),
    h_with_delete_var as *const (),
    h_with_make_ref as *const (),
    h_with_get_ref as *const (),
    h_delete_var as *const (),
    h_throw as *const (),
    h_throw_error as *const (),
    h_regexp as *const (),
    h_push_bigint_i32 as *const (),
]);
