use std::cmp::Ordering;

use crate::{
    bytecode::OpCode,
    context::JSContext,
    value::{
        object::JSObject, JSFloat64Union, JSTag, JSValue, JSValueConst, JS_ConcatString,
        JS_FreeValue, JS_IsHTMLDDA, HINT_NONE, HINT_NUMBER, JS_NAN, JS_TAG_INT,
        JS_VALUE_GET_FLOAT64, JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_PTR,
        JS_VALUE_GET_TAG,
    },
};

pub(super) enum BinaryOp {
    Int(i32, i32),
    Float(f64, f64),
    /// at least one string
    HasString(JSValue, JSValue),
}

impl BinaryOp {
    /// value tag check fast path
    #[inline(always)]
    pub unsafe fn new_fast(op1: JSValue, op2: JSValue, allow_float: bool) -> Option<Self> {
        match (JS_VALUE_GET_NORM_TAG(op1), JS_VALUE_GET_NORM_TAG(op2)) {
            (JSTag::Int, JSTag::Int) => {
                Some(Self::Int(JS_VALUE_GET_INT(op1), JS_VALUE_GET_INT(op2)))
            }
            (JSTag::Float64, JSTag::Float64) if allow_float => Some(Self::Float(
                JS_VALUE_GET_FLOAT64(op1),
                JS_VALUE_GET_FLOAT64(op2),
            )),
            (JSTag::Int, JSTag::Float64) if allow_float => Some(Self::Float(
                JS_VALUE_GET_INT(op1) as f64,
                JS_VALUE_GET_FLOAT64(op2),
            )),
            (JSTag::Float64, JSTag::Int) if allow_float => Some(Self::Float(
                JS_VALUE_GET_FLOAT64(op1),
                JS_VALUE_GET_INT(op2) as f64,
            )),
            _ => None,
        }
    }

    /// 将两个JSValue创建双目操作(int,float,string)
    /// mode: 0 = allow int, 1 = allow int/float, 2 = allow int/float/string
    /// primitive_hint: Some(_) 使用toPrimitive(), None 不使用toPrimitive()
    /// Note: op1, op2 are consumed
    pub unsafe fn new_slow(
        ctx: *mut JSContext,
        op1: JSValue,
        op2: JSValue,
        mode: u8,
        primitive_hint: Option<i32>,
    ) -> Result<Self, i32> {
        debug_assert!(mode <= 3, "BinaryOp::new_slow: mode={mode}");

        if mode == 0 {
            let n1 = if JS_VALUE_GET_TAG(op1) == JS_TAG_INT {
                JS_VALUE_GET_INT(op1)
            } else {
                if let Ok(n) = op1.coerce_i32_free(ctx.as_mut().unwrap()) {
                    n
                } else {
                    JS_FreeValue(ctx, op2);
                    return Err(-1);
                }
            };

            let n2 = if JS_VALUE_GET_TAG(op2) == JS_TAG_INT {
                JS_VALUE_GET_INT(op2)
            } else {
                if let Ok(n) = op2.coerce_i32_free(ctx.as_mut().unwrap()) {
                    n
                } else {
                    return Err(-1);
                }
            };

            Ok(Self::Int(n1, n2))
        } else {
            let mut op1 = op1.into_rc((*ctx).rt());
            let mut op2 = op2.into_rc((*ctx).rt());

            loop {
                match (JS_VALUE_GET_NORM_TAG(*op1), JS_VALUE_GET_NORM_TAG(*op2)) {
                    (JSTag::Int, JSTag::Int) => {
                        break Ok(Self::Int(JS_VALUE_GET_INT(*op1), JS_VALUE_GET_INT(*op2)));
                    }
                    (JSTag::Float64, JSTag::Float64) => {
                        break Ok(Self::Float(
                            JS_VALUE_GET_FLOAT64(*op1),
                            JS_VALUE_GET_FLOAT64(*op2),
                        ));
                    }
                    (JSTag::Int, JSTag::Float64) => {
                        break Ok(Self::Float(
                            JS_VALUE_GET_INT(*op1) as f64,
                            JS_VALUE_GET_FLOAT64(*op2),
                        ));
                    }
                    (JSTag::Float64, JSTag::Int) => {
                        break Ok(Self::Float(
                            JS_VALUE_GET_FLOAT64(*op1),
                            JS_VALUE_GET_INT(*op2) as f64,
                        ));
                    }
                    (JSTag::String, _) | (_, JSTag::String) if mode == 3 => {
                        break Ok(Self::HasString(op1.into(), op2.into()));
                    }

                    (JSTag::Object, _) | (_, JSTag::Object) if primitive_hint.is_some() => {
                        let hint = primitive_hint.unwrap();
                        let p1 = if let Ok(v) = op1.coerce_to_primitive(ctx.as_mut().unwrap(), hint)
                        {
                            v.into_rc((*ctx).rt())
                        } else {
                            break Err(-1);
                        };
                        let p2 = if let Ok(v) = op2.coerce_to_primitive(ctx.as_mut().unwrap(), hint)
                        {
                            v.into_rc((*ctx).rt())
                        } else {
                            break Err(-1);
                        };

                        op1 = p1;
                        op2 = p2;
                    }

                    _ => {
                        if let Some(hint) = primitive_hint {
                            let p1 = if let Ok(v) =
                                op1.coerce_to_primitive(ctx.as_mut().unwrap(), hint)
                            {
                                v.into_rc((*ctx).rt())
                            } else {
                                break Err(-1);
                            };

                            let p2 = if let Ok(v) =
                                op2.coerce_to_primitive(ctx.as_mut().unwrap(), hint)
                            {
                                v.into_rc((*ctx).rt())
                            } else {
                                break Err(-1);
                            };

                            op1 = p1;
                            op2 = p2;
                        } else {
                            let n1 = JSValue::from(op1)
                                .coerce_numeric_free(ctx.as_mut().unwrap())?
                                .into_rc((*ctx).rt());
                            let n2 = JSValue::from(op2)
                                .coerce_number_free(ctx.as_mut().unwrap())?
                                .into_rc((*ctx).rt());

                            op1 = n1;
                            op2 = n2;
                        }
                    }
                }
            }
        }
    }

