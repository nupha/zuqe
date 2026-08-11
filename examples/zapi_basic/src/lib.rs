//! Zapi — value-type & standard-object demo extension.
//!
//! Authored entirely with the safe `zapi` layer + `zapi-macros` — no raw
//! `extern "C"` FFI in user code. Exercises every JS value type crossing the
//! extension boundary (number / string / symbol / object / array / function /
//! arguments), plus standard host objects (Date, Uint8Array) inside the
//! extension.

use zapi::prelude::*;
use zapi_macros::{GcTrace, zq_class, zq_class_impl, zq_function, zq_method, zq_module};

// ──────────────────────────────────────
// Value introspection & round-trip
// ──────────────────────────────────────

/// `typeOf(x)` → JS `typeof` name ("number", "string", "symbol", ...).
#[zq_function(name = "typeOf")]
fn type_of<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let v = cc.arg(&cx, 0);
    Ok(cx.string(v.type_of(&cx)?.name()))
}

/// `id(x)` → returns the argument unchanged (round-trip for any value).
#[zq_function(name = "id")]
fn id<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let _ = cx;
    Ok(cc.arg(&cx, 0))
}

#[zq_function]
fn add<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let a = cc.arg_as::<f64>(&cx, 0)?;
    let b = cc.arg_as::<f64>(&cx, 1)?;
    Ok(cx.number(a + b))
}

#[zq_function]
fn mul<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let a = cc.arg_as::<f64>(&cx, 0)?;
    let b = cc.arg_as::<f64>(&cx, 1)?;
    Ok(cx.number(a * b))
}

#[zq_function]
fn greet<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let _ = cc;
    Ok(cx.number(42.0))
}

// ──────────────────────────────────────
// String
// ──────────────────────────────────────

/// `echoString(s)` → `"echo:" + s` (reads a string argument, returns a string).
#[zq_function(name = "echoString")]
fn echo_string<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let s = cc.arg_as::<String>(&cx, 0)?;
    Ok(cx.string(&format!("echo:{s}")))
}

/// `strLen(s)` → byte length of the string.
#[zq_function(name = "strLen")]
fn str_len<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let s = cc.arg_as::<String>(&cx, 0)?;
    Ok(cx.number(s.len() as f64))
}

// ──────────────────────────────────────
// Symbol
// ──────────────────────────────────────

/// `makeSymbol(desc)` → `Symbol(desc)`.
#[zq_function(name = "makeSymbol")]
fn make_symbol<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let desc = cc.arg_as::<String>(&cx, 0)?;
    Ok(cx.symbol(&desc))
}

/// `symDesc(sym)` → the symbol's description ("" when absent).
#[zq_function(name = "symDesc")]
fn sym_desc<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let desc = cc.arg(&cx, 0).symbol_description(&cx)?;
    Ok(cx.string(&desc))
}

// ──────────────────────────────────────
// Object
// ──────────────────────────────────────

/// `makeObject()` → a fresh plain object with a few properties (incl. a nested
/// array), built entirely inside the extension.
#[zq_function(name = "makeObject")]
fn make_object<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let _ = cc;
    let obj = cx.object();
    obj.set(&cx, "kind", "demo")?;
    obj.set(&cx, "count", 7i32)?;
    let nested = cx.array(&[cx.bool(true), cx.number(1.5), cx.string("x")]);
    obj.set(&cx, "nested", nested)?;
    Ok(obj)
}

#[zq_function(name = "getProp")]
fn get_prop<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = cc.arg(&cx, 0);
    obj.get(&cx, cc.arg(&cx, 1))
}

#[zq_function(name = "setProp")]
fn set_prop<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = cc.arg(&cx, 0);
    obj.set(&cx, cc.arg(&cx, 1), cc.arg(&cx, 2))?;
    Ok(cx.undefined())
}

#[zq_function(name = "hasProp")]
fn has_prop<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = cc.arg(&cx, 0);
    Ok(cx.bool(obj.has(&cx, cc.arg(&cx, 1))?))
}

#[zq_function(name = "delProp")]
fn del_prop<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = cc.arg(&cx, 0);
    Ok(cx.bool(obj.delete(&cx, cc.arg(&cx, 1))?))
}

// ──────────────────────────────────────
// Array
// ──────────────────────────────────────

/// `makeArray()` → `[1, 2, 3, "four"]` built inside the extension.
#[zq_function(name = "makeArray")]
fn make_array<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let _ = cc;
    let arr = cx.array(&[cx.number(1.0), cx.number(2.0), cx.number(3.0), cx.string("four")]);
    arr.array_push(&cx, cx.bool(true))?;
    Ok(arr)
}

