// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cmp::Ordering;

use crate::{
    bytecode::OpCode,
    context::ScopedContext,
    error::JSResult,
    number::{IntOrFloat, f64_to_i32},
    utils::unlikely,
    value::{HINT_NONE, HINT_NUMBER, JSValue},
};

#[derive(Debug, Clone)]
pub(super) enum TwoNumbers {
    Int(i32, i32),
    Float(f64, f64),
}

impl TwoNumbers {
    /// fast path
    const fn from_values_fast(op1: JSValue, op2: JSValue) -> Option<Self> {
        match (op1.tag(), op2.tag()) {
            (
                crate::JSValueTag::Int | crate::JSValueTag::Bool,
                crate::JSValueTag::Int | crate::JSValueTag::Bool,
            ) => Some(Self::Int(op1.payload_i32(), op2.payload_i32())),

            (crate::JSValueTag::Float64, crate::JSValueTag::Int | crate::JSValueTag::Bool) => {
                Some(Self::Float(
                    unsafe { op1.as_float_unchecked() },
                    op2.payload_i32() as f64,
                ))
            }

            (crate::JSValueTag::Int | crate::JSValueTag::Bool, crate::JSValueTag::Float64) => {
                Some(Self::Float(op1.payload_i32() as f64, unsafe {
                    op2.as_float_unchecked()
                }))
            }

            _ => None,
        }
    }

    /// if `coerce`is true, try coerce non-number, non-boolean value to number.
    pub fn from_values(
        ctx: &mut ScopedContext,
        val1: JSValue,
        val2: JSValue,
        coerce: bool,
    ) -> Result<Self, i32> {
        if let Some(n2) = Self::from_values_fast(val1, val2) {
            Ok(n2)
        } else if coerce {
            match (val1.to_number(ctx)?, val2.to_number(ctx)?) {
                (IntOrFloat::Int64(a), IntOrFloat::Int64(b))
                    if (i32::MIN as i64..=i32::MAX as i64).contains(&a)
                        && (i32::MIN as i64..=i32::MAX as i64).contains(&b) =>
                {
                    Ok(Self::Int(a as i32, b as i32))
                }
                (n1, n2) => Ok(Self::Float(n1.to_f64(), n2.to_f64())),
            }
        } else {
            Err(-1)
        }
    }

    /// returns None if it has NaN.
    pub fn partial_cmp(&self) -> Option<std::cmp::Ordering> {
        match self {
            Self::Int(a, b) => Some(a.cmp(b)),
            Self::Float(a, b) => a.partial_cmp(b),
        }
    }

    pub fn add(self) -> JSValue {
        match self {
            Self::Int(a, b) => {
                JSValue::from(
                    a as i64 + b as i64, // result may overflow i32 ranges
                )
            }
            Self::Float(a, b) => JSValue::from(a + b),
        }
    }

    pub fn sub(self) -> JSValue {
        match self {
            Self::Int(a, b) => JSValue::from(a - b),
            Self::Float(a, b) => JSValue::from(a - b),
        }
    }

    pub fn mul(self) -> JSValue {
        match self {
            Self::Int(a, b) => {
                let n = (a as i64) * (b as i64);
                if unlikely(n == 0 && (a | b) < 0) {
                    JSValue::make_float(-0.0)
                } else {
                    JSValue::from(n)
                }
            }
            Self::Float(a, b) => JSValue::from(a * b),
        }
    }

    pub fn div(self) -> JSValue {
        match self {
            Self::Int(a, b) => JSValue::from((a as f64) / (b as f64)),
            Self::Float(a, b) => JSValue::from(a / b),
        }
    }

    pub fn mod_(self) -> JSValue {
        match self {
            Self::Int(a, b) => {
                if unlikely(b == 0) {
                    JSValue::NAN
                } else if a < 0 {
                    JSValue::from((a as f64) % (b as f64))
                } else {
                    JSValue::from(a % b)
                }
            }
            Self::Float(a, b) => {
                if unlikely(b == 0.0) {
                    JSValue::NAN
                } else {
                    JSValue::from(a % b)
                }
            }
        }
    }

