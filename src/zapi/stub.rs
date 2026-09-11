// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{borrow::Cow, ffi::c_void, marker::PhantomData, sync::LazyLock};

use gc_lite::{GcTrace, GcTraceCtx};
use zuqe_zapi::ffi::{
    ZQ_CONST_VALUE_SLOT_COUNT, ZQ_CONST_VALUE_SLOT_EXCEPTION, ZQ_CONST_VALUE_SLOT_FALSE,
    ZQ_CONST_VALUE_SLOT_INFINITY, ZQ_CONST_VALUE_SLOT_MINUS_INFINITY, ZQ_CONST_VALUE_SLOT_NAN,
    ZQ_CONST_VALUE_SLOT_NULL, ZQ_CONST_VALUE_SLOT_TRUE, ZQ_CONST_VALUE_SLOT_UNDEFINED,
    ZQ_CONST_VALUE_SLOT_ZERO, ZQ_PROP_CONFIGURABLE, ZQ_PROP_ENUMERABLE, ZQ_PROP_WRITABLE,
    ZQ_VALUE_TYPE_BIGINT, ZQ_VALUE_TYPE_BOOL, ZQ_VALUE_TYPE_EXCEPTION, ZQ_VALUE_TYPE_FUNCTION,
    ZQ_VALUE_TYPE_NULL, ZQ_VALUE_TYPE_NUMBER, ZQ_VALUE_TYPE_OBJECT, ZQ_VALUE_TYPE_STRING,
    ZQ_VALUE_TYPE_SYMBOL, ZQ_VALUE_TYPE_UNDEFINED, ZqApi, ZqBodyFn, ZqCallInfoS, ZqClassDef,
    ZqFieldDef, ZqGcTraceCtxS, ZqGcTraceFn, ZqJobRunFn, ZqJobTraceFn, ZqRawCtx, ZqRawValue,
    ZqWakeHandleS,
};

use crate::{
    Array, Atom, CallContext, ErrorKind, JSValue, JSValueTag, ScopedContext,
    builtins::array::js_array_push,
    class::ClassID,
    function::Callback,
    object::PayloadPtr,
    object::{JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_WRITABLE, ThrowPolicy},
};

/// Translate FFI `ZQ_PROP_*` flags into engine `JS_PROP_*` flags bit-by-bit.
pub(super) const fn zq_to_js_prop_flags(flags: u32) -> u32 {
    let mut p = 0;
    if flags & ZQ_PROP_CONFIGURABLE != 0 {
        p |= JS_PROP_CONFIGURABLE;
    }
    if flags & ZQ_PROP_WRITABLE != 0 {
        p |= JS_PROP_WRITABLE;
    }
    if flags & ZQ_PROP_ENUMERABLE != 0 {
        p |= JS_PROP_ENUMERABLE;
    }
    p
}

use super::host::{push_class_fields, register_zapi_func_magic, zapi_trampoline_fn};

/// Layout-compatible opaque struct for the FFI environment handle.
unsafe fn ctx_from_env<'a>(env: ZqRawCtx) -> &'a mut ScopedContext<'a> {
    unsafe { &mut *(env as *mut ScopedContext) }
}

fn symbol_from_desc(ctx: &mut ScopedContext, desc: JSValue) -> Result<JSValue, i32> {
    if desc.is_undefined() {
        ctx.create_symbol(None, crate::atom::AtomKind::Symbol)
    } else {
        let sr = desc.to_string(ctx)?;
        ctx.create_symbol(Some(sr), crate::atom::AtomKind::Symbol)
    }
}

unsafe extern "C" fn zapi_new_number(n: f64) -> ZqRawValue {
    JSValue::from(n).0
}

unsafe extern "C" fn zapi_new_int32(n: i32) -> ZqRawValue {
    JSValue::from(n).0
}

