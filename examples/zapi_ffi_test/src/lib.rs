//! Zapi — FFI / high-level API conformance test extension.
//!
//! Every `ZqApi` function pointer is exercised at least once, directly or via
//! the safe `zapi` wrapper. The companion `test_ffi.mjs` drives it and asserts
//! behavior in JS test-case form.
//!
//! Build & run (zjs must be built with the `zapi` feature):
//!   cargo build --manifest-path examples/zapi_ffi_test/Cargo.toml
//!   cargo build --example zjs --features zapi
//!   target/debug/examples/zjs --module examples/zapi_ffi_test/test_ffi.mjs

// `#[zq_function]` generates snake_case marker types for the export manifest.
#![allow(non_camel_case_types)]

use std::cell::Cell;
use std::ffi::c_void;

use zuqe_zapi::ffi::{
    ZqApi, ZqCallInfoS, ZqGcTraceCtxS, ZQ_CONST_VALUE_SLOT_EXCEPTION, ZQ_CONST_VALUE_SLOT_FALSE,
    ZQ_CONST_VALUE_SLOT_INFINITY, ZQ_CONST_VALUE_SLOT_MINUS_INFINITY, ZQ_CONST_VALUE_SLOT_NAN,
    ZQ_CONST_VALUE_SLOT_NULL, ZQ_CONST_VALUE_SLOT_TRUE, ZQ_CONST_VALUE_SLOT_UNDEFINED,
    ZQ_CONST_VALUE_SLOT_ZERO,
};
use zuqe_zapi::{
    AsyncIterResult, Class, ZqAsyncIterable, ZqCallInfo, ZqCtx, ZqError, ZqErrorType, ZqGcTraceCtx,
    ZqRawCtx, ZqRawValue, ZqValue,
};
use zuqe_zapi_macros::{
    zq_class, zq_class_impl, zq_function, zq_method, zq_module, zq_static_method, ZqGcTrace,
};

// ──────────────────────────────────────
// Constants (ZqApi::const_values)
// ──────────────────────────────────────

#[zq_function(name = "kUndefined")]
fn k_undefined<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::undefined())
}

#[zq_function(name = "kNull")]
fn k_null<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::null())
}

#[zq_function(name = "kTrue")]
fn k_true<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::bool(true))
}

#[zq_function(name = "kFalse")]
fn k_false<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::bool(false))
}

#[zq_function(name = "kZero")]
fn k_zero<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::number(0.0))
}

#[zq_function(name = "kNaN")]
fn k_nan<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::number(f64::NAN))
}

#[zq_function(name = "kPosInf")]
fn k_pos_inf<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::number(f64::INFINITY))
}

#[zq_function(name = "kNegInf")]
fn k_neg_inf<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::number(f64::NEG_INFINITY))
}

/// Reads each `const_values` slot raw and counts how many match the safe
/// constructors (should be 9/9).
#[zq_function(name = "constSlots")]
fn const_slots<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let cv = &ZqApi::singleton().const_values;
    let checks = [
        cv[ZQ_CONST_VALUE_SLOT_UNDEFINED] == ZqValue::undefined().raw(),
        cv[ZQ_CONST_VALUE_SLOT_NULL] == ZqValue::null().raw(),
        cv[ZQ_CONST_VALUE_SLOT_TRUE] == ZqValue::bool(true).raw(),
        cv[ZQ_CONST_VALUE_SLOT_FALSE] == ZqValue::bool(false).raw(),
        cv[ZQ_CONST_VALUE_SLOT_ZERO] == ZqValue::number(0.0).raw(),
        cv[ZQ_CONST_VALUE_SLOT_NAN] == ZqValue::number(f64::NAN).raw(),
        cv[ZQ_CONST_VALUE_SLOT_INFINITY] == ZqValue::number(f64::INFINITY).raw(),
        cv[ZQ_CONST_VALUE_SLOT_MINUS_INFINITY] == ZqValue::number(f64::NEG_INFINITY).raw(),
        cv[ZQ_CONST_VALUE_SLOT_EXCEPTION] == ZqValue::exception_value(),
    ];
    Ok(ZqValue::number(checks.iter().filter(|&&b| b).count() as f64))
}