    pub fn pow(self) -> JSValue {
        match self {
            Self::Int(a, b) => {
                if b >= 0 {
                    JSValue::from((a as i64).pow(b as u32))
                    // JSValue::from(a.pow(b as u32)) // <-- this may overflow
                } else {
                    JSValue::from((a as f64).powf(b as f64))
                }
            }
            Self::Float(a, b) => {
                if unlikely(!b.is_finite() && a.abs() == 1.0) {
                    // not compatible with IEEE 754
                    JSValue::NAN
                } else {
                    JSValue::from(a.powf(b))
                }
            }
        }
    }

    pub fn shl(self) -> JSValue {
        let (a, b) = match self {
            Self::Int(v1, v2) => (v1, v2),
            Self::Float(v1, v2) => {
                let v1 = f64_to_i32(v1);
                let v2 = f64_to_i32(v2);
                (v1, v2)
            }
        };
        JSValue::from(a << (b & 0x1f))
    }

    pub fn sar(self) -> JSValue {
        let (a, b) = match self {
            Self::Int(v1, v2) => (v1, v2),
            Self::Float(v1, v2) => {
                let v1 = f64_to_i32(v1);
                let v2 = f64_to_i32(v2);
                (v1, v2)
            }
        };
        JSValue::from(a >> (b & 0x1f))
    }

    pub fn shr(self) -> JSValue {
        let (a, b) = match self {
            Self::Int(v1, v2) => (v1 as u32, v2 as u32),
            Self::Float(v1, v2) => {
                let v1 = f64_to_i32(v1) as u32;
                let v2 = f64_to_i32(v2) as u32;
                (v1, v2)
            }
        };
        JSValue::from(a >> (b & 0x1f))
    }

    pub fn and(self) -> JSValue {
        match self {
            Self::Int(v1, v2) => JSValue::from((v1 as u32) & (v2 as u32)),
            Self::Float(v1, v2) => JSValue::from((v1.trunc() as u32) & (v2.trunc() as u32)),
        }
    }

    pub fn or(self) -> JSValue {
        match self {
            Self::Int(v1, v2) => JSValue::from((v1 as u32) | (v2 as u32)),
            Self::Float(v1, v2) => JSValue::from((v1.trunc() as u32) | (v2.trunc() as u32)),
        }
    }

    pub fn xor(self) -> JSValue {
        match self {
            Self::Int(v1, v2) => JSValue::from((v1 as u32) ^ (v2 as u32)),
            Self::Float(v1, v2) => JSValue::from((v1.trunc() as u32) ^ (v2.trunc() as u32)),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum UnaryOp {
    Int(i32),
    Float(f64),
}

impl UnaryOp {
    pub(crate) fn new(
        ctx: &mut ScopedContext,
        val: JSValue,
        allow_float: bool,
    ) -> Result<Self, i32> {
        match val.tag() {
            crate::JSValueTag::Int => Ok(UnaryOp::Int(val.payload_i32())),
            crate::JSValueTag::Float64 if allow_float => {
                Ok(UnaryOp::Float(unsafe { val.as_float_unchecked() }))
            }
            _ if allow_float => match val.to_number(ctx)? {
                IntOrFloat::Int64(i) if (i32::MIN as i64..=i32::MAX as i64).contains(&i) => {
                    Ok(UnaryOp::Int(i as i32))
                }
                n => Ok(UnaryOp::Float(n.to_f64())),
            },
            _ => val.to_number(ctx).map(|n| Self::Int(n.to_i32())),
        }
    }

    /// a shortcut for ::new(ctx, val, false)
    #[inline(always)]
    pub fn new_int(ctx: &mut ScopedContext, val: JSValue) -> Result<Self, i32> {
        if let Some(i) = val.as_int() {
            Ok(Self::Int(i))
        } else {
            val.to_number(ctx).map(|n| Self::Int(n.to_i32()))
        }
    }

    pub fn neg(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if unlikely(a == 0) {
                    // -0 cannot be expressed as integer
                    JSValue::make_float(-0.0)
                } else if a == i32::MIN {
                    JSValue::make_float(-(a as f64))
                } else {
                    JSValue::from(-a)
                }
            }
            UnaryOp::Float(a) => {
                if unlikely(a == 0.0) {
                    if a.is_sign_negative() {
                        JSValue::from(0)
                    } else {
                        JSValue::make_float(-0.0)
                    }
                } else {
                    JSValue::from(-a)
                }
            }
        }
    }

    pub fn plus(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => JSValue::from(a),
            UnaryOp::Float(a) => JSValue::make_float(a),
        }
    }

