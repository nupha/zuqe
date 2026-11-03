// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Arithmetic, comparison, conversion and update helpers.

use super::*;

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
        pub(super) unsafe extern "C" fn $name(f: *mut JitExecFrame) -> u32 {
            unsafe { binary(f, $op) }
        }
    };
}

binary_wrapper!(h_add, JSValue::arith_add);
binary_wrapper!(h_sub, JSValue::arith_sub);
binary_wrapper!(h_mul, JSValue::arith_mul);
binary_wrapper!(h_div, JSValue::arith_div);
binary_wrapper!(h_mod, JSValue::arith_mod);

pub(super) unsafe extern "C" fn h_bit_shl(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::SHL) }
}
pub(super) unsafe extern "C" fn h_bit_shr(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::SHR) }
}
pub(super) unsafe extern "C" fn h_bit_sar(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::SAR) }
}
pub(super) unsafe extern "C" fn h_bit_and(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::AND) }
}
pub(super) unsafe extern "C" fn h_bit_or(f: *mut JitExecFrame) -> u32 {
    unsafe { logic(f, OpCode::OR) }
}
pub(super) unsafe extern "C" fn h_bit_xor(f: *mut JitExecFrame) -> u32 {
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
        pub(super) unsafe extern "C" fn $name(f: *mut JitExecFrame) -> u32 {
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
pub(super) unsafe extern "C" fn h_eq(f: *mut JitExecFrame) -> u32 {
    unsafe { loose_eq(f, false) }
}
pub(super) unsafe extern "C" fn h_neq(f: *mut JitExecFrame) -> u32 {
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
pub(super) unsafe extern "C" fn h_strict_eq(f: *mut JitExecFrame) -> u32 {
    unsafe { strict_eq(f, false) }
}
pub(super) unsafe extern "C" fn h_strict_neq(f: *mut JitExecFrame) -> u32 {
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
        pub(super) unsafe extern "C" fn $name(f: *mut JitExecFrame) -> u32 {
            unsafe { unary(f, $kind) }
        }
    };
}
// 0 = plus, 1 = neg, 2 = inc, 3 = dec
unary_wrapper!(h_plus, 0);
unary_wrapper!(h_neg, 1);
unary_wrapper!(h_inc, 2);
unary_wrapper!(h_dec, 3);

pub(super) unsafe extern "C" fn h_lnot(f: *mut JitExecFrame) -> u32 {
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

pub(super) unsafe extern "C" fn h_not(f: *mut JitExecFrame) -> u32 {
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

pub(super) unsafe extern "C" fn h_typeof(f: *mut JitExecFrame) -> u32 {
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
pub(super) unsafe extern "C" fn h_push_atom(f: *mut JitExecFrame) -> u32 {
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

/// Tier-down instrumentation: count one speculative guard miss when the frame
/// marks this helper call as the bail of an `int` fast path. The count lives in
/// the activation, so `invoke_native` can fold it without double-counting
/// recursive calls.
pub(super) const unsafe fn note_spec_miss(f: &mut JitExecFrame) {
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

pub(super) unsafe extern "C" fn h_inc_local(f: *mut JitExecFrame) -> u32 {
    unsafe { local_unary(f, true) }
}
pub(super) unsafe extern "C" fn h_dec_local(f: *mut JitExecFrame) -> u32 {
    unsafe { local_unary(f, false) }
}

// `pow`: the interpreter's `arith_pow` (Int fast paths, float, BigInt).
binary_wrapper!(h_pow, JSValue::arith_pow);

/// `to_object`: objectify a primitive, no-op for an object.
pub(super) unsafe extern "C" fn h_to_object(f: *mut JitExecFrame) -> u32 {
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
pub(super) unsafe extern "C" fn h_to_propkey(f: *mut JitExecFrame) -> u32 {
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
pub(super) unsafe extern "C" fn h_in(f: *mut JitExecFrame) -> u32 {
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
pub(super) unsafe extern "C" fn h_instanceof(f: *mut JitExecFrame) -> u32 {
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
pub(super) unsafe extern "C" fn h_delete(f: *mut JitExecFrame) -> u32 {
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
pub(super) unsafe extern "C" fn h_add_loc(f: *mut JitExecFrame) -> u32 {
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
pub(super) unsafe fn post_inc_dec(f: *mut JitExecFrame, inc: bool) -> u32 {
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
