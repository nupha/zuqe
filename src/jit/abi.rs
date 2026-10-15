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
        OpCode,
        arithmatic::{UnaryOp, op_binary_logic},
    },
};

/// Status returned by every helper. Non-zero exits the native frame.
pub(crate) const HELPER_OK: u32 = 0;
pub(crate) const HELPER_EXCEPTION: u32 = 1;
pub(crate) const HELPER_INTERRUPT: u32 = 2;

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
    /// Immediate for the few helpers that need one (`push_atom_value`).
    pub imm: u32,
    /// Return value for `Done`.
    pub out_value: JSValue,
    /// Byte offset into the opcode stream for `Resume`/`Exception`.
    pub out_pc: u32,
    /// Slot offset from `sp_base` for `Resume`/`Exception`.
    pub out_sp: i32,
}

/// The compiled entry. Returns a [`JitExitKind`] as a `u32`.
pub(crate) type NativeEntry = unsafe extern "C" fn(*mut JitExecFrame) -> u32;

/// Per-function storage for the installed native entry, living on
/// `BytecodeImpl`. Null until a compile installs one. The field is atomic only
/// because the slot is written through `&Bytecode` (interior mutability); all
/// access happens on the single runtime thread.
#[repr(transparent)]
pub(crate) struct FunctionSlot {
    entry: std::sync::atomic::AtomicPtr<()>,
}

impl Default for FunctionSlot {
    fn default() -> Self {
        Self {
            entry: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

impl FunctionSlot {
    /// The installed entry, or `None` while the function is interpreted.
    #[inline]
    pub(crate) fn entry(&self) -> Option<NativeEntry> {
        use std::sync::atomic::Ordering;
        let raw = self.entry.load(Ordering::Acquire);
        if raw.is_null() {
            None
        } else {
            // SAFETY: the pointer was produced by `install` from a `NativeEntry`.
            Some(unsafe { std::mem::transmute::<*mut (), NativeEntry>(raw) })
        }
    }

    /// Publish `entry`. Callers must have already made the backing code
    /// executable. (A tier tag arrives with Tier 2.)
    pub(crate) fn install(&self, entry: NativeEntry) {
        self.entry
            .store(entry as *mut (), std::sync::atomic::Ordering::Release);
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

/// `inc_loc` / `dec_loc`: unary update of local `f.imm`, no operand-stack change
/// (so an exception exit leaves `sp` exactly as the interpreter would).
unsafe fn local_unary(f: *mut JitExecFrame, inc: bool) -> u32 {
    unsafe {
        let f = &mut *f;
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

/// Stack helpers in call order; the compiler loads entry `id` and calls it.
pub(crate) const HELPER_COUNT: usize = 31;
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
]);