    pub fn inc(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if unlikely(a == i32::MAX) {
                    JSValue::from(a as u32 + 1)
                } else {
                    JSValue::from(a + 1)
                }
            }
            UnaryOp::Float(a) => JSValue::make_float(a + 1.0),
        }
    }

    pub fn dec(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if unlikely(a == i32::MIN) {
                    JSValue::from(a as i64 - 1)
                } else {
                    JSValue::from(a - 1)
                }
            }
            UnaryOp::Float(a) => JSValue::make_float(a - 1.0),
        }
    }

    pub fn not(self) -> JSValue {
        match self {
            Self::Int(a) => JSValue::from(!a),
            _ => unreachable!(),
        }
    }
}

#[inline]
pub(super) fn op_binary_logic(
    ctx: &mut ScopedContext,
    opcode: OpCode,
    sp: *mut JSValue,
) -> Result<*mut JSValue, i32> {
    let res = TwoNumbers::from_values(
        ctx,
        unsafe { std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED) },
        unsafe { std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED) },
        true,
    )
    .map(|bop| match opcode {
        OpCode::SHL => bop.shl(),
        OpCode::SHR => bop.shr(),
        OpCode::SAR => bop.sar(),
        OpCode::AND => bop.and(),
        OpCode::XOR => bop.xor(),
        OpCode::OR => bop.or(),
        _ => unreachable!(),
    })?;

    unsafe {
        std::ptr::replace(sp.offset(-2), res);
        Ok(sp.offset(-1))
    }
}

// /// compare op1, op2
// pub(super) fn relational(
//     ctx: ContextPtr,
//     op1: JSValue,
//     op2: JSValue,
//     opcode: OpCode,
// ) -> Result<bool, i32> {
//     let bop = NumBinOp::new(ctx, op1, op2)?;
//     match opcode {
//         OpCode::LT => Ok(bop.lt()),
//         OpCode::LTE => Ok(bop.lte()),
//         OpCode::GT => Ok(bop.gt()),
//         OpCode::GTE => Ok(bop.gte()),
//         _ => unreachable!(),
//     }
// }

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum StrictEqMode {
    JS_EQ_STRICT,
    JS_EQ_SAME_VALUE,
    JS_EQ_SAME_VALUE_ZERO,
}

impl JSValue {
    /// arithmatic compare of two values.
    /// returns None only if value converted to NaN
    pub fn arith_cmp(
        &self,
        ctx: &mut ScopedContext,
        other: JSValue,
    ) -> Result<Option<std::cmp::Ordering>, i32> {
        // fast path
        if let Some(n2) = TwoNumbers::from_values_fast(*self, other) {
            return Ok(n2.partial_cmp());
        }

        // slow path
        let mut op1 = *self;
        let mut op2 = other;

        loop {
            match (op1.tag(), op2.tag()) {
                // string vs string
                (crate::JSValueTag::String, crate::JSValueTag::String) => unsafe {
                    let s1 = op1.as_string_unchecked();
                    let s2 = &op2.as_string_unchecked();
                    return Ok(Some(s1.cmp(s2)));
                },

                // object vs any
                (crate::JSValueTag::Object, crate::JSValueTag::Object) => {
                    op1 = op1.to_primitive(ctx, HINT_NUMBER)?;
                    op2 = op2.to_primitive(ctx, HINT_NUMBER)?;
                }
                (crate::JSValueTag::Object, _) => {
                    op1 = op1.to_primitive(ctx, HINT_NUMBER)?;
                }
                (_, crate::JSValueTag::Object) => {
                    op2 = op2.to_primitive(ctx, HINT_NUMBER)?;
                }

                _ => {
                    let n1 = op1.to_number(ctx)?;
                    let n2 = op2.to_number(ctx)?;
                    return Ok(n1.partial_cmp(&n2));
                }
            }

            if let Some(n2) = TwoNumbers::from_values_fast(op1, op2) {
                return Ok(n2.partial_cmp());
            }
        }
    }

