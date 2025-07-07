use core::f64;
use std::cmp::Ordering;

use crate::{
    bytecode::OpCode,
    context::JSContext,
    number::JSFloat64Union,
    string::JS_ConcatString,
    value::{HINT_NONE, HINT_NUMBER, JSValue},
};

#[derive(Debug, Clone)]
pub(super) enum BinaryOp {
    Int(i32, i32),
    Float(f64, f64),
    Bool(bool, bool),
    /// at least one string
    HasString(JSValue, JSValue),
}

impl BinaryOp {
    #[inline(always)]
    pub fn new_fast(op1: JSValue, op2: JSValue, allow_float: bool) -> Option<Self> {
        match (op1, op2) {
            (JSValue::Bool(a), JSValue::Bool(b)) => Some(Self::Bool(a, b)),
            (JSValue::Int(a), JSValue::Int(b)) => Some(Self::Int(a, b)),
            (JSValue::Float64(a), JSValue::Float64(b)) if allow_float => Some(Self::Float(a, b)),
            (JSValue::Int(a), JSValue::Float64(b)) if allow_float => Some(Self::Float(a as f64, b)),
            (JSValue::Float64(a), JSValue::Int(b)) if allow_float => Some(Self::Float(a, b as f64)),
            _ => None,
        }
    }

    /// 将两个JSValue创建双目操作(int,float,string)
    /// mode: 0 = allow int, 1 = allow int/float, 2 = allow int/float/string
    /// primitive_hint: Some(_) 使用toPrimitive(), None 不使用toPrimitive()
    pub fn new_slow(
        ctx: &JSContext,
        op1: JSValue,
        op2: JSValue,
        mode: u8,
        primitive_hint: Option<i32>,
    ) -> Result<Self, i32> {
        debug_assert!(mode <= 3, "BinaryOp::new_slow: mode={mode}");

        if mode == 0 {
            let n1 = if let JSValue::Int(n) = op1 {
                n
            } else {
                op1.coerce_to_i32(ctx)?
            };
            let n2 = if let JSValue::Int(n) = op2 {
                n
            } else {
                op2.coerce_to_i32(ctx)?
            };
            Ok(Self::Int(n1, n2))
        } else {
            let mut op1 = op1;
            let mut op2 = op2;

            loop {
                match (&op1, &op2) {
                    (JSValue::Int(a), JSValue::Int(b)) => {
                        break Ok(Self::Int(*a, *b));
                    }
                    (JSValue::Float64(a), JSValue::Float64(b)) => {
                        break Ok(Self::Float(*a, *b));
                    }
                    (JSValue::Int(a), JSValue::Float64(b)) => {
                        break Ok(Self::Float(*a as f64, *b));
                    }
                    (JSValue::Float64(a), JSValue::Int(b)) => {
                        break Ok(Self::Float(*a, *b as f64));
                    }
                    (JSValue::String(_), _) | (_, JSValue::String(_)) if mode == 3 => {
                        break Ok(Self::HasString(op1.clone(), op2.clone()));
                    }

                    (JSValue::Object(_), _) | (_, JSValue::Object(_))
                        if primitive_hint.is_some() =>
                    {
                        let hint = primitive_hint.unwrap();
                        op1 = op1.coerce_to_primitive(ctx, hint)?;
                        op2 = op2.coerce_to_primitive(ctx, hint)?;
                    }
                    _ => {
                        if let Some(hint) = primitive_hint {
                            op1 = op1.coerce_to_primitive(ctx, hint)?;
                            op2 = op2.coerce_to_primitive(ctx, hint)?;
                        } else {
                            op1 = op1.coerce_to_numeric(ctx)?;
                            op2 = op2.coerce_to_numeric(ctx)?;
                        }
                    }
                }
            }
        }
    }