    pub fn add(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => {
                let r = a as i64 + b as i64; // 结果可能超出i32范围
                Ok(JSValue::from_i64(r))
            }
            BinaryOp::Float(a, b) => Ok(JSValue::from_f64(a + b)),
            BinaryOp::HasString(a, b) => {
                let s = unsafe { JS_ConcatString(ctx, a, b) };
                if s.is_exception() {
                    unsafe {
                        JS_FreeValue(ctx, a);
                        JS_FreeValue(ctx, b);
                    }
                    Err(-1)
                } else {
                    Ok(s)
                }
            }
        }
    }

    pub fn sub(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => Ok(JSValue::from_i32(a - b)),
            BinaryOp::Float(a, b) => Ok(JSValue::from_f64(a + b)),
            BinaryOp::HasString(a, b) => {
                unsafe {
                    JS_FreeValue(ctx, a);
                    JS_FreeValue(ctx, b);
                }
                Err(-1)
            }
        }
    }

    pub fn mul(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => {
                let v = (a as i64) * (b as i64);
                if v == 0 && (a | b) < 0 {
                    Ok(JSValue::from_f64_as_is(-0.0)) // -0.0
                } else {
                    Ok(JSValue::from_i64(v))
                }
            }
            BinaryOp::Float(a, b) => Ok(JSValue::from_f64_as_is(a * b)),
            BinaryOp::HasString(a, b) => {
                unsafe {
                    JS_FreeValue(ctx, a);
                    JS_FreeValue(ctx, b);
                }
                Err(-1)
            }
        }
    }

    pub fn div(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => Ok(JSValue::from_f64((a as f64) / (b as f64))),
            BinaryOp::Float(a, b) => Ok(JSValue::from_f64(a / b)),
            BinaryOp::HasString(a, b) => {
                unsafe {
                    JS_FreeValue(ctx, a);
                    JS_FreeValue(ctx, b);
                }
                Err(-1)
            }
        }
    }

    pub fn mod_(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => {
                if b == 0 {
                    Ok(JS_NAN)
                } else {
                    Ok(JSValue::from_i32(a % b))
                }
            }
            BinaryOp::Float(a, b) => {
                if b == 0.0 {
                    Ok(JS_NAN)
                } else {
                    // JS mod 遵循IEEE754标准，fmod():
                    let n = (a / b).trunc(); // 向零取整
                    let mut m = a - n * b; // 计算 fmod 结果
                    m = if m == 0.0 && (a < 0.0 || a < 0.0) {
                        -0.0
                    } else {
                        m
                    };
                    Ok(JSValue::from_f64(m))
                }
            }
            BinaryOp::HasString(a, b) => {
                unsafe {
                    JS_FreeValue(ctx, a);
                    JS_FreeValue(ctx, b);
                }
                Err(-1)
            }
        }
    }

    pub fn pow(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            BinaryOp::Int(a, b) => {
                if b == 1 || (a == 0 && b == 0) {
                    // pow(0,0)=1
                    Ok(JSValue::from_i32(1))
                } else if a == 0 {
                    Ok(JSValue::ZERO)
                } else if b > 0 {
                    Ok(JSValue::from_i64((a as i64).pow(b as u32)))
                } else {
                    Ok(JSValue::from_f64((a as f64).powf(b as f64)))
                }
            }
            BinaryOp::Float(a, b) => {
                if b.is_infinite() && a.abs() == 1.0 {
                    // not compatible with IEEE 754
                    Ok(JS_NAN)
                } else {
                    Ok(JSValue::from_f64(a.powf(b)))
                }
            }
            BinaryOp::HasString(a, b) => {
                unsafe {
                    JS_FreeValue(ctx, a);
                    JS_FreeValue(ctx, b);
                }
                Err(-1)
            }
        }
    }

    pub fn shl(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from_u32((v1 as u32) << (v2 & 0x1f))),
            _ => unreachable!(),
        }
    }

    pub fn shr(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from_u32((v1 as u32) >> (v2 & 0x1f))),
            _ => unreachable!(),
        }
    }

    pub fn sar(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from_i32(v1 >> (v2 & 0x1f))),
            _ => unreachable!(),
        }
    }

    pub fn and(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from_u32((v1 as u32) & (v2 as u32))),
            _ => unreachable!(),
        }
    }

    pub fn or(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from_u32((v1 as u32) | (v2 as u32))),
            _ => unreachable!(),
        }
    }

    pub fn xor(self, ctx: *mut JSContext) -> Result<JSValue, i32> {
        match self {
            Self::Int(v1, v2) => Ok(JSValue::from_u32((v1 as u32) ^ (v2 as u32))),
            _ => unreachable!(),
        }
    }
}