// ──────────────────────────────────────
// Constructors: new_number / new_string / new_symbol / ordinary_object_create /
// create_array_from_list / BigInt / new_error
// ──────────────────────────────────────

#[zq_function(name = "nInt32")]
fn n_int32<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::int32(cc.arg(0).to_i32(cx)?))
}

#[zq_function(name = "nNumber")]
fn n_number<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::number(cc.arg(0).to_f64(cx)?))
}

#[zq_function(name = "nString")]
fn n_string<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let s = cc.arg(0).string_encode_utf8(cx)?;
    cx.new_string(&s)
}

#[zq_function(name = "nSymbol")]
fn n_symbol<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let d = cc.arg(0).string_encode_utf8(cx)?;
    cx.new_symbol(&d)
}

#[zq_function(name = "symbolDesc")]
fn symbol_desc<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let d = cc.arg(0).symbol_description_utf8(cx)?;
    cx.new_string(&d)
}

#[zq_function(name = "nObject")]
fn n_object<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = cx.new_object()?;
    obj.set_str(cx, "kind", cx.new_string("demo")?)?;
    obj.set_str(cx, "n", 3i32)?;
    let arr = cx.new_array(&[ZqValue::int32(1).raw(), ZqValue::int32(2).raw()])?;
    obj.set_str(cx, "arr", arr)?;
    Ok(obj)
}

#[zq_function(name = "nArray")]
fn n_array<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let arr = cx.new_array(&[
        ZqValue::int32(1).raw(),
        ZqValue::int32(2).raw(),
        ZqValue::int32(3).raw(),
    ])?;
    arr.array_push(cx, cx.new_string("four")?)?;
    Ok(arr)
}

#[zq_function(name = "nBigIntI64")]
fn n_bigint_i64<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cx.new_bigint_i64(cc.arg(0).to_i64(cx)?)
}

#[zq_function(name = "nBigIntU64")]
fn n_bigint_u64<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cx.new_bigint_u64(cc.arg(0).to_i64(cx)? as u64)
}

#[zq_function(name = "nBigIntU64Str")]
fn n_bigint_u64_str<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let s = cc.arg(0).string_encode_utf8(cx)?;
    let v: u64 = s
        .parse()
        .map_err(|_| cx.throw_range("not a u64 decimal string"))?;
    cx.new_bigint_u64(v)
}

#[zq_function(name = "nError")]
fn n_error<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let kind = error_kind(cc.arg(0).to_i32(cx)?);
    let msg = cc.arg(1).string_encode_utf8(cx)?;
    cx.new_error(kind, &msg)
}

// ──────────────────────────────────────
// Value query / conversion
// ──────────────────────────────────────

#[zq_function(name = "vType")]
fn v_type<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cx.new_string(cc.arg(0).value_type().name())
}

#[zq_function(name = "vBool")]
fn v_bool<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::bool(cc.arg(0).to_bool(cx)?))
}

#[zq_function(name = "vI64")]
fn v_i64<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::number(cc.arg(0).to_i64(cx)? as f64))
}

#[zq_function(name = "vF64")]
fn v_f64<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::number(cc.arg(0).to_f64(cx)?))
}

#[zq_function(name = "vInt32")]
fn v_int32<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::number(cc.arg(0).to_int32(cx)? as f64))
}

#[zq_function(name = "vToString")]
fn v_to_string<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(0).to_string(cx)
}

#[zq_function(name = "vUtf8")]
fn v_utf8<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let s = cc.arg(0).string_encode_utf8(cx)?;
    cx.new_string(&s)
}

#[zq_function(name = "vStrict")]
fn v_strict<'a>(_cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let a = cc.arg(0);
    let b = cc.arg(1);
    Ok(ZqValue::bool(a.strict_equal(&b)))
}

// ──────────────────────────────────────
// Properties (raw key + UTF-8 key) + get_length_prop
// ──────────────────────────────────────

#[zq_function(name = "pGet")]
fn p_get<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(0).get(cx, cc.arg(1))
}

