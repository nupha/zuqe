use std::cmp::Ordering;

use crate::{
    bytecode::OpCode,
    context::JSContext,
    error::JSResult,
    number::JSFloat64Union,
    string::JS_ConcatString,
    value::{HINT_NONE, HINT_NUMBER, JSValue},
};

#[derive(Debug, Clone)]
pub(super) enum TwoNumbers {
    Int(i32, i32),
    Float(f64, f64),
}

impl TwoNumbers {
    /// if value is not int, bool or float, and force_convert is true, try convert value to number.
    pub fn from_values(
        ctx: &JSContext,
        val1: JSValue,
        val2: JSValue,
        force_convert: bool,
    ) -> Result<Self, i32> {
        let mut op1 = val1;
        let mut op2 = val2;

        loop {
            match (op1, op2) {
                (JSValue::Int(a), JSValue::Int(b)) => {
                    return Ok(Self::Int(a, b));
                }
                (JSValue::Bool(a), JSValue::Bool(b)) => {
                    return Ok(Self::Int(a as i32, b as i32));
                }
                (JSValue::Int(a), JSValue::Bool(b)) => {
                    return Ok(Self::Int(a, b as i32));
                }
                (JSValue::Bool(a), JSValue::Int(b)) => {
                    return Ok(Self::Int(a as i32, b));
                }
                (JSValue::Float64(a), JSValue::Float64(b)) => {
                    return Ok(Self::Float(a, b));
                }
                (JSValue::Int(a), JSValue::Float64(b)) => {
                    return Ok(Self::Float(a as f64, b));
                }
                (JSValue::Float64(a), JSValue::Int(b)) => {
                    return Ok(Self::Float(a, b as f64));
                }
                (JSValue::Bool(a), JSValue::Float64(b)) => {
                    return Ok(Self::Float(a as i32 as f64, b));
                }
                (JSValue::Float64(a), JSValue::Bool(b)) => {
                    return Ok(Self::Float(a, b as i32 as f64));
                }

                (v1, v2) if force_convert => {
                    op1 = v1.to_number(ctx)?;
                    op2 = v2.to_number(ctx)?;
                }

                _ => {
                    return Err(-1); // don't convert to number
                }
            }
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
            TwoNumbers::Int(a, b) => {
                JSValue::from(
                    a as i64 + b as i64, // result may overflow i32 ranges
                )
            }
            TwoNumbers::Float(a, b) => JSValue::from(a + b),
        }
    }

    pub fn sub(self) -> JSValue {
        match self {
            TwoNumbers::Int(a, b) => JSValue::from(a - b),
            TwoNumbers::Float(a, b) => JSValue::from(a - b),
        }
    }

    pub fn mul(self) -> JSValue {
        match self {
            TwoNumbers::Int(a, b) => {
                let n = (a as i64) * (b as i64);
                if n == 0 && (a | b) < 0 {
                    JSValue::Float64(-0.0)
                } else {
                    JSValue::from(n)
                }
            }
            TwoNumbers::Float(a, b) => JSValue::Float64(a * b),
        }
    }

    pub fn div(self) -> JSValue {
        match self {
            TwoNumbers::Int(a, b) => JSValue::from((a as f64) / (b as f64)),
            TwoNumbers::Float(a, b) => JSValue::from(a / b),
        }
    }

    pub fn mod_(self) -> JSValue {
        match self {
            TwoNumbers::Int(a, b) => {
                if b == 0 {
                    JSValue::Float64(f64::NAN)
                } else {
                    JSValue::from(a % b)
                }
            }
            TwoNumbers::Float(a, b) => {
                if b == 0.0 {
                    JSValue::Float64(f64::NAN)
                } else {
                    // IEEE754 fmod()
                    let n = (a / b).trunc(); // 向零取整
                    let mut m = a - n * b; // 计算 fmod 结果
                    m = if m == 0.0 && (a < 0.0 || a < 0.0) {
                        -0.0
                    } else {
                        m
                    };
                    JSValue::from(m)
                }
            }
        }
    }