unsafe extern "C" fn zapi_new_string(env: ZqRawCtx, ptr: *const u8, len: usize) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    match ctx.create_string_from_utf8(bytes.to_vec()) {
        Ok(s) => JSValue::make_string(s).0,
        Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

unsafe extern "C" fn zapi_new_symbol(env: ZqRawCtx, desc: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match symbol_from_desc(ctx, JSValue(desc, std::marker::PhantomData)) {
        Ok(v) => v.0,
        Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

unsafe extern "C" fn zapi_symbol_get_description(env: ZqRawCtx, sym: ZqRawValue) -> ZqRawValue {
    let Some(sr) = JSValue(sym, std::marker::PhantomData).as_symbol(true) else {
        let ctx = unsafe { ctx_from_env(env) };
        let _ = ctx.throw_type_error("not a Symbol value");
        return ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION];
    };

    if sr.content().is_ident() {
        JSValue::UNDEFINED.0
    } else {
        JSValue::make_string(sr).0
    }
}

unsafe extern "C" fn zapi_string_read_utf8(
    env: ZqRawCtx,
    s: ZqRawValue,
    buf: *mut u8,
    buf_len: usize,
    offset: usize,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(sr) = JSValue(s, std::marker::PhantomData).as_string(true) else {
        let _ = ctx.throw_type_error("not a string");
        return -1;
    };

    let total = sr.utf8_len();
    let start = offset.min(total);
    let remaining = total - start;

    // Measure mode: a null `buf` writes nothing and returns the bytes needed
    // from `offset`, so the caller can size its buffer. The length is computed
    // directly, without encoding or allocating.
    if buf.is_null() {
        return remaining as i32;
    }
    let bytes = sr.content().encode_utf8_bytes(false);
    let n = remaining.min(buf_len);
    if n > 0 {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr().add(start), buf, n) };
    }
    n as i32
}

unsafe extern "C" fn zapi_new_object(env: ZqRawCtx) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match ctx.create_base_object() {
        Ok(o) => JSValue::from(o).0,
        Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

unsafe extern "C" fn zapi_new_array(
    env: ZqRawCtx,
    elements: *const ZqRawValue,
    len: usize,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    Array::from_slice(
        ctx,
        if len > 0 {
            unsafe { std::slice::from_raw_parts(elements.cast::<JSValue>(), len) }
        } else {
            &[]
        },
    )
    .map_or(
        ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
        |a| JSValue::from(a).0,
    )
}

unsafe extern "C" fn zapi_get_property(
    env: ZqRawCtx,
    target: ZqRawValue,
    key: ZqRawValue,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    JSValue(target, std::marker::PhantomData)
        .get_property_value(ctx, JSValue(key, std::marker::PhantomData))
        .map_or(
            ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
            |v| v.0,
        )
}

/// returns 1=set, 0=not set, -1=error
unsafe extern "C" fn zapi_set_property(
    env: ZqRawCtx,
    target: ZqRawValue,
    key: ZqRawValue,
    val: ZqRawValue,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    JSValue(target, std::marker::PhantomData)
        .set_property_value(
            ctx,
            JSValue(key, std::marker::PhantomData),
            JSValue(val, std::marker::PhantomData),
            ThrowPolicy::Always,
            false,
        )
        .map_or(-1, |b| b as i32)
}

/// 0=false, 1=true, -1=error
unsafe extern "C" fn zapi_has_property(env: ZqRawCtx, obj: ZqRawValue, key: ZqRawValue) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(o) = JSValue(obj, std::marker::PhantomData).as_object() else {
        return 0;
    };
    let atom = match JSValue(key, std::marker::PhantomData).to_atom(ctx) {
        Ok(a) => a,
        Err(_) => return -1,
    };
    match o.has_property(ctx, atom) {
        Ok(b) => b as i32,
        Err(_) => -1,
    }
}

/// 1=deleted, 0=can't delete, -1=error
unsafe extern "C" fn zapi_delete_property(
    env: ZqRawCtx,
    target: ZqRawValue,
    key: ZqRawValue,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = JSValue(key, std::marker::PhantomData).to_atom(ctx) else {
        return -1;
    };
    JSValue(target, std::marker::PhantomData)
        .delete_property(ctx, atom)
        .map_or(-1, |b| b as i32)
}

