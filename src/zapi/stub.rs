// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use once_cell::sync::Lazy;
use zuqe_zapi::ffi::{
    ZQ_ABI_VERSION, ZqApi, ZqCallbackInfoS, ZqClassDef, ZqConvertFn, ZqEnv, ZqEnvS, ZqFieldDef,
    ZqGcTraceCtxS, ZqGcTraceFn, ZqJobRunFn, ZqJobTraceFn, ZqRawValue, ZqWakeHandleS, ZqWorkFn,
};

use crate::{JSValue, ScopedContext, class::ClassID};

use super::host::{push_class_fields, register_zapi_func_magic, zapi_trampoline_fn};

pub(crate) unsafe fn env_from_ctx(ctx: &mut ScopedContext) -> ZqEnvS {
    let rt_raw: *const crate::RuntimeImpl = &*ctx.rt_ptr();
    ZqEnvS {
        ctx: (ctx as *mut ScopedContext).cast::<c_void>(),
        trace: std::ptr::null_mut::<ZqGcTraceCtxS>(),
        rt: rt_raw as *mut c_void,
    }
}

unsafe fn ctx_from_env<'a>(env: ZqEnv) -> &'a mut ScopedContext<'a> {
    unsafe { &mut *((*env).ctx as *mut ScopedContext) }
}

fn symbol_from_desc(ctx: &mut ScopedContext, desc: JSValue) -> Result<JSValue, i32> {
    if desc.is_undefined() {
        ctx.create_symbol(None, crate::atom::AtomKind::Symbol)
    } else {
        let sr = desc.to_string(ctx)?;
        ctx.create_symbol(Some(sr), crate::atom::AtomKind::Symbol)
    }
}

unsafe extern "C" fn zapi_new_number(_env: ZqEnv, v: f64) -> ZqRawValue {
    JSValue::from(v).0
}