macro_rules! rel {
    ($self:expr, $ctx:expr, $op:pat) => {
        match $self {
            Self::Int(a, b) => matches!(a.cmp(&b), $op),
            Self::Float(a, b) => a.partial_cmp(&b).map_or(false, |c|matches!(c , $op)), // got false if NaN
            Self::HasString(s1, s2) => {
                if JS_VALUE_GET_TAG(s1) == JS_VALUE_GET_TAG(s2) {
                    // both is string
                    unsafe {
                        let b = s1.string().content().cmp(s2.string().content());
                        JS_FreeValue($ctx, s1);
                        JS_FreeValue($ctx, s2);
                        matches!(b, $op)
                    }
                } else {
                    false
                }
            }
        }
    };
}

impl BinaryOp {
    pub fn lt(self, ctx: *mut JSContext) -> bool {
        rel!(self, ctx, Ordering::Less)
    }
    pub fn lte(self, ctx: *mut JSContext) -> bool {
        rel!(self, ctx, Ordering::Less | Ordering::Equal)
    }
    pub fn gt(self, ctx: *mut JSContext) -> bool {
        rel!(self, ctx, Ordering::Greater)
    }
    pub fn gte(self, ctx: *mut JSContext) -> bool {
        rel!(self, ctx, Ordering::Greater | Ordering::Equal)
    }
}

pub(super) enum UnaryOp {
    Int(i32),
    Float(f64),
}

impl UnaryOp {
    /// `val` is consumed
    #[inline(always)]
    pub fn new(ctx: &mut JSContext, val: JSValue, allow_float: bool) -> Result<Self, i32> {
        match JS_VALUE_GET_NORM_TAG(val) {
            JSTag::Int => Ok(UnaryOp::Int(JS_VALUE_GET_INT(val))),
            JSTag::Float64 if allow_float => Ok(UnaryOp::Float(JS_VALUE_GET_FLOAT64(val))),
            _ => {
                if allow_float {
                    let n = val.coerce_number_free(ctx)?;
                    match JS_VALUE_GET_NORM_TAG(n) {
                        JSTag::Int => Ok(UnaryOp::Int(JS_VALUE_GET_INT(n))),
                        JSTag::Float64 => Ok(UnaryOp::Float(JS_VALUE_GET_FLOAT64(val))),
                        _ => unreachable!(),
                    }
                } else {
                    unsafe { val.coerce_i32_free(ctx).map(|i| Self::Int(i)) }
                }
            }
        }
    }

