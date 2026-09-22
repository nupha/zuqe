//! Zapi — value-type & standard-object demo extension.
//!
//! Authored entirely with the safe `zapi` layer + `zapi-macros` — no raw
//! `extern "C"` FFI in user code. Exercises every JS value type crossing the
//! extension boundary (number / string / symbol / object / array / function /
//! arguments), plus standard host objects (Date, Uint8Array) inside the
//! extension.
//!
//! Run after building (zjs must be built with the `zapi` feature):
//!   cargo build --release --manifest-path examples/zapi_basic/Cargo.toml
//!   target/release/examples/zjs --module examples/zapi_basic/test_basic.mjs

// `#[zq_function]` generates snake_case marker types for the export manifest.
#![allow(non_camel_case_types)]

use std::{cell::RefCell, collections::HashMap};

use zuqe_zapi::*;
use zuqe_zapi_macros::{
    ZqGcTrace, zq_class, zq_class_impl, zq_export, zq_function, zq_method, zq_module,
    zq_static_method,
};

// ──────────────────────────────────────
// Value introspection & round-trip
// ──────────────────────────────────────

/// `typeOf(x)` → JS `typeof` name ("number", "string", "symbol", ...).
#[zq_function(name = "typeOf")]
fn type_of<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let v = cc.arg(0);
    ZqValue::new_string(cx, v.kind().name())
}

/// `id(x)` → returns the argument unchanged (round-trip for any value).
#[zq_function(name = "id")]
fn id<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let _ = cx;
    Ok(cc.arg(0))
}

#[zq_function]
fn add<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let a = cc.arg(0).to_f64(cx)?;
    let b = cc.arg(1).to_f64(cx)?;
    Ok(ZqValue::number(a + b))
}

#[zq_function]
fn mul<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let a = cc.arg(0).to_f64(cx)?;
    let b = cc.arg(1).to_f64(cx)?;
    Ok(ZqValue::number(a * b))
}

#[zq_function]
fn greet<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let _ = cx;
    Ok(ZqValue::number(42.0))
}

// ──────────────────────────────────────
// String
// ──────────────────────────────────────

/// `echoString(s)` → `"echo:" + s` (reads a string argument, returns a string).
#[zq_function(name = "echoString")]
fn echo_string<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let s = cc.arg(0).to_utf8_string(cx)?;
    ZqValue::new_string(cx, &format!("echo:{s}"))
}

/// `strLen(s)` → byte length of the string.
#[zq_function(name = "strLen")]
fn str_len<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let s = cc.arg(0).to_utf8_string(cx)?;
    Ok(ZqValue::number(s.len() as f64))
}

// ──────────────────────────────────────
// Symbol
// ──────────────────────────────────────

/// `makeSymbol(desc)` → `Symbol(desc)`.
#[zq_function(name = "makeSymbol")]
fn make_symbol<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let desc = cc.arg(0).to_utf8_string(cx)?;
    ZqValue::new_symbol(cx, &desc)
}

/// `symDesc(sym)` → the symbol's description ("" when absent).
#[zq_function(name = "symDesc")]
fn sym_desc<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let desc = cc.arg(0).symbol_description(cx)?;
    ZqValue::new_string(cx, &desc)
}

// ──────────────────────────────────────
// Object
// ──────────────────────────────────────

/// `makeObject()` → a fresh plain object with a few properties (incl. a nested
/// array), built entirely inside the extension.
#[zq_function(name = "makeObject")]
fn make_object<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let obj = ZqValue::new_object(cx)?;
    obj.set_str(cx, "kind", ZqValue::new_string(cx, "demo")?)?;
    obj.set_str(cx, "count", 7i32)?;
    let nested = ZqValue::new_array(
        cx,
        &[
            ZqValue::bool(true),
            ZqValue::number(1.5),
            ZqValue::new_string(cx, "x")?,
        ],
    )?;
    obj.set_str(cx, "nested", nested)?;
    Ok(obj)
}

