// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    borrow::Cow,
    ffi::c_void,
    marker::PhantomData,
    sync::{Arc, LazyLock, RwLock},
};

use gc_lite::{GcTrace, GcTraceCtx};
use zuqe_zapi::ffi::{
    ZQ_BUF_ARRAY_BUFFER, ZQ_BUF_DATAVIEW, ZQ_BUF_NONE, ZQ_BUF_SHARED_ARRAY_BUFFER,
    ZQ_BUF_TYPED_ARRAY_BIGINT64, ZQ_BUF_TYPED_ARRAY_BIGUINT64, ZQ_BUF_TYPED_ARRAY_FLOAT16,
    ZQ_BUF_TYPED_ARRAY_FLOAT32, ZQ_BUF_TYPED_ARRAY_FLOAT64, ZQ_BUF_TYPED_ARRAY_INT8,
    ZQ_BUF_TYPED_ARRAY_INT16, ZQ_BUF_TYPED_ARRAY_INT32, ZQ_BUF_TYPED_ARRAY_UINT8,
    ZQ_BUF_TYPED_ARRAY_UINT8C, ZQ_BUF_TYPED_ARRAY_UINT16, ZQ_BUF_TYPED_ARRAY_UINT32,
    ZQ_CONST_VALUE_SLOT_COUNT, ZQ_CONST_VALUE_SLOT_EXCEPTION, ZQ_CONST_VALUE_SLOT_FALSE,
    ZQ_CONST_VALUE_SLOT_INFINITY, ZQ_CONST_VALUE_SLOT_MINUS_INFINITY, ZQ_CONST_VALUE_SLOT_NAN,
    ZQ_CONST_VALUE_SLOT_NULL, ZQ_CONST_VALUE_SLOT_TRUE, ZQ_CONST_VALUE_SLOT_UNDEFINED,
    ZQ_CONST_VALUE_SLOT_ZERO, ZQ_PROP_CONFIGURABLE, ZQ_PROP_ENUMERABLE, ZQ_PROP_KIND_ACCESSOR,
    ZQ_PROP_KIND_DATA, ZQ_PROP_WRITABLE, ZQ_VALUE_TYPE_BIGINT, ZQ_VALUE_TYPE_BOOL,
    ZQ_VALUE_TYPE_EXCEPTION, ZQ_VALUE_TYPE_FUNCTION, ZQ_VALUE_TYPE_NULL, ZQ_VALUE_TYPE_NUMBER,
    ZQ_VALUE_TYPE_OBJECT, ZQ_VALUE_TYPE_STRING, ZQ_VALUE_TYPE_SYMBOL, ZQ_VALUE_TYPE_UNDEFINED,
    ZqApi, ZqBodyFn, ZqCallInfoS, ZqClassDef, ZqFieldDef, ZqGcTraceCtxS, ZqGcTraceFn, ZqJobRunFn,
    ZqJobTraceFn, ZqPromiseCapabilityS, ZqPropertyDescriptorS, ZqRawCtx, ZqRawValue, ZqWakeHandleS,
};

use crate::{
    Array, Atom, ErrorKind, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT16_ARRAY, JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY,
    JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY, JSValue, JSValueTag,
    ScopedContext,
    atom::AtomKind,
    class::{ClassDef, ClassID},
    object::{
        ArrayBuffer, DataView, JS_GPN_STR_OR_INT_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE,
        JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_THROW, JS_PROP_WRITABLE, PayloadPtr,
        ThrowPolicy, TypedArray,
    },
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

/// Translate engine `JS_PROP_*` attribute bits back into `ZQ_PROP_*`.
const fn js_to_zq_prop_flags(flags: u32) -> u32 {
    let mut p = 0;
    if flags & JS_PROP_CONFIGURABLE != 0 {
        p |= ZQ_PROP_CONFIGURABLE;
    }
    if flags & JS_PROP_WRITABLE != 0 {
        p |= ZQ_PROP_WRITABLE;
    }
    if flags & JS_PROP_ENUMERABLE != 0 {
        p |= ZQ_PROP_ENUMERABLE;
    }
    p
}

/// The exception sentinel handle from the API table.
fn zq_exc() -> ZqRawValue {
    ZQ_API_TABLE.const_values[ZQ_CONST_VALUE_SLOT_EXCEPTION]
}

use super::host::{push_class_fields, register_zapi_func_magic, zapi_trampoline_fn};

/// Layout-compatible opaque struct for the FFI environment handle.
unsafe fn ctx_from_env<'a>(env: ZqRawCtx) -> &'a mut ScopedContext<'a> {
    unsafe { &mut *(env as *mut ScopedContext) }
}

