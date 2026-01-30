use std::cmp::Ordering;

use crate::{
    bytecode::OpCode,
    context::ContextImpl,
    error::JSResult,
    number::{JSFloat64Union, js_f64_to_i32},
    utils::unlikely,
    value::{HINT_NONE, HINT_NUMBER, JSValue},
};

#[derive(Debug, Clone)]
pub(super) enum TwoNumbers {
    Int(i32, i32),
    Float(f64, f64),
}

impl TwoNumbers {
    pub const fn from_values0(op1: JSValue, op2: JSValue) -> Option<Self> {
        match (op1, op2) {
            (JSValue::Int(a), JSValue::Int(b)) => Some(Self::Int(a, b)),
            (JSValue::Bool(a), JSValue::Bool(b)) => Some(Self::Int(a as i32, b as i32)),
            (JSValue::Float64(a), JSValue::Float64(b)) => Some(Self::Float(a, b)),

            (JSValue::Int(a), JSValue::Float64(b)) => Some(Self::Float(a as f64, b)),
            (JSValue::Float64(a), JSValue::Int(b)) => Some(Self::Float(a, b as f64)),

            (JSValue::Int(a), JSValue::Bool(b)) => Some(Self::Int(a, b as i32)),
            (JSValue::Bool(a), JSValue::Int(b)) => Some(Self::Int(a as i32, b)),

            (JSValue::Bool(a), JSValue::Float64(b)) => Some(Self::Float(a as i32 as f64, b)),
            (JSValue::Float64(a), JSValue::Bool(b)) => Some(Self::Float(a, b as i32 as f64)),

            _ => None,
        }
    }

    /// if `coerce`is true, try coerce non-number, non-boolean value to number.
    pub fn from_values(
        ctx: &ContextImpl,
        val1: JSValue,
        val2: JSValue,
        coerce: bool,
    ) -> Result<Self, i32> {
        if let Some(n2) = Self::from_values0(val1, val2) {
            Ok(n2)
        } else if coerce {
            let mut op1 = val1;
            let mut op2 = val2;

            loop {
                op1 = op1.to_number(ctx)?;
                op2 = op2.to_number(ctx)?;
                if let Some(n2) = Self::from_values0(op1, op2) {
                    break Ok(n2);
                }
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
                    JSValue::Float64(-0.0)
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
                    JSValue::Float64(f64::NAN)
                } else if a < 0 {
                    JSValue::from((a as f64) % (b as f64))
                } else {
                    JSValue::from(a % b)
                }
            }
            Self::Float(a, b) => {
                if unlikely(b == 0.0) {
                    JSValue::Float64(f64::NAN)
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
                    JSValue::Float64(f64::NAN)
                } else {
                    JSValue::from(a.powf(b))
                }
            }
        }
    }

    pub const fn shl(self) -> JSValue {
        let (a, b) = match self {
            Self::Int(v1, v2) => (v1, v2),
            Self::Float(v1, v2) => {
                let v1 = js_f64_to_i32(v1);
                let v2 = js_f64_to_i32(v2);
                (v1, v2)
            }
        };
        JSValue::Int(a << (b & 0x1f))
    }

    pub const fn sar(self) -> JSValue {
        let (a, b) = match self {
            Self::Int(v1, v2) => (v1, v2),
            Self::Float(v1, v2) => {
                let v1 = js_f64_to_i32(v1);
                let v2 = js_f64_to_i32(v2);
                (v1, v2)
            }
        };
        JSValue::Int(a >> (b & 0x1f))
    }

    pub fn shr(self) -> JSValue {
        let (a, b) = match self {
            Self::Int(v1, v2) => (v1 as u32, v2 as u32),
            Self::Float(v1, v2) => {
                let v1 = js_f64_to_i32(v1) as u32;
                let v2 = js_f64_to_i32(v2) as u32;
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
    /// `val` is consumed
    pub fn new(ctx: &ContextImpl, val: JSValue, allow_float: bool) -> Result<Self, i32> {
        match val {
            JSValue::Int(i) => Ok(UnaryOp::Int(i)),
            JSValue::Float64(f) if allow_float => Ok(UnaryOp::Float(f)),
            _ => {
                if allow_float {
                    match val.to_number(ctx)? {
                        JSValue::Int(i) => Ok(UnaryOp::Int(i)),
                        JSValue::Float64(f) => Ok(UnaryOp::Float(f)),
                        _ => unreachable!(),
                    }
                } else {
                    val.to_i32(ctx).map(Self::Int)
                }
            }
        }
    }

    /// a shortcut for ::new(ctx, op1, false)
    #[inline(always)]
    pub fn new_int(ctx: &ContextImpl, op1: JSValue) -> Result<Self, i32> {
        match op1 {
            JSValue::Int(i) => Ok(Self::Int(i)),
            _ => op1.to_i32(ctx).map(Self::Int),
        }
    }

    pub fn neg(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if unlikely(a == 0) {
                    // -0 cannot be expressed as integer
                    JSValue::Float64(-0.0)
                } else if a == i32::MIN {
                    JSValue::Float64(-(a as f64))
                } else {
                    JSValue::Int(-a)
                }
            }
            UnaryOp::Float(a) => {
                if unlikely(a == 0.0) {
                    if a.is_sign_negative() {
                        JSValue::Int(0)
                    } else {
                        JSValue::Float64(-0.0)
                    }
                } else {
                    JSValue::from(-a)
                }
            }
        }
    }

    pub const fn plus(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => JSValue::Int(a),
            UnaryOp::Float(a) => JSValue::Float64(a),
        }
    }

    pub fn inc(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if unlikely(a == i32::MAX) {
                    JSValue::from(a as u32 + 1)
                } else {
                    JSValue::Int(a + 1)
                }
            }
            UnaryOp::Float(a) => JSValue::Float64(a + 1.0),
        }
    }

    pub fn dec(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if unlikely(a == i32::MIN) {
                    JSValue::from(a as i64 - 1)
                } else {
                    JSValue::Int(a - 1)
                }
            }
            UnaryOp::Float(a) => JSValue::Float64(a - 1.0),
        }
    }

    pub fn not(self) -> JSValue {
        match self {
            Self::Int(a) => JSValue::Int(!a),
            _ => unreachable!(),
        }
    }
}

