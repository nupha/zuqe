use std::ffi::{c_char, c_void};

use zapi::ffi::{
    ZQ_ABI_VERSION, ZqApi, ZqCallbackInfoS, ZqClassDef, ZqEnvS, ZqFuncInfo, ZqGcTraceCtxS,
    ZqModuleInfo, ZqRawValue,
};

// ── Global API table pointer ──

static mut ZQ_API: *mut ZqApi = std::ptr::null_mut();
fn api() -> &'static ZqApi {
    unsafe { &*ZQ_API }
}

// ──────────────────────────────────────
// Demo functions
// ──────────────────────────────────────

unsafe extern "C" fn add_body(env: *mut ZqEnvS, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let info = &mut *info;
    if info.argc < 2 {
        info.threw = 1;
        return 0;
    }
    let a = f64::from_bits(*info.args.add(0));
    let b = f64::from_bits(*info.args.add(1));
    info.ret = (api().number_raw)(env, a + b);
    0
}

unsafe extern "C" fn mul_body(env: *mut ZqEnvS, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let info = &mut *info;
    if info.argc < 2 {
        info.threw = 1;
        return 0;
    }
    let a = f64::from_bits(*info.args.add(0));
    let b = f64::from_bits(*info.args.add(1));
    info.ret = (api().number_raw)(env, a * b);
    0
}

unsafe extern "C" fn greet_body(env: *mut ZqEnvS, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let info = &mut *info;
    info.ret = (api().number_raw)(env, 42.0);
    0
}

// ──────────────────────────────────────
// Counter class
// ──────────────────────────────────────

#[repr(C)]
struct CounterPayload {
    count: i32,
    label: ZqRawValue,
}

unsafe extern "C" fn counter_finalize(_rt: *mut c_void, opaque: *mut c_void) {
    if !opaque.is_null() {
        unsafe {
            let _ = Box::from_raw(opaque as *mut CounterPayload);
        }
    }
}

unsafe extern "C" fn counter_gc_trace(trace_ctx: *mut ZqGcTraceCtxS, opaque: *mut c_void) {
    if opaque.is_null() {
        return;
    }
    let payload = unsafe { &*(opaque as *const CounterPayload) };
    if payload.label != 0 {
        let add_trace = api().gc_add_trace;
        unsafe {
            add_trace(std::ptr::null_mut(), trace_ctx, payload.label);
        }
    }
}

unsafe extern "C" fn counter_constructor(
    env: *mut ZqEnvS,
    info: *mut ZqCallbackInfoS,
) -> ZqRawValue {
    let info = unsafe { &mut *info };
    let label_raw = if info.argc > 0 { *info.args } else { 0 };
    let payload = Box::new(CounterPayload {
        count: 0,
        label: label_raw,
    });
    let opaque = Box::into_raw(payload) as *mut c_void;
    let new_inst = api().new_instance;
    let instance = unsafe {
        new_inst(
            env,
            &raw const COUNTER_CLASS_DEF as *const ZqClassDef,
            opaque,
        )
    };
    if instance == 0 {
        unsafe {
            let _ = Box::from_raw(opaque as *mut CounterPayload);
        }
    }
    info.ret = instance;
    0
}

unsafe extern "C" fn counter_increment(env: *mut ZqEnvS, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let info = unsafe { &mut *info };
    if info.argc < 1 {
        info.threw = 1;
        return 0;
    }
    let obj = *info.args.add(0);
    let get_ptr = api().get_native_ptr;
    let opaque = unsafe { get_ptr(env, obj) };
    if opaque.is_null() {
        info.threw = 1;
        return 0;
    }
    let payload = unsafe { &mut *(opaque as *mut CounterPayload) };
    payload.count += 1;
    info.ret = (api().number_raw)(env, payload.count as f64);
    0
}

unsafe extern "C" fn counter_value(env: *mut ZqEnvS, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let info = unsafe { &mut *info };
    if info.argc < 1 {
        info.threw = 1;
        return 0;
    }
    let obj = *info.args.add(0);
    let get_ptr = api().get_native_ptr;
    let opaque = unsafe { get_ptr(env, obj) };
    if opaque.is_null() {
        info.threw = 1;
        return 0;
    }
    let payload = unsafe { &*(opaque as *const CounterPayload) };
    info.ret = (api().number_raw)(env, payload.count as f64);
    0
}

static mut COUNTER_CLASS_DEF: ZqClassDef = ZqClassDef {
    name: b"Counter\0".as_ptr() as *const c_char,
    parent_class: std::ptr::null(),
    finalize: Some(counter_finalize),
    gc_trace: Some(counter_gc_trace),
    call: Some(counter_constructor),
    has_instance: None,
    call_arity: 1,
    proto_methods: std::ptr::null(),
    proto_methods_len: 0,
    proto_accessors: std::ptr::null(),
    proto_accessors_len: 0,
    static_methods: std::ptr::null(),
    static_methods_len: 0,
    static_accessors: std::ptr::null(),
    static_accessors_len: 0,
};

// ──────────────────────────────────────
// Module entry
// ──────────────────────────────────────

const DEMO_FUNCS: [ZqFuncInfo; 6] = [
    ZqFuncInfo {
        name: b"add\0".as_ptr() as *const c_char,
        body: Some(add_body),
        arity: 2,
    magic: 0,
    },
    ZqFuncInfo {
        name: b"mul\0".as_ptr() as *const c_char,
        body: Some(mul_body),
        arity: 2,
    magic: 0,
    },
    ZqFuncInfo {
        name: b"greet\0".as_ptr() as *const c_char,
        body: Some(greet_body),
        arity: 0,
    magic: 0,
    },
    ZqFuncInfo {
        name: b"Counter\0".as_ptr() as *const c_char,
        body: Some(counter_constructor),
        arity: 1,
    magic: 0,
    },
    ZqFuncInfo {
        name: b"counter_increment\0".as_ptr() as *const c_char,
        body: Some(counter_increment),
        arity: 1,
    magic: 0,
    },
    ZqFuncInfo {
        name: b"counter_value\0".as_ptr() as *const c_char,
        body: Some(counter_value),
        arity: 1,
    magic: 0,
    },
];

#[unsafe(no_mangle)]
pub unsafe extern "C" fn zq_module_register(api: *mut ZqApi, info: *mut ZqModuleInfo) {
    unsafe {
        assert!((*api).version == ZQ_ABI_VERSION);
        ZQ_API = api;
        (*info).name = b"zapi_demo\0".as_ptr() as *const c_char;
        (*info).funcs = DEMO_FUNCS.as_ptr();
        (*info).funcs_len = DEMO_FUNCS.len();
        (*info).init = None;
        (*info).classes = &raw const COUNTER_CLASS_DEF as *const ZqClassDef;
        (*info).classes_len = 1;
    }
}
