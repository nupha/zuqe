//! Zapi — EventEmitter demo.
//!
//! Authored entirely with the `#[zq_class]` / `#[zq_class_impl]` / `#[zq_module]`
//! macros and the safe `zapi` layer — no raw `extern "C"` FFI in user code.
//!
//! The payload is a real registry: multiple events, multiple handlers per
//! event, `on` / `once` / `off` / `removeAllListeners` / `emit` (Node-style).
//! `#[zq_method]` installs instance methods on the prototype, `#[zq_static_method]`
//! on the constructor. Also show-cases:
//!   * one Rust body exposed under two JS names, dispatched on `magic`
//!   * static **get/set accessors** (a `get`/`set` property on the constructor)
//!   * instance JS handles stored in the payload, traced for GC
//!
//! Run after building (zjs must be built with the `zapi` feature):
//!   cargo build --release --manifest-path examples/zapi_event/Cargo.toml
//!   target/release/examples/zjs --module examples/zapi_event/test_event.mjs

use zuqe_zapi::*;
use zuqe_zapi_macros::{
    ZqGcTrace, zq_class, zq_class_impl, zq_method, zq_module, zq_static_method,
};

use std::cell::Cell;

/// One registered (event, handler) pair. All JS handles are traced for GC.
#[derive(ZqGcTrace)]
struct Listener {
    event: ZqRawValue,
    handler: ZqRawValue,
    once: bool,
}

/// Per-instance registry: one list of listeners per instance.
#[derive(ZqGcTrace)]
struct EmitterPayload {
    listeners: Vec<Listener>,
}

/// Class identity: JS name + payload type. Behavior lives in the impl below.
#[zq_class(name = "EventEmitter", payload = EmitterPayload)]
struct EventEmitter;