unsafe extern "C" fn zapi_new_number(n: f64) -> ZqRawValue {
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

unsafe extern "C" fn zapi_read_string_utf8(
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
    len: usize,
    elements: *const ZqRawValue,
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
unsafe extern "C" fn zapi_get_length(env: ZqRawCtx, arr: ZqRawValue) -> i64 {
    let ctx = unsafe { ctx_from_env(env) };
    let v = JSValue(arr, std::marker::PhantomData);
    v.get_length_prop(ctx).unwrap_or(-1)
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
        unsafe { std::slice::from_raw_parts(info.argv, info.argc) }
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

/// Build a `JSValue` vec from a raw `argv`/`argc` slice and call `f` with
/// `this`. Returns the result or the exception sentinel.
unsafe fn invoke_with_argv(
    ctx: &mut ScopedContext,
    f: JSValue,
    this: JSValue,
    argc: usize,
    argv: *mut ZqRawValue,
) -> ZqRawValue {
    let args: Vec<JSValue> = if argc > 0 {
        unsafe { std::slice::from_raw_parts(argv as *const ZqRawValue, argc) }
            .iter()
            .map(|&r| JSValue(r, std::marker::PhantomData))
            .collect()
    } else {
        Vec::new()
    };

    match f.call(ctx, this, &args) {
        Ok(v) => v.0,
        Err(_) => ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

unsafe extern "C" fn zapi_invoke(
    env: ZqRawCtx,
    target: ZqRawValue,
    method: ZqRawValue,
    argc: usize,
    argv: *mut ZqRawValue,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let target = JSValue(target, std::marker::PhantomData);
    let method = JSValue(method, std::marker::PhantomData);
    let Ok(f) = target.get_property_value(ctx, method) else {
        return ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION];
    };
    unsafe { invoke_with_argv(ctx, f, target, argc, argv) }
}

unsafe extern "C" fn zapi_invoke_str(
    env: ZqRawCtx,
    target: ZqRawValue,
    method: *const u8,
    method_len: usize,
    argc: usize,
    argv: *mut ZqRawValue,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = str_key_atom(ctx, method, method_len) else {
        return ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION];
    };
    let target = JSValue(target, std::marker::PhantomData);
    let Ok(f) = target.get_property(ctx, atom) else {
        return ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION];
    };
    unsafe { invoke_with_argv(ctx, f, target, argc, argv) }
}

fn zapi_gc_trace_bridge(
    payload_opaque: *mut c_void,
    gcx: &mut gc_lite::GcTraceCtx,
    class_id: usize,
) {
    let rt = unsafe { &*(gcx.opaque() as *const crate::RuntimeImpl) };
    // Resolve this class's own ZqClassDef via its ClassID (one entry per class).
    let Some(def_ptr) = rt.zapi_def(class_id as ClassID) else {
        return;
    };
    if def_ptr.is_null() {
        return;
    }
    let def = unsafe { &*def_ptr };
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

impl crate::RuntimeImpl {
    /// Register a zapi-provided class, minting the class-name atom. Ids grow
    /// from the static class count. The extension's ABI def is stored alongside
    /// the engine-native def for later callback dispatch.
    pub(crate) fn register_class_def(
        &mut self,
        mut class_def: ClassDef,
        zq_def: *const ZqClassDef,
    ) -> Result<ClassID, i32> {
        class_def.name_atom = {
            let a = Atom::root_str(self, class_def.name)?;
            debug_assert!(!a.is_small_int());
            debug_assert!(
                a.is_const() || self.atom_desc_string(a.id()).unwrap().gc_head().is_root(),
                "class name atom should be root gc node"
            );
            a
        };

        let id = (crate::class::static_class_count() + self.zapi_class_defs.len()) as ClassID;
        if id >= u16::MAX {
            self.cur_exception = crate::Exception::TooManyClasses;
            return Err(-1);
        }

        self.zapi_class_defs.push(crate::zapi::ZapiClassDef {
            def: class_def,
            zq_def,
        });
        Ok(id)
    }

    /// The extension ABI def registered for a zapi class id, if any.
    fn zapi_def(&self, class_id: ClassID) -> Option<*const ZqClassDef> {
        let base = crate::class::static_class_count();
        class_id
            .checked_sub(base as ClassID)
            .and_then(|i| self.zapi_class_defs.get(i as usize))
            .map(|c| c.zq_def)
    }

    /// Reverse lookup: the class id registered for an extension ABI def.
    fn zapi_class_id_of(&self, zq_def: *const ZqClassDef) -> Option<ClassID> {
        let base = crate::class::static_class_count() as ClassID;
        self.zapi_class_defs
            .iter()
            .position(|c| c.zq_def == zq_def)
            .map(|i| base + i as ClassID)
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
        let Some(class_def) = ctx.rt().zapi_def(class_id) else {
            return Ok(());
        };
        if class_def.is_null() {
            return Ok(());
        }
        let def = unsafe { &*class_def };
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
        // resolves to its own ZqClassDef (one entry per class).
        cd = cd.on_finalize(|rt, payload_opaque, class_id, _| {
            let Some(def_ptr) = rt.zapi_def(class_id as ClassID) else {
                return;
            };
            if def_ptr.is_null() {
                return;
            }
            let def = unsafe { &*def_ptr };
            if std::env::var_os("ZUQE_ZAPI_DEBUG").is_some() {
                let name = unsafe { std::ffi::CStr::from_ptr(def.name) }
                    .to_string_lossy()
                    .into_owned();
                eprintln!("[zapi] finalize class={name}");
            }
            if let Some(fin) = def.finalize {
                unsafe { fin(std::ptr::null_mut(), payload_opaque) };
            }
        });
    }

    let class_id = match ctx.rt_mut().register_class_def(cd, class_def) {
        Ok(id) => id,
        Err(_) => {
            return -1;
        }
    };

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
    let class_id = ctx.rt().zapi_class_id_of(class_def);
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

unsafe extern "C" fn zapi_get_object_payload(obj: ZqRawValue) -> *mut c_void {
    let val = JSValue(obj, std::marker::PhantomData);
    // Unified payload storage: a class instance's opaque pointer IS the payload
    // pointer, so read it directly. Macro-only (`__private`) API; no class check
    // — the generated accessors know the payload type.
    val.as_object()
        .map_or(std::ptr::null_mut(), |o| o.payload_raw())
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
/// `NewPromiseCapability(%Promise%)`: a pending Promise plus its
/// `[[Resolve]]` / `[[Reject]]` functions. The promise stays rooted
/// (`gc_pending_promises`) until settled; settling happens through the
/// resolving functions, which remove it automatically. The resolving functions
/// are *not* rooted here — the caller keeps them alive (e.g. via a job trace).
unsafe extern "C" fn zapi_new_promise_capability(
    env: ZqRawCtx,
    out: *mut ZqPromiseCapabilityS,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    match crate::builtins::promise::Promise::create(ctx) {
        Ok((p, rr)) => {
            ctx.rt_mut().gc_pending_promises.push(p);
            unsafe {
                (*out).promise = crate::value::JSValue::from(p).0;
                (*out).resolve = rr[0].0;
                (*out).reject = rr[1].0;
            }
            0
        }
        Err(_) => -1,
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
unsafe extern "C" fn zapi_drop_waker(h: *mut ZqWakeHandleS) {
    if !h.is_null() {
        drop(unsafe { Box::from_raw(h as *mut ZqWakeHandle) });
    }
}

/// each tick runs `run(data)` on the JS thread until it reports Done/Err.
unsafe extern "C" fn zapi_add_job(
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
unsafe extern "C" fn zapi_new_builtin_function(
    env: ZqRawCtx,
    run_fn: ZqBodyFn,
    name: ZqRawValue,
    magic: i32,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };

    // Register the body and reuse the returned token as the function's magic so
    // the trampoline resolves the right body (a separate serial would miss it).
    let (cb, token) = register_zapi_func_magic(ctx, run_fn, magic);

    let name_val = JSValue(name, std::marker::PhantomData);
    let func_name = name_val
        .to_string(ctx)
        .map(|s| s.content().encode_utf8(false).to_string())
        .unwrap_or_default();

    match ctx.create_native_function(cb, &func_name, 0, token as _) {
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

/// Raise an arbitrary value as the pending exception. Returns `value_exception`
/// (the extension should propagate it).
unsafe extern "C" fn zapi_throw(env: ZqRawCtx, val: ZqRawValue) {
    let ctx = unsafe { ctx_from_env(env) };
    let _ = ctx.throw_value(JSValue(val, std::marker::PhantomData));
}

unsafe extern "C" fn zapi_get_type_of(val: ZqRawValue) -> i32 {
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

unsafe extern "C" fn zapi_value_to_float64(env: ZqRawCtx, val: ZqRawValue, ret: *mut f64) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    JSValue(val, std::marker::PhantomData)
        .to_number(ctx)
        .map_or(-1, |n| unsafe {
            *ret = n.to_f64();
            0 // success
        })
}

unsafe extern "C" fn zapi_value_to_int64(env: ZqRawCtx, val: ZqRawValue, ret: *mut i64) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    // ToNumeric: Number saturates, BigInt converts exactly (RangeError on
    // overflow). This is the single BigInt→i64 path, so no separate FFI.
    let num = JSValue(val, std::marker::PhantomData).to_numeric(ctx);
    match num {
        Ok(crate::number::NumValue::Float(n)) => {
            unsafe { *ret = n.to_i64_saturating() };
            0
        }
        #[cfg(feature = "big_int")]
        Ok(crate::number::NumValue::BigInt(b)) => match crate::bigint::to_i64_exact(&b) {
            Some(v) => {
                unsafe { *ret = v };
                0
            }
            None => {
                let _ = ctx.throw_range_error("BigInt does not fit in i64");
                -1
            }
        },
        Err(_) => -1,
    }
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

// ── numeric conversion (ToInt32) ──

unsafe extern "C" fn zapi_value_to_int32(env: ZqRawCtx, val: ZqRawValue, ret: *mut i32) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    JSValue(val, PhantomData)
        .to_number(ctx)
        .map_or(-1, |n| unsafe {
            *ret = n.to_i32();
            0
        })
}

unsafe extern "C" fn zapi_value_strict_equal(a: ZqRawValue, b: ZqRawValue) -> i32 {
    JSValue(a, PhantomData).strict_eq(JSValue(b, PhantomData)) as i32
}

// ── instanceof ──

unsafe extern "C" fn zapi_is_instance_of_class(
    env: ZqRawCtx,
    val: ZqRawValue,
    class_def: *const ZqClassDef,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let class_id = ctx.rt().zapi_class_id_of(class_def);
    let Some(cid) = class_id else {
        return -1;
    };
    let Some(ctor) = ctx.class_slots[cid as usize].ctor else {
        return 0;
    };
    JSValue(val, PhantomData)
        .instance_of(ctx, ctor.into())
        .map_or(-1, |b| b as i32)
}

// ── error object creation ──

unsafe extern "C" fn zapi_new_error(
    env: ZqRawCtx,
    err_type: i32,
    message: *const u8,
    message_len: usize,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(kind) = err_type_to_kind(err_type) else {
        let _ = ctx.throw_internal_error("unknown error type");
        return ZQ_API_TABLE.const_values[ZQ_CONST_VALUE_SLOT_EXCEPTION];
    };
    let msg: Cow<str> = if message.is_null() || message_len == 0 {
        Cow::Borrowed("")
    } else {
        let bytes = unsafe { std::slice::from_raw_parts(message, message_len) };
        String::from_utf8_lossy(bytes)
    };
    // Capture a backtrace like `new NativeError(msg)`, so errors created here
    // (and raised via `throw_value`) carry a `.stack`.
    ctx.create_error_object(kind, &msg, true).map_or(
        ZQ_API_TABLE.const_values[ZQ_CONST_VALUE_SLOT_EXCEPTION],
        |o| JSValue::from(o).0,
    )
}

// ── BigInt ──

#[cfg(feature = "big_int")]
unsafe extern "C" fn zapi_new_bigint_i64(env: ZqRawCtx, v: i64) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match crate::bigint::from_i64(ctx, v) {
        Ok(bi) => JSValue::make_bigint(bi).0,
        Err(_) => ZQ_API_TABLE.const_values[ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

#[cfg(not(feature = "big_int"))]
unsafe extern "C" fn zapi_new_bigint_i64(env: ZqRawCtx, _v: i64) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let _ = ctx.throw_type_error("BigInt is not supported");
    ZQ_API_TABLE.const_values[ZQ_CONST_VALUE_SLOT_EXCEPTION]
}

#[cfg(feature = "big_int")]
unsafe extern "C" fn zapi_new_bigint_u64(env: ZqRawCtx, v: u64) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match crate::bigint::from_u64(ctx, v) {
        Ok(bi) => JSValue::make_bigint(bi).0,
        Err(_) => ZQ_API_TABLE.const_values[ZQ_CONST_VALUE_SLOT_EXCEPTION],
    }
}

#[cfg(not(feature = "big_int"))]
unsafe extern "C" fn zapi_new_bigint_u64(env: ZqRawCtx, _v: u64) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let _ = ctx.throw_type_error("BigInt is not supported");
    ZQ_API_TABLE.const_values[ZQ_CONST_VALUE_SLOT_EXCEPTION]
}

// ── binary buffers ──

/// Map a typed array engine class id to a `ZQ_BUF_TYPED_ARRAY_*` discriminant.
const fn typed_array_class_id_to_buf_kind(class_id: ClassID) -> Option<i32> {
    Some(match class_id {
        JS_CLASS_INT8_ARRAY => ZQ_BUF_TYPED_ARRAY_INT8,
        JS_CLASS_UINT8_ARRAY => ZQ_BUF_TYPED_ARRAY_UINT8,
        JS_CLASS_UINT8C_ARRAY => ZQ_BUF_TYPED_ARRAY_UINT8C,
        JS_CLASS_INT16_ARRAY => ZQ_BUF_TYPED_ARRAY_INT16,
        JS_CLASS_UINT16_ARRAY => ZQ_BUF_TYPED_ARRAY_UINT16,
        JS_CLASS_INT32_ARRAY => ZQ_BUF_TYPED_ARRAY_INT32,
        JS_CLASS_UINT32_ARRAY => ZQ_BUF_TYPED_ARRAY_UINT32,
        JS_CLASS_FLOAT16_ARRAY => ZQ_BUF_TYPED_ARRAY_FLOAT16,
        JS_CLASS_FLOAT32_ARRAY => ZQ_BUF_TYPED_ARRAY_FLOAT32,
        JS_CLASS_FLOAT64_ARRAY => ZQ_BUF_TYPED_ARRAY_FLOAT64,
        JS_CLASS_BIG_INT64_ARRAY => ZQ_BUF_TYPED_ARRAY_BIGINT64,
        JS_CLASS_BIG_UINT64_ARRAY => ZQ_BUF_TYPED_ARRAY_BIGUINT64,
        _ => return None,
    })
}

/// Map a `ZQ_BUF_TYPED_ARRAY_*` discriminant back to an engine class id.
const fn buf_kind_to_typed_array_class_id(kind: i32) -> Option<ClassID> {
    Some(match kind {
        ZQ_BUF_TYPED_ARRAY_INT8 => JS_CLASS_INT8_ARRAY,
        ZQ_BUF_TYPED_ARRAY_UINT8 => JS_CLASS_UINT8_ARRAY,
        ZQ_BUF_TYPED_ARRAY_UINT8C => JS_CLASS_UINT8C_ARRAY,
        ZQ_BUF_TYPED_ARRAY_INT16 => JS_CLASS_INT16_ARRAY,
        ZQ_BUF_TYPED_ARRAY_UINT16 => JS_CLASS_UINT16_ARRAY,
        ZQ_BUF_TYPED_ARRAY_INT32 => JS_CLASS_INT32_ARRAY,
        ZQ_BUF_TYPED_ARRAY_UINT32 => JS_CLASS_UINT32_ARRAY,
        ZQ_BUF_TYPED_ARRAY_FLOAT16 => JS_CLASS_FLOAT16_ARRAY,
        ZQ_BUF_TYPED_ARRAY_FLOAT32 => JS_CLASS_FLOAT32_ARRAY,
        ZQ_BUF_TYPED_ARRAY_FLOAT64 => JS_CLASS_FLOAT64_ARRAY,
        ZQ_BUF_TYPED_ARRAY_BIGINT64 => JS_CLASS_BIG_INT64_ARRAY,
        ZQ_BUF_TYPED_ARRAY_BIGUINT64 => JS_CLASS_BIG_UINT64_ARRAY,
        _ => return None,
    })
}

/// `ZQ_BUF_*` kind of a binary buffer; `ZQ_BUF_NONE` for anything else.
unsafe extern "C" fn zapi_get_buf_type(_env: ZqRawCtx, val: ZqRawValue) -> i32 {
    let v = JSValue(val, PhantomData);
    if let Some(ta) = v.as_object_TypedArray() {
        return typed_array_class_id_to_buf_kind(ta.class_id()).unwrap_or(ZQ_BUF_NONE);
    }
    if let Some(ab) = v.as_object_ArrayBuffer() {
        // `as_object_ArrayBuffer` matches both shared and non-shared buffers.
        return if ab.class_id() == JS_CLASS_SHARED_ARRAY_BUFFER {
            ZQ_BUF_SHARED_ARRAY_BUFFER
        } else {
            ZQ_BUF_ARRAY_BUFFER
        };
    }
    if v.as_object_DataView().is_some() {
        return ZQ_BUF_DATAVIEW;
    }
    ZQ_BUF_NONE
}

/// Measure-then-copy read of a binary buffer's raw bytes. See
/// `ZqApi::buf_read_bytes`.
unsafe extern "C" fn zapi_buf_read_bytes(
    env: ZqRawCtx,
    val: ZqRawValue,
    dst: *mut u8,
    dst_len: usize,
    offset: usize,
) -> i64 {
    let ctx = unsafe { ctx_from_env(env) };
    let v = JSValue(val, PhantomData);

    // Current readable byte length; 0 for a detached / out-of-bounds view.
    let total = if let Some(ta) = v.as_object_TypedArray() {
        ta.byte_length()
    } else if let Some(ab) = v.as_object_ArrayBuffer() {
        ab.payload().byte_length()
    } else if let Some(dv) = v.as_object_DataView() {
        let p = dv.payload();
        if p.is_detached() || p.is_out_of_bounds() {
            0
        } else {
            p.view_byte_length()
        }
    } else {
        let _ = ctx.throw_type_error("value is not a binary buffer");
        return -1;
    };

    if dst.is_null() {
        return total as i64;
    }
    let n = total.saturating_sub(offset).min(dst_len);
    if n == 0 {
        return 0;
    }

    // Copy through `with_bytes`: it handles detached / out-of-bounds views and
    // clamps to a resized buffer, so the read is always in bounds.
    let mut copied = 0usize;
    let mut copy = |bytes: &[u8]| {
        if offset < bytes.len() {
            let end = (offset + n).min(bytes.len());
            let src = &bytes[offset..end];
            unsafe { std::ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len()) };
            copied = src.len();
        }
    };
    if let Some(ta) = v.as_object_TypedArray() {
        ta.with_bytes(&mut copy);
    } else if let Some(ab) = v.as_object_ArrayBuffer() {
        ab.payload().with_bytes(&mut copy);
    } else if let Some(dv) = v.as_object_DataView() {
        // `total != 0` rules out the detached / out-of-bounds case above.
        dv.payload().with_bytes(&mut copy);
    }
    copied as i64
}

/// Create a binary buffer by copying `len` bytes from `ptr`.
unsafe extern "C" fn zapi_new_buf_from_bytes(
    env: ZqRawCtx,
    kind: i32,
    ptr: *const u8,
    len: usize,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let bytes = if len == 0 {
        Vec::new()
    } else if ptr.is_null() {
        let _ = ctx.throw_type_error("null buffer source pointer");
        return zq_exc();
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec()
    };

    let built = match kind {
        ZQ_BUF_ARRAY_BUFFER => ArrayBuffer::from_bytes(
            ctx,
            JSValue::UNDEFINED,
            bytes.into_boxed_slice(),
            std::ptr::null_mut(),
        )
        .map(JSValue::from),
        ZQ_BUF_SHARED_ARRAY_BUFFER => {
            ArrayBuffer::from_shared(ctx, Arc::new(RwLock::new(bytes.into_boxed_slice())))
                .map(JSValue::from)
        }
        ZQ_BUF_DATAVIEW => {
            let len = bytes.len();
            match ArrayBuffer::from_bytes(
                ctx,
                JSValue::UNDEFINED,
                bytes.into_boxed_slice(),
                std::ptr::null_mut(),
            ) {
                Ok(ab) => {
                    DataView::create(ctx, JSValue::UNDEFINED, ab, 0, len, false).map(JSValue::from)
                }
                Err(e) => Err(e),
            }
        }
        _ => match buf_kind_to_typed_array_class_id(kind) {
            Some(class_id) => {
                TypedArray::create_from_bytes(ctx, bytes, class_id).map(JSValue::from)
            }
            None => {
                let _ = ctx.throw_type_error("invalid binary buffer kind");
                return zq_exc();
            }
        },
    };
    match built {
        Ok(v) => v.0,
        Err(_) => zq_exc(),
    }
}

unsafe extern "C" fn zapi_call_constructor(env: ZqRawCtx, info: *mut ZqCallInfoS) -> ZqRawValue {
    let ctx = unsafe { &mut *(env as *mut ScopedContext) };
    let info = unsafe { &*info };

    let target = JSValue(info.target, std::marker::PhantomData);
    let args: Vec<JSValue> = if info.argc > 0 {
        unsafe { std::slice::from_raw_parts(info.argv, info.argc) }
            .iter()
            .map(|&r| JSValue(r, std::marker::PhantomData))
            .collect()
    } else {
        Vec::new()
    };

    target.call_ctor(ctx, &args).map_or(
        ZQ_API_TABLE.const_values[zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION],
        |v| v.0,
    )
}

// ── property definition (descriptor) ──

unsafe extern "C" fn zapi_define_property_str(
    env: ZqRawCtx,
    target: ZqRawValue,
    key: *const u8,
    key_len: usize,
    val: ZqRawValue,
    flags: u32,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = str_key_atom(ctx, key, key_len) else {
        return -1;
    };
    define_property_impl(ctx, target, atom, val, flags)
}

unsafe extern "C" fn zapi_define_property(
    env: ZqRawCtx,
    target: ZqRawValue,
    key: ZqRawValue,
    val: ZqRawValue,
    flags: u32,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = JSValue(key, PhantomData).to_atom(ctx) else {
        return -1;
    };
    define_property_impl(ctx, target, atom, val, flags)
}

fn define_property_impl(
    ctx: &mut ScopedContext,
    target: ZqRawValue,
    atom: Atom,
    val: ZqRawValue,
    flags: u32,
) -> i32 {
    let Some(mut obj) = JSValue(target, PhantomData).as_object() else {
        let _ = ctx.throw_type_error("not an object");
        return -1;
    };
    obj.define_property_value(
        ctx,
        atom,
        JSValue(val, PhantomData),
        zq_to_js_prop_flags(flags) | JS_PROP_THROW,
    )
    .map_or(-1, |b| b as i32)
}

unsafe extern "C" fn zapi_define_accessor_str(
    env: ZqRawCtx,
    target: ZqRawValue,
    key: *const u8,
    key_len: usize,
    get: ZqRawValue,
    set: ZqRawValue,
    flags: u32,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = str_key_atom(ctx, key, key_len) else {
        return -1;
    };
    define_accessor_impl(ctx, target, atom, get, set, flags)
}

unsafe extern "C" fn zapi_define_accessor(
    env: ZqRawCtx,
    target: ZqRawValue,
    key: ZqRawValue,
    get: ZqRawValue,
    set: ZqRawValue,
    flags: u32,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = JSValue(key, PhantomData).to_atom(ctx) else {
        return -1;
    };
    define_accessor_impl(ctx, target, atom, get, set, flags)
}

fn define_accessor_impl(
    ctx: &mut ScopedContext,
    target: ZqRawValue,
    atom: Atom,
    get: ZqRawValue,
    set: ZqRawValue,
    flags: u32,
) -> i32 {
    let Some(mut obj) = JSValue(target, PhantomData).as_object() else {
        let _ = ctx.throw_type_error("not an object");
        return -1;
    };
    obj.define_property_getset(
        ctx,
        atom,
        JSValue(get, PhantomData),
        JSValue(set, PhantomData),
        zq_to_js_prop_flags(flags) | JS_PROP_THROW,
    )
    .map_or(-1, |b| b as i32)
}

// ── own property introspection ──

fn fill_own_property_descriptor(
    ctx: &mut ScopedContext,
    target: ZqRawValue,
    atom: Atom,
    out: *mut ZqPropertyDescriptorS,
) -> i32 {
    let Some(mut obj) = JSValue(target, PhantomData).as_object() else {
        let _ = ctx.throw_type_error("not an object");
        return -1;
    };
    match obj.get_own_property(ctx, atom) {
        Ok(Some(desc)) => {
            let accessor = desc.flags & JS_PROP_GETSET != 0;
            unsafe {
                *out = ZqPropertyDescriptorS {
                    kind: if accessor {
                        ZQ_PROP_KIND_ACCESSOR
                    } else {
                        ZQ_PROP_KIND_DATA
                    },
                    flags: js_to_zq_prop_flags(desc.flags),
                    value: if accessor { 0 } else { desc.value.0 },
                    get: if accessor { desc.getter.0 } else { 0 },
                    set: if accessor { desc.setter.0 } else { 0 },
                };
            }
            1
        }
        Ok(None) => 0,
        Err(_) => -1,
    }
}

unsafe extern "C" fn zapi_get_own_property_descriptor_str(
    env: ZqRawCtx,
    target: ZqRawValue,
    key: *const u8,
    key_len: usize,
    out: *mut ZqPropertyDescriptorS,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = str_key_atom(ctx, key, key_len) else {
        return -1;
    };
    fill_own_property_descriptor(ctx, target, atom, out)
}

unsafe extern "C" fn zapi_get_own_property_descriptor(
    env: ZqRawCtx,
    target: ZqRawValue,
    key: ZqRawValue,
    out: *mut ZqPropertyDescriptorS,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Ok(atom) = JSValue(key, PhantomData).to_atom(ctx) else {
        return -1;
    };
    fill_own_property_descriptor(ctx, target, atom, out)
}

unsafe extern "C" fn zapi_get_own_property_names(
    env: ZqRawCtx,
    target: ZqRawValue,
    include_symbols: i32,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(mut obj) = JSValue(target, PhantomData).as_object() else {
        let _ = ctx.throw_type_error("not an object");
        return zq_exc();
    };
    let mut flags = JS_GPN_STR_OR_INT_MASK;
    if include_symbols != 0 {
        flags |= JS_GPN_SYMBOL_MASK;
    }

    // `get_own_property_names` returns OwnPropertyKeys order (integer keys
    // ascending, then strings, then symbols); the `_foreach` variant would
    // yield shape/definition order instead.
    let keys = match obj.get_own_property_names(ctx, flags) {
        Ok(k) => k,
        Err(_) => return zq_exc(),
    };
    let mut vals: Vec<JSValue> = Vec::with_capacity(keys.len());
    for pk in keys.iter() {
        match pk.atom.to_value(ctx) {
            Ok(v) => vals.push(v),
            Err(_) => return zq_exc(),
        }
    }
    Array::from_slice(ctx, &vals).map_or(zq_exc(), |a| JSValue::from(a).0)
}

// ── value queries ──

unsafe extern "C" fn zapi_is_array(env: ZqRawCtx, val: ZqRawValue) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    match JSValue(val, PhantomData).as_object() {
        Some(o) => o.is_array(ctx).map_or(-1, |b| b as i32),
        None => 0,
    }
}

unsafe extern "C" fn zapi_is_callable(val: ZqRawValue) -> i32 {
    JSValue(val, PhantomData).is_callable() as i32
}

unsafe extern "C" fn zapi_is_constructor(val: ZqRawValue) -> i32 {
    JSValue(val, PhantomData)
        .as_object()
        .is_some_and(|o| o.is_constructor()) as i32
}

unsafe extern "C" fn zapi_is_instance_of(
    env: ZqRawCtx,
    val: ZqRawValue,
    target: ZqRawValue,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    JSValue(val, PhantomData)
        .instance_of(ctx, JSValue(target, PhantomData))
        .map_or(-1, |b| b as i32)
}

// ── value constructors (symbol / object with proto) ──

unsafe extern "C" fn zapi_new_symbol(env: ZqRawCtx, ptr: *const u8, len: usize) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let Ok(s) = std::str::from_utf8(bytes) else {
        let _ = ctx.throw_type_error("symbol description is not valid UTF-8");
        return zq_exc();
    };
    let Ok(desc) = ctx.create_string_interned(s) else {
        return zq_exc();
    };
    match ctx.create_symbol(Some(desc), AtomKind::Symbol) {
        Ok(sym) => sym.0,
        Err(_) => zq_exc(),
    }
}

unsafe extern "C" fn zapi_new_object_with_proto(env: ZqRawCtx, proto: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let pv = JSValue(proto, PhantomData);
    let proto = if pv.is_undefined_or_null() {
        None
    } else if let Some(o) = pv.as_object() {
        Some(o)
    } else {
        let _ = ctx.throw_type_error("prototype must be an object or null");
        return zq_exc();
    };
    match ctx.create_base_object_with_proto(proto) {
        Ok(o) => JSValue::from(o).0,
        Err(_) => zq_exc(),
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
        new_string: zapi_new_string,
        new_symbol: zapi_new_symbol,
        new_object: zapi_new_object,
        new_object_with_proto: zapi_new_object_with_proto,
        new_array: zapi_new_array,

        // ── value query / conversion ──
        get_type_of: zapi_get_type_of,
        is_array: zapi_is_array,
        is_callable: zapi_is_callable,
        is_constructor: zapi_is_constructor,
        to_boolean: zapi_value_to_bool,
        to_int64: zapi_value_to_int64,
        to_number: zapi_value_to_float64,
        to_string: zapi_value_to_string,
        read_string_utf8: zapi_read_string_utf8,

        // ── property access (raw value key: String | Symbol) ──
        get_property: zapi_get_property,
        set_property: zapi_set_property,
        has_property: zapi_has_property,
        delete_property: zapi_delete_property,

        // ── property access (UTF-8 key, interned) ──
        get_property_str: zapi_get_property_str,
        set_property_str: zapi_set_property_str,
        has_property_str: zapi_has_property_str,
        delete_property_str: zapi_delete_property_str,

        // ── property definition (descriptor) ──
        define_property: zapi_define_property,
        define_property_str: zapi_define_property_str,
        define_accessor: zapi_define_accessor,
        define_accessor_str: zapi_define_accessor_str,

        // ── array-like length ──
        get_length: zapi_get_length,

        // ── own property introspection ──
        get_own_property_descriptor: zapi_get_own_property_descriptor,
        get_own_property_descriptor_str: zapi_get_own_property_descriptor_str,
        get_own_property_names: zapi_get_own_property_names,

        // ── function call ──
        call: zapi_call_function,
        construct: zapi_call_constructor,
        invoke: zapi_invoke,
        invoke_str: zapi_invoke_str,

        // ── global object ──
        get_global_str: zapi_get_global_str,
        set_global_str: zapi_set_global_str,

        // ── native object / class ──
        register_class: zapi_register_class,
        new_instance: zapi_new_instance,
        get_object_payload: zapi_get_object_payload,
        new_builtin_function: zapi_new_builtin_function,

        // ── promise ──
        new_promise_capability: zapi_new_promise_capability,

        // ── async iterator ──
        new_async_iterator: zapi_new_async_iterator,

        // ── job queue ──
        add_job: zapi_add_job,

        // ── GC ──
        gc_mark: zapi_gc_mark,
        gc_set_trace_func: zapi_gc_set_trace_func,

        // ── exception ──
        throw: zapi_throw,
        take_exception: zapi_take_exception,
        exception: zapi_exception,

        // ── event-loop wake ──
        new_waker: zapi_new_waker,
        wake: zapi_waker_wake,
        drop_waker: zapi_drop_waker,

        // ── numeric conversion (ToInt32) ──
        to_int32: zapi_value_to_int32,
        is_strictly_equal: zapi_value_strict_equal,

        // ── instanceof ──
        instanceof: zapi_is_instance_of,
        instanceof_class: zapi_is_instance_of_class,

        // ── error object creation ──
        new_error: zapi_new_error,

        // ── BigInt construction ──
        new_bigint_i64: zapi_new_bigint_i64,
        new_bigint_u64: zapi_new_bigint_u64,

        // ── binary buffers ──
        new_buf_from_bytes: zapi_new_buf_from_bytes,
        get_buf_type: zapi_get_buf_type,
        read_buf_bytes: zapi_buf_read_bytes,
    })
}

/// Tests for zapi dynamic class registration (`register_class_def`), the path
/// used by zapi extension classes. Host native classes (the
/// `def_native_object!` linkme path) are covered in `src/class/native.rs`.
#[cfg(test)]
mod class_registration_tests {
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    use gc_lite::GcTrace;

    use crate::{ClassDef, JSValue, Runtime, RuntimeImpl, ScopedContext};

    /// Serializes registration so debug output from concurrent tests stays
    /// readable.
    static REG_LOCK: Mutex<()> = Mutex::new(());

    /// Payload carrying a magic value: `on_finalize` must observe it while the
    /// allocation is still valid.
    #[derive(GcTrace)]
    struct DispPayload {
        magic: u32,
    }

    /// Monotonic sequence: bumped once when the free channel runs.
    static FREE_SEQ: AtomicUsize = AtomicUsize::new(0);
    /// Sequence value observed by `on_finalize` — `0` proves finalize ran
    /// strictly before the free channel.
    static DISPOSE_SEQ: AtomicUsize = AtomicUsize::new(usize::MAX);
    /// Magic read through the payload pointer inside `on_finalize`.
    static DISPOSE_MAGIC: AtomicU32 = AtomicU32::new(0);

    /// Free channel for the test class (custom, to make the ordering
    /// observable; the macro normally wires `drop_payload_fn::<P>` here).
    unsafe fn test_free(ptr: *mut c_void) {
        FREE_SEQ.fetch_add(1, Ordering::SeqCst);
        if !ptr.is_null() {
            drop(unsafe { Box::from_raw(ptr as *mut DispPayload) });
        }
    }

    fn test_dispose(_rt: &mut RuntimeImpl, ptr: *mut c_void, _cid: usize, _id: usize) {
        DISPOSE_SEQ.store(FREE_SEQ.load(Ordering::SeqCst), Ordering::SeqCst);
        if !ptr.is_null() {
            // Reading through the pointer is only sound because dispose runs
            // BEFORE the free channel — exactly the contract under test.
            let p = unsafe { &*(ptr as *const DispPayload) };
            DISPOSE_MAGIC.store(p.magic, Ordering::SeqCst);
        }
    }

    /// `on_finalize` must fire before the payload is freed, with a still-valid
    /// payload pointer; `payload_drop` releases it afterwards.
    #[test]
    fn on_finalize_fires_before_payload_free() -> Result<(), i32> {
        let _lock = REG_LOCK.lock().unwrap();
        let mut rt = Runtime::create()?;
        let class_id = rt.register_class_def(
            ClassDef::new("DispTest", &[], None)
                .on_finalize(test_dispose)
                .payload_drop(test_free),
            std::ptr::null(),
        )?;

        let mut ctx = rt.create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            cx.use_class(class_id)?;
            Ok(())
        })?;

        // Create the instance in a child scope and let it die with the scope:
        // objects created in a live scope are scope-locals and survive
        // collection until the scope pops (the context's root scope outlives
        // the test).
        ctx.with_new_scope(|cx| {
            let _obj = cx.create_class_instance(
                JSValue::UNDEFINED,
                class_id,
                DispPayload { magic: 0xCAFE },
            )?;
            Ok::<(), i32>(())
        })?;

        rt.gc_collect();

        assert_eq!(DISPOSE_MAGIC.load(Ordering::SeqCst), 0xCAFE);
        assert_eq!(DISPOSE_SEQ.load(Ordering::SeqCst), 0);
        assert!(FREE_SEQ.load(Ordering::SeqCst) >= 1);
        Ok(())
    }

    /// user class ids are sequential starting from the first dynamic id.
    #[test]
    fn user_class_ids_are_sequential() -> Result<(), i32> {
        let _lock = REG_LOCK.lock().unwrap();
        let mut rt = Runtime::create()?;
        let base = crate::class::class_count() as crate::ClassID;
        let id1 = rt.register_class_def(ClassDef::new("SeqA", &[], None), std::ptr::null())?;
        let id2 = rt.register_class_def(ClassDef::new("SeqB", &[], None), std::ptr::null())?;
        let id3 = rt.register_class_def(ClassDef::new("SeqC", &[], None), std::ptr::null())?;

        assert_eq!(id1, base);
        assert_eq!(id2, base + 1);
        assert_eq!(id3, base + 2);
        Ok(())
    }

    /// duplicate class name triggers a debug warning (visible in
    /// debug-assert stderr). We only verify the second registration
    /// succeeds — the debug warning is fire-and-forget.
    #[test]
    fn duplicate_class_name_allowed() -> Result<(), i32> {
        let _lock = REG_LOCK.lock().unwrap();
        let mut rt = Runtime::create()?;
        let _id1 = rt.register_class_def(ClassDef::new("DupTest", &[], None), std::ptr::null())?;
        let id2 = rt.register_class_def(ClassDef::new("DupTest", &[], None), std::ptr::null())?;
        // Both registrations succeed (slot uniqueness is by id, not name).
        assert!(id2 > _id1);
        Ok(())
    }

    /// `payload_type_name` is set on user class via the macro.
    #[test]
    #[cfg(debug_assertions)]
    fn user_class_payload_type_name_set() -> Result<(), i32> {
        use crate::HOST_CLASS_ID_START;

        let _lock = REG_LOCK.lock().unwrap();
        let mut rt = Runtime::create()?;
        let id = rt.register_class_def(
            ClassDef::new("TypeCheckTest", &[], None).payload_type_name(Some("u32")),
            std::ptr::null(),
        )?;
        assert!(id >= HOST_CLASS_ID_START);
        let cd = rt.class_def(id).unwrap();
        assert_eq!(cd.payload_type_name, Some("u32"));
        Ok(())
    }
}
