use std::ffi::c_void;

use libloading::Library;
use zapi::ffi::{
    ZQ_ABI_VERSION, ZqApi, ZqCallbackInfoS, ZqClassDef, ZqEnvS, ZqFuncInfo, ZqGcTraceCtxS,
    ZqModuleInfo, ZqRawValue,
};

use crate::{JSValue, ModuleDef, ScopedContext, class::ClassID, function::Callback};

unsafe fn env_from_ctx(ctx: &mut ScopedContext) -> ZqEnvS {
    let rt_raw: *const crate::RuntimeImpl = &*ctx.rt_ptr();
    ZqEnvS {
        ctx: (ctx as *mut ScopedContext).cast::<c_void>(),
        trace: std::ptr::null_mut::<ZqGcTraceCtxS>(),
        rt: rt_raw as *mut c_void,
    }
}

unsafe fn ctx_from_env<'a>(env: *mut ZqEnvS) -> &'a mut ScopedContext<'a> {
    unsafe { &mut *((*env).ctx as *mut ScopedContext) }
}

const unsafe extern "C" fn zapi_undefined_raw(_env: *mut ZqEnvS) -> ZqRawValue {
    JSValue::UNDEFINED.0
}

const unsafe extern "C" fn zapi_null_raw(_env: *mut ZqEnvS) -> ZqRawValue {
    JSValue::NULL.0
}

const unsafe extern "C" fn zapi_bool_raw(_env: *mut ZqEnvS, v: bool) -> ZqRawValue {
    if v { 1 } else { 0 }
}

unsafe extern "C" fn zapi_number_raw(_env: *mut ZqEnvS, v: f64) -> ZqRawValue {
    JSValue::from(v).0
}

unsafe extern "C" fn zapi_string_raw(env: *mut ZqEnvS, ptr: *const u8, len: usize) -> ZqRawValue {
    let ctx = unsafe { ctx_from_env(env) };
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    match ctx.create_string_from_utf8(bytes.to_vec()) {
        Ok(s) => JSValue::make_string(s).0,
        Err(_) => 0,
    }
}

unsafe extern "C" fn zapi_symbol_raw(_env: *mut ZqEnvS, _desc: ZqRawValue) -> ZqRawValue {
    todo!("api_symbol_raw")
}

unsafe extern "C" fn zapi_new_symbol(_env: *mut ZqEnvS, _desc: ZqRawValue) -> ZqRawValue {
    todo!("api_new_symbol")
}

unsafe extern "C" fn zapi_symbol_to_string(_env: *mut ZqEnvS, _sym: ZqRawValue) -> ZqRawValue {
    todo!("api_symbol_to_string")
}

unsafe extern "C" fn zapi_get_symbol_description(
    _env: *mut ZqEnvS,
    _sym: ZqRawValue,
) -> ZqRawValue {
    todo!("api_get_symbol_description")
}

unsafe extern "C" fn zapi_string_len(_env: *mut ZqEnvS, _s: ZqRawValue) -> usize {
    todo!("api_string_len")
}

unsafe extern "C" fn zapi_get_string_utf8(
    _env: *mut ZqEnvS,
    _s: ZqRawValue,
    _buf: *mut u8,
    _buf_len: usize,
    _offset: usize,
) -> i32 {
    todo!("api_get_string_utf8")
}

unsafe extern "C" fn zapi_new_string_builder(_env: *mut ZqEnvS, _cap: usize) -> ZqRawValue {
    todo!("api_new_string_builder")
}

unsafe extern "C" fn zapi_string_builder_append(
    _env: *mut ZqEnvS,
    _builder: ZqRawValue,
    _ptr: *const u8,
    _len: usize,
) -> i32 {
    todo!("api_string_builder_append")
}

unsafe extern "C" fn zapi_string_builder_finish(
    _env: *mut ZqEnvS,
    _builder: ZqRawValue,
) -> ZqRawValue {
    todo!("api_string_builder_finish")
}

unsafe extern "C" fn zapi_free_string(_env: *mut ZqEnvS, _s: ZqRawValue) {
    todo!("api_free_string")
}

unsafe extern "C" fn zapi_new_object(_env: *mut ZqEnvS) -> ZqRawValue {
    todo!("api_new_object")
}

unsafe extern "C" fn zapi_new_array(_env: *mut ZqEnvS, _len: usize) -> ZqRawValue {
    todo!("api_new_array")
}