#[zq_function(name = "pSet")]
fn p_set<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(0).set(cx, cc.arg(1), cc.arg(2))?;
    Ok(ZqValue::undefined())
}

#[zq_function(name = "pHas")]
fn p_has<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::bool(cc.arg(0).has(cx, cc.arg(1))?))
}

#[zq_function(name = "pDel")]
fn p_del<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::bool(cc.arg(0).delete(cx, cc.arg(1))?))
}

#[zq_function(name = "pGetStr")]
fn p_get_str<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(0).get_str(cx, "abc")
}

#[zq_function(name = "pSetStr")]
fn p_set_str<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(0).set_str(cx, "abc", cc.arg(1))?;
    Ok(ZqValue::undefined())
}

#[zq_function(name = "pHasStr")]
fn p_has_str<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::bool(cc.arg(0).has_str(cx, "abc")?))
}

#[zq_function(name = "pDelStr")]
fn p_del_str<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::bool(cc.arg(0).delete_str(cx, "abc")?))
}

#[zq_function(name = "pLen")]
fn p_len<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::number(cc.arg(0).get_length_prop(cx)? as f64))
}

// ──────────────────────────────────────
// Globals: raw-key (raw FFI only), UTF-8 key, and the global object
// ──────────────────────────────────────

#[zq_function(name = "gGetStr")]
fn g_get_str<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let name = cc.arg(0).string_encode_utf8(cx)?;
    cx.get_global_str(&name)
}

#[zq_function(name = "gSetStr")]
fn g_set_str<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let name = cc.arg(0).string_encode_utf8(cx)?;
    Ok(ZqValue::bool(cx.set_global_str(&name, cc.arg(1))?))
}

// ──────────────────────────────────────
// Function calls + raw create_builtin_function
// ──────────────────────────────────────

#[zq_function(name = "cCall")]
fn c_call<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let f = cc.arg(0);
    let mut args = Vec::with_capacity(cc.len().saturating_sub(1));
    for i in 1..cc.len() {
        args.push(cc.arg(i));
    }
    f.call(cx, ZqValue::undefined(), &args)
}

#[zq_function(name = "cCtor")]
fn c_call_ctor<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let ctor = cc.arg(0);
    let mut args = Vec::with_capacity(cc.len().saturating_sub(1));
    for i in 1..cc.len() {
        args.push(cc.arg(i));
    }
    ctor.call_constructor(cx, &args)
}

#[zq_function(name = "cInvoke")]
fn c_invoke<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let target = cc.arg(0);
    let method = cc.arg(1);
    let mut args = Vec::with_capacity(cc.len().saturating_sub(2));
    for i in 2..cc.len() {
        args.push(cc.arg(i));
    }
    target.invoke(cx, method, &args)
}

#[zq_function(name = "cInvokeStr")]
fn c_invoke_str<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let target = cc.arg(0);
    let method = cc.arg(1).string_encode_utf8(cx)?;
    let mut args = Vec::with_capacity(cc.len().saturating_sub(2));
    for i in 2..cc.len() {
        args.push(cc.arg(i));
    }
    target.invoke_str(cx, &method, &args)
}

/// Raw `ZqApi::create_builtin_function`: a callable backed by a fixed body.
unsafe extern "C" fn native_answer(_env: ZqRawCtx, _info: *mut ZqCallInfoS) -> ZqRawValue {
    ZqValue::int32(123).raw()
}

#[zq_function(name = "makeNativeFn")]
fn make_native_fn<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let api = ZqApi::singleton();
    let raw = ZqRawCtx::from(cx);
    let name = cx.new_string("answer")?;
    let f = unsafe { (api.create_builtin_function)(raw, native_answer, name.raw(), 0) };
    if f == ZqValue::exception_value() {
        Err(ZqError)
    } else {
        Ok(ZqValue::from(f))
    }
}

// ──────────────────────────────────────
// Exceptions: throw / exception / take_exception
// ──────────────────────────────────────

#[zq_function(name = "eThrowType")]
fn e_throw_type<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Err(cx.throw_type("thrown TypeError"))
}