#[inline]
pub(super) fn op_binary_logic(
    ctx: &ContextImpl,
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
        ctx: &ContextImpl,
        other: JSValue,
    ) -> Result<Option<std::cmp::Ordering>, i32> {
        // fast path
        if let Some(n2) = TwoNumbers::from_values0(*self, other) {
            return Ok(n2.partial_cmp());
        }

        // slow path
        let mut op1 = *self;
        let mut op2 = other;
        loop {
            match (&op1, &op2) {
                // string vs string
                (JSValue::String(s1), JSValue::String(s2)) => {
                    return Ok(Some(s1.content().cmp(s2.content())));
                }

                // object vs any
                (JSValue::Object(o1), JSValue::Object(o2)) => {
                    op1 = o1.to_primitive(ctx, HINT_NUMBER)?;
                    op2 = o2.to_primitive(ctx, HINT_NUMBER)?;
                }
                (JSValue::Object(o), _) => {
                    op1 = o.to_primitive(ctx, HINT_NUMBER)?;
                }
                (_, JSValue::Object(o)) => {
                    op2 = o.to_primitive(ctx, HINT_NUMBER)?;
                }

                _ => {
                    op1 = op1.to_number(ctx)?;
                    op2 = op2.to_number(ctx)?;
                }
            }

            if let Some(n2) = TwoNumbers::from_values0(op1, op2) {
                return Ok(n2.partial_cmp());
            }
        }
    }

    /// arithmatic add of two values.
    pub fn arith_add(ctx: &ContextImpl, mut op1: Self, mut op2: Self) -> JSResult {
        loop {
            match (op1, op2) {
                // path to exit:
                (Self::Int(a), Self::Int(b)) => {
                    return Ok(JSValue::from(
                        a as i64 + b as i64, // result may overflow i32 ranges
                    ));
                }
                (Self::Float64(a), Self::Float64(b)) => {
                    return Ok(JSValue::from(a + b));
                }
                (Self::Int(i), Self::Float64(f)) | (Self::Float64(f), Self::Int(i)) => {
                    return Ok(JSValue::from(i as f64 + f));
                }

                //
                // conversion - don't change following arm order
                //
                (a @ Self::Object(_), b) | (a, b @ Self::Object(_)) => {
                    op1 = a.to_primitive(ctx, HINT_NONE)?;
                    op2 = b.to_primitive(ctx, HINT_NONE)?;
                }
                (a @ JSValue::String(_), b) | (a, b @ JSValue::String(_)) => {
                    return a.to_string(ctx)?.concat(ctx, b).map(JSValue::String);
                }
                (a, b) => {
                    op1 = a.to_number(ctx)?;
                    op2 = b.to_number(ctx)?;
                }
            }
        }
    }

    #[inline(always)]
    pub fn arith_sub(ctx: &ContextImpl, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.sub())
    }

    #[inline(always)]
    pub fn arith_mul(ctx: &ContextImpl, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.mul())
    }

    #[inline(always)]
    pub fn arith_div(ctx: &ContextImpl, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.div())
    }

    #[inline(always)]
    pub fn arith_mod(ctx: &ContextImpl, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.mod_())
    }

    #[inline(always)]
    pub fn arith_pow(ctx: &ContextImpl, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.pow())
    }

    /// test arithmatical equality (no-strict-eq): self == other?
    #[inline(always)]
    pub fn arith_eq(&self, ctx: &ContextImpl, other: Self) -> Result<bool, i32> {
        js_eq(ctx, *self, other)
    }

    /// compare JSValues equality in given mode.
    /// Note: `self` and `other` are not consumed.
    pub(crate) fn strict_eq_with_mode(&self, other: Self, eq_mode: StrictEqMode) -> bool {
        if std::mem::discriminant(self) == std::mem::discriminant(&other) {
            match (*self, other) {
                (JSValue::Float64(a), JSValue::Int(b)) => cmp_f64(a, b as f64, eq_mode),
                (JSValue::Float64(a), JSValue::Float64(b)) => cmp_f64(a, b, eq_mode),
                _ => self.eq(&other),
            }
        } else {
            match (*self, other) {
                // int vs float
                (JSValue::Int(i), JSValue::Float64(f)) | (JSValue::Float64(f), JSValue::Int(i))
                    if f.is_finite() && f.trunc() == 0.0 =>
                {
                    cmp_f64(i as f64, f, eq_mode)
                }
                _ => false,
            }
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
    pub fn sort_cmp(&self, ctx: &ContextImpl, other: Self) -> Result<Ordering, i32> {
        let mut v1 = *self;
        let mut v2 = other;

        loop {
            match (&v1, &v2) {
                (JSValue::Int(a), JSValue::Int(b)) => {
                    return Ok(a.cmp(b));
                }
                (JSValue::Bool(a), JSValue::Bool(b)) => {
                    return Ok(a.cmp(b));
                }
                (JSValue::String(a), JSValue::String(b)) => {
                    return Ok(a.partial_cmp(b).unwrap());
                }
                #[cfg(feature = "builtin_symbol")]
                (JSValue::Symbol(a), JSValue::Symbol(b)) => {
                    // compare atom desc content
                    let s1 = a.desc_string(ctx.rt_ref());
                    let s2 = b.desc_string(ctx.rt_ref());
                    return Ok(s1.content().partial_cmp(s2.content()).unwrap());
                }

                (JSValue::Float64(a), JSValue::Float64(b)) => {
                    return Ok(match (a.is_nan(), b.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        (false, false) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
                    });
                }
                (JSValue::Int(a), JSValue::Float64(b)) => {
                    let d = *a as f64;
                    let a = &d;
                    return Ok(match (a.is_nan(), b.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        (false, false) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
                    });
                }
                (JSValue::Float64(a), JSValue::Int(b)) => {
                    let d = *b as f64;
                    let b = &d;
                    return Ok(match (a.is_nan(), b.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        (false, false) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
                    });
                }

                (JSValue::UNDEFINED, JSValue::UNDEFINED) | (JSValue::NULL, JSValue::NULL) => {
                    return Ok(Ordering::Equal);
                }

                // string is prior to symbol
                #[cfg(feature = "builtin_symbol")]
                (JSValue::String(_), JSValue::Symbol(_)) => {
                    return Ok(Ordering::Less);
                }
                #[cfg(feature = "builtin_symbol")]
                (JSValue::Symbol(_), JSValue::String(_)) => {
                    return Ok(Ordering::Greater);
                }

                // undefined is always the last
                (JSValue::UNDEFINED, _) => {
                    return Ok(Ordering::Greater);
                }
                (_, JSValue::UNDEFINED) => {
                    return Ok(Ordering::Less);
                }

                // null is prior to NaN
                (JSValue::NULL, JSValue::Float64(d)) => {
                    return if d.is_nan() {
                        Ok(Ordering::Less)
                    } else {
                        Ok(0.0.partial_cmp(d).unwrap())
                    };
                }
                (JSValue::Float64(d), JSValue::NULL) => {
                    return if d.is_nan() {
                        Ok(Ordering::Greater)
                    } else {
                        Ok(d.partial_cmp(&0.0).unwrap())
                    };
                }

                // convert
                (JSValue::NULL, _) => {
                    v1 = JSValue::Int(0);
                }
                (_, JSValue::NULL) => {
                    v2 = JSValue::Int(0);
                }
                (a, b) => {
                    v1 = a.to_primitive(ctx, HINT_NUMBER)?;
                    v2 = b.to_primitive(ctx, HINT_NUMBER)?;
                }
            }
        }
    }
}

/// test arith equality (no-strict eq): val1 == val2
fn js_eq(ctx: &ContextImpl, val1: JSValue, val2: JSValue) -> Result<bool, i32> {
    if std::mem::discriminant(&val1) == std::mem::discriminant(&val2) {
        return Ok(val1 == val2);
    } else if let Some(n2) = TwoNumbers::from_values0(val1, val2) {
        // fast path
        return Ok(n2.partial_cmp() == Some(std::cmp::Ordering::Equal));
    }

    // slow path
    let mut op1 = val1;
    let mut op2 = val2;

    let b = loop {
        match (&op1, &op2) {
            // null == undefined
            (JSValue::NULL | JSValue::UNDEFINED, JSValue::NULL | JSValue::UNDEFINED) => {
                break true;
            }

            // null,undefined vs others
            (_, JSValue::NULL | JSValue::UNDEFINED) | (JSValue::NULL | JSValue::UNDEFINED, _) => {
                break false;
            }

            // number vs string
            (JSValue::Int(i), JSValue::String(s)) | (JSValue::String(s), JSValue::Int(i)) => {
                break match s.to_number(ctx) {
                    JSValue::Int(v) => v == *i,
                    JSValue::Float64(v) => v == 0.0 && *i == 0, // -0 == 0
                    _ => false,
                };
            }
            (JSValue::Float64(f), JSValue::String(s))
            | (JSValue::String(s), JSValue::Float64(f)) => {
                break match s.to_number(ctx) {
                    JSValue::Int(v) => v as f64 == *f,
                    JSValue::Float64(v) => v == *f,
                    _ => false,
                };
            }

            // boolean vs string
            (JSValue::Bool(b), JSValue::String(s)) | (JSValue::String(s), JSValue::Bool(b)) => {
                break match s.to_number(ctx) {
                    JSValue::Int(v) => v == *b as i32,
                    JSValue::Float64(v) => v == 0.0 && !(*b), // -0 == false
                    _ => false,
                };
            }

            // object vs object
            (JSValue::Object(o1), JSValue::Object(o2)) => {
                op1 = o1.to_primitive(ctx, HINT_NONE)?;
                op2 = o2.to_primitive(ctx, HINT_NONE)?;
            }

            // object vs number/string/boolean/string
            (
                JSValue::Object(o),
                JSValue::Int(_) | JSValue::Float64(_) | JSValue::Bool(_) | JSValue::String(_),
            ) => {
                op1 = o.to_primitive(ctx, HINT_NONE)?;
            }
            (
                JSValue::Int(_) | JSValue::Float64(_) | JSValue::Bool(_) | JSValue::String(_),
                JSValue::Object(o),
            ) => {
                op2 = o.to_primitive(ctx, HINT_NONE)?;
            }

            // object vs symbol
            #[cfg(feature = "builtin_symbol")]
            (JSValue::Object(o), JSValue::Symbol(_)) => {
                op1 = o.to_primitive(ctx, HINT_NONE)?;
            }
            #[cfg(feature = "builtin_symbol")]
            (JSValue::Symbol(_), JSValue::Object(o)) => {
                op2 = o.to_primitive(ctx, HINT_NONE)?;
            }

            #[cfg(debug_assertions)]
            (
                JSValue::Int(_) | JSValue::Float64(_) | JSValue::Bool(_),
                JSValue::Int(_) | JSValue::Float64(_) | JSValue::Bool(_),
            ) => {
                unreachable!("op1={:?}, op2={:?}, handled in fast path", op1, op2);
            }

            _ => {
                break false;
            }
        }

        if std::mem::discriminant(&op1) == std::mem::discriminant(&op2) {
            break op1 == op2;
        } else if let Some(n2) = TwoNumbers::from_values0(op1, op2) {
            break n2.partial_cmp() == Some(std::cmp::Ordering::Equal);
        }
    };

    Ok(b)
}

/// compare two f64 numbers
fn cmp_f64(d1: f64, d2: f64, eq_mode: StrictEqMode) -> bool {
    match eq_mode {
        StrictEqMode::JS_EQ_STRICT => {
            // NaN return false, +0 == -0
            d1 == d2
        }

        // cmp NaN
        _ if d1.is_nan() || d2.is_nan() => d1.is_nan() == d2.is_nan(),

        StrictEqMode::JS_EQ_SAME_VALUE => {
            // +0 != -0
            let u1 = JSFloat64Union { d: d1 };
            let u2 = JSFloat64Union { d: d2 };
            unsafe { u1.uint64 == u2.uint64 }
        }
        StrictEqMode::JS_EQ_SAME_VALUE_ZERO => {
            // +0 == -0
            d1 == d2
        }
    }
}