// ── str-keyed property access ──
//
// These mirror the value-keyed functions above, but take the key as UTF-8
// bytes and resolve it to an interned atom **without allocating a JS string**.
// `prop_str_to_atom` → `Atom::from_str` now takes the allocation-free
// `Atom::lookup_str` fast path for already-interned keys, so repeated access
// with a literal key performs no allocation.

/// Resolve a UTF-8 `(ptr, len)` key to an interned atom, throwing a
/// `TypeError` on invalid input.
fn str_key_atom(ctx: &mut ScopedContext, key: *const u8, key_len: usize) -> Result<Atom, i32> {
    if key_len == 0 {
        return Ok(Atom::CONST_empty_string);
    } else if key.is_null() {
        return Err(ctx.throw_type_error("property key is null"));
    }

    let bytes = unsafe { std::slice::from_raw_parts(key, key_len) };
    let Ok(s) = std::str::from_utf8(bytes) else {
        return Err(ctx.throw_type_error("property key is not valid UTF-8"));
    };
    crate::function::prop_str_to_atom(ctx, s)
}

unsafe extern "C" fn zapi_get_property_str(
    env: ZqRawCtx,
    target: ZqRawValue,
    key: *const u8,
    key_len: usize,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = str_key_atom(ctx, key, key_len) else {
        return ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION];
    };

    JSValue(target, std::marker::PhantomData)
        .get_property(ctx, atom)
        .map_or(
            ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
            |v| v.0,
        )
}

/// returns 1=set, 0=not set, -1=error
unsafe extern "C" fn zapi_set_property_str(
    env: ZqRawCtx,
    obj: ZqRawValue,
    key: *const u8,
    key_len: usize,
    val: ZqRawValue,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = str_key_atom(ctx, key, key_len) else {
        return -1;
    };

    JSValue(obj, std::marker::PhantomData)
        .set_property(ctx, atom, JSValue(val, std::marker::PhantomData))
        .map_or(-1, |b| b as i32)
}

/// 0=false, 1=true, -1=error
unsafe extern "C" fn zapi_has_property_str(
    env: ZqRawCtx,
    obj: ZqRawValue,
    key: *const u8,
    key_len: usize,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = str_key_atom(ctx, key, key_len) else {
        return -1;
    };

    JSValue(obj, std::marker::PhantomData)
        .has_property(ctx, atom)
        .map_or(-1, |b| b as i32)
}

/// 1=deleted, 0=can't delete, -1=error
unsafe extern "C" fn zapi_delete_property_str(
    env: ZqRawCtx,
    obj: ZqRawValue,
    key: *const u8,
    key_len: usize,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = str_key_atom(ctx, key, key_len) else {
        return -1;
    };

    JSValue(obj, std::marker::PhantomData)
        .delete_property(ctx, atom)
        .map_or(-1, |b| b as i32)
}

unsafe extern "C" fn zapi_get_global_str(
    env: ZqRawCtx,
    name: *const u8,
    name_len: usize,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = str_key_atom(ctx, name, name_len) else {
        return ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION];
    };

    ctx.global_obj().get_property(ctx, atom).map_or(
        ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
        |v| v.0,
    )
}

/// minus=error, otherwise the length
unsafe extern "C" fn zapi_get_length_prop(env: ZqRawCtx, arr: ZqRawValue) -> i64 {
    let ctx = unsafe { ctx_from_env(env) };
    let v = JSValue(arr, std::marker::PhantomData);
    v.get_length_prop(ctx).unwrap_or(-1)
}

/// minus:error, otherwise: new length
unsafe extern "C" fn zapi_array_push(env: ZqRawCtx, arr: ZqRawValue, val: ZqRawValue) -> i64 {
    let ctx = unsafe { ctx_from_env(env) };
    let val = JSValue(val, PhantomData);
    let cc = CallContext::new(
        JSValue(arr, PhantomData),
        std::slice::from_ref(&val),
        0,
        None,
    );

    js_array_push(ctx, cc).map_or(-1, |n| n.to_index(ctx).unwrap())
}