#[zq_function(name = "eThrowRange")]
fn e_throw_range<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Err(cx.throw_range("thrown RangeError"))
}

/// Throw an arbitrary value (not an Error) via `throw_value`.
#[zq_function(name = "eThrowValue")]
fn e_throw_value<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cx.throw_value(cc.arg(0));
    Err(ZqError)
}

/// Calls `f()` (which throws), peeks the pending exception, takes it, and
/// returns it. Verifies peek == take and that the pending slot is cleared.
#[zq_function(name = "eTakePeek")]
fn e_take_peek<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let f = cc.arg(0);
    let _ = f.call(cx, ZqValue::undefined(), &[]);

    let peeked = cx.exception();
    if peeked.is_undefined() {
        return Err(cx.throw_internal("expected a pending exception"));
    }
    let taken = cx.take_exception();
    if !taken.strict_equal(&peeked) {
        return Err(cx.throw_internal("take_exception != exception"));
    }
    if !cx.exception().is_undefined() {
        return Err(cx.throw_internal("exception not cleared after take"));
    }
    Ok(taken)
}

// ──────────────────────────────────────
// GC: gc_set_trace_func / gc_mark
// ──────────────────────────────────────

thread_local! {
    static TRACED: Cell<ZqRawValue> = const { Cell::new(0) };
}

#[zq_function(name = "gcTracePin")]
fn gc_trace_pin<'a>(_cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let v = cc.arg(0);
    TRACED.with(|c| c.set(v.raw()));
    Ok(ZqValue::undefined())
}

#[zq_function(name = "gcTraced")]
fn gc_traced<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let raw = TRACED.with(|c| c.get());
    if raw == 0 {
        Ok(ZqValue::undefined())
    } else {
        Ok(ZqValue::from(raw))
    }
}

#[zq_function(name = "gcTraceUnpin")]
fn gc_trace_unpin<'a>(_cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    TRACED.with(|c| c.set(0));
    Ok(ZqValue::undefined())
}

/// Allocates a lot of garbage; with the runtime's tiny GC threshold this sets
/// the collection request, which the next event-loop tick services.
#[zq_function(name = "gcForce")]
fn gc_force<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    for i in 0..20_000i32 {
        let o = cx.new_object()?;
        o.set_str(cx, "i", i)?;
        o.set_str(cx, "s", cx.new_string("garbage-garbage-garbage-garbage")?)?;
    }
    Ok(ZqValue::undefined())
}

/// Global trace registered via `gc_set_trace_func`; marks `TRACED` through
/// `ZqGcTraceCtx::mark` (the `gc_mark` FFI) on every collection.
unsafe extern "C" fn trace_thread_locals(gcx: *mut ZqGcTraceCtxS, _opaque: *mut c_void) {
    let raw = TRACED.with(|c| c.get());
    if raw != 0 {
        let ctx = ZqGcTraceCtx::from(gcx);
        let _ = ctx.mark(raw);
    }
}

// ──────────────────────────────────────
// Async: new_promise_capability, spawn_promise (add_polling_job / new_waker),
// and new_async_iterator
// ──────────────────────────────────────

/// `NewPromiseCapability(%Promise%)` then settle via the returned `[[Resolve]]`.
#[zq_function(name = "capResolve")]
fn cap_resolve<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let cap = cx.new_promise_capability()?;
    cap.resolve
        .call(cx, ZqValue::undefined(), &[cc.arg(0)])?;
    Ok(cap.promise)
}

/// `NewPromiseCapability(%Promise%)` then settle via the returned `[[Reject]]`.
#[zq_function(name = "capReject")]
fn cap_reject<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let cap = cx.new_promise_capability()?;
    cap.reject
        .call(cx, ZqValue::undefined(), &[cc.arg(0)])?;
    Ok(cap.promise)
}

#[zq_function(name = "asyncResolve")]
fn async_resolve<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let x = cc.arg(0).to_i64(cx)?;
    cx.spawn_promise(move || x + 1, |_cx, t| Ok(ZqValue::number(t as f64)))
}