unsafe extern "C" fn zapi_new_string_utf8(env: ZqEnv, ptr: *const u8, len: usize) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    match ctx.create_string_from_utf8(bytes.to_vec()) {
        Ok(s) => JSValue::make_string(s).0,
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

unsafe extern "C" fn zapi_new_symbol(env: ZqEnv, desc: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match symbol_from_desc(ctx, JSValue(desc, std::marker::PhantomData)) {
        Ok(v) => v.0,
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

unsafe extern "C" fn zapi_get_symbol_description(env: ZqEnv, sym: ZqRawValue) -> ZqRawValue {
    let _ctx = unsafe { ctx_from_env(env) };
    let sym = JSValue(sym, std::marker::PhantomData);
    let sr = match sym.as_symbol(true) {
        Some(s) => s,
        None => return ZQ_API_TABLE.value_exception,
    };
    if sr.content().is_ident() {
        JSValue::UNDEFINED.0
    } else {
        JSValue::make_string(sr).0
    }
}

unsafe extern "C" fn zapi_get_string_utf8(
    _env: ZqEnv,
    s: ZqRawValue,
    buf: *mut u8,
    buf_len: usize,
    offset: usize,
) -> i32 {
    let sr = match JSValue(s, std::marker::PhantomData).as_string(false) {
        Some(x) => x,
        None => return -1,
    };
    let bytes = sr.content().encode_utf8_bytes(false);
    let start = offset.min(bytes.len());
    let remaining = bytes.len() - start;
    // Measure mode (quickjs `JS_GetStringUtf8` habit): a null `buf` writes
    // nothing and returns the bytes needed from `offset`, so the caller can
    // size its buffer.
    if buf.is_null() {
        return remaining as i32;
    }
    let n = remaining.min(buf_len);
    if n > 0 {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr().add(start), buf, n) };
    }
    n as i32
}

unsafe extern "C" fn zapi_new_object(env: ZqEnv) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match ctx.create_base_object() {
        Ok(o) => JSValue::from(o).0,
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

unsafe extern "C" fn zapi_new_array(env: ZqEnv, len: usize) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match crate::object::Array::create(ctx) {
        Ok(mut arr) => {
            if arr.set_length(ctx, len as u32, true).is_err() {
                return ZQ_API_TABLE.value_exception;
            }
            JSValue::from(arr).0
        }
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

unsafe extern "C" fn zapi_get_property(env: ZqEnv, obj: ZqRawValue, key: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let val = JSValue(key, std::marker::PhantomData);
    let Some(obj) = JSValue(obj, std::marker::PhantomData).as_object() else {
        return ZQ_API_TABLE.value_exception;
    };
    match obj.get_property_value(ctx, val) {
        Ok(v) => v.0,
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

unsafe extern "C" fn zapi_set_property(
    env: ZqEnv,
    obj: ZqRawValue,
    key: ZqRawValue,
    val: ZqRawValue,
    _throw: i32,
    _flags: u32,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let k = JSValue(key, std::marker::PhantomData);
    let v = JSValue(val, std::marker::PhantomData);
    let Some(mut obj) = JSValue(obj, std::marker::PhantomData).as_object() else {
        return ZQ_API_TABLE.value_exception;
    };
    match obj.set_property_value(ctx, k, v, 0) {
        Ok(_) => val,
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

unsafe extern "C" fn zapi_has_property(env: ZqEnv, obj: ZqRawValue, key: ZqRawValue) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(o) = JSValue(obj, std::marker::PhantomData).as_object() else {
        return 0;
    };
    let atom = match JSValue(key, std::marker::PhantomData).to_atom(ctx) {
        Ok(a) => a,
        Err(_) => return 0,
    };
    match o.has_property(ctx, atom) {
        Ok(b) => b as i32,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_delete_property(
    env: ZqEnv,
    obj: ZqRawValue,
    key: ZqRawValue,
    _throw: i32,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(mut o) = JSValue(obj, std::marker::PhantomData).as_object() else {
        return -1;
    };
    let atom = match JSValue(key, std::marker::PhantomData).to_atom(ctx) {
        Ok(a) => a,
        Err(_) => return -1,
    };
    match o.delete_property(ctx, atom, 0) {
        Ok(b) => b as i32,
        Err(_) => -1,
    }
}

unsafe extern "C" fn zapi_array_push(env: ZqEnv, arr: ZqRawValue, val: ZqRawValue) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(mut o) = JSValue(arr, std::marker::PhantomData).as_object() else {
        return -1;
    };
    let len = match o.get_length_prop(ctx) {
        Ok(i) => i,
        Err(_) => return -1,
    };
    if o.set_property_i64(ctx, len, JSValue(val, std::marker::PhantomData))
        .is_err()
    {
        return -1;
    }
    if o.set_property(ctx, crate::atom::Atom::CONST_length, JSValue::from(len + 1))
        .is_err()
    {
        return -1;
    }
    0
}

unsafe extern "C" fn zapi_array_set(
    env: ZqEnv,
    arr: ZqRawValue,
    idx: ZqRawValue,
    val: ZqRawValue,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(mut o) = JSValue(arr, std::marker::PhantomData).as_object() else {
        return -1;
    };
    match o.set_property_value(
        ctx,
        JSValue(idx, std::marker::PhantomData),
        JSValue(val, std::marker::PhantomData),
        0,
    ) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

unsafe extern "C" fn zapi_array_len(env: ZqEnv, arr: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(o) = JSValue(arr, std::marker::PhantomData).as_object() else {
        return ZQ_API_TABLE.value_exception;
    };
    match o.get_length_prop(ctx) {
        Ok(i) => JSValue::from(i as i32).0,
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

unsafe extern "C" fn zapi_get_global(env: ZqEnv, name: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let gobj = ctx.global_obj();
    match gobj.get_property_value(ctx, JSValue(name, std::marker::PhantomData)) {
        Ok(v) => v.0,
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

unsafe extern "C" fn zapi_call_function(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let ctx = unsafe { &mut *((*env).ctx as *mut ScopedContext) };
    let info = unsafe { &*info };

    let target = crate::JSValue(info.target, std::marker::PhantomData);
    let this = crate::JSValue(info.this, std::marker::PhantomData);
    let args: Vec<crate::JSValue> = if info.argc > 0 {
        unsafe { std::slice::from_raw_parts(info.args, info.argc as usize) }
            .iter()
            .map(|&r| crate::JSValue(r, std::marker::PhantomData))
            .collect()
    } else {
        Vec::new()
    };

    match target.call(ctx, this, &args) {
        Ok(v) => v.0,
        // quickjs convention: signal the error with the exception sentinel.
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

fn zapi_gc_trace_bridge(
    payload_opaque: *mut c_void,
    gcx: &mut gc_lite::GcTraceCtx,
    class_id: usize,
) {
    let rt = unsafe { &*(gcx.opaque() as *const crate::RuntimeImpl) };
    // Resolve this class's own ZqClassDef via its ClassID (one entry per class).
    let def_ptr = rt
        .zapi_class_map
        .lock()
        .unwrap()
        .get(&(class_id as u16))
        .copied()
        .unwrap_or(0);
    if def_ptr == 0 {
        return;
    }
    let def = unsafe { &*(def_ptr as *const ZqClassDef) };
    if let Some(gc_trace) = def.gc_trace {
        let mut trace_ctx = ZqGcTraceCtxS {
            inner: (gcx as *mut gc_lite::GcTraceCtx).cast::<c_void>(),
        };
        unsafe { gc_trace(&mut trace_ctx, payload_opaque) };
    }
}

/// Reconstruct a `ZqFieldDef` slice from its `(*const T, usize)` ABI form.
const fn field_slice(ptr: *const ZqFieldDef, len: usize) -> &'static [ZqFieldDef] {
    if ptr.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len) }
    }
}

unsafe extern "C" fn zapi_register_class(env: ZqEnv, class_def: *const ZqClassDef) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let def = unsafe { &*class_def };
    let name_bytes = unsafe { std::ffi::CStr::from_ptr(def.name) };
    let name = match name_bytes.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());

    fn zapi_class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        let o = ctx
            .create_instance_with_proto_payload(
                None,
                crate::object::ObjectPayload::OTHER(crate::class::OtherPayload::new(
                    class_id,
                    std::ptr::null_mut(),
                )),
            )
            .map_err(|_| -1)?;
        ctx.class_slots[class_id as usize].proto = Some(o);

        // Build the constructor (ClassCtorDef equivalent): a constructor-flagged
        // native function wired to the zapi `call` body, plus static methods.
        let class_def = ctx
            .rt()
            .zapi_class_map
            .lock()
            .unwrap()
            .get(&class_id)
            .copied()
            .unwrap_or(0);
        if class_def == 0 {
            return Ok(());
        }
        let def = unsafe { &*(class_def as *const ZqClassDef) };
        let Some(body) = def.call else {
            return Ok(());
        };

        let (_, magic) = register_zapi_func_magic(ctx, body, 0);

        // Instance entries (installed on the prototype) and static entries
        // (installed on the constructor), mirroring zuqe's native classes:
        // proto_entries + ClassCtorDef.entries.
        let mut proto_entries: Vec<crate::FieldDef> = Vec::new();
        let mut static_entries: Vec<crate::FieldDef> = Vec::new();
        push_class_fields(
            ctx,
            &mut proto_entries,
            field_slice(def.proto_entries, def.proto_entries_len),
        )?;
        push_class_fields(
            ctx,
            &mut static_entries,
            field_slice(def.static_entries, def.static_entries_len),
        )?;

        // Install instance methods/accessors on the prototype.
        let mut proto = ctx.class_proto(class_id).ok_or(-1)?;
        proto.install_fields(ctx, &proto_entries)?;

        ctx.init_class_constructor(
            class_id,
            zapi_trampoline_fn,
            def.call_arity as i32,
            magic as i16,
            proto,
            &static_entries,
        )?;
        // The constructor lives in `class_slots[class_id].ctor` (GC-traced).
        // `load_extension` publishes it to the module namespace; it is not
        // installed on `globalThis`.
        Ok(())
    }

    let mut cd = crate::class::ClassDef::new_with(leaked, zapi_class_init);

    if def.gc_trace.is_some() {
        cd = cd.on_gc_trace(zapi_gc_trace_bridge);
    }
    if def.finalize.is_some() {
        // Per-class dispatch: the host forwards this object's ClassID, which
        // maps to its own ZqClassDef in `zapi_class_map` (one entry per class).
        cd = cd.on_dispose(|rt, payload_opaque, class_id, _| {
            let def_ptr = rt
                .zapi_class_map
                .lock()
                .unwrap()
                .get(&(class_id as u16))
                .copied()
                .unwrap_or(0);
            if def_ptr != 0 {
                let def = unsafe { &*(def_ptr as *const ZqClassDef) };
                if std::env::var_os("ZUQE_ZAPI_DEBUG").is_some() {
                    let name = unsafe { std::ffi::CStr::from_ptr(def.name) }
                        .to_string_lossy()
                        .into_owned();
                    eprintln!("[zapi] finalize class={name}");
                }
                if let Some(fin) = def.finalize {
                    unsafe { fin(std::ptr::null_mut(), payload_opaque) };
                }
            }
        });
    }

    let class_id = match ctx.rt_mut().register_class_def(cd) {
        Ok(id) => id,
        Err(_) => {
            return -1;
        }
    };

    // Register the class-def pointer before use_class() so zapi_class_init can
    // resolve the constructor body / static methods during initialization.
    ctx.rt_mut()
        .zapi_class_map
        .lock()
        .unwrap()
        .insert(class_id, class_def as usize);

    if ctx.use_class(class_id).is_err() {
        return -1;
    }

    class_id as i32
}

unsafe extern "C" fn zapi_new_instance(
    env: ZqEnv,
    class_def: *const ZqClassDef,
    payload: *mut c_void,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let class_id = {
        ctx.rt()
            .zapi_class_map
            .lock()
            .unwrap()
            .iter()
            .find(|(_, v)| **v == class_def as usize)
            .map(|(k, _)| *k)
    };
    let Some(cid) = class_id else {
        return ZQ_API_TABLE.value_exception;
    };
    match ctx.create_class_instance_with_opaque(JSValue::UNDEFINED, cid, payload) {
        Ok(obj) => JSValue::from(obj).0,
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

unsafe extern "C" fn zapi_get_native_ptr(_env: ZqEnv, obj: ZqRawValue) -> *mut c_void {
    let val = JSValue(obj, std::marker::PhantomData);
    val.opaque()
        .map(|p| p.as_ptr())
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn zapi_gc_add_trace(
    _env: ZqEnv,
    trace_ctx: *mut ZqGcTraceCtxS,
    val: ZqRawValue,
) -> i32 {
    let gcx: &mut gc_lite::GcTraceCtx =
        unsafe { &mut *((*trace_ctx).inner as *mut gc_lite::GcTraceCtx) };
    let jsval = JSValue(val, std::marker::PhantomData);
    gc_lite::GcTrace::trace(&jsval, gcx);
    0
}

/// One registered global (thread_local) trace: the extension's callback plus
/// its opaque. Leaked for the process lifetime (libraries are kept loaded).
struct ZapiTraceReg {
    cb: ZqGcTraceFn,
    opaque: *mut c_void,
}

/// Runtime-side adapter: `(gcx, opaque)` → extension `(tc, opaque)`.
fn zapi_gc_trace_adapter(gcx: &mut gc_lite::GcTraceCtx, reg: *mut c_void) {
    let reg = unsafe { &*(reg as *const ZapiTraceReg) };
    let mut tc = ZqGcTraceCtxS {
        inner: (gcx as *mut gc_lite::GcTraceCtx).cast::<c_void>(),
    };
    unsafe { (reg.cb)(&mut tc, reg.opaque) };
}

unsafe extern "C" fn zapi_set_gc_trace_func(
    env: ZqEnv,
    cb: Option<ZqGcTraceFn>,
    opaque: *mut c_void,
) -> i32 {
    let Some(cb) = cb else {
        return -1;
    };
    let rt = unsafe { &mut *((*env).rt as *mut crate::RuntimeImpl) };
    let reg = Box::into_raw(Box::new(ZapiTraceReg { cb, opaque }));
    rt.gc_add_trace_callback(zapi_gc_trace_adapter, reg as *mut c_void);
    0
}

/// Extension-side job callbacks: see `ZqJobRunFn` / `ZqJobTraceFn` in ffi.

/// GC node for a queued job: forwards tracing to the extension so JS values
/// held inside `data` stay reachable across collections.
struct ZqJobNode {
    trace: Option<ZqJobTraceFn>,
    data: *mut c_void,
}

impl crate::microtask::JobGcTrace for ZqJobNode {
    fn job_trace_values(&self, gcx: &mut gc_lite::GcTraceCtx) {
        if let Some(f) = self.trace {
            let mut tc = ZqGcTraceCtxS {
                inner: (gcx as *mut gc_lite::GcTraceCtx).cast::<c_void>(),
            };
            unsafe { f(self.data, &mut tc) };
        }
    }
}

/// Take the pending exception and return its raw handle
/// (value_undefined if none). Mirrors ScopedContext::take_exception.
unsafe extern "C" fn zapi_take_exception(env: ZqEnv) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let exc = ctx.take_exception();
    let raw = match exc {
        crate::error::Exception::Error(val) => val.0,
        _ => ZQ_API_TABLE.value_undefined,
    };
    debug_assert_ne!(
        raw, ZQ_API_TABLE.value_exception,
        "take_exception must never return the exception sentinel"
    );
    raw
}

/// Peek the pending exception without consuming it (value_undefined if none).
unsafe extern "C" fn zapi_exception(env: ZqEnv) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let raw = match ctx.rt().exception() {
        crate::error::Exception::Error(val) => val.0,
        _ => ZQ_API_TABLE.value_undefined,
    };
    debug_assert_ne!(
        raw, ZQ_API_TABLE.value_exception,
        "exception() must never return the exception sentinel"
    );
    raw
}
/// Create a pending Promise. Stays GC-rooted (gc_pending_promises) until
/// resolved/rejected — the extension may hold the raw handle across ticks.
unsafe extern "C" fn zapi_new_promise(env: ZqEnv) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match crate::builtins::promise::Promise::create(ctx) {
        Ok((p, _rr)) => {
            // Root while pending; settle() removes it automatically.
            ctx.rt_mut().gc_pending_promises.push(p);
            crate::value::JSValue::from(p).0
        }
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

/// Settle a pending Promise as fulfilled. Double-settle is a no-op.
unsafe extern "C" fn zapi_promise_resolve(env: ZqEnv, promise: ZqRawValue, value: ZqRawValue) {
    let ctx = unsafe { ctx_from_env(env) };
    let pv = crate::value::JSValue(promise, std::marker::PhantomData);
    let v = crate::value::JSValue(value, std::marker::PhantomData);
    if let Ok(mut p) = crate::builtins::promise::Promise::try_from(pv) {
        p.resolve(ctx, v);
    }
}

/// Settle a pending Promise as rejected. Double-settle is a no-op.
unsafe extern "C" fn zapi_promise_reject(env: ZqEnv, promise: ZqRawValue, reason: ZqRawValue) {
    let ctx = unsafe { ctx_from_env(env) };
    let pv = crate::value::JSValue(promise, std::marker::PhantomData);
    let r = crate::value::JSValue(reason, std::marker::PhantomData);
    if let Ok(mut p) = crate::builtins::promise::Promise::try_from(pv) {
        p.reject(ctx, r);
    }
}

/// Concrete wake handle handed to extensions as an opaque `*mut ZqWakeHandleS`.
/// `WakeHandle` is `Send + Sync` (Arc<ParkState>), so `wake` is safe from any
/// worker thread; it wakes a parked JS event loop immediately.
#[repr(transparent)]
pub struct ZqWakeHandle {
    wake: crate::runtime::WakeHandle,
}

/// Create a wake handle bound to the env's runtime event loop (JS thread).
unsafe extern "C" fn zapi_wake_handle_new(env: ZqEnv) -> *mut ZqWakeHandleS {
    let ctx = unsafe { ctx_from_env(env) };
    let h = Box::new(ZqWakeHandle {
        wake: ctx.rt().wake_handle(),
    });
    Box::into_raw(h) as *mut ZqWakeHandleS
}

/// Wake the parked event loop. Thread-safe; cheap when not parked.
unsafe extern "C" fn zapi_wake_handle_wake(h: *mut ZqWakeHandleS) {
    if h.is_null() {
        return;
    }
    let wh = unsafe { &*(h as *const ZqWakeHandle) };
    wh.wake.wake();
}

/// Release a wake handle.
unsafe extern "C" fn zapi_wake_handle_free(h: *mut ZqWakeHandleS) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h as *mut ZqWakeHandle) });
    }
}

/// each tick runs `run(data)` on the JS thread until it reports Done/Err.
unsafe extern "C" fn zapi_add_polling_job(
    env: ZqEnv,
    run: ZqJobRunFn,
    trace: Option<ZqJobTraceFn>,
    data: *mut c_void,
) -> i32 {
    let ctx = ctx_from_env(env);
    ctx.queue_job(
        move |cx| {
            let mut env_s = env_from_ctx(cx);
            let env = &mut env_s as *mut ZqEnvS as ZqEnv;
            match unsafe { run(data, env) } {
                0 => Ok(crate::microtask::JobState::Done),
                1 => Ok(crate::microtask::JobState::Again),
                2 => Ok(crate::microtask::JobState::AgainNoHold),
                _ => Err(-1),
            }
        },
        ZqJobNode { trace, data },
    );
    0
}

unsafe extern "C" fn zapi_get_value_type(_env: ZqEnv, val: ZqRawValue) -> zuqe_zapi::ffi::ZqType {
    use zuqe_zapi::ffi::ZqType;
    let v = JSValue(val, std::marker::PhantomData);
    match v.tag() {
        crate::value::JSValueTag::Undefined | crate::value::JSValueTag::Uninitialized => {
            ZqType::Undefined
        }
        crate::value::JSValueTag::Null => ZqType::Null,
        crate::value::JSValueTag::Bool => ZqType::Bool,
        crate::value::JSValueTag::Int | crate::value::JSValueTag::Float64 => ZqType::Number,
        crate::value::JSValueTag::String | crate::value::JSValueTag::StringRope => ZqType::String,
        crate::value::JSValueTag::Symbol => ZqType::Symbol,
        crate::value::JSValueTag::BigInt => ZqType::BigInt,
        crate::value::JSValueTag::Object
        | crate::value::JSValueTag::FunctionBytecode
        | crate::value::JSValueTag::Module => {
            if v.is_callable() {
                ZqType::Function
            } else {
                ZqType::Object
            }
        }
        crate::value::JSValueTag::CatchOffset => ZqType::Object,
        crate::value::JSValueTag::Exception => ZqType::Exception,
    }
}

unsafe extern "C" fn zapi_value_to_float64(_env: ZqEnv, val: ZqRawValue) -> f64 {
    JSValue(val, std::marker::PhantomData)
        .as_number()
        .map(|n| n.to_f64())
        .unwrap_or(f64::NAN)
}

unsafe extern "C" fn zapi_value_to_bool(_env: ZqEnv, val: ZqRawValue) -> i32 {
    match JSValue(val, std::marker::PhantomData).to_bool() {
        Ok(b) => b as i32,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_value_to_string(env: ZqEnv, val: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match JSValue(val, std::marker::PhantomData).to_string(ctx) {
        Ok(s) => JSValue::make_string(s).0,
        Err(_) => ZQ_API_TABLE.value_exception,
    }
}

unsafe extern "C" fn zapi_call_constructor(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let ctx = unsafe { &mut *((*env).ctx as *mut ScopedContext) };
    let info = unsafe { &*info };

    let target = crate::JSValue(info.target, std::marker::PhantomData);
    let args: Vec<crate::JSValue> = if info.argc > 0 {
        unsafe { std::slice::from_raw_parts(info.args, info.argc as usize) }
            .iter()
            .map(|&r| crate::JSValue(r, std::marker::PhantomData))
            .collect()
    } else {
        Vec::new()
    };

    match target.as_object() {
        Some(o) => match o.call_ctor(ctx, &args) {
            Ok(v) => v.0,
            Err(_) => ZQ_API_TABLE.value_exception,
        },
        None => ZQ_API_TABLE.value_exception,
    }
}

/// 全局共享的 ZqApi 表 — 纯函数指针，无运行时状态，多 Runtime 安全复用。
pub(crate) static ZQ_API_TABLE: Lazy<Box<ZqApi>> = Lazy::new(new_zapi);

/// 构造 host 侧的 `ZqApi` 表，填充全部函数指针，返回堆分配的 Box。
pub(crate) fn new_zapi() -> Box<ZqApi> {
    Box::new(ZqApi {
        version: ZQ_ABI_VERSION,
        new_number: zapi_new_number,
        new_string_utf8: zapi_new_string_utf8,
        new_symbol: zapi_new_symbol,
        get_symbol_description: zapi_get_symbol_description,
        get_string_utf8: zapi_get_string_utf8,
        new_object: zapi_new_object,
        new_array: zapi_new_array,
        set_property: zapi_set_property,
        get_property: zapi_get_property,
        has_property: zapi_has_property,
        delete_property: zapi_delete_property,

        array_push: zapi_array_push,
        array_set: zapi_array_set,
        array_len: zapi_array_len,

        get_global: zapi_get_global,
        call_function: zapi_call_function,
        gc_add_trace: zapi_gc_add_trace,
        register_class: zapi_register_class,
        new_instance: zapi_new_instance,
        get_native_ptr: zapi_get_native_ptr,
        get_value_type: zapi_get_value_type,
        value_to_float64: zapi_value_to_float64,
        value_to_bool: zapi_value_to_bool,
        value_to_string: zapi_value_to_string,
        call_constructor: zapi_call_constructor,
        set_gc_trace_func: zapi_set_gc_trace_func,

        new_promise: zapi_new_promise,
        promise_resolve: zapi_promise_resolve,
        promise_reject: zapi_promise_reject,
        add_polling_job: zapi_add_polling_job,

        wake_handle_new: zapi_wake_handle_new,
        wake_handle_wake: zapi_wake_handle_wake,
        wake_handle_free: zapi_wake_handle_free,

        take_exception: zapi_take_exception,
        exception: zapi_exception,

        // host-provided constant value handles
        value_undefined: crate::JSValue::UNDEFINED.0,
        value_null: crate::JSValue::NULL.0,
        value_true: crate::JSValue::TRUE.0,
        value_false: crate::JSValue::FALSE.0,
        value_nan: crate::JSValue::NAN.0,
        value_zero: crate::JSValue::ZERO.0,
        value_one: crate::JSValue::from(1i32).0,
        value_negative_one: crate::JSValue::from(-1i32).0,
        value_infinity: crate::JSValue::from(f64::INFINITY).0,
        value_minus_infinity: crate::JSValue::from(f64::NEG_INFINITY).0,
        value_exception: crate::JSValue::EXCEPTION.0,
    })
}