#[zq_function(name = "getProp")]
fn get_prop<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(0).get(cx, cc.arg(1))
}

#[zq_function(name = "setProp")]
fn set_prop<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(0).set(cx, cc.arg(1), cc.arg(2))?;
    Ok(ZqValue::undefined())
}

#[zq_function(name = "hasProp")]
fn has_prop<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::bool(cc.arg(0).has(cx, cc.arg(1))?))
}

#[zq_function(name = "delProp")]
fn del_prop<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    Ok(ZqValue::bool(cc.arg(0).delete(cx, cc.arg(1))?))
}

// ──────────────────────────────────────
// Array
// ──────────────────────────────────────

/// `makeArray()` → `[1, 2, 3, "four", true]` built inside the extension.
#[zq_function(name = "makeArray")]
fn make_array<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let arr = ZqValue::new_array(
        cx,
        &[
            ZqValue::number(1.0),
            ZqValue::number(2.0),
            ZqValue::number(3.0),
            ZqValue::new_string(cx, "four")?,
        ],
    )?;
    arr.push(cx, ZqValue::bool(true))?;
    Ok(arr)
}

#[zq_function(name = "arrayLen")]
fn array_len<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let len = cc.arg(0).length(cx)?;
    Ok(ZqValue::number(len as f64))
}

#[zq_function(name = "arrayGet")]
fn array_get<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let idx = cc.arg(1).to_i32(cx)?;
    cc.arg(0).get(cx, ZqValue::number(idx as f64))
}

#[zq_function(name = "arraySet")]
fn array_set<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let idx = cc.arg(1).to_i32(cx)?;
    cc.arg(0).set(cx, ZqValue::number(idx as f64), cc.arg(2))?;
    Ok(ZqValue::undefined())
}

// ──────────────────────────────────────
// Function
// ──────────────────────────────────────

/// `callJs(f, ...args)` → invoke `f(...args)` from inside the extension.
#[zq_function(name = "callJs")]
fn call_js<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let f = cc.arg(0);
    let mut args: Vec<ZqValue<'a>> = Vec::with_capacity(cc.len().saturating_sub(1));
    for i in 1..cc.len() {
        args.push(cc.arg(i));
    }
    f.call(cx, ZqValue::undefined(), &args)
}

// ──────────────────────────────────────
// Standard host objects: Date & Uint8Array
// ──────────────────────────────────────

/// `makeDate(ms)` → `new Date(ms)` (constructor invoked from the extension).
#[zq_function(name = "makeDate")]
fn make_date<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let date = cx.get_global_str("Date")?;
    cx.call_constructor(date, &[cc.arg(0)])
}

/// `dateTime(d)` → `d.getTime()` (method call on a Date from the extension).
#[zq_function(name = "dateTime")]
fn date_time<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let d = cc.arg(0);
    let get_time = d.get_str(cx, "getTime")?;
    get_time.call(cx, d, &[])
}

/// `makeU8(n)` → `new Uint8Array(n)`.
#[zq_function(name = "makeU8")]
fn make_u8<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    let ctor = cx.get_global_str("Uint8Array")?;
    cx.call_constructor(ctor, &[cc.arg(0)])
}

/// `u8Get(a, i)` → `a[i]`.
#[zq_function(name = "u8Get")]
fn u8_get<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(0).get(cx, cc.arg(1))
}

/// `u8Set(a, i, v)` → `a[i] = v`.
#[zq_function(name = "u8Set")]
fn u8_set<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
    cc.arg(0).set(cx, cc.arg(1), cc.arg(2))?;
    Ok(ZqValue::undefined())
}

// ──────────────────────────────────────
// Counter class: payload + GC-tracked JS handle + static method
// ──────────────────────────────────────

#[derive(ZqGcTrace)]
struct CounterPayload {
    count: i32,
    label: ZqRawValue,
}