#[zq_function(name = "arrayLen")]
fn array_len<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let len = cc.arg(&cx, 0).array_len(&cx)?;
    Ok(cx.number(len as f64))
}

#[zq_function(name = "arrayGet")]
fn array_get<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let idx = cc.arg_as::<i32>(&cx, 1)?;
    cc.arg(&cx, 0).array_get(&cx, idx as usize)
}

#[zq_function(name = "arraySet")]
fn array_set<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let idx = cc.arg_as::<i32>(&cx, 1)?;
    cc.arg(&cx, 0).array_set(&cx, idx as usize, cc.arg(&cx, 2))?;
    Ok(cx.undefined())
}

// ──────────────────────────────────────
// Function
// ──────────────────────────────────────

/// `callJs(f, ...args)` → invoke `f(...args)` from inside the extension.
#[zq_function(name = "callJs")]
fn call_js<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let f = cc.arg(&cx, 0);
    let mut args: Vec<ZqValue<'a>> = Vec::with_capacity(cc.len().saturating_sub(1));
    for i in 1..cc.len() {
        args.push(cc.arg(&cx, i));
    }
    f.call(&cx, cx.undefined(), &args)
}

// ──────────────────────────────────────
// Standard host objects: Date & Uint8Array
// ──────────────────────────────────────

/// `makeDate(ms)` → `new Date(ms)` (constructor invoked from the extension).
#[zq_function(name = "makeDate")]
fn make_date<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let date = cx.get_global("Date")?;
    cx.call_constructor(date, &[cc.arg(&cx, 0)])
}

/// `dateTime(d)` → `d.getTime()` (method call on a Date from the extension).
#[zq_function(name = "dateTime")]
fn date_time<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let d = cc.arg(&cx, 0);
    let get_time = d.get(&cx, "getTime")?;
    get_time.call(&cx, d, &[])
}

/// `makeU8(n)` → `new Uint8Array(n)`.
#[zq_function(name = "makeU8")]
fn make_u8<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let ctor = cx.get_global("Uint8Array")?;
    cx.call_constructor(ctor, &[cc.arg(&cx, 0)])
}

/// `u8Get(a, i)` → `a[i]`.
#[zq_function(name = "u8Get")]
fn u8_get<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(&cx, 0).get(&cx, cc.arg(&cx, 1))
}

/// `u8Set(a, i, v)` → `a[i] = v`.
#[zq_function(name = "u8Set")]
fn u8_set<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(&cx, 0).set(&cx, cc.arg(&cx, 1), cc.arg(&cx, 2))?;
    Ok(cx.undefined())
}

// ──────────────────────────────────────
// Counter class (from the original demo)
// ──────────────────────────────────────

#[derive(GcTrace)]
struct CounterPayload {
    count: i32,
    label: ZqRawValue,
}

#[zq_class(name = "Counter", payload = CounterPayload)]
struct Counter;

#[zq_class_impl]
impl Counter {
    fn constructor<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let label = cc.arg(&cx, 0).raw();
        Counter::new_instance(&cx, CounterPayload { count: 0, label })
    }

    #[zq_method]
    fn increment<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let c = cc.this(&cx);
        let p = Counter::payload_mut(&cx, c).ok_or_else(|| ZqError::new(ZqStatus::Error, None))?;
        p.count += 1;
        Ok(cx.number(p.count as f64))
    }

    #[zq_method(name = "value")]
    fn value<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let c = cc.this(&cx);
        let p = Counter::payload(&cx, c).ok_or_else(|| ZqError::new(ZqStatus::Error, None))?;
        Ok(cx.number(p.count as f64))
    }

    #[zq_method(name = "label")]
    fn label<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let c = cc.this(&cx);
        let p = Counter::payload(&cx, c).ok_or_else(|| ZqError::new(ZqStatus::Error, None))?;
        Ok(cx.wrap_value(p.label))
    }
}

// ──────────────────────────────────────
// Module
// ──────────────────────────────────────

#[zq_module(
    name = "zapi_demo",
    funcs = [
        type_of, id, add, mul, greet,
        echo_string, str_len,
        make_symbol, sym_desc,
        make_object, get_prop, set_prop, has_prop, del_prop,
        make_array, array_len, array_get, array_set,
        call_js,
        make_date, date_time,
        make_u8, u8_get, u8_set,
    ],
    classes = [Counter]
)]
fn init(_cx: &ZqCtx, _exports: &Exports) -> Result<(), ZqError> {
    Ok(())
}