unsafe extern "C" fn zapi_get_property(
    env: *mut ZqEnvS,
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
    env: *mut ZqEnvS,
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

unsafe extern "C" fn zapi_has_property(
    _env: *mut ZqEnvS,
    _obj: ZqRawValue,
    _key: ZqRawValue,
) -> i32 {
    todo!("api_has_property")
}

unsafe extern "C" fn zapi_delete_property(
    _env: *mut ZqEnvS,
    _obj: ZqRawValue,
    _key: ZqRawValue,
    _throw: i32,
) -> i32 {
    todo!("api_delete_property")
}

unsafe extern "C" fn zapi_array_push(_env: *mut ZqEnvS, _arr: ZqRawValue, _val: ZqRawValue) -> i32 {
    todo!("api_array_push")
}

unsafe extern "C" fn zapi_array_get(
    _env: *mut ZqEnvS,
    _arr: ZqRawValue,
    _idx: ZqRawValue,
) -> ZqRawValue {
    todo!("api_array_get")
}

unsafe extern "C" fn zapi_array_set(
    _env: *mut ZqEnvS,
    _arr: ZqRawValue,
    _idx: ZqRawValue,
    _val: ZqRawValue,
) -> i32 {
    todo!("api_array_set")
}

unsafe extern "C" fn zapi_array_len(_env: *mut ZqEnvS, _arr: ZqRawValue) -> ZqRawValue {
    todo!("api_array_len")
}

unsafe extern "C" fn zapi_set_global(
    _env: *mut ZqEnvS,
    _name: ZqRawValue,
    _val: ZqRawValue,
) -> i32 {
    todo!("api_set_global")
}

unsafe extern "C" fn zapi_get_global(_env: *mut ZqEnvS, _name: ZqRawValue) -> ZqRawValue {
    todo!("api_get_global")
}

unsafe extern "C" fn zapi_has_global(_env: *mut ZqEnvS, _name: ZqRawValue) -> i32 {
    todo!("api_has_global")
}

unsafe extern "C" fn zapi_new_function(
    _env: *mut ZqEnvS,
    _name: ZqRawValue,
    _body: Option<unsafe extern "C" fn(env: *mut ZqEnvS, info: *mut ZqCallbackInfoS) -> ZqRawValue>,
    _len: u8,
) -> ZqRawValue {
    todo!("api_new_function")
}

unsafe extern "C" fn zapi_register_function(
    _env: *mut ZqEnvS,
    _obj: ZqRawValue,
    _name: ZqRawValue,
    _body: Option<unsafe extern "C" fn(env: *mut ZqEnvS, info: *mut ZqCallbackInfoS) -> ZqRawValue>,
    _len: u8,
) -> i32 {
    todo!("api_register_function")
}

unsafe extern "C" fn zapi_remove_wrap(
    _env: *mut ZqEnvS,
    _obj: ZqRawValue,
    _name: *const std::ffi::c_char,
) -> i32 {
    todo!("api_remove_wrap")
}

unsafe extern "C" fn zapi_call_function(
    _env: *mut ZqEnvS,
    _info: *mut ZqCallbackInfoS,
) -> ZqRawValue {
    todo!("api_call_function")
}

unsafe extern "C" fn zapi_exception_pending(_env: *mut ZqEnvS) -> i32 {
    todo!("api_exception_pending")
}

unsafe extern "C" fn zapi_throw(_env: *mut ZqEnvS, _val: ZqRawValue) -> i32 {
    todo!("api_throw")
}

unsafe extern "C" fn zapi_throw_type_error(_env: *mut ZqEnvS, _msg: *const u8, _len: usize) -> i32 {
    todo!("api_throw_type_error")
}

unsafe extern "C" fn zapi_throw_range_error(
    _env: *mut ZqEnvS,
    _msg: *const u8,
    _len: usize,
) -> i32 {
    todo!("api_throw_range_error")
}

unsafe extern "C" fn zapi_set_exception(_env: *mut ZqEnvS, _val: ZqRawValue) {
    todo!("api_set_exception")
}

unsafe extern "C" fn zapi_get_last_error_message(_env: *mut ZqEnvS) -> ZqRawValue {
    todo!("api_get_last_error_message")
}

unsafe extern "C" fn zapi_get_and_clear_last_exception(_env: *mut ZqEnvS) -> ZqRawValue {
    todo!("api_get_and_clear_last_exception")
}

unsafe extern "C" fn zapi_gc_add_ref(_env: *mut ZqEnvS, _val: ZqRawValue) {
    todo!("api_gc_add_ref")
}

unsafe extern "C" fn zapi_gc_release(_env: *mut ZqEnvS, _val: ZqRawValue) {
    todo!("api_gc_release")
}

unsafe extern "C" fn zapi_set_global_trace(_env: *mut ZqEnvS, _trace: usize) {
    todo!("api_set_global_trace")
}

unsafe extern "C" fn zapi_register_class(env: *mut ZqEnvS, class_def: *const ZqClassDef) -> i32 {
    let ctx = unsafe { ctx_from_env(env) };
    let def = unsafe { &*class_def };
    let name_bytes = unsafe { std::ffi::CStr::from_ptr(def.name) };
    let name = match name_bytes.to_str() {
        Ok(s) => s,
        Err(_) => return -1,
    };
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());

    let zapi_call = def.call;

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
        Ok(())
    }

    let mut cd = crate::class::ClassDef::new_with(leaked, zapi_class_init);

    if zapi_call.is_some() {
        cd.call = Some(zapi_class_call_bridge);
    }

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

    if ctx.use_class(class_id).is_err() {
        return -1;
    }

    ctx.rt_mut()
        .zapi_class_map
        .lock()
        .unwrap()
        .insert(class_id, class_def as usize);

    class_id as i32
}