unsafe extern "C" fn zapi_get_global(env: ZqRawCtx, name: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let gobj = ctx.global_obj();
    match gobj.get_property_value(ctx, JSValue(name, std::marker::PhantomData)) {
        Ok(v) => v.0,
        Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

/// Set a property on the global object by raw value key.
/// 1=set, 0=not set, -1=error
unsafe extern "C" fn zapi_set_global(env: ZqRawCtx, name: ZqRawValue, val: ZqRawValue) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    ctx.global_obj()
        .set_property_value(
            ctx,
            JSValue(name, std::marker::PhantomData),
            JSValue(val, std::marker::PhantomData),
            ThrowPolicy::Always,
            false,
        )
        .map_or(-1, |b| b as i32)
}

/// Set a property on the global object by UTF-8 name.
/// 1=set, 0=not set, -1=error
unsafe extern "C" fn zapi_set_global_str(
    env: ZqRawCtx,
    name: *const u8,
    name_len: usize,
    val: ZqRawValue,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = str_key_atom(ctx, name, name_len) else {
        return -1;
    };

    ctx.global_obj()
        .set_property(ctx, atom, JSValue(val, std::marker::PhantomData))
        .map_or(-1, |b| b as i32)
}

unsafe extern "C" fn zapi_call_function(env: ZqRawCtx, info: *mut ZqCallInfoS) -> ZqRawValue {
    let ctx = unsafe { &mut *(env as *mut ScopedContext) };
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
        Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
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
        let trace_ctx: &mut ZqGcTraceCtxS =
            unsafe { std::mem::transmute::<&mut gc_lite::GcTraceCtx, _>(gcx) };
        unsafe { gc_trace(trace_ctx, payload_opaque) };
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

unsafe extern "C" fn zapi_register_class(env: ZqRawCtx, class_def: *const ZqClassDef) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let def = unsafe { &*class_def };
    let name_bytes = unsafe { std::ffi::CStr::from_ptr(def.name) };
    let name = match name_bytes.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());

    fn zapi_class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        let o = ctx.create_instance_with_proto(None, PayloadPtr::none(class_id))?;
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
        cd = cd.on_finalize(|rt, payload_opaque, class_id, _| {
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
    env: ZqRawCtx,
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
        return ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION];
    };
    match ctx.ordinary_create_from_constructor_payload(
        JSValue::UNDEFINED,
        PayloadPtr::opaque(cid, payload),
    ) {
        Ok(obj) => JSValue::from(obj).0,
        Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

unsafe extern "C" fn zapi_get_native_ptr(_env: ZqRawCtx, obj: ZqRawValue) -> *mut c_void {
    let val = JSValue(obj, std::marker::PhantomData);
    // Unified payload storage: a zapi class instance's opaque pointer IS the
    // payload pointer, so read it directly (user-class domain only).
    val.as_object()
        .filter(|o| o.class_id() >= crate::USER_CLASS_ID_START)
        .map(|o| o.payload_raw())
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn zapi_gc_mark(gcx: *mut ZqGcTraceCtxS, val: ZqRawValue) -> i32 {
    let gcx: &mut gc_lite::GcTraceCtx =
        unsafe { std::mem::transmute::<&mut ZqGcTraceCtxS, _>(&mut *gcx) };
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
    let tc: &mut ZqGcTraceCtxS = unsafe { std::mem::transmute(gcx) };
    unsafe { (reg.cb)(tc, reg.opaque) };
}

unsafe extern "C" fn zapi_gc_set_trace_func(
    env: ZqRawCtx,
    cb: Option<ZqGcTraceFn>,
    opaque: *mut c_void,
) -> i32 {
    let Some(cb) = cb else {
        return -1;
    };

    let reg = Box::into_raw(Box::new(ZapiTraceReg { cb, opaque }));
    unsafe {
        let rt = (**env.cast::<ScopedContext>()).rt_mut();
        rt.gc_add_trace_callback(zapi_gc_trace_adapter, reg as *mut c_void);
    }

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
            let tc: &mut ZqGcTraceCtxS = unsafe { std::mem::transmute(gcx) };
            unsafe { f(self.data, tc) };
        }
    }
}

/// Take the pending exception and return its raw handle
/// (value_undefined if none). Mirrors ScopedContext::take_exception.
unsafe extern "C" fn zapi_take_exception(env: ZqRawCtx) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let exc = ctx.take_exception();
    let raw = match exc {
        crate::error::Exception::Error(val) => val.0,
        _ => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_UNDEFINED],
    };
    debug_assert_ne!(
        raw,
        ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
        "take_exception must never return the exception sentinel"
    );
    raw
}