    /// a shortcut for ::new(ctx, op1, false)
    #[inline(always)]
    pub fn new_int(ctx: &mut JSContext, op1: JSValue) -> Result<Self, i32> {
        match JS_VALUE_GET_NORM_TAG(op1) {
            JSTag::Int => Ok(Self::Int(JS_VALUE_GET_INT(op1))),
            _ => unsafe { op1.coerce_i32_free(ctx).map(|i| Self::Int(i)) },
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
                    JSValue::from_i32(-a)
                }
            }
            UnaryOp::Float(a) => {
                if a == 0.0 {
                    JSValue::from_f64_as_is(-0.0)
                } else {
                    JSValue::from_f64(-a)
                }
            }
        }
    }

    pub fn plus(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => JSValue::from_i32(a),
            UnaryOp::Float(a) => JSValue::from_f64_as_is(a),
        }
    }

    pub fn inc(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if a == i32::MAX {
                    JSValue::from_i64(a as i64 + 1)
                } else {
                    JSValue::from_i32(a + 1)
                }
            }
            UnaryOp::Float(a) => JSValue::from_f64_as_is(a + 1.0),
        }
    }

    pub fn dec(self) -> JSValue {
        match self {
            UnaryOp::Int(a) => {
                if a == i32::MIN {
                    JSValue::from_i64(a as i64 - 1)
                } else {
                    JSValue::from_i32(a - 1)
                }
            }
            UnaryOp::Float(a) => JSValue::from_f64_as_is(a - 1.0),
        }
    }

    pub fn not(self) -> JSValue {
        match self {
            Self::Int(a) => JSValue::from_i32(!a),
            _ => unreachable!(),
        }
    }
}

#[inline(always)]
pub(super) unsafe fn op_binary_logic(
    ctx: *mut JSContext,
    opcode: OpCode,
    sp: *mut JSValue,
) -> Result<*mut JSValue, i32> {
    if let Ok(res) =
        BinaryOp::new_slow(ctx, *sp.offset(-2), *sp.offset(-1), 0, None).and_then(|binop| {
            match opcode {
                OpCode::SHL => binop.shl(ctx),
                OpCode::SHR => binop.shr(ctx),
                OpCode::SAR => binop.sar(ctx),
                OpCode::AND => binop.and(ctx),
                OpCode::OR => binop.or(ctx),
                OpCode::XOR => binop.xor(ctx),
                _ => unreachable!(),
            }
        })
    {
        *sp.offset(-2) = res;
        Ok(sp.offset(-1))
    } else {
        *sp.offset(-2) = JSValue::UNDEFINED;
        *sp.offset(-1) = JSValue::UNDEFINED;
        Err(-1)
    }
}