fn zapi_class_call_bridge(
    ctx: &mut ScopedContext,
    _func_obj: JSValue,
    this_val: JSValue,
    args: &[JSValue],
    _flags: i32,
) -> crate::JSResult {
    let mut env = unsafe { env_from_ctx(ctx) };
    let raw_args: Vec<ZqRawValue> = args
        .iter()
        .map(|v| v.as_number().map(|n| n.to_f64().to_bits()).unwrap_or(v.0))
        .collect();
    let mut info = ZqCallbackInfoS {
        target: 0,
        this: this_val.0,
        args: raw_args.as_ptr(),
        argc: args.len() as u32,
        ret: 0,
        threw: 0,
    };

    // Look up the class definition to find the call body.
    let class_def: Option<*const ZqClassDef> = {
        if let Some(o) = this_val.as_object() {
            let cid = o.class_id();
            ctx.rt()
                .zapi_class_map
                .lock()
                .unwrap()
                .get(&cid)
                .map(|&p| p as *const ZqClassDef)
        } else {
            None
        }
    };

    if let Some(def_ptr) = class_def
        && let Some(call_body) = unsafe { (*def_ptr).call }
    {
        unsafe { call_body(&mut env, &mut info) };
    } else {
        info.threw = 1;
    }

    if info.threw != 0 {
        Err(-1)
    } else {
        Ok(JSValue(info.ret, std::marker::PhantomData))
    }
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

unsafe extern "C" fn zapi_new_instance(
    env: *mut ZqEnvS,
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

unsafe extern "C" fn zapi_get_native_ptr(_env: *mut ZqEnvS, obj: ZqRawValue) -> *mut c_void {
    let val = JSValue(obj, std::marker::PhantomData);
    val.opaque()
        .map(|p| p.as_ptr())
        .unwrap_or(std::ptr::null_mut())
}

unsafe extern "C" fn zapi_set_native_ptr(
    _env: *mut ZqEnvS,
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
    _env: *mut ZqEnvS,
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
    _env: *mut ZqEnvS,
    _trace_ctx: *mut ZqGcTraceCtxS,
) {
    // GcTraceCtx cleanup is handled by the GC engine.
}

unsafe extern "C" fn zapi_get_class_def(_env: *mut ZqEnvS, _obj: ZqRawValue) -> *const ZqClassDef {
    todo!("api_get_class_def")
}

unsafe extern "C" fn zapi_get_raw(_env: *mut ZqEnvS, _obj: ZqRawValue) -> ZqRawValue {
    todo!("api_get_raw")
}

unsafe extern "C" fn zapi_module_register(_api: *mut ZqApi, _info: *mut ZqModuleInfo) {
    todo!("api_module_register")
}

use once_cell::sync::Lazy;

/// 全局共享的 ZqApi 表 — 纯函数指针，无运行时状态，多 Runtime 安全复用。
static ZQ_API_TABLE: Lazy<Box<ZqApi>> = Lazy::new(new_zapi);

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
        module_register: zapi_module_register,
    })
}