    pub fn pow(self) -> JSValue {
        match self {
            TwoNumbers::Int(a, b) => {
                if b == 1 || (a == 0 && b == 0) {
                    // pow(0,0)=1
                    JSValue::Int(1)
                } else if a == 0 {
                    JSValue::Int(0)
                } else if b > 0 {
                    let v = (a as i64).pow(b as u32);
                    JSValue::from(v)
                } else {
                    JSValue::from((a as f64).powf(b as f64))
                }
            }
            TwoNumbers::Float(a, b) => {
                if !b.is_finite() && a.abs() == 1.0 {
                    // not compatible with IEEE 754
                    JSValue::Float64(f64::NAN)
                } else {
                    JSValue::from(a.powf(b))
                }
            }
        }
    }

    pub fn shl(self) -> JSValue {
        match self {
            Self::Int(v1, v2) => JSValue::from((v1 as u32) << (v2 & 0x1f)),
            Self::Float(v1, v2) => JSValue::from((v1.trunc() as u32) << (v2.trunc() as u32 & 0x1f)),
        }
    }

    pub fn shr(self) -> JSValue {
        match self {
            Self::Int(v1, v2) => JSValue::from((v1 as u32) >> (v2 & 0x1f)),
            Self::Float(v1, v2) => JSValue::from((v1.trunc() as u32) >> (v2.trunc() as u32 & 0x1f)),
        }
    }

    pub fn sar(self) -> JSValue {
        self.shr()
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
    pub fn new(ctx: &JSContext, val: JSValue, allow_float: bool) -> Result<Self, i32> {
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
    pub fn new_int(ctx: &JSContext, op1: JSValue) -> Result<Self, i32> {
        match op1 {
            JSValue::Int(i) => Ok(Self::Int(i)),
            _ => op1.to_i32(ctx).map(Self::Int),
        }
    }

    pub fn neg(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if a == 0 {
                    // -0 cannot be expressed as integer
                    JSValue::Float64(-0.0)
                } else if a == i32::MIN {
                    JSValue::Float64(-(a as f64))
                } else {
                    JSValue::Int(-a)
                }
            }
            UnaryOp::Float(a) => {
                if a == 0.0 {
                    JSValue::Float64(-0.0)
                } else {
                    JSValue::from(-a)
                }
            }
        }
    }