#[zq_function(name = "asyncReject")]
fn async_reject<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cx.spawn_promise(move || (), |cx, _| Err(cx.throw_range("async boom")))
}

struct CountIter {
    n: i32,
    max: i32,
}

impl ZqAsyncIterable for CountIter {
    fn next<'a>(&mut self, _cx: ZqCtx<'a>) -> Result<AsyncIterResult<'a>, ZqError> {
        if self.n >= self.max {
            return Ok(AsyncIterResult::Done);
        }
        let v = self.n;
        self.n += 1;
        Ok(AsyncIterResult::Value(ZqValue::number(v as f64)))
    }
}

#[zq_function(name = "asyncSeq")]
fn async_seq<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let n = cc.arg(0).to_i32(cx)?;
    cx.new_async_iterator(CountIter { n: 0, max: n })
}

// ──────────────────────────────────────
// Native class: register_class / new_instance / get_native_ptr + GC trace
// ──────────────────────────────────────

#[derive(ZqGcTrace)]
struct PointPayload {
    x: f64,
    y: f64,
    tag: ZqRawValue,
}

#[zq_class(name = "Point", payload = PointPayload)]
struct Point;

#[zq_class_impl]
impl Point {
    fn constructor<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let x = cc.arg(0).to_f64(cx)?;
        let y = cc.arg(1).to_f64(cx)?;
        let tag = cc.arg(2).raw();
        Point::new_instance(cx, PointPayload { x, y, tag })
    }

    #[zq_method]
    fn sum<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = Point::payload(cx, cc.this()).ok_or_else(|| cx.throw_type("not a Point"))?;
        Ok(ZqValue::number(p.x + p.y))
    }

    #[zq_method(name = "tag")]
    fn tag<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let p = Point::payload(cx, cc.this()).ok_or_else(|| cx.throw_type("not a Point"))?;
        Ok(ZqValue::from(p.tag))
    }

    #[zq_static_method(name = "isPoint")]
    fn is_point<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        Ok(ZqValue::bool(Point::payload(cx, cc.arg(0)).is_some()))
    }
}

/// `instanceof_operator`: JS `val instanceof Point`, routed through the class def.
#[zq_function(name = "isPointViaClass")]
fn is_point_via_class<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let class: Class<'_, Point> = unsafe { Class::from_def(Point::class_def()) };
    Ok(ZqValue::bool(cx.is_instance_of(cc.arg(0), &class)?))
}

// ──────────────────────────────────────
// Module
// ──────────────────────────────────────

#[zq_module(
    name = "zapi_ffi_test",
    exports = [
        k_undefined, k_null, k_true, k_false, k_zero, k_nan, k_pos_inf, k_neg_inf, const_slots,
        n_int32, n_number, n_string, n_symbol, symbol_desc, n_object, n_array,
        n_bigint_i64, n_bigint_u64, n_bigint_u64_str, n_error,
        v_type, v_bool, v_i64, v_f64, v_int32, v_to_string, v_utf8, v_strict,
        p_get, p_set, p_has, p_del, p_get_str, p_set_str, p_has_str, p_del_str, p_len,
        g_get_str, g_set_str,
        c_call, c_call_ctor, c_invoke, c_invoke_str, make_native_fn,
        e_throw_type, e_throw_range, e_throw_value, e_take_peek,
        gc_trace_pin, gc_traced, gc_trace_unpin, gc_force,
        async_resolve, async_reject, async_seq,
        cap_resolve, cap_reject,
        Point, is_point_via_class,
    ]
)]
fn module_init(cx: &ZqCtx) -> Result<(), ZqError> {
    cx.gc_set_trace_func(Some(trace_thread_locals), std::ptr::null_mut())
}

fn error_kind(code: i32) -> ZqErrorType {
    match code {
        0 => ZqErrorType::Eval,
        1 => ZqErrorType::Range,
        2 => ZqErrorType::Reference,
        3 => ZqErrorType::Syntax,
        4 => ZqErrorType::Type,
        5 => ZqErrorType::Uri,
        6 => ZqErrorType::Internal,
        _ => ZqErrorType::Aggregate,
    }
}