// ──────────────────────────────────────
// Thunk
// ──────────────────────────────────────

/// 通用 zapi 跳板函数：从 `cc.magic()` 读取 token，在 RuntimeImpl 的
/// `zapi_body_registry` 中查找对应的扩展 body。
fn zapi_trampoline_fn(
    ctx: &mut ScopedContext,
    cc: crate::function::CallContext,
) -> crate::JSResult {
    let token = cc.magic();
    let body = match ctx
        .rt()
        .zapi_body_registry
        .lock()
        .unwrap()
        .get(&token)
        .copied()
    {
        Some(b) => b,
        None => return Err(-1),
    };

    let mut env = unsafe { env_from_ctx(ctx) };

    let raw_args: Vec<ZqRawValue> = cc
        .arguments()
        .iter()
        .map(|v| v.as_number().map(|n| n.to_f64().to_bits()).unwrap_or(v.0))
        .collect();

    let mut info = ZqCallbackInfoS {
        target: 0,
        this: cc.this().0,
        args: raw_args.as_ptr(),
        argc: cc.length() as u32,
        ret: 0,
        threw: 0,
    };

    unsafe { body(&mut env, &mut info) };

    if info.threw != 0 {
        Err(-1)
    } else {
        Ok(crate::JSValue(info.ret, std::marker::PhantomData))
    }
}

/// 将 zapi 扩展的 `body` FFI 入口注册到当前 Runtime 的跳板。
///
/// 返回 `(Callback, token)`，调用方通过 `create_native_function(callback, name, len, token)`
/// 将 token 写入 native function 的 magic 字段。运行时 `zapi_trampoline_fn` 根据
/// `cc.magic()` 反查 `RuntimeImpl.zapi_body_registry` 找到正确的 body。
pub fn register_zapi_func(
    ctx: &mut ScopedContext,
    body: unsafe extern "C" fn(env: *mut ZqEnvS, info: *mut ZqCallbackInfoS) -> ZqRawValue,
) -> (Callback, i32) {
    let rt = ctx.rt_mut();
    let token = rt
        .zapi_next_token
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    rt.zapi_body_registry.lock().unwrap().insert(token, body);
    (Callback::Func(zapi_trampoline_fn), token)
}

/// 将 zapi 扩展的 body FFI 入口包装为 zuqe 引擎的 `Callback`。
///
/// 因为 `CallbackFn = fn(...)` 是函数指针（无法捕获环境），该宏为每次调用
/// 生成一个独立的跳板函数，将 `body` 编译时内联到函数体中。
///
/// 使用方式：
/// ```ignore
/// let cb = zapi_make_thunk!(my_body);
/// ```
#[macro_export]
macro_rules! zapi_make_thunk {
    ($body:expr) => {{
        use zapi::ffi::ZqCallbackInfoS;
        use $crate::{JSResult, JSValue, ScopedContext, function::CallContext};

        fn trampoline(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
            let mut env = $crate::zapi::host::env_from_scoped(ctx);
            let raw_args: Vec<u64> = cc
                .arguments()
                .iter()
                .map(|v| v.as_number().map(|n| n.to_f64().to_bits()).unwrap_or(v.0))
                .collect();
            let mut info = ZqCallbackInfoS {
                target: 0,
                this: cc.this().0,
                args: raw_args.as_ptr(),
                argc: cc.length() as u32,
                ret: 0,
                threw: 0,
            };
            let _result_raw = unsafe { ($body)(&mut env, &mut info) };
            if info.threw != 0 {
                Err(-1)
            } else {
                Ok(JSValue(info.ret, std::marker::PhantomData))
            }
        }
        $crate::function::Callback::Func(trampoline)
    }};
}

/// 将一组 ZqFuncInfo 转换为 zuqe Callback 列表并注册。
///
/// # 实现说明
///
/// 因为 `CallbackFn = fn(...)` 是函数指针，动态 thunk 需要配合过程宏
/// （`#[zq_function]` / `#[zq_module]`）在编译期生成独立的跳板函数。
/// 运行时动态加载的扩展需由过程宏生成注册代码，不可使用此函数直接注册。
pub fn register_funcs<F>(_funcs: &[ZqFuncInfo], _register: F) -> Result<(), &'static str>
where
    F: FnMut(&str, Callback, u8),
{
    // 注：动态 thunk 需过程宏支持，此处预留接口。
    // 编译期静态注册请使用 `zapi_make_thunk!` 宏。
    todo!("register_funcs requires proc-macro support for dynamic thunk generation")
}