/// Peek the pending exception without consuming it (value_undefined if none).
unsafe extern "C" fn zapi_exception(env: ZqRawCtx) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let raw = match ctx.rt().exception() {
        crate::error::Exception::Error(val) => val.0,
        _ => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_UNDEFINED],
    };
    debug_assert_ne!(
        raw,
        ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
        "exception() must never return the exception sentinel"
    );
    raw
}
/// Create a pending Promise. Stays GC-rooted (gc_pending_promises) until
/// resolved/rejected — the extension may hold the raw handle across ticks.
unsafe extern "C" fn zapi_new_promise(env: ZqRawCtx) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match crate::builtins::promise::Promise::create(ctx) {
        Ok((p, _rr)) => {
            // Root while pending; settle() removes it automatically.
            ctx.rt_mut().gc_pending_promises.push(p);
            crate::value::JSValue::from(p).0
        }
        Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

/// Settle a pending Promise as fulfilled. Double-settle is a no-op.
unsafe extern "C" fn zapi_promise_resolve(env: ZqRawCtx, promise: ZqRawValue, value: ZqRawValue) {
    let ctx = unsafe { ctx_from_env(env) };
    let pv = crate::value::JSValue(promise, std::marker::PhantomData);
    let v = crate::value::JSValue(value, std::marker::PhantomData);
    if let Ok(mut p) = crate::builtins::promise::Promise::try_from(pv) {
        p.resolve(ctx, v);
    }
}

/// Settle a pending Promise as rejected. Double-settle is a no-op.
unsafe extern "C" fn zapi_promise_reject(env: ZqRawCtx, promise: ZqRawValue, reason: ZqRawValue) {
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

/// Create a waker bound to the env's runtime event loop (JS thread).
unsafe extern "C" fn zapi_new_waker(env: ZqRawCtx) -> *mut ZqWakeHandleS {
    let ctx = unsafe { ctx_from_env(env) };
    let h = Box::new(ZqWakeHandle {
        wake: ctx.rt().wake_handle(),
    });
    Box::into_raw(h) as *mut ZqWakeHandleS
}

/// Wake the parked event loop. Thread-safe; cheap when not parked.
unsafe extern "C" fn zapi_waker_wake(h: *mut ZqWakeHandleS) {
    if h.is_null() {
        return;
    }
    let wh = unsafe { &*(h as *const ZqWakeHandle) };
    wh.wake.wake();
}

/// Release a waker.
unsafe extern "C" fn zapi_waker_free(h: *mut ZqWakeHandleS) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h as *mut ZqWakeHandle) });
    }
}

/// each tick runs `run(data)` on the JS thread until it reports Done/Err.
unsafe extern "C" fn zapi_add_polling_job(
    env: ZqRawCtx,
    run: ZqJobRunFn,
    trace: Option<ZqJobTraceFn>,
    data: *mut c_void,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    ctx.queue_job(
        move |cx| {
            let env = cx as *mut ScopedContext as ZqRawCtx;
            match unsafe { run(data, env) } {
                0 => Ok(crate::microtask::JobState::Done),
                1 => Ok(crate::microtask::JobState::Block),
                2 => Ok(crate::microtask::JobState::Continue),
                _ => Err(-1),
            }
        },
        ZqJobNode { trace, data },
    );
    0
}