    pub fn plus(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => JSValue::Int(a),
            UnaryOp::Float(a) => JSValue::Float64(a),
        }
    }

    pub fn inc(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if a == i32::MAX {
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
                if a == i32::MIN {
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

#[inline(always)]
pub(super) fn op_binary_logic(
    ctx: &JSContext,
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
        OpCode::OR => bop.or(),
        OpCode::XOR => bop.xor(),
        _ => unreachable!(),
    })?;

    unsafe {
        std::ptr::replace(sp.offset(-2), res);
        Ok(sp.offset(-1))
    }
}

// /// compare op1, op2
// pub(super) fn relational(
//     ctx: &JSContext,
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
pub enum JSStrictEqModeEnum {
    JS_EQ_STRICT,
    JS_EQ_SAME_VALUE,
    JS_EQ_SAME_VALUE_ZERO,
}

impl JSValue {
    /// arithmatic compare of two values.
    /// returns None only if value converted to NaN
    pub fn arith_compare(
        &self,
        ctx: &JSContext,
        other: &Self,
    ) -> Result<Option<std::cmp::Ordering>, i32> {
        match (self, other) {
            (JSValue::String(s1), JSValue::String(s2)) => {
                // string compare
                Ok(Some(s1.content().cmp(s2.content())))
            }
            _ => {
                let op = TwoNumbers::from_values(ctx, self.clone(), other.clone(), true)?;
                Ok(op.partial_cmp())
            }
        }
    }

    /// arithmatic add of two values.
    pub fn arith_add(ctx: &JSContext, mut op1: Self, mut op2: Self) -> JSResult {
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
                // conversion
                // don't change following arm order
                //
                (a @ Self::Object(_), b) | (a, b @ Self::Object(_)) => {
                    op1 = a.to_primitive(ctx, HINT_NONE)?;
                    op2 = b.to_primitive(ctx, HINT_NONE)?;
                }
                (a @ JSValue::String(_), b) | (a, b @ JSValue::String(_)) => {
                    // add string
                    return JS_ConcatString(ctx, a, b).map(JSValue::String);
                }
                (a, b) => {
                    op1 = a.to_number(ctx)?;
                    op2 = b.to_number(ctx)?;
                }
            }
        }
    }

    #[inline(always)]
    pub fn arith_sub(ctx: &JSContext, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.sub())
    }

    #[inline(always)]
    pub fn arith_mul(ctx: &JSContext, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.mul())
    }

    #[inline(always)]
    pub fn arith_div(ctx: &JSContext, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.div())
    }

    #[inline(always)]
    pub fn arith_mod(ctx: &JSContext, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.mod_())
    }

    #[inline(always)]
    pub fn arith_pow(ctx: &JSContext, op1: Self, op2: Self) -> JSResult {
        TwoNumbers::from_values(ctx, op1, op2, true).map(|bop| bop.pow())
    }

    /// compare JSValues equality in given mode.
    /// Note: `self` and `other` are not consumed.
    pub fn strict_eq_with_mode(&self, other: &JSValue, eq_mode: JSStrictEqModeEnum) -> bool {
        let cmp_f64 = |d1: f64, d2: f64| -> bool {
            if eq_mode >= JSStrictEqModeEnum::JS_EQ_SAME_VALUE {
                if d1.is_nan() || d2.is_nan() {
                    d1.is_nan() == d2.is_nan()
                } else if eq_mode == JSStrictEqModeEnum::JS_EQ_SAME_VALUE_ZERO {
                    // +0 == -0
                    d1 == d2
                } else {
                    // +0 != -0
                    let u1 = JSFloat64Union { d: d1 };
                    let u2 = JSFloat64Union { d: d2 };
                    unsafe { u1.uint64 == u2.uint64 }
                }
            } else {
                d1 == d2 // if NaN return false and +0 == -0
            }
        };

        match (self, other) {
            (JSValue::Int(a), JSValue::Int(b)) => *a == *b,
            (JSValue::Bool(a), JSValue::Bool(b)) => *a == *b,
            (JSValue::NULL, JSValue::NULL) | (JSValue::UNDEFINED, JSValue::UNDEFINED) => true,
            (JSValue::String(a), JSValue::String(b)) => a.content() == b.content(),
            (JSValue::Symbol(a), JSValue::Symbol(b)) => a.atom_info() == b.atom_info(),
            (JSValue::Object(a), JSValue::Object(b)) => a == b,
            (JSValue::Int(a), JSValue::Float64(b)) => cmp_f64(*a as f64, *b),
            (JSValue::Float64(a), JSValue::Int(b)) => cmp_f64(*a, *b as f64),
            (JSValue::Float64(a), JSValue::Float64(b)) => cmp_f64(*a, *b),
            _ => false,
        }
    }

    /// compare JSValues equality.
    /// both `self` and `other` are not consumed.
    #[inline(always)]
    pub fn strict_eq(&self, other: &JSValue) -> bool {
        self.strict_eq_with_mode(other, JSStrictEqModeEnum::JS_EQ_STRICT)
    }

    /// compare if two values are same.
    /// both `self` and `other` are not consumed.
    #[inline(always)]
    pub fn is_same(&self, other: &JSValue) -> bool {
        self.strict_eq_with_mode(other, JSStrictEqModeEnum::JS_EQ_SAME_VALUE)
    }

    /// both `self` and `other` are not consumed.
    #[inline(always)]
    pub fn is_same0(&self, other: &JSValue) -> bool {
        self.strict_eq_with_mode(other, JSStrictEqModeEnum::JS_EQ_SAME_VALUE_ZERO)
    }

    /// compare to order value `self` to `other`.
    pub fn sort_cmp(&self, ctx: &JSContext, other: &Self) -> Result<Ordering, i32> {
        let mut v1 = self.clone();
        let mut v2 = other.clone();

        loop {
            match (&v1, &v2) {
                (JSValue::Int(a), JSValue::Int(b)) => {
                    return Ok(a.cmp(b));
                }
                (JSValue::Bool(a), JSValue::Bool(b)) => {
                    return Ok(a.cmp(b));
                }
                (JSValue::String(a), JSValue::String(b))
                | (JSValue::Symbol(a), JSValue::Symbol(b)) => {
                    return Ok(a.0.partial_cmp(&b.0).unwrap());
                }

                // string is prior to symbol
                (JSValue::String(_), JSValue::Symbol(_)) => {
                    return Ok(Ordering::Less);
                }
                (JSValue::Symbol(_), JSValue::String(_)) => {
                    return Ok(Ordering::Greater);
                }

                (JSValue::Float64(a), JSValue::Float64(b)) => {
                    return Ok(match (a.is_nan(), b.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
                    });
                }
                (JSValue::Int(a), JSValue::Float64(b)) => {
                    let d = *a as f64;
                    let a = &d;
                    return Ok(match (a.is_nan(), b.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
                    });
                }
                (JSValue::Float64(a), JSValue::Int(b)) => {
                    let d = *b as f64;
                    let b = &d;
                    return Ok(match (a.is_nan(), b.is_nan()) {
                        (true, true) => Ordering::Equal,
                        (true, false) => Ordering::Greater,
                        (false, true) => Ordering::Less,
                        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
                    });
                }

                (JSValue::UNDEFINED, JSValue::UNDEFINED) | (JSValue::NULL, JSValue::NULL) => {
                    return Ok(Ordering::Equal);
                }

                // undefined is always the last
                (JSValue::UNDEFINED, _) => {
                    return Ok(Ordering::Greater);
                }
                (_, JSValue::UNDEFINED) => {
                    return Ok(Ordering::Less);
                }

                (JSValue::NULL, JSValue::Float64(d)) => {
                    return if d.is_nan() {
                        Ok(Ordering::Less) // null < NaN
                    } else {
                        Ok(0.0.partial_cmp(d).unwrap())
                    };
                }
                (JSValue::Float64(d), JSValue::NULL) => {
                    return if d.is_nan() {
                        Ok(Ordering::Greater) // NaN > null
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

// pub(super) fn arith_binary(ctx: &JSContext, op1: JSValue, op2: JSValue, opcode: OpCode) -> JSValue {
//     if opcode == OpCode::ADD
//         && matches!(
//             (&op1, &op2),
//             (JSValue::String(_), _) | (_, JSValue::String(_))
//         )
//     {
//         // add string
//         unsafe { JS_ConcatString(ctx, op1, op2) }
//     } else {
//         // operation on two numbers
//         NumBinOp::new(ctx, op1, op2)
//             .map(|bop| match opcode {
//                 OpCode::ADD => bop.add(),
//                 OpCode::SUB => bop.sub(),
//                 OpCode::MUL => bop.mul(),
//                 OpCode::DIV => bop.div(),
//                 OpCode::MOD => bop.mod_(),
//                 OpCode::POW => bop.pow(),
//                 _ => unreachable!(),
//             })
//             .unwrap_or(JSValue::EXCEPTION)
//     }
// }

/// test arith equality: op1 == op2
pub fn js_eq(ctx: &JSContext, op1: JSValue, op2: JSValue) -> Result<bool, i32> {
    if op1 == op2 {
        Ok(true)
    } else if let Ok(bop) = TwoNumbers::from_values(ctx, op1.clone(), op2.clone(), false) {
        // fast path
        let res = bop
            .partial_cmp()
            .map_or(false, |r| r == std::cmp::Ordering::Equal);
        Ok(res)
    } else {
        // slow path
        let b = match (&op1, &op2) {
            (JSValue::NULL | JSValue::UNDEFINED, JSValue::NULL | JSValue::UNDEFINED) => true,
            (_, JSValue::NULL | JSValue::UNDEFINED) => false, //op1.is_htmldda(),
            (JSValue::NULL | JSValue::UNDEFINED, _) => false, //op2.is_htmldda(),

            (JSValue::String(s1), JSValue::String(s2)) => s1.content() == s2.content(),
            (JSValue::Symbol(s1), JSValue::Symbol(s2)) => s1.atom_info() == s2.atom_info(),

            // compare object primitive
            (JSValue::Object(o1), JSValue::Object(o2)) => {
                // op1, op2 are non-same objects, compare their primitive
                js_eq(
                    ctx,
                    o1.to_primitive(ctx, HINT_NONE)?,
                    o2.to_primitive(ctx, HINT_NONE)?,
                )?
            }
            (
                JSValue::Object(o),
                v @ (JSValue::Int(_)
                | JSValue::Float64(_)
                | JSValue::Bool(_)
                | JSValue::String(_)
                | JSValue::Symbol(_)),
            )
            | (
                v @ (JSValue::Int(_)
                | JSValue::Float64(_)
                | JSValue::Bool(_)
                | JSValue::String(_)
                | JSValue::Symbol(_)),
                JSValue::Object(o),
            ) => o.to_primitive(ctx, HINT_NONE)?.eq(v),

            (
                JSValue::Int(_) | JSValue::Float64(_) | JSValue::Bool(_),
                JSValue::Int(_) | JSValue::Float64(_) | JSValue::Bool(_),
            ) => {
                unreachable!("op1={op1:?}, op2={op2:?}");
            }

            _ => {
                false
                // let op = TwoNumbers::new(ctx, op1, op2)?;
                // op.partial_cmp()
                //     .map_or(false, |r| r == std::cmp::Ordering::Equal)
            }
        };

        Ok(b)
    }
}

// /// slow test equality for op1 and op2.
// fn js_eq_slow(ctx: &JSContext, mut op1: JSValue, mut op2: JSValue) -> Result<bool, i32> {
//     let res = loop {
//         let test: bool = match (&op1, &op2) {
//             (JSValue::NULL | JSValue::UNDEFINED, JSValue::NULL | JSValue::UNDEFINED) => true,
//             (_, JSValue::NULL | JSValue::UNDEFINED) if op1.is_htmldda() => true,
//             (JSValue::NULL | JSValue::UNDEFINED, _) if op2.is_htmldda() => true,

//             (JSValue::String(s1), JSValue::String(s2)) => s1.content().eq(s2.content()),

//             (
//                 JSValue::Int(_) | JSValue::Float64(_) | JSValue::Bool(_),
//                 JSValue::Int(_) | JSValue::Float64(_) | JSValue::Bool(_),
//             ) => {
//                 unreachable!("op1={op1:?}, op2={op2:?}");
//                 // let op = TwoNumbers::from_numeric(op1, op2, true).unwrap();
//                 // op.partial_cmp()
//                 //     .map_or(false, |r| r == std::cmp::Ordering::Equal)
//             }

//             _ => {
//                 let op = TwoNumbers::new(ctx, op1, op2)?;
//                 op.partial_cmp()
//                     .map_or(false, |r| r == std::cmp::Ordering::Equal)
//             }

//             (JSValue::String(_), JSValue::Int(_) | JSValue::Float64(_))
//             | (JSValue::Int(_) | JSValue::Float64(_), JSValue::String(_)) => {
//                 op1 = op1.coerce_to_number(ctx)?;
//                 op2 = op2.coerce_to_number(ctx)?;
//                 op1.strict_eq(&op2)
//             }

//             (
//                 JSValue::Object(_),
//                 JSValue::Int(_) | JSValue::Float64(_) | JSValue::String(_) | JSValue::Symbol(_),
//             )
//             | (
//                 JSValue::Int(_) | JSValue::Float64(_) | JSValue::String(_) | JSValue::Symbol(_),
//                 JSValue::Object(_),
//             ) => {
//                 op1 = op1.coerce_to_primitive(ctx, HINT_NONE)?;
//                 op2 = op2.coerce_to_primitive(ctx, HINT_NONE)?;
//                 continue;
//             }

//             (a, b) => match (a, b) {
//                 (JSValue::String(_), JSValue::String(_))
//                 | (JSValue::Symbol(_), JSValue::Symbol(_))
//                 | (JSValue::Object(_), JSValue::Object(_)) => op1.strict_eq(&op2),

//                 (JSValue::Int(_), JSValue::Int(_))
//                 | (JSValue::Bool(_), JSValue::Bool(_))
//                 | (JSValue::Float64(_), JSValue::Float64(_)) => {
//                     unreachable!("op1={:?}, op2={:?}", a, b)
//                 }

//                 (_, JSValue::NULL | JSValue::UNDEFINED) if a.is_htmldda() => true,
//                 (JSValue::NULL | JSValue::UNDEFINED, _) if b.is_htmldda() => true,
//                 _ => false,
//             },
//         };

//         break test; // done
//     };

//     Ok(res ^ is_neq)
// }