// ──────────────────────────────────────
// 扩展加载
// ──────────────────────────────────────

/// Load a zapi extension cdylib and register its exports as a native module.
///
/// `path` is the filesystem path to the `.dylib`/`.so`/`.dll` file.
///
/// Steps:
/// 1. dlopen the library, resolve `zq_module_register`
/// 2. Lazy-init Runtime's `ZqApi` table (created once, shared across extensions)
/// 3. Call `zq_module_register` → extension fills `ZqModuleInfo`
/// 4. Register classes via `api.register_class`
/// 5. Register functions on `globalThis` via `create_native_function`
/// 6. Return a `ModuleDef` so the import system tracks this module
pub fn load_extension(ctx: &mut ScopedContext, path: &std::path::Path) -> Result<ModuleDef, i32> {
    type ZqModuleRegisterFn = unsafe extern "C" fn(api: *mut ZqApi, info: *mut ZqModuleInfo);

    // Resolve absolute path, then try dlopen.
    let abs_path = path.canonicalize().map_err(|e| {
        ctx.throw_reference_error(&format!(
            "could not resolve extension path '{}': {e}",
            path.display()
        ))
    })?;

    let lib = unsafe { Library::new(&abs_path) }.map_err(|e| {
        ctx.throw_reference_error(&format!(
            "could not load library '{}': {e}",
            abs_path.display()
        ))
    })?;

    let module_register_fn: libloading::Symbol<ZqModuleRegisterFn> =
        unsafe { lib.get(c"zq_module_register") }.map_err(|_| {
            ctx.throw_reference_error(&format!(
                "could not find zq_module_register in '{}'",
                abs_path.display()
            ))
        })?;

    // Only build the URL after the library has loaded successfully.
    let file_url = crate::path_to_url(abs_path.to_str().unwrap_or("")).map_err(|_| {
        ctx.throw_reference_error(&format!(
            "could not convert path to URL: {}",
            abs_path.display()
        ))
    })?;

    let api: &ZqApi = &ZQ_API_TABLE;
    let api_ptr = api as *const ZqApi as *mut ZqApi;

    let mut md_info = ZqModuleInfo::default();
    unsafe { module_register_fn(api_ptr, &mut md_info) };

    // Register classes.
    let register_class = api.register_class;
    for cd in md_info.class_defs() {
        let mut env = ZqEnvS {
            ctx: (ctx as *mut ScopedContext).cast::<c_void>(),
            trace: std::ptr::null_mut(),
            rt: std::ptr::null_mut(),
        };
        let r = unsafe { register_class(&mut env, cd) };
        if r < 0 {
            return Err(ctx.throw_reference_error(&format!(
                "zapi module '{file_url}': failed to register class"
            )));
        }
    }

    let funcs = md_info.funcs();
    if funcs.is_empty() {
        return Err(
            ctx.throw_reference_error(&format!("zapi module '{file_url}' exported no functions"))
        );
    }

    let mod_name = if md_info.name.is_null() {
        "zapi_module"
    } else {
        unsafe { std::ffi::CStr::from_ptr(md_info.name) }
            .to_str()
            .unwrap_or("zapi_module")
    };

    // Register functions on globalThis.
    let mut export_names: Vec<&str> = Vec::new();
    for f in funcs {
        let func_name = unsafe { std::ffi::CStr::from_ptr(f.name) }
            .to_str()
            .map_err(|_| ctx.throw_type_error("invalid func name in zapi module"))?;

        let Some(body) = f.body else {
            continue;
        };

        let (cb, token) = register_zapi_func(ctx, body);
        let fn_obj = ctx.create_native_function(cb, func_name, f.arity, token as i16)?;
        let js_val: JSValue = fn_obj.into();
        ctx.global_obj()
            .define_property_value_str(ctx, func_name, js_val, 0)?;
        export_names.push(func_name);
    }

    // Keep the library alive.
    std::mem::forget(lib);

    ModuleDef::create(ctx, mod_name, export_names.iter().copied(), |_, _| Ok(()))
}