/// Create a callable JS function backed by a Rust callback.
/// Since opaque data is always null, `run_fn` is equivalent to `ZapiBodyFn`
/// and is registered in the existing body registry via `register_zapi_func_magic`.
/// No separate callable registry is needed — the trampoline dispatches from
/// the body registry by token, same as normal zapi functions.
unsafe extern "C" fn zapi_new_native_function(
    env: ZqRawCtx,
    run_fn: ZqBodyFn,
    name: ZqRawValue,
    magic: i32,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let rt = ctx.rt_mut();

    let serial = rt
        .zapi_next_serial
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    // register_zapi_func_magic inserts into zapi_body_registry and
    // returns (Callback, user_magic). We only need the token for the
    // NativeFunction's magic field.
    let (_cb, _user_magic) = register_zapi_func_magic(ctx, run_fn, magic);

    let name_val = JSValue(name, std::marker::PhantomData);
    let func_name = name_val
        .to_string(ctx)
        .map(|s| s.content().encode_utf8(false).to_string())
        .unwrap_or_default();

    match ctx.create_native_function(
        Callback::Func(super::host::zapi_trampoline_fn),
        &func_name,
        0,
        serial as _,
    ) {
        Ok(f) => {
            let js_val: JSValue = f.into();
            js_val.0
        }
        Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

/// Wrapper struct that implements `AsyncIteratorFetcher` for zapi FFI.
struct ZapiFetcher {
    fetch_fn: zuqe_zapi::ffi::ZqAsyncIterFetchFn,
    opaque: *mut c_void,
    trace_fn: Option<zuqe_zapi::ffi::ZqGcTraceFn>,
    drop_fn: Option<unsafe extern "C" fn(*mut c_void)>,
}

unsafe impl Send for ZapiFetcher {}

impl GcTrace for ZapiFetcher {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if !self.opaque.is_null()
            && let Some(f) = self.trace_fn
        {
            unsafe {
                let c =
                    std::mem::transmute::<&mut GcTraceCtx, *mut zuqe_zapi::ffi::ZqGcTraceCtxS>(tr);
                f(c, self.opaque)
            }
        }
    }
}

impl crate::builtins::iterator::AsyncIteratorFetcher for ZapiFetcher {
    fn try_fetch(
        &self,
        ctx: &mut ScopedContext,
    ) -> Result<crate::builtins::iterator::IterFetchStatus, i32> {
        let env = ctx as *mut ScopedContext as ZqRawCtx;
        let mut val = 0u64;
        let code = unsafe { (self.fetch_fn)(env, self.opaque, &mut val) };

        match code {
            0 => Ok(crate::builtins::iterator::IterFetchStatus::Value(JSValue(
                val,
                std::marker::PhantomData,
            ))),
            1 => Ok(crate::builtins::iterator::IterFetchStatus::Done),
            2 => Ok(crate::builtins::iterator::IterFetchStatus::Wait),
            _ => Err(-1),
        }
    }

    fn close(&self) {
        if let Some(drop_fn) = self.drop_fn {
            unsafe { drop_fn(self.opaque) };
        }
    }
}

unsafe extern "C" fn zapi_new_async_iterator(
    env: ZqRawCtx,
    fetch_fn: zuqe_zapi::ffi::ZqAsyncIterFetchFn,
    trace_fn: Option<ZqGcTraceFn>,
    drop_fn: Option<unsafe extern "C" fn(*mut c_void)>,
    opaque: *mut c_void,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };

    let fetcher = ZapiFetcher {
        fetch_fn,
        opaque,
        trace_fn,
        drop_fn,
    };

    // ZapiFetcher implements GcTrace (see impl above), so AsyncIterPayload
    // will trace opaque data via the fetcher's trace method during GC.
    match crate::builtins::iterator::AsyncIterableIterator::create(ctx, fetcher) {
        Ok(iter) => {
            let js_val: JSValue = iter.0.into();
            js_val.0
        }
        Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

/// Map a raw `i32` error type tag to the engine's `ErrorKind`.
const fn err_type_to_kind(ty: i32) -> Option<ErrorKind> {
    Some(match ty {
        0 => ErrorKind::Eval,
        1 => ErrorKind::Range,
        2 => ErrorKind::Reference,
        3 => ErrorKind::Syntax,
        4 => ErrorKind::Type,
        5 => ErrorKind::Uri,
        6 => ErrorKind::Internal,
        7 => ErrorKind::Aggregate,
        _ => return None,
    })
}

/// Throw a JS error of the given type with a UTF-8 message.
/// Returns `value_exception` (the extension should propagate it).
unsafe extern "C" fn zapi_throw(
    env: ZqRawCtx,
    err_type: i32,
    err_message: *const u8,
    err_len: i32,
) {
    let ctx = unsafe { ctx_from_env(env) };

    let kind = match err_type_to_kind(err_type) {
        Some(k) => k,
        None => {
            let msg = if err_message.is_null() || err_len <= 0 {
                "unknown error type"
            } else {
                let bytes = unsafe { std::slice::from_raw_parts(err_message, err_len as usize) };
                std::str::from_utf8(bytes).unwrap_or("unknown error type")
            };
            let _ = ctx.throw_error(ErrorKind::Internal, msg);
            return;
        }
    };

    let msg: Cow<str> = if err_message.is_null() || err_len <= 0 {
        Cow::Borrowed("")
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(err_message, err_len as usize) };
        String::from_utf8_lossy(bytes)
    };
    let _ = ctx.throw_error(kind, &msg);
}

unsafe extern "C" fn zapi_value_get_type(val: ZqRawValue) -> i32 {
    let v = JSValue(val, std::marker::PhantomData);
    match v.tag() {
        JSValueTag::Undefined | JSValueTag::Uninitialized => ZQ_VALUE_TYPE_UNDEFINED,
        JSValueTag::Null => ZQ_VALUE_TYPE_NULL,
        JSValueTag::Bool => ZQ_VALUE_TYPE_BOOL,
        JSValueTag::Int | JSValueTag::Float64 => ZQ_VALUE_TYPE_NUMBER,
        JSValueTag::String | JSValueTag::StringRope => ZQ_VALUE_TYPE_STRING,
        JSValueTag::Symbol => ZQ_VALUE_TYPE_SYMBOL,
        JSValueTag::BigInt => ZQ_VALUE_TYPE_BIGINT,
        JSValueTag::Object | JSValueTag::FunctionBytecode | JSValueTag::Module => {
            if v.is_callable() {
                ZQ_VALUE_TYPE_FUNCTION
            } else {
                ZQ_VALUE_TYPE_OBJECT
            }
        }
        JSValueTag::CatchOffset => ZQ_VALUE_TYPE_OBJECT,
        JSValueTag::Exception => ZQ_VALUE_TYPE_EXCEPTION,
    }
}

unsafe extern "C" fn zapi_value_to_float64(_: ZqRawCtx, val: ZqRawValue, ret: *mut f64) -> i32 {
    JSValue(val, std::marker::PhantomData)
        .as_number()
        .map_or(-1, |n| unsafe {
            *ret = n.to_f64();
            0 // success
        })
}

unsafe extern "C" fn zapi_value_to_int64(_: ZqRawCtx, val: ZqRawValue, ret: *mut i64) -> i32 {
    JSValue(val, std::marker::PhantomData)
        .as_number()
        .map_or(-1, |n| unsafe {
            *ret = n.to_i64_saturating();
            0 // success
        })
}

unsafe extern "C" fn zapi_value_to_bool(_: ZqRawCtx, val: ZqRawValue) -> i32 {
    JSValue(val, std::marker::PhantomData)
        .to_bool()
        .map_or(-1, |b| b as i32)
}

unsafe extern "C" fn zapi_value_to_string(env: ZqRawCtx, val: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    JSValue(val, std::marker::PhantomData)
        .to_string(ctx)
        .map_or(
            ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
            |v| JSValue::make_string(v).0,
        )
}

unsafe extern "C" fn zapi_call_constructor(env: ZqRawCtx, info: *mut ZqCallInfoS) -> ZqRawValue {
    let ctx = unsafe { &mut *(env as *mut ScopedContext) };
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
            Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
        },
        None => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

/// 全局共享的 ZqApi 表 — 纯函数指针，无运行时状态，多 Runtime 安全复用。
pub(crate) static ZQ_API_TABLE: LazyLock<Box<ZqApi>> = LazyLock::new(new_zapi);

/// 构造 host 侧的 `ZqApi` 表，填充全部函数指针，返回堆分配的 Box。
pub(crate) fn new_zapi() -> Box<ZqApi> {
    Box::new(ZqApi {
        // ── constant value handles ──
        const_values: {
            let mut v = [0u64; ZQ_CONST_VALUE_SLOT_COUNT];
            v[ZQ_CONST_VALUE_SLOT_UNDEFINED] = crate::JSValue::UNDEFINED.0;
            v[ZQ_CONST_VALUE_SLOT_NULL] = crate::JSValue::NULL.0;
            v[ZQ_CONST_VALUE_SLOT_TRUE] = crate::JSValue::TRUE.0;
            v[ZQ_CONST_VALUE_SLOT_FALSE] = crate::JSValue::FALSE.0;
            v[ZQ_CONST_VALUE_SLOT_NAN] = crate::JSValue::NAN.0;
            v[ZQ_CONST_VALUE_SLOT_ZERO] = crate::JSValue::ZERO.0;
            v[ZQ_CONST_VALUE_SLOT_INFINITY] = JSValue::make_float(f64::INFINITY).0;
            v[ZQ_CONST_VALUE_SLOT_MINUS_INFINITY] = JSValue::make_float(f64::NEG_INFINITY).0;
            v[ZQ_CONST_VALUE_SLOT_EXCEPTION] = crate::JSValue::EXCEPTION.0;
            v
        },

        // ── value constructors ──
        new_number: zapi_new_number,
        new_int32: zapi_new_int32,
        new_string: zapi_new_string,
        new_symbol: zapi_new_symbol,
        new_object: zapi_new_object,
        new_array: zapi_new_array,

        // ── value query / conversion ──
        value_get_type: zapi_value_get_type,
        value_to_bool: zapi_value_to_bool,
        value_to_int64: zapi_value_to_int64,
        value_to_float64: zapi_value_to_float64,
        value_to_string: zapi_value_to_string,
        string_read_utf8: zapi_string_read_utf8,
        symbol_get_description: zapi_symbol_get_description,

        // ── property access (raw key) ──
        get_property: zapi_get_property,
        set_property: zapi_set_property,
        has_property: zapi_has_property,
        delete_property: zapi_delete_property,

        // ── property access (UTF-8 key) ──
        get_property_str: zapi_get_property_str,
        set_property_str: zapi_set_property_str,
        has_property_str: zapi_has_property_str,
        delete_property_str: zapi_delete_property_str,

        // ── array ──
        get_length_prop: zapi_get_length_prop,
        array_push: zapi_array_push,

        // ── function call ──
        call_function: zapi_call_function,
        call_constructor: zapi_call_constructor,

        // ── global object ──
        get_global: zapi_get_global,
        get_global_str: zapi_get_global_str,
        set_global: zapi_set_global,
        set_global_str: zapi_set_global_str,

        // ── native object / class ──
        register_class: zapi_register_class,
        new_instance: zapi_new_instance,
        get_native_ptr: zapi_get_native_ptr,
        new_native_function: zapi_new_native_function,

        // ── promise ──
        new_promise: zapi_new_promise,
        promise_resolve: zapi_promise_resolve,
        promise_reject: zapi_promise_reject,

        // ── async iterator ──
        new_async_iterator: zapi_new_async_iterator,

        // ── job queue ──
        add_polling_job: zapi_add_polling_job,

        // ── GC ──
        gc_mark: zapi_gc_mark,
        gc_set_trace_func: zapi_gc_set_trace_func,

        // ── exception ──
        throw: zapi_throw,
        take_exception: zapi_take_exception,
        exception: zapi_exception,

        // ── event-loop wake ──
        new_waker: zapi_new_waker,
        waker_wake: zapi_waker_wake,
        waker_free: zapi_waker_free,
    })
}