pub(super) unsafe fn relational(
    ctx: *mut JSContext,
    op1: JSValue,
    op2: JSValue,
    opcode: OpCode,
) -> Result<bool, i32> {
    let binop = if let Some(b) = BinaryOp::new_fast(op1, op2, true) {
        b
    } else {
        BinaryOp::new_slow(ctx, op1, op2, 3, Some(HINT_NUMBER))?
    };
    match opcode {
        OpCode::LT => Ok(binop.lt(ctx)),
        OpCode::LTE => Ok(binop.lte(ctx)),
        OpCode::GT => Ok(binop.gt(ctx)),
        OpCode::GTE => Ok(binop.gte(ctx)),
        _ => todo!(),
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
    /// Note: both values keep ref_count unchanged after calling.
    pub fn strict_eq2(self, other: JSValue, eq_mode: JSStrictEqModeEnum) -> bool {
        let tag1 = JS_VALUE_GET_NORM_TAG(self);
        let tag2 = JS_VALUE_GET_NORM_TAG(other);

        let res: bool = match (tag1, tag2) {
            (JSTag::Int, JSTag::Int) | (JSTag::Bool, JSTag::Bool) => {
                JS_VALUE_GET_INT(self) == JS_VALUE_GET_INT(other)
            }
            (JSTag::Null, JSTag::Null) | (JSTag::Undefined, JSTag::Undefined) => true,
            (JSTag::String, JSTag::String) => unsafe {
                self.string().content() == other.string().content()
            },
            (JSTag::Symbol, JSTag::Symbol) => JS_VALUE_GET_PTR(self) == JS_VALUE_GET_PTR(other),
            (JSTag::Object, JSTag::Object) => unsafe {
                self.object() as *mut JSObject == other.object() as *mut JSObject
            },
            (JSTag::Int, JSTag::Float64) | (JSTag::Float64, JSTag::Int) => {
                let d1 = if tag1 == JSTag::Int {
                    JS_VALUE_GET_INT(self) as f64
                } else {
                    JS_VALUE_GET_FLOAT64(self)
                };
                let d2 = if tag2 == JSTag::Int {
                    JS_VALUE_GET_FLOAT64(other)
                } else {
                    JS_VALUE_GET_FLOAT64(other)
                };

                if eq_mode >= JSStrictEqModeEnum::JS_EQ_SAME_VALUE {
                    if d1.is_nan() || d2.is_nan() {
                        d1.is_nan() == d2.is_nan()
                    } else if eq_mode == JSStrictEqModeEnum::JS_EQ_SAME_VALUE_ZERO {
                        d1 == d2 // +0 == -0
                    } else {
                        let u1 = JSFloat64Union { d: d1 };
                        let u2 = JSFloat64Union { d: d2 };
                        unsafe { u1.u64 == u2.u64 } // +0 != -0
                    }
                } else {
                    d1 == d2 // if NaN return false and +0 == -0
                }
            }

            (JSTag::BigInt, _) | (_, JSTag::BigInt) => {
                println!("BigInt is not supported");
                false
            }
            _ => false,
        };

        res
    }

    /// compare JSValues equality. both values keep ref_count unchanged after calling.
    #[inline(always)]
    pub fn strict_eq(self, other: JSValue) -> bool {
        self.strict_eq2(other, JSStrictEqModeEnum::JS_EQ_STRICT)
    }

    /// compare if two values are same. both values keep ref_count unchanged after calling.
    #[inline(always)]
    pub fn is_same(self, other: JSValue) -> bool {
        self.strict_eq2(other, JSStrictEqModeEnum::JS_EQ_SAME_VALUE)
    }

    /// both values keep ref_count unchanged after calling.
    #[inline(always)]
    pub fn is_same0(self, other: JSValueConst) -> bool {
        self.strict_eq2(other, JSStrictEqModeEnum::JS_EQ_SAME_VALUE_ZERO)
    }
}

/// op1, op2 are consumed
#[deprecated(note = "use JSValue::strict_eq2() instead")]
pub unsafe fn js_strict_eq2(
    ctx: *mut JSContext,
    op1: JSValue,
    op2: JSValue,
    eq_mode: JSStrictEqModeEnum,
) -> bool {
    let res = op1.strict_eq2(op2, eq_mode);
    JS_FreeValue(ctx, op1);
    JS_FreeValue(ctx, op2);
    res
}

/// op1, op2 are consumed
#[deprecated(note = "use JSValue::strict_eq() instead")]
pub unsafe fn js_strict_eq(ctx: *mut JSContext, op1: JSValue, op2: JSValue) -> bool {
    let res = op1.strict_eq(op2);
    JS_FreeValue(ctx, op1);
    JS_FreeValue(ctx, op2);
    res
}

#[deprecated(note = "use JSValue::is_same0() instead")]
pub unsafe fn js_same_value_zero(
    ctx: *mut JSContext,
    op1: JSValueConst,
    op2: JSValueConst,
) -> bool {
    op1.is_same0(op2)
}

#[inline(always)]
pub unsafe fn js_eq(ctx: *mut JSContext, sp: *mut JSValue, is_neq: bool) -> Result<bool, i32> {
    let op1 = *sp.offset(-2);
    let op2 = *sp.offset(-1);

    if let Some(bop) = BinaryOp::new_fast(op1, op2, true) {
        // fast path
        let b = match bop {
            BinaryOp::Int(a, b) => a == b,
            BinaryOp::Float(a, b) => a == b,
            _ => unreachable!(),
        };
        Ok(b ^ is_neq)
    } else {
        js_eq_slow(ctx, sp, is_neq)
    }
}

pub unsafe fn js_eq_slow(ctx: *mut JSContext, sp: *mut JSValue, is_neq: bool) -> Result<bool, i32> {
    macro_rules! exception {
        () => {
            (*sp.offset(-2)) = JSValue::UNDEFINED;
            (*sp.offset(-1)) = JSValue::UNDEFINED;
            return Err(-1);
        };
    }
    macro_rules! number_tags {
        () => {
            JSTag::Int | JSTag::Float64 | JSTag::BigInt
        };
    }

    let mut op1 = *sp.offset(-2);
    let mut op2 = *sp.offset(-1);

    let res = loop {
        let tag1 = JS_VALUE_GET_NORM_TAG(op1);
        let tag2 = JS_VALUE_GET_NORM_TAG(op2);

        let b: bool = match (tag1, tag2) {
            (JSTag::Int, JSTag::Int) => JS_VALUE_GET_INT(op1) == JS_VALUE_GET_INT(op2),
            (JSTag::Int, JSTag::Float64) => {
                (JS_VALUE_GET_INT(op1) as f64 == JS_VALUE_GET_FLOAT64(op2)) ^ is_neq
            }
            (JSTag::Float64, JSTag::Int) => {
                (JS_VALUE_GET_FLOAT64(op1) == JS_VALUE_GET_INT(op2) as f64) ^ is_neq
            }
            (JSTag::Float64, JSTag::Float64) => {
                JS_VALUE_GET_FLOAT64(op1) == JS_VALUE_GET_FLOAT64(op2)
            }
            (JSTag::Null, JSTag::Undefined) | (JSTag::Undefined, JSTag::Null) => true,

            (JSTag::Bool, _) => {
                op1 = JSValue::from_i32(JS_VALUE_GET_INT(op1));
                continue; // goto redo
            }
            (_, JSTag::Bool) => {
                op2 = JSValue::from_i32(JS_VALUE_GET_INT(op2));
                continue; // goto redo
            }

            (JSTag::String, number_tags!()) | (number_tags!(), JSTag::String) => {
                if tag1 == JSTag::BigInt || tag2 == JSTag::BigInt {
                    println!("BigInt is not supported");
                    break false;
                } else {
                    if let Ok(n) = op1.coerce_numeric_free(ctx.as_mut().unwrap()) {
                        op1 = n;
                    } else {
                        JS_FreeValue(ctx, op2);
                        exception!();
                    }

                    if let Ok(n) = op2.coerce_numeric_free(ctx.as_mut().unwrap()) {
                        op2 = n;
                    } else {
                        JS_FreeValue(ctx, op1);
                        exception!();
                    }
                }

                let r = op1.strict_eq(op2);
                JS_FreeValue(ctx, op1);
                JS_FreeValue(ctx, op2);
                r
            }

            (JSTag::Object, number_tags!() | JSTag::String | JSTag::Symbol)
            | (number_tags!() | JSTag::String | JSTag::Symbol, JSTag::Object) => {
                op1 = if let Ok(v) = op1.coerce_to_primitive(ctx.as_mut().unwrap(), HINT_NONE) {
                    JS_FreeValue(ctx, op1);
                    v
                } else {
                    JS_FreeValue(ctx, op1);
                    JS_FreeValue(ctx, op2);
                    exception!();
                };

                op2 = if let Ok(v) = op2.coerce_to_primitive(ctx.as_mut().unwrap(), HINT_NONE) {
                    JS_FreeValue(ctx, op2);
                    v
                } else {
                    JS_FreeValue(ctx, op1);
                    JS_FreeValue(ctx, op2);
                    exception!();
                };

                continue;
            }

            _ if tag1 == tag2 => {
                let r = op1.strict_eq(op2);
                JS_FreeValue(ctx, op1);
                JS_FreeValue(ctx, op2);
                r
            }
            _ => {
                let r = (JS_IsHTMLDDA(op1) && matches!(tag2, JSTag::Null | JSTag::Undefined))
                    || (JS_IsHTMLDDA(op2) && matches!(tag1, JSTag::Null | JSTag::Undefined));
                JS_FreeValue(ctx, op1);
                JS_FreeValue(ctx, op2);
                r
            }
        };

        break b; // done
    };

    Ok(res ^ is_neq)
}