    pub fn add(self, ctx: &JSContext) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => {
                let r = a as i64 + b as i64; // 结果可能超出i32范围
                Ok(JSValue::from(r))
            }
            BinaryOp::Float(a, b) => Ok(JSValue::from(a + b)),
            BinaryOp::Bool(a, b) => Ok(JSValue::Int(a as i32 + b as i32)),
            BinaryOp::HasString(a, b) => unsafe { JS_ConcatString(ctx, a, b).check_exception() },
        }
    }

    pub fn sub(self) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => Ok(JSValue::from(a - b)),
            BinaryOp::Float(a, b) => Ok(JSValue::from(a + b)),
            BinaryOp::Bool(a, b) => Ok(JSValue::Int(a as i32 - b as i32)),
            BinaryOp::HasString(_, _) => Err(-1),
        }
    }

    pub fn mul(self) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => {
                let v = (a as i64) * (b as i64);
                if v == 0 && (a | b) < 0 {
                    Ok(JSValue::Float64(-0.0))
                } else {
                    Ok(JSValue::from(v))
                }
            }
            BinaryOp::Float(a, b) => Ok(JSValue::Float64(a * b)),
            BinaryOp::Bool(a, b) => Ok(JSValue::Int(a as i32 * b as i32)),
            BinaryOp::HasString(_, _) => Err(-1),
        }
    }

    pub fn div(self) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => Ok(JSValue::from((a as f64) / (b as f64))),
            BinaryOp::Float(a, b) => Ok(JSValue::from(a / b)),
            BinaryOp::Bool(a, b) => Ok(if b {
                JSValue::Int(a as i32)
            } else {
                JSValue::Float64(f64::INFINITY)
            }),
            BinaryOp::HasString(a, b) => Err(-1),
        }
    }

    pub fn mod_(self) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => {
                if b == 0 {
                    Ok(JSValue::Float64(f64::NAN))
                } else {
                    Ok(JSValue::from(a % b))
                }
            }
            BinaryOp::Float(a, b) => {
                if b == 0.0 {
                    Ok(JSValue::Float64(f64::NAN))
                } else {
                    // JS mod 遵循IEEE754标准，fmod():
                    let n = (a / b).trunc(); // 向零取整
                    let mut m = a - n * b; // 计算 fmod 结果
                    m = if m == 0.0 && (a < 0.0 || a < 0.0) {
                        -0.0
                    } else {
                        m
                    };
                    Ok(JSValue::from(m))
                }
            }
            BinaryOp::Bool(_, b) => Ok(if b {
                JSValue::Int(0)
            } else {
                JSValue::Float64(f64::NAN)
            }),
            BinaryOp::HasString(a, b) => Err(-1),
        }
    }

    pub fn pow(self) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => {
                if b == 1 || (a == 0 && b == 0) {
                    // pow(0,0)=1
                    Ok(JSValue::Int(1))
                } else if a == 0 {
                    Ok(JSValue::Int(0))
                } else if b > 0 {
                    Ok(JSValue::from((a as i64).pow(b as u32)))
                } else {
                    Ok(JSValue::from((a as f64).powf(b as f64)))
                }
            }
            BinaryOp::Float(a, b) => {
                if b.is_infinite() && a.abs() == 1.0 {
                    // not compatible with IEEE 754
                    Ok(JSValue::Float64(f64::NAN))
                } else {
                    Ok(JSValue::from(a.powf(b)))
                }
            }
            BinaryOp::Bool(_, _) => Ok(JSValue::Int(1)),
            BinaryOp::HasString(a, b) => Err(-1),
        }
    }

    pub fn shl(self) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from((v1 as u32) << (v2 & 0x1f))),
            Self::Bool(v1, v2) => Ok(JSValue::Int((v1 as i32) << (v2 as i32))),
            _ => Err(-1),
        }
    }

    pub fn shr(self) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from((v1 as u32) >> (v2 & 0x1f))),
            Self::Bool(v1, v2) => Ok(JSValue::Int((v1 as i32) >> (v2 as i32))),
            _ => Err(-1),
        }
    }

    pub fn sar(self) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::Int(v1 >> (v2 & 0x1f))),
            Self::Bool(v1, v2) => Ok(JSValue::Int((v1 as i32) >> (v2 as i32))),
            _ => Err(-1),
        }
    }

    pub fn and(self) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from((v1 as u32) & (v2 as u32))),
            Self::Bool(v1, v2) => Ok(JSValue::Int((v1 & v2) as i32)),
            _ => unreachable!(),
        }
    }

    pub fn or(self) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from((v1 as u32) | (v2 as u32))),
            Self::Bool(v1, v2) => Ok(JSValue::Int((v1 | v2) as i32)),
            _ => unreachable!(),
        }
    }

    pub fn xor(self) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from((v1 as u32) ^ (v2 as u32))),
            Self::Bool(v1, v2) => Ok(JSValue::Int((v1 ^ v2) as i32)),
            _ => unreachable!(),
        }
    }
}