    /// arithmatic add of two values.
    pub fn arith_add(ctx: &mut ScopedContext, mut op1: Self, mut op2: Self) -> JSResult {
        loop {
            match (op1.tag(), op2.tag()) {
                // path to exit:
                (crate::JSValueTag::Int, crate::JSValueTag::Int) => {
                    let a = op1.payload_i32();
                    let b = op2.payload_i32();
                    // result may overflow i32 ranges
                    return Ok(JSValue::from(a as i64 + b as i64));
                }
                (crate::JSValueTag::Float64, crate::JSValueTag::Float64) => unsafe {
                    let a = op1.as_float_unchecked();
                    let b = op2.as_float_unchecked();
                    return Ok(JSValue::from(a + b));
                },
                (crate::JSValueTag::Int, crate::JSValueTag::Float64) => {
                    let a = op1.payload_i32() as f64;
                    let b = unsafe { op2.as_float_unchecked() };
                    return Ok(JSValue::from(a + b));
                }
                (crate::JSValueTag::Float64, crate::JSValueTag::Int) => {
                    let a = unsafe { op1.as_float_unchecked() };
                    let b = op2.payload_i32() as f64;
                    return Ok(JSValue::from(a + b));
                }

                //
                // conversion - don't change following arm order
                //
                (crate::JSValueTag::Object, _) | (_, crate::JSValueTag::Object) => {
                    op1 = op1.to_primitive(ctx, HINT_NONE)?;
                    op2 = op2.to_primitive(ctx, HINT_NONE)?;
                }
                (crate::JSValueTag::String, _) | (_, crate::JSValueTag::String) => {
                    return JSValue::concat_value(ctx, op1, op2);
                }
                _ => {
                    let n1 = op1.to_number(ctx)?;
                    let n2 = op2.to_number(ctx)?;
                    return Ok(n1.add(n2).into());
                }
            }
        }
    }