#[zq_class(name = "Counter", payload = CounterPayload)]
struct Counter;

#[zq_class_impl]
impl Counter {
    fn constructor<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let label = cc.arg(0).raw();
        Counter::new_instance(cx, CounterPayload { count: 0, label })
    }

    #[zq_method]
    fn increment<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let c = cc.this();
        let p = Counter::payload_mut(cx, c).ok_or_else(|| cx.throw_type("not a Counter"))?;
        p.count += 1;
        Ok(ZqValue::number(p.count as f64))
    }

    #[zq_method(name = "value")]
    fn value<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let c = cc.this();
        let p = Counter::payload(cx, c).ok_or_else(|| cx.throw_type("not a Counter"))?;
        Ok(ZqValue::number(p.count as f64))
    }

    #[zq_method(name = "label")]
    fn label<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let c = cc.this();
        let p = Counter::payload(cx, c).ok_or_else(|| cx.throw_type("not a Counter"))?;
        Ok(ZqValue::from(p.label))
    }

    /// `Counter.isCounter(v)` — static method (on the constructor).
    #[zq_static_method(name = "isCounter")]
    fn is_counter<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let v = cc.arg(0);
        Ok(ZqValue::bool(Counter::payload(cx, v).is_some()))
    }
}

// ──────────────────────────────────────
// Value export: a module-level constant
// ──────────────────────────────────────

/// `VERSION` → `"0.1.0"` (built lazily at evaluation time).
#[zq_export(name = "VERSION")]
fn version<'a>(cx: ZqCtx<'a>) -> Result<ZqValue<'a>, ZqError> {
    ZqValue::new_string(cx, "0.1.0")
}

// ──────────────────────────────────────
// Per-runtime thread_local resource
// ──────────────────────────────────────

/// Per-runtime state owned by the extension, kept in a thread_local keyed by
/// `ZqRuntime::id`. Created in `module_init` and removed by the runtime-finalize
/// hook when that runtime is torn down. Its `Drop` prints the lifecycle so the
/// path is observable on stderr.
struct RtResource {
    rt_id: usize,
}

impl Drop for RtResource {
    fn drop(&mut self) {
        eprintln!(
            "[zapi_basic] drop     thread_local resource (runtime id={:#x})",
            self.rt_id
        );
    }
}

thread_local! {
    static RT_RESOURCES: RefCell<HashMap<usize, RtResource>> = RefCell::new(HashMap::new());
}

// ──────────────────────────────────────
// Module (exports = explicit manifest; init optional)
// ──────────────────────────────────────

#[zq_module(
    name = "zapi_basic",
    exports = [
        type_of, id, add, mul, greet,
        echo_string, str_len,
        make_symbol, sym_desc,
        make_object, get_prop, set_prop, has_prop, del_prop,
        make_array, array_len, array_get, array_set,
        call_js,
        make_date, date_time,
        make_u8, u8_get, u8_set,
        Counter, version,
    ],
    finalize = module_finalize,
)]
fn module_init(cx: &ZqCtx) -> Result<(), ZqError> {
    // Bind a per-runtime resource to this runtime (keyed by its identity).
    let rt_id = cx.runtime().id();
    RT_RESOURCES.with(|m| {
        m.borrow_mut().insert(rt_id, RtResource { rt_id });
    });
    eprintln!("[zapi_basic] new      thread_local resource (runtime id={rt_id:#x})");
    Ok(())
}

/// Runtime-finalize hook: runs once per runtime at teardown. Removes the
/// resource registered in `module_init`; dropping it prints the "drop" line.
fn module_finalize(rt: *mut ZqRuntime) {
    let rt_id = unsafe { &*rt }.id();
    RT_RESOURCES.with(|m| {
        let removed = m.borrow_mut().remove(&rt_id);
        eprintln!(
            "[zapi_basic] finalize runtime id={rt_id:#x} (resource present={})",
            removed.is_some()
        );
    });
}