macro_rules! rel {
    ($self:expr, $op:pat) => {
        match $self {
            Self::Int(a, b) => matches!(a.cmp(&b), $op),
            Self::Float(a, b) => a.partial_cmp(&b).map_or(false, |c|matches!(c , $op)), // false if NaN
            Self::Bool(a, b) => matches!(a.cmp(&b), $op),
            Self::HasString(s1, s2) => {
                if let (JSValue::String(s1), JSValue::String(s2)) = (s1,s2) {
                    // both is string
                    let b = s1.content().cmp(s2.content());
                    matches!(b, $op)
                } else {
                    false
                }
            }
        }
    };
}

impl BinaryOp {
    pub fn lt(self) -> bool {
        rel!(self, Ordering::Less)
    }
    pub fn lte(self) -> bool {
        rel!(self, Ordering::Less | Ordering::Equal)
    }
    pub fn gt(self) -> bool {
        rel!(self, Ordering::Greater)
    }
    pub fn gte(self) -> bool {
        rel!(self, Ordering::Greater | Ordering::Equal)
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) enum UnaryOp {
    Int(i32),
    Float(f64),
}

impl UnaryOp {
    /// `val` is consumed
    #[inline(always)]
    pub fn new(ctx: &JSContext, val: JSValue, allow_float: bool) -> Result<Self, i32> {
        match val {
            JSValue::Int(i) => Ok(UnaryOp::Int(i)),
            JSValue::Float64(f) if allow_float => Ok(UnaryOp::Float(f)),
            _ => {
                if allow_float {
                    match val.coerce_to_number(ctx)? {
                        JSValue::Int(i) => Ok(UnaryOp::Int(i)),
                        JSValue::Float64(f) => Ok(UnaryOp::Float(f)),
                        _ => unreachable!(),
                    }
                } else {
                    val.coerce_to_i32(ctx).map(Self::Int)
                }
            }
        }
    }

    /// a shortcut for ::new(ctx, op1, false)
    #[inline(always)]
    pub fn new_int(ctx: &JSContext, op1: JSValue) -> Result<Self, i32> {
        match op1 {
            JSValue::Int(i) => Ok(Self::Int(i)),
            _ => op1.coerce_to_i32(ctx).map(Self::Int),
        }
    }