/// Constructor + methods. `constructor` is auto-recognized; `#[zq_method]`
/// methods go on the prototype (instance), `#[zq_static_method]` on the
/// constructor (static). Note `ZqCtx` is `Copy`, so it can be passed around
/// freely (e.g. in closures) without borrow friction.
#[zq_class_impl]
impl EventEmitter {
    /// `new EventEmitter()` — boxes an empty payload and wraps it in a new instance.
    fn constructor<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        EventEmitter::new_instance(
            cx,
            EmitterPayload {
                listeners: Vec::new(),
            },
        )
    }

    /// `ee.on(event, handler)` — register a permanent listener.
    #[zq_method]
    fn on<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let ee = cc.this();
        let event = cc.arg(0).raw();
        let handler = cc.arg(1).raw();
        let p = EventEmitter::payload_mut(cx, ee)
            .ok_or_else(|| cx.throw_type("not an EventEmitter"))?;
        p.listeners.push(Listener {
            event,
            handler,
            once: false,
        });
        Ok(ee)
    }

    /// `ee.once(event, handler)` — register a one-time listener (removed after
    /// the first dispatch).
    #[zq_method]
    fn once<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let ee = cc.this();
        let event = cc.arg(0).raw();
        let handler = cc.arg(1).raw();
        let p = EventEmitter::payload_mut(cx, ee)
            .ok_or_else(|| cx.throw_type("not an EventEmitter"))?;
        p.listeners.push(Listener {
            event,
            handler,
            once: true,
        });
        Ok(ee)
    }

    /// `ee.off(event, handler)` — remove the first matching listener.
    #[zq_method]
    fn off<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let ee = cc.this();
        let event = cc.arg(0).raw();
        let handler = cc.arg(1).raw();
        let p = EventEmitter::payload_mut(cx, ee)
            .ok_or_else(|| cx.throw_type("not an EventEmitter"))?;
        if let Some(pos) = p
            .listeners
            .iter()
            .position(|l| l.event == event && l.handler == handler)
        {
            p.listeners.remove(pos);
        }
        Ok(ee)
    }

    /// `ee.removeAllListeners(event)` — remove every listener for `event`.
    #[zq_method(name = "removeAllListeners")]
    fn remove_all<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let ee = cc.this();
        let event = cc.arg(0).raw();
        let p = EventEmitter::payload_mut(cx, ee)
            .ok_or_else(|| cx.throw_type("not an EventEmitter"))?;
        p.listeners.retain(|l| l.event != event);
        Ok(ee)
    }

    /// `ee.emit(event, ...payload)` — call every listener for `event` in
    /// registration order; `once` listeners are removed after dispatch.
    /// Returns `true` if any listener was invoked, `false` otherwise.
    #[zq_method]
    fn emit<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let ee = cc.this();
        let event = cc.arg(0).raw();

        // Snapshot the matching handlers (copies of raw handles), so no payload
        // borrow is held across the handler calls below (handlers may re-enter
        // the emitter).
        let pending: Vec<(ZqRawValue, bool)> = {
            let p = EventEmitter::payload_mut(cx, ee)
                .ok_or_else(|| cx.throw_type("not an EventEmitter"))?;
            p.listeners
                .iter()
                .filter(|l| l.event == event)
                .map(|l| (l.handler, l.once))
                .collect()
        };
        if pending.is_empty() {
            return Ok(ZqValue::bool(false));
        }

        // Forward (event, ...payload) to every handler.
        let mut args: Vec<ZqValue<'a>> = Vec::with_capacity(cc.len());
        args.push(ZqValue::from(event));
        for i in 1..cc.len() {
            args.push(cc.arg(i));
        }
        for &(handler, _) in &pending {
            cx.call_function(ZqValue::from(handler), ZqValue::undefined(), &args)?;
        }

        // Drop one-time listeners after dispatch.
        if pending.iter().any(|&(_, once)| once) {
            let once_handlers: Vec<ZqRawValue> = pending
                .iter()
                .filter(|&&(_, o)| o)
                .map(|&(h, _)| h)
                .collect();
            let p = EventEmitter::payload_mut(cx, ee)
                .ok_or_else(|| cx.throw_type("not an EventEmitter"))?;
            p.listeners
                .retain(|l| l.event != event || !once_handlers.contains(&l.handler));
        }
        Ok(ZqValue::bool(true))
    }

    /// One Rust body serving two JS names, dispatched on `cc.magic()`.
    #[zq_method(name = "fire", magic = 1)]
    #[zq_method(name = "spark", magic = 2)]
    fn magic_demo<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let _ = cx;
        Ok(ZqValue::number(cc.magic() as f64))
    }

    /// Static method: `EventEmitter.isEmitter(val)`.
    #[zq_static_method(name = "isEmitter")]
    fn is_emitter<'a>(cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        let v = cc.arg(0);
        Ok(ZqValue::bool(EventEmitter::payload(cx, v).is_some()))
    }

    // Static accessor backed by a class-level flag (get/set share one body).
    #[zq_static_method(name = "privateMode", get, set)]
    fn private_mode<'a>(_cx: ZqCtx<'a>, cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        if cc.len() > 0 {
            PRIVATE_MODE.with(|c| c.set(cc.arg(0).raw()));
            Ok(ZqValue::undefined())
        } else {
            let v = PRIVATE_MODE.with(|c| c.get());
            if v == 0 {
                Ok(ZqValue::undefined())
            } else {
                Ok(ZqValue::from(v))
            }
        }
    }

    /// Static getter-only property.
    #[zq_static_method(get)]
    fn kind<'a>(cx: ZqCtx<'a>, _cc: ZqCallInfo<'a>) -> Result<ZqValue<'a>, ZqError> {
        ZqValue::new_string(cx, "native")
    }
}

// Class-level flag for the `privateMode` static accessor.
thread_local! {
    static PRIVATE_MODE: Cell<ZqRawValue> = const { Cell::new(0) };
}

#[zq_module(name = "zapi_event", exports = [EventEmitter])]
fn module_init() {}