    #[inline(always)]
    pub fn arith_sub(ctx: &mut ScopedContext, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.sub())
    }

    #[inline(always)]
    pub fn arith_mul(ctx: &mut ScopedContext, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.mul())
    }

    #[inline(always)]
    pub fn arith_div(ctx: &mut ScopedContext, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.div())
    }

    #[inline(always)]
    pub fn arith_mod(ctx: &mut ScopedContext, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.mod_())
    }

    #[inline(always)]
    pub fn arith_pow(ctx: &mut ScopedContext, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.pow())
    }

    /// test arithmatical equality (no-strict-eq): self == other?
    #[inline(always)]
    pub fn arith_eq(&self, ctx: &mut ScopedContext, other: Self) -> Result<bool, i32> {
        js_eq(ctx, *self, other)
    }

    /// compare JSValues equality in given mode.
    /// Note: `self` and `other` are not consumed.
    pub(crate) fn strict_eq_with_mode(&self, other: Self, eq_mode: StrictEqMode) -> bool {
        match (self.tag(), other.tag()) {
            (crate::JSValueTag::Int, crate::JSValueTag::Int)
            | (crate::JSValueTag::Bool, crate::JSValueTag::Bool)
            | (crate::JSValueTag::CatchOffset, crate::JSValueTag::CatchOffset) => {
                self.0 == other.0 // i32 payload eq
            }

            (crate::JSValueTag::String, crate::JSValueTag::String) => unsafe {
                // string: content equality (matches non-nan-boxing StringRef::eq).
                // runtime-built strings (e.g. ropes from concat) are distinct
                // nodes with equal content, so raw-pointer compare rejects them.
                self.as_string_unchecked() == other.as_string_unchecked()
            },

            // gc node: identity
            (crate::JSValueTag::Object, crate::JSValueTag::Object)
            | (crate::JSValueTag::FunctionBytecode, crate::JSValueTag::FunctionBytecode)
            | (crate::JSValueTag::Module, crate::JSValueTag::Module) => {
                self.0 == other.0 // raw pointer equal
            }

            (crate::JSValueTag::Float64, crate::JSValueTag::Float64) => unsafe {
                let a = self.as_float_unchecked();
                let b = other.as_float_unchecked();
                cmp_f64(a, b, eq_mode)
            },
            (crate::JSValueTag::Float64, crate::JSValueTag::Int) => unsafe {
                let a = self.as_float_unchecked();
                let b = other.payload_i32() as f64;
                cmp_f64(a, b, eq_mode)
            },
            (crate::JSValueTag::Int, crate::JSValueTag::Float64) => unsafe {
                let a = self.payload_i32() as f64;
                let b = other.as_float_unchecked();
                cmp_f64(a, b, eq_mode)
            },

            // singleton types
            (crate::JSValueTag::Undefined, crate::JSValueTag::Undefined)
            | (crate::JSValueTag::Null, crate::JSValueTag::Null)
            | (crate::JSValueTag::Exception, crate::JSValueTag::Exception)
            | (crate::JSValueTag::Uninitialized, crate::JSValueTag::Uninitialized) => true,

            (crate::JSValueTag::Symbol, crate::JSValueTag::Symbol) => {
                // Symbols are compared by identity (their unique atom id), not by the
                // descriptor string content. `StringRef`'s `PartialEq` compares content,
                // so `a == b` here would wrongly equate two distinct `Symbol('x')` calls
                // that share the same description. Match `JSValue::eq` (val.rs), which
                // keys on `atom_info().id()`.
                let a = self.as_symbol(true).unwrap();
                let b = other.as_symbol(true).unwrap();
                matches!((a.atom_info(), b.atom_info()), (Some(x), Some(y)) if x.id() == y.id())
            }

            _ => false,
        }
    }

    /// test strict equality (strict-eq): self === other?
    #[inline(always)]
    pub fn strict_eq(&self, other: Self) -> bool {
        self.strict_eq_with_mode(other, StrictEqMode::JS_EQ_STRICT)
    }

    /// test if two values are strictly same. (-0, 0) not same
    #[inline(always)]
    pub fn is_same(&self, other: Self) -> bool {
        self.strict_eq_with_mode(other, StrictEqMode::JS_EQ_SAME_VALUE)
    }

    /// test if two values are strictly same. (-0, 0) is same
    #[inline(always)]
    pub fn is_same0(&self, other: Self) -> bool {
        self.strict_eq_with_mode(other, StrictEqMode::JS_EQ_SAME_VALUE_ZERO)
    }

    /// get order of `self` and `other`.
    pub fn sort_cmp(&self, ctx: &mut ScopedContext, other: Self) -> Result<Ordering, i32> {
        let mut v1 = *self;
        let mut v2 = other;

        loop {
            match (v1.tag(), v2.tag()) {
                (
                    crate::JSValueTag::Int | crate::JSValueTag::Bool,
                    crate::JSValueTag::Int | crate::JSValueTag::Bool,
                ) => {
                    return Ok(v1.payload_i32().cmp(&v2.payload_i32()));
                }

                (crate::JSValueTag::String, crate::JSValueTag::String) => unsafe {
                    let a = v1.as_string_unchecked();
                    let b = v2.as_string_unchecked();
                    return Ok(a.partial_cmp(&b).unwrap());
                },

                (crate::JSValueTag::Symbol, crate::JSValueTag::Symbol) => {
                    // compare atom descriptor content
                    let s1 = v1.as_symbol(true).unwrap();
                    let s2 = v2.as_symbol(true).unwrap();
                    return Ok(s1.content().partial_cmp(s2.content()).unwrap());
                }

                (crate::JSValueTag::Float64, crate::JSValueTag::Float64) => unsafe {
                    let a = v1.as_float_unchecked();
                    let b = v2.as_float_unchecked();
                    return Ok(match (a.is_nan(), b.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
                    });
                },
                (crate::JSValueTag::Int, crate::JSValueTag::Float64) => {
                    let a = v1.payload_i32() as f64;
                    let b = unsafe { v2.as_float_unchecked() };
                    return Ok(match (a.is_nan(), b.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
                    });
                }
                (crate::JSValueTag::Float64, crate::JSValueTag::Int) => {
                    let a = unsafe { v1.as_float_unchecked() };
                    let b = v2.payload_i32() as f64;
                    return Ok(match (a.is_nan(), b.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
                    });
                }

                (crate::JSValueTag::Undefined, crate::JSValueTag::Undefined)
                | (crate::JSValueTag::Null, crate::JSValueTag::Null) => {
                    return Ok(Ordering::Equal);
                }

                // string is prior to symbol
                (crate::JSValueTag::String, crate::JSValueTag::Symbol) => {
                    return Ok(Ordering::Less);
                }
                (crate::JSValueTag::Symbol, crate::JSValueTag::String) => {
                    return Ok(Ordering::Greater);
                }

                // undefined is always the last
                (crate::JSValueTag::Undefined, _) => {
                    return Ok(Ordering::Greater);
                }
                (_, crate::JSValueTag::Undefined) => {
                    return Ok(Ordering::Less);
                }

                // null is prior to NaN
                (crate::JSValueTag::Null, crate::JSValueTag::Float64) => {
                    let d = unsafe { v2.as_float_unchecked() };
                    return if d.is_nan() {
                        Ok(Ordering::Less)
                    } else {
                        Ok(0.0.partial_cmp(&d).unwrap())
                    };
                }
                (crate::JSValueTag::Float64, crate::JSValueTag::Null) => {
                    let d = unsafe { v1.as_float_unchecked() };
                    return if d.is_nan() {
                        Ok(Ordering::Greater)
                    } else {
                        Ok(d.partial_cmp(&0.0).unwrap())
                    };
                }

                // convert
                (crate::JSValueTag::Null, _) => {
                    v1 = JSValue::ZERO;
                }
                (_, crate::JSValueTag::Null) => {
                    v2 = JSValue::ZERO;
                }
                _ => {
                    v1 = v1.to_primitive(ctx, HINT_NUMBER)?;
                    v2 = v2.to_primitive(ctx, HINT_NUMBER)?;
                }
            }
        }
    }
}