    pub fn neg(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if a == 0 {
                    // -0 cannot be expressed as integer
                    JSValue::from_f64_as_is(-0.0)
                } else if a == i32::MIN {
                    JSValue::from_f64_as_is(-(a as f64))
                } else {
                    JSValue::Int(-a)
                }
            }
            UnaryOp::Float(a) => {
                if a == 0.0 {
                    JSValue::from_f64_as_is(-0.0)
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
                    JSValue::from(a as i64 + 1)
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
pub(super) unsafe fn op_binary_logic(
    ctx: &JSContext,
    opcode: OpCode,
    sp: *mut JSValue,
) -> Result<*mut JSValue, i32> {
    let res = BinaryOp::new_slow(
        ctx,
        unsafe { std::ptr::replace(sp.offset(-2), JSValue::UNDEFINED) },
        unsafe { std::ptr::replace(sp.offset(-1), JSValue::UNDEFINED) },
        0,
        None,
    )
    .and_then(|bop| match opcode {
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

/// op1, op2 are consumed
pub(super) unsafe fn relational(
    ctx: &JSContext,
    op1: JSValue,
    op2: JSValue,
    opcode: OpCode,
) -> Result<bool, i32> {
    let bop = if let Some(b) = BinaryOp::new_fast(op1.clone(), op2.clone(), true) {
        b
    } else {
        BinaryOp::new_slow(ctx, op1, op2, 3, Some(HINT_NUMBER))?
    };

    match opcode {
        OpCode::LT => Ok(bop.lt()),
        OpCode::LTE => Ok(bop.lte()),
        OpCode::GT => Ok(bop.gt()),
        OpCode::GTE => Ok(bop.gte()),
        _ => unreachable!(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum JSStrictEqModeEnum {
    JS_EQ_STRICT,
    JS_EQ_SAME_VALUE,
    JS_EQ_SAME_VALUE_ZERO,
}

impl JSValue {
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
            (JSValue::String(a), JSValue::String(b)) | (JSValue::Symbol(a), JSValue::Symbol(b)) => {
                a.content() == b.content()
            }
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
}

/// test equality for op1 and op2.
pub fn js_eq(ctx: &JSContext, op1: JSValue, op2: JSValue, is_neq: bool) -> Result<bool, i32> {
    if !is_neq && op1 == op2 {
        return Ok(true);
    }

    if let Some(bop) = BinaryOp::new_fast(op1.clone(), op2.clone(), true) {
        // fast path: numbers
        let res = match bop {
            BinaryOp::Int(a, b) => a == b,
            BinaryOp::Float(a, b) => a == b,
            BinaryOp::Bool(a, b) => a == b,
            _ => unreachable!(),
        };
        Ok(res ^ is_neq)
    } else {
        js_eq_slow(ctx, op1, op2, is_neq)
    }
}

/// slow test equality for op1 and op2.
fn js_eq_slow(
    ctx: &JSContext,
    mut op1: JSValue,
    mut op2: JSValue,
    is_neq: bool,
) -> Result<bool, i32> {
    let res = loop {
        let test: bool = match (&op1, &op2) {
            // (JSValue::Int(a), JSValue::Int(b)) => *a == *b,
            // (JSValue::Bool(a), JSValue::Bool(b)) => *a == *b,
            // (JSValue::Float64(a), JSValue::Float64(b)) => *a == *b,
            (JSValue::Int(a), JSValue::Float64(b)) => *a as f64 == *b,
            (JSValue::Float64(a), JSValue::Int(b)) => *a == *b as f64,
            (JSValue::NULL | JSValue::UNDEFINED, JSValue::NULL | JSValue::UNDEFINED) => true,
            (JSValue::Bool(a), _) => {
                op1 = JSValue::Int(*a as i32);
                continue;
            }
            (_, JSValue::Bool(b)) => {
                op2 = JSValue::Int(*b as i32);
                continue;
            }

            (JSValue::String(_), JSValue::Int(_) | JSValue::Float64(_))
            | (JSValue::Int(_) | JSValue::Float64(_), JSValue::String(_)) => {
                op1 = op1.coerce_to_numeric(ctx)?;
                op2 = op2.coerce_to_numeric(ctx)?;
                op1.strict_eq(&op2)
            }

            (
                JSValue::Object(_),
                JSValue::Int(_) | JSValue::Float64(_) | JSValue::String(_) | JSValue::Symbol(_),
            )
            | (
                JSValue::Int(_) | JSValue::Float64(_) | JSValue::String(_) | JSValue::Symbol(_),
                JSValue::Object(_),
            ) => {
                op1 = op1.coerce_to_primitive(ctx, HINT_NONE)?;
                op2 = op2.coerce_to_primitive(ctx, HINT_NONE)?;
                continue;
            }

            (a, b) => match (a, b) {
                (JSValue::String(_), JSValue::String(_))
                | (JSValue::Symbol(_), JSValue::Symbol(_))
                | (JSValue::Object(_), JSValue::Object(_)) => op1.strict_eq(&op2),

                (JSValue::Int(_), JSValue::Int(_))
                | (JSValue::Bool(_), JSValue::Bool(_))
                | (JSValue::Float64(_), JSValue::Float64(_)) => {
                    unreachable!()
                }

                (_, JSValue::NULL | JSValue::UNDEFINED) if a.is_htmldda() => true,
                (JSValue::NULL | JSValue::UNDEFINED, _) if b.is_htmldda() => true,
                _ => false,
            },
        };

        break test; // done
    };

    Ok(res ^ is_neq)
}
