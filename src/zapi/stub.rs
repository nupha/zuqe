// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use once_cell::sync::Lazy;
use zapi::ffi::{
    ZQ_ABI_VERSION, ZqApi, ZqCallbackInfoS, ZqClassDef, ZqEnv, ZqEnvS, ZqGcTraceCtxS, ZqModuleInfo,
    ZqRawValue,
};

use crate::{JSValue, ScopedContext, class::ClassID};

use super::host::{
    push_accessor_fields, push_method_fields, register_zapi_func_magic, zapi_trampoline_fn,
};

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

fn msg_str<'a>(ptr: *const u8, len: usize) -> &'a str {
    if ptr.is_null() {
        return "";
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(bytes).unwrap_or("")
}

fn symbol_from_desc(ctx: &mut ScopedContext, desc: JSValue) -> Result<JSValue, i32> {
    if desc.is_undefined() {
        ctx.create_symbol(None, crate::atom::AtomKind::Symbol)
    } else {
        let sr = desc.to_string(ctx)?;
        ctx.create_symbol(Some(sr), crate::atom::AtomKind::Symbol)
    }
}

const unsafe extern "C" fn zapi_undefined_raw(_env: ZqEnv) -> ZqRawValue {
    JSValue::UNDEFINED.0
}

const unsafe extern "C" fn zapi_null_raw(_env: ZqEnv) -> ZqRawValue {
    JSValue::NULL.0
}

const unsafe extern "C" fn zapi_bool_raw(_env: ZqEnv, v: bool) -> ZqRawValue {
    if v {
        crate::JSValue::TRUE.0
    } else {
        crate::JSValue::FALSE.0
    }
}

unsafe extern "C" fn zapi_number_raw(_env: ZqEnv, v: f64) -> ZqRawValue {
    JSValue::from(v).0
}

unsafe extern "C" fn zapi_string_raw(env: ZqEnv, ptr: *const u8, len: usize) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    match ctx.create_string_from_utf8(bytes.to_vec()) {
        Ok(s) => JSValue::make_string(s).0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_symbol_raw(env: ZqEnv, desc: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match symbol_from_desc(ctx, JSValue(desc, std::marker::PhantomData)) {
        Ok(v) => v.0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_new_symbol(env: ZqEnv, desc: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match symbol_from_desc(ctx, JSValue(desc, std::marker::PhantomData)) {
        Ok(v) => v.0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_symbol_to_string(env: ZqEnv, sym: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let sym = JSValue(sym, std::marker::PhantomData);
    let sr = match sym.as_symbol(true) {
        Some(s) => s,
        None => return 0,
    };
    let repr = if sr.content().is_ident() {
        "Symbol()".to_string()
    } else {
        format!("Symbol({})", sr.encode_utf8(false))
    };
    match ctx.create_string_from_utf8(repr.into_bytes()) {
        Ok(s) => JSValue::make_string(s).0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_get_symbol_description(env: ZqEnv, sym: ZqRawValue) -> ZqRawValue {
    let _ctx = unsafe { ctx_from_env(env) };
    let sym = JSValue(sym, std::marker::PhantomData);
    let sr = match sym.as_symbol(true) {
        Some(s) => s,
        None => return 0,
    };
    if sr.content().is_ident() {
        JSValue::UNDEFINED.0
    } else {
        JSValue::make_string(sr).0
    }
}

unsafe extern "C" fn zapi_string_len(_env: ZqEnv, s: ZqRawValue) -> usize {
    match JSValue(s, std::marker::PhantomData).as_string(false) {
        Some(sr) => sr.content().encode_utf8_bytes(false).len(),
        None => 0,
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
    let n = (bytes.len() - start).min(buf_len);
    if n > 0 && !buf.is_null() {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr().add(start), buf, n) };
    }
    n as i32
}

// The string builder is a JS array of UTF-8 chunk strings (the ABI cannot
// hand back a mutable handle, so each append pushes a chunk; finish joins).
unsafe extern "C" fn zapi_new_string_builder(env: ZqEnv, _cap: usize) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match crate::object::Array::create(ctx) {
        Ok(arr) => JSValue::from(arr).0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_string_builder_append(
    env: ZqEnv,
    builder: ZqRawValue,
    ptr: *const u8,
    len: usize,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let bytes = if ptr.is_null() {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len) }
    };
    let chunk = match ctx.create_string_from_utf8(bytes.to_vec()) {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let Some(mut o) = JSValue(builder, std::marker::PhantomData).as_object() else {
        return -1;
    };
    let idx = match o.get_length_prop(ctx) {
        Ok(i) => i,
        Err(_) => return -1,
    };
    if o.set_property_i64(ctx, idx, JSValue::make_string(chunk))
        .is_err()
    {
        return -1;
    }
    if o.set_property(ctx, crate::atom::Atom::CONST_length, JSValue::from(idx + 1))
        .is_err()
    {
        return -1;
    }
    0
}

unsafe extern "C" fn zapi_string_builder_finish(
    env: ZqEnv,
    builder: ZqRawValue,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(o) = JSValue(builder, std::marker::PhantomData).as_object() else {
        return 0;
    };
    let len = match o.get_length_prop(ctx) {
        Ok(i) => i,
        Err(_) => return 0,
    };
    let mut out: Vec<u8> = Vec::new();
    for i in 0..len {
        let key = JSValue::from(i as i32);
        let v = match o.get_property_value(ctx, key) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(sr) = v.as_string(false) {
            out.extend_from_slice(&sr.content().encode_utf8_bytes(false));
        }
    }
    match ctx.create_string_from_utf8(out) {
        Ok(s) => JSValue::make_string(s).0,
        Err(_) => 0,
    }
}

// Strings are GC-managed; nothing to free.
const unsafe extern "C" fn zapi_free_string(_env: ZqEnv, _s: ZqRawValue) {}

unsafe extern "C" fn zapi_new_object(env: ZqEnv) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match ctx.create_base_object() {
        Ok(o) => JSValue::from(o).0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_new_array(env: ZqEnv, len: usize) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match crate::object::Array::create(ctx) {
        Ok(mut arr) => {
            if arr.set_length(ctx, len as u32, true).is_err() {
                return 0;
            }
            JSValue::from(arr).0
        }
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_get_property(
    env: ZqEnv,
    obj: ZqRawValue,
    key: ZqRawValue,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let val = JSValue(key, std::marker::PhantomData);
    let Some(obj) = JSValue(obj, std::marker::PhantomData).as_object() else {
        return 0;
    };
    match obj.get_property_value(ctx, val) {
        Ok(v) => v.0,
        Err(_) => 0,
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
        return 0;
    };
    match obj.set_property_value(ctx, k, v, 0) {
        Ok(_) => val,
        Err(_) => 0,
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

unsafe extern "C" fn zapi_array_get(
    env: ZqEnv,
    arr: ZqRawValue,
    idx: ZqRawValue,
) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(o) = JSValue(arr, std::marker::PhantomData).as_object() else {
        return 0;
    };
    match o.get_property_value(ctx, JSValue(idx, std::marker::PhantomData)) {
        Ok(v) => v.0,
        Err(_) => 0,
    }
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
        return 0;
    };
    match o.get_length_prop(ctx) {
        Ok(i) => JSValue::from(i as i32).0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_set_global(env: ZqEnv, name: ZqRawValue, val: ZqRawValue) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let mut gobj = ctx.global_obj();
    match gobj.set_property_value(
        ctx,
        JSValue(name, std::marker::PhantomData),
        JSValue(val, std::marker::PhantomData),
        0,
    ) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

unsafe extern "C" fn zapi_get_global(env: ZqEnv, name: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let gobj = ctx.global_obj();
    match gobj.get_property_value(ctx, JSValue(name, std::marker::PhantomData)) {
        Ok(v) => v.0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_has_global(env: ZqEnv, name: ZqRawValue) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let gobj = ctx.global_obj();
    let atom = match JSValue(name, std::marker::PhantomData).to_atom(ctx) {
        Ok(a) => a,
        Err(_) => return 0,
    };
    match gobj.has_property(ctx, atom) {
        Ok(b) => b as i32,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_new_function(
    env: ZqEnv,
    name: ZqRawValue,
    body: Option<unsafe extern "C" fn(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue>,
    len: u8,
) -> ZqRawValue {
    let Some(body) = body else {
        return 0;
    };
    let ctx = unsafe { ctx_from_env(env) };
    let name = match JSValue(name, std::marker::PhantomData).to_string(ctx) {
        Ok(s) => s.encode_utf8(false).into_owned(),
        Err(_) => return 0,
    };
    let (cb, token) = register_zapi_func_magic(ctx, body, 0);
    match ctx.create_native_function(cb, &name, len, token as i16) {
        Ok(nf) => JSValue::from(nf).0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_register_function(
    env: ZqEnv,
    obj: ZqRawValue,
    name: ZqRawValue,
    body: Option<unsafe extern "C" fn(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue>,
    len: u8,
) -> i32 {
    let Some(body) = body else {
        return -1;
    };
    let ctx = unsafe { ctx_from_env(env) };
    let name = match JSValue(name, std::marker::PhantomData).to_string(ctx) {
        Ok(s) => s.encode_utf8(false).into_owned(),
        Err(_) => return -1,
    };
    let Some(mut o) = JSValue(obj, std::marker::PhantomData).as_object() else {
        return -1;
    };
    let (cb, token) = register_zapi_func_magic(ctx, body, 0);
    let nf = match ctx.create_native_function(cb, &name, len, token as i16) {
        Ok(nf) => nf,
        Err(_) => return -1,
    };
    match o.define_property_value_str(
        ctx,
        &name,
        JSValue::from(nf),
        crate::object::JS_PROP_WRITABLE | crate::object::JS_PROP_CONFIGURABLE,
    ) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

unsafe extern "C" fn zapi_remove_wrap(
    env: ZqEnv,
    obj: ZqRawValue,
    name: *const std::ffi::c_char,
) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(mut o) = JSValue(obj, std::marker::PhantomData).as_object() else {
        return -1;
    };
    let name = if name.is_null() {
        ""
    } else {
        unsafe { std::ffi::CStr::from_ptr(name) }
            .to_str()
            .unwrap_or("")
    };
    match o.delete_property_str(ctx, name, 0) {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

unsafe extern "C" fn zapi_call_function(
    env: ZqEnv,
    info: *mut ZqCallbackInfoS,
) -> ZqRawValue {
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
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_exception_pending(env: ZqEnv) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    (ctx.rt().exception() != crate::error::Exception::No) as i32
}

unsafe extern "C" fn zapi_throw(env: ZqEnv, val: ZqRawValue) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    ctx.throw(crate::error::Exception::Error(JSValue(
        val,
        std::marker::PhantomData,
    )));
    -1
}

unsafe extern "C" fn zapi_throw_type_error(env: ZqEnv, msg: *const u8, len: usize) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    ctx.throw_type_error(msg_str(msg, len))
}

unsafe extern "C" fn zapi_throw_range_error(env: ZqEnv, msg: *const u8, len: usize) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    ctx.throw_range_error(msg_str(msg, len))
}

unsafe extern "C" fn zapi_set_exception(env: ZqEnv, val: ZqRawValue) {
    let ctx = unsafe { ctx_from_env(env) };
    ctx.throw(crate::error::Exception::Error(JSValue(
        val,
        std::marker::PhantomData,
    )));
}

unsafe extern "C" fn zapi_get_last_error_message(env: ZqEnv) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let text = ctx.rt().exception().to_text(ctx);
    match ctx.create_string_from_str(&text.message) {
        Ok(s) => JSValue::make_string(s).0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_get_and_clear_last_exception(env: ZqEnv) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let exc = ctx.take_exception();
    exc.to_value(ctx).0
}

unsafe extern "C" fn zapi_gc_add_ref(env: ZqEnv, val: ZqRawValue) {
    let ctx = unsafe { ctx_from_env(env) };
    let v = JSValue(val, std::marker::PhantomData);
    ctx.gc_protect_transient_force(&v);
}

unsafe extern "C" fn zapi_gc_release(env: ZqEnv, val: ZqRawValue) {
    let ctx = unsafe { ctx_from_env(env) };
    if let Some(node) = JSValue(val, std::marker::PhantomData).as_gc_node() {
        ctx.remove_local(node);
    }
}

unsafe extern "C" fn zapi_set_global_trace(env: ZqEnv, trace: usize) {
    if trace == 0 {
        return;
    }
    let ctx = unsafe { ctx_from_env(env) };
    let handler = move |gcx: &mut gc_lite::GcTraceCtx| {
        let mut trace_ctx = ZqGcTraceCtxS {
            inner: (gcx as *mut gc_lite::GcTraceCtx).cast::<c_void>(),
        };
        unsafe {
            (std::mem::transmute::<usize, unsafe extern "C" fn(*mut ZqGcTraceCtxS)>(trace))(
                &mut trace_ctx,
            )
        };
    };
    ctx.rt_mut().gc_set_trace_handler(handler);
}

fn zapi_gc_trace_bridge(opaque: *mut c_void, gcx: &mut gc_lite::GcTraceCtx) {
    let rt = unsafe { &*(gcx.opaque() as *const crate::RuntimeImpl) };
    let def = unsafe { &*rt.zapi_pending_class_def };
    if let Some(gc_trace) = def.gc_trace {
        let mut trace_ctx = ZqGcTraceCtxS {
            inner: (gcx as *mut gc_lite::GcTraceCtx).cast::<c_void>(),
        };
        unsafe { gc_trace(&mut trace_ctx, opaque) };
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
        if !def.proto_methods.is_null() {
            let ms =
                unsafe { std::slice::from_raw_parts(def.proto_methods, def.proto_methods_len) };
            push_method_fields(ctx, &mut proto_entries, ms)?;
        }
        if !def.proto_accessors.is_null() {
            let as_ =
                unsafe { std::slice::from_raw_parts(def.proto_accessors, def.proto_accessors_len) };
            push_accessor_fields(ctx, &mut proto_entries, as_)?;
        }
        if !def.static_methods.is_null() {
            let ms =
                unsafe { std::slice::from_raw_parts(def.static_methods, def.static_methods_len) };
            push_method_fields(ctx, &mut static_entries, ms)?;
        }
        if !def.static_accessors.is_null() {
            let as_ = unsafe {
                std::slice::from_raw_parts(def.static_accessors, def.static_accessors_len)
            };
            push_accessor_fields(ctx, &mut static_entries, as_)?;
        }

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

    // Store ZqClassDef in RuntimeImpl so bridges can access it.
    ctx.rt_mut().zapi_pending_class_def = class_def;

    if def.gc_trace.is_some() {
        cd = cd.on_gc_trace(zapi_gc_trace_bridge);
    }
    if def.finalize.is_some() {
        cd = cd.on_dispose(|_rt, opaque, _| {
            let def = unsafe { &*_rt.zapi_pending_class_def };
            if let Some(fin) = def.finalize {
                unsafe { fin(std::ptr::null_mut(), opaque) };
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
        return 0;
    };
    match ctx.create_class_instance_with_opaque(JSValue::UNDEFINED, cid, payload) {
        Ok(obj) => JSValue::from(obj).0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_get_native_ptr(_env: ZqEnv, obj: ZqRawValue) -> *mut c_void {
    let val = JSValue(obj, std::marker::PhantomData);
    val.opaque()
        .map(|p| p.as_ptr())
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn zapi_set_native_ptr(
    _env: ZqEnv,
    obj: ZqRawValue,
    ptr: *mut c_void,
) -> i32 {
    let val = JSValue(obj, std::marker::PhantomData);
    let Some(mut o) = val.as_object() else {
        return -1;
    };
    if let Some(other) = o.payload_mut().other_mut() {
        unsafe { other.replace_opaque(ptr) };
        0
    } else {
        -1
    }
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

const unsafe extern "C" fn zapi_gc_release_trace(
    _env: ZqEnv,
    _trace_ctx: *mut ZqGcTraceCtxS,
) {
    // GcTraceCtx cleanup is handled by the GC engine.
}

unsafe extern "C" fn zapi_get_class_def(env: ZqEnv, obj: ZqRawValue) -> *const ZqClassDef {
    let ctx = unsafe { ctx_from_env(env) };
    let Some(o) = JSValue(obj, std::marker::PhantomData).as_object() else {
        return std::ptr::null();
    };
    let cid = o.class_id();
    let ptr = ctx
        .rt()
        .zapi_class_map
        .lock()
        .unwrap()
        .get(&cid)
        .copied()
        .unwrap_or(0);
    ptr as *const ZqClassDef
}

unsafe extern "C" fn zapi_get_raw(_env: ZqEnv, obj: ZqRawValue) -> ZqRawValue {
    JSValue(obj, std::marker::PhantomData).0
}

unsafe extern "C" fn zapi_type_of(_env: ZqEnv, val: ZqRawValue) -> i32 {
    use zapi::ffi::{
        ZQ_TYPE_BOOL, ZQ_TYPE_FUNCTION, ZQ_TYPE_NULL, ZQ_TYPE_NUMBER, ZQ_TYPE_OBJECT,
        ZQ_TYPE_STRING, ZQ_TYPE_SYMBOL, ZQ_TYPE_UNDEFINED,
    };
    let v = JSValue(val, std::marker::PhantomData);
    match v.tag() {
        crate::value::JSValueTag::Undefined | crate::value::JSValueTag::Uninitialized => {
            ZQ_TYPE_UNDEFINED
        }
        crate::value::JSValueTag::Null => ZQ_TYPE_NULL,
        crate::value::JSValueTag::Bool => ZQ_TYPE_BOOL,
        crate::value::JSValueTag::Int | crate::value::JSValueTag::Float64 => ZQ_TYPE_NUMBER,
        crate::value::JSValueTag::String | crate::value::JSValueTag::StringRope => ZQ_TYPE_STRING,
        crate::value::JSValueTag::Symbol => ZQ_TYPE_SYMBOL,
        crate::value::JSValueTag::Object
        | crate::value::JSValueTag::FunctionBytecode
        | crate::value::JSValueTag::Module => {
            if v.is_callable() {
                ZQ_TYPE_FUNCTION
            } else {
                ZQ_TYPE_OBJECT
            }
        }
        crate::value::JSValueTag::CatchOffset | crate::value::JSValueTag::Exception => {
            ZQ_TYPE_OBJECT
        }
    }
}

unsafe extern "C" fn zapi_number_value(_env: ZqEnv, val: ZqRawValue) -> f64 {
    JSValue(val, std::marker::PhantomData)
        .as_number()
        .map(|n| n.to_f64())
        .unwrap_or(f64::NAN)
}

unsafe extern "C" fn zapi_bool_value(_env: ZqEnv, val: ZqRawValue) -> i32 {
    match JSValue(val, std::marker::PhantomData).to_bool() {
        Ok(b) => b as i32,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_string_value(env: ZqEnv, val: ZqRawValue) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    match JSValue(val, std::marker::PhantomData).to_string(ctx) {
        Ok(s) => JSValue::make_string(s).0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_call_constructor(
    env: ZqEnv,
    info: *mut ZqCallbackInfoS,
) -> ZqRawValue {
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
            Err(_) => 0,
        },
        None => 0,
    }
}

// Dead hook: extensions self-register through their own `zq_module_register`,
// which fills the `ZqModuleInfo` the host consumes in `load_extension`.
const unsafe extern "C" fn zapi_module_register(_api: *mut ZqApi, _info: *mut ZqModuleInfo) {}

/// 全局共享的 ZqApi 表 — 纯函数指针，无运行时状态，多 Runtime 安全复用。
pub(crate) static ZQ_API_TABLE: Lazy<Box<ZqApi>> = Lazy::new(new_zapi);

/// 构造 host 侧的 `ZqApi` 表，填充全部函数指针，返回堆分配的 Box。
pub(crate) fn new_zapi() -> Box<ZqApi> {
    Box::new(ZqApi {
        version: ZQ_ABI_VERSION,
        undefined_raw: zapi_undefined_raw,
        null_raw: zapi_null_raw,
        bool_raw: zapi_bool_raw,
        number_raw: zapi_number_raw,
        string_raw: zapi_string_raw,
        symbol_raw: zapi_symbol_raw,
        new_symbol: zapi_new_symbol,
        symbol_to_string: zapi_symbol_to_string,
        get_symbol_description: zapi_get_symbol_description,
        string_len: zapi_string_len,
        get_string_utf8: zapi_get_string_utf8,
        new_string_builder: zapi_new_string_builder,
        string_builder_append: zapi_string_builder_append,
        string_builder_finish: zapi_string_builder_finish,
        free_string: zapi_free_string,
        new_object: zapi_new_object,
        new_array: zapi_new_array,
        set_property: zapi_set_property,
        get_property: zapi_get_property,
        has_property: zapi_has_property,
        delete_property: zapi_delete_property,
        array_push: zapi_array_push,
        array_get: zapi_array_get,
        array_set: zapi_array_set,
        array_len: zapi_array_len,
        set_global: zapi_set_global,
        get_global: zapi_get_global,
        has_global: zapi_has_global,
        new_function: zapi_new_function,
        register_function: zapi_register_function,
        remove_wrap: zapi_remove_wrap,
        call_function: zapi_call_function,
        exception_pending: zapi_exception_pending,
        throw: zapi_throw,
        throw_type_error: zapi_throw_type_error,
        throw_range_error: zapi_throw_range_error,
        set_exception: zapi_set_exception,
        get_last_error_message: zapi_get_last_error_message,
        get_and_clear_last_exception: zapi_get_and_clear_last_exception,
        gc_add_ref: zapi_gc_add_ref,
        gc_release: zapi_gc_release,
        gc_add_trace: zapi_gc_add_trace,
        gc_release_trace: zapi_gc_release_trace,
        set_global_trace: zapi_set_global_trace,
        register_class: zapi_register_class,
        new_instance: zapi_new_instance,
        get_class_def: zapi_get_class_def,
        get_native_ptr: zapi_get_native_ptr,
        set_native_ptr: zapi_set_native_ptr,
        get_raw: zapi_get_raw,
        type_of: zapi_type_of,
        number_value: zapi_number_value,
        bool_value: zapi_bool_value,
        string_value: zapi_string_value,
        call_constructor: zapi_call_constructor,
        module_register: zapi_module_register,
    })
}