/// test arith equality (no-strict eq): val1 == val2
fn js_eq(ctx: &mut ScopedContext, val1: JSValue, val2: JSValue) -> Result<bool, i32> {
    let tag1 = val1.tag();
    let tag2 = val2.tag();
    if tag1 == tag2 {
        return Ok(val1 == val2);
    } else if let Some(n2) = TwoNumbers::from_values_fast(val1, val2) {
        // fast path for numbers
        return Ok(n2.partial_cmp() == Some(std::cmp::Ordering::Equal));
    }

    // slow path
    let mut op1 = val1;
    let mut op2 = val2;

    let b = loop {
        match (op1.tag(), op2.tag()) {
            // null == undefined
            (
                crate::JSValueTag::Null | crate::JSValueTag::Undefined,
                crate::JSValueTag::Null | crate::JSValueTag::Undefined,
            ) => {
                break true;
            }
            // null,undefined != others
            (_, crate::JSValueTag::Null | crate::JSValueTag::Undefined)
            | (crate::JSValueTag::Null | crate::JSValueTag::Undefined, _) => {
                break false;
            }

            // int|boolean vs string
            (crate::JSValueTag::Int | crate::JSValueTag::Bool, crate::JSValueTag::String) => {
                let i = op1.payload_i32();
                let s = unsafe { op2.as_string_unchecked() };
                break match s.to_number(ctx) {
                    IntOrFloat::Int64(v) => v == i as i64,
                    IntOrFloat::Float64(v) => v == 0.0 && i == 0, // -0 == 0
                };
            }
            (crate::JSValueTag::String, crate::JSValueTag::Int | crate::JSValueTag::Bool) => {
                let s = unsafe { op1.as_string_unchecked() };
                let i = op2.payload_i32();
                break match s.to_number(ctx) {
                    IntOrFloat::Int64(v) => v == i as i64,
                    IntOrFloat::Float64(v) => v == 0.0 && i == 0, // -0 == 0
                };
            }

            // float vs string
            (crate::JSValueTag::Float64, crate::JSValueTag::String) => {
                let f = unsafe { op1.as_float_unchecked() };
                let s = unsafe { op2.as_string_unchecked() };
                break s.to_number(ctx).to_f64() == f;
            }
            (crate::JSValueTag::String, crate::JSValueTag::Float64) => {
                let s = unsafe { op1.as_string_unchecked() };
                let f = unsafe { op2.as_float_unchecked() };
                break s.to_number(ctx).to_f64() == f;
            }

            // object vs object
            (crate::JSValueTag::Object, crate::JSValueTag::Object) => unsafe {
                op1 = op1.as_object_unchecked().to_primitive(ctx, HINT_NONE)?;
                op2 = op2.as_object_unchecked().to_primitive(ctx, HINT_NONE)?;
            },

            // object vs number/string/boolean/string
            (
                crate::JSValueTag::Object,
                crate::JSValueTag::Int
                | crate::JSValueTag::Float64
                | crate::JSValueTag::Bool
                | crate::JSValueTag::String,
            ) => unsafe {
                op1 = op1.as_object_unchecked().to_primitive(ctx, HINT_NONE)?;
            },
            (
                crate::JSValueTag::Int
                | crate::JSValueTag::Float64
                | crate::JSValueTag::Bool
                | crate::JSValueTag::String,
                crate::JSValueTag::Object,
            ) => unsafe {
                op2 = op2.as_object_unchecked().to_primitive(ctx, HINT_NONE)?;
            },

            // object vs symbol
            (crate::JSValueTag::Object, crate::JSValueTag::Symbol) => unsafe {
                op1 = op1.as_object_unchecked().to_primitive(ctx, HINT_NONE)?;
            },
            (crate::JSValueTag::Symbol, crate::JSValueTag::Object) => unsafe {
                op2 = op2.as_object_unchecked().to_primitive(ctx, HINT_NONE)?;
            },

            #[cfg(debug_assertions)]
            (
                crate::JSValueTag::Int | crate::JSValueTag::Float64 | crate::JSValueTag::Bool,
                crate::JSValueTag::Int | crate::JSValueTag::Float64 | crate::JSValueTag::Bool,
            ) => {
                unreachable!("op1={:?}, op2={:?}, handled in fast path", op1, op2);
            }

            _ => {
                break false;
            }
        }

        if op1.tag() == op2.tag() {
            break op1 == op2;
        } else if let Some(n2) = TwoNumbers::from_values_fast(op1, op2) {
            break n2.partial_cmp() == Some(std::cmp::Ordering::Equal);
        }
    };

    Ok(b)
}

/// compare two f64 numbers
#[inline(always)]
fn cmp_f64(d1: f64, d2: f64, eq_mode: StrictEqMode) -> bool {
    match eq_mode {
        StrictEqMode::JS_EQ_STRICT => d1 == d2,
        // SameValue: NaN == NaN, +0 != -0
        StrictEqMode::JS_EQ_SAME_VALUE => d1.to_bits() == d2.to_bits(),
        // SameValueZero: NaN == NaN, +0 == -0
        StrictEqMode::JS_EQ_SAME_VALUE_ZERO => d1 == d2 || (d1.is_nan() && d2.is_nan()),
    }
}
