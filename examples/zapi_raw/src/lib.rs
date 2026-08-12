//! Zapi — raw FFI native-extension demo.
//!
//! This crate is written against Layer 1 of the zapi contract only.
//!
//! What the demo shows:
//!
//! * **basic types** — number / string / boolean / symbol / object / array /
//!   function, crossing the FFI boundary in both directions;
//! * **module** — a hand-built `ZqFieldDef` manifest (`FUNC` / `CLASS` /
//!   `VALUE` / constant kinds) plus a module `init` hook;
//! * **class** — a native class (`Box`) whose payload lives in a `Box<BoxData>`,
//!   published via `ZQ_FIELD_CLASS`;
//! * **class-payload GC trace** — `Box` keeps raw JS value handles in its
//!   payload; a `gc_trace` callback reports them to the host every collection;
//! * **thread_local GC trace** — a `thread_local` slot holds a raw JS handle the
//!   host can never scan; a global trace callback (registered through
//!   `api.set_gc_trace_func` in `init`) keeps it alive.
//!
//! Conventions of the raw ABI (all visible below):
//!
//! * a `body` writes its result into `info.ret` and returns `0`;
//! * setting `info.threw` to a nonzero value signals an uncaught exception
//!   (this demo never throws);
//! * `ZqRawValue` is a plain `u64` — raw `0` is an *invalid* handle, never a JS
//!   value, so it doubles as a "no value" sentinel in native storage;
//! * `ZqFieldDef` is the single unified descriptor for module exports and class
//!   members; `ZqFieldDef::func` / `getset` / `class` / `value` / `str` are
//!   const constructors that fill the right `kind` + slots;
//! * `C`-string pointers passed into `ZqFieldDef.name` / `ZqClassDef.name`
//!   must stay valid for the process lifetime (statics here).

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};

use zuqe_zapi::ffi::*;

// ──────────────────────────────────────
// The ZqApi table (kept per-thread, as in the sugar layer's __private)
// ──────────────────────────────────────

thread_local! {
    static API: Cell<*const ZqApi> = const { Cell::new(std::ptr::null()) };
}

/// Raw accessor to the host-provided function-pointer table.
fn api() -> &'static ZqApi {
    unsafe { &*API.with(|c| c.get()) }
}

// ──────────────────────────────────────
// Thin raw helpers (FFI boilerplate only — not the safe layer)
// ──────────────────────────────────────

/// Read the `i`-th argument as a raw handle, or `undefined` when missing.
fn arg_at(env: ZqEnv, info: *const ZqCallbackInfoS, i: u32) -> ZqRawValue {
    let info = unsafe { &*info };
    if i < info.argc {
        unsafe { *info.args.add(i as usize) }
    } else {
        unsafe { (api().undefined_raw)(env) }
    }
}

/// Read a JS string value into UTF-8 bytes ("" for non-strings).
fn read_string(env: ZqEnv, v: ZqRawValue) -> Vec<u8> {
    let api = api();
    let len = unsafe { (api.string_len)(env, v) };
    let mut buf = vec![0u8; len];
    if len > 0 {
        unsafe { (api.get_string_utf8)(env, v, buf.as_mut_ptr(), len, 0) };
    }
    buf
}

/// Build a JS string value from UTF-8 bytes.
fn make_string(env: ZqEnv, bytes: &[u8]) -> ZqRawValue {
    unsafe { (api().string_raw)(env, bytes.as_ptr(), bytes.len()) }
}

/// `ZQ_TYPE_*` code → the JS `typeof` name.
fn type_name(t: i32) -> &'static [u8] {
    match t {
        ZQ_TYPE_UNDEFINED => b"undefined",
        ZQ_TYPE_NULL => b"object", // `typeof null` is "object" in JS
        ZQ_TYPE_BOOL => b"boolean",
        ZQ_TYPE_NUMBER => b"number",
        ZQ_TYPE_STRING => b"string",
        ZQ_TYPE_SYMBOL => b"symbol",
        ZQ_TYPE_FUNCTION => b"function",
        ZQ_TYPE_OBJECT => b"object",
        _ => b"unknown",
    }
}

// ──────────────────────────────────────
// Basic-type exports (FUNC)
// ──────────────────────────────────────

/// `add(a, b)` → the sum of two numbers.
unsafe extern "C" fn add(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let a = unsafe { (api.number_value)(env, arg_at(env, info, 0)) };
    let b = unsafe { (api.number_value)(env, arg_at(env, info, 1)) };
    unsafe { (*info).ret = (api.number_raw)(env, a + b) };
    0
}

/// `concat(a, b)` → two strings joined, read and rebuilt through the FFI.
unsafe extern "C" fn concat(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let a = read_string(env, arg_at(env, info, 0));
    let b = read_string(env, arg_at(env, info, 1));
    let mut out = Vec::with_capacity(a.len() + b.len());
    out.extend_from_slice(&a);
    out.extend_from_slice(&b);
    unsafe { (*info).ret = make_string(env, &out) };
    0
}

/// `typeOf(v)` → the JS `typeof` name of `v`.
unsafe extern "C" fn type_of(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let t = unsafe { (api.type_of)(env, arg_at(env, info, 0)) };
    let name = type_name(t);
    unsafe { (*info).ret = make_string(env, name) };
    0
}

/// `id(v)` → the argument unchanged (round-trip for any value).
unsafe extern "C" fn id(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let v = arg_at(env, info, 0);
    unsafe { (*info).ret = v };
    0
}

/// `truthy(v)` → ToBoolean(v).
unsafe extern "C" fn truthy(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let b = unsafe { (api.bool_value)(env, arg_at(env, info, 0)) != 0 };
    unsafe { (*info).ret = (api.bool_raw)(env, b) };
    0
}

/// `makeSymbol(desc)` → `Symbol(desc)`.
unsafe extern "C" fn make_symbol(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let desc = arg_at(env, info, 0);
    unsafe { (*info).ret = (api.new_symbol)(env, desc) };
    0
}

/// `symDesc(sym)` → the symbol's description ("" when absent).
unsafe extern "C" fn sym_desc(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let desc = unsafe { (api.get_symbol_description)(env, arg_at(env, info, 0)) };
    let bytes = read_string(env, desc);
    unsafe { (*info).ret = make_string(env, &bytes) };
    0
}

// ──────────────────────────────────────
// Object / array exports (FUNC)
// ──────────────────────────────────────

/// `makePoint(x, y)` → `{x, y}` built inside the extension.
unsafe extern "C" fn make_point(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let x = unsafe { (api.number_value)(env, arg_at(env, info, 0)) };
    let y = unsafe { (api.number_value)(env, arg_at(env, info, 1)) };
    let obj = unsafe { (api.new_object)(env) };
    let key_x = make_string(env, b"x");
    let key_y = make_string(env, b"y");
    unsafe {
        (api.set_property)(env, obj, key_x, (api.number_raw)(env, x), 0, 0);
        (api.set_property)(env, obj, key_y, (api.number_raw)(env, y), 0, 0);
    }
    unsafe { (*info).ret = obj };
    0
}

/// `pointDist(p)` → `sqrt(p.x² + p.y²)` — reads object properties via FFI.
unsafe extern "C" fn point_dist(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let obj = arg_at(env, info, 0);
    let key_x = make_string(env, b"x");
    let key_y = make_string(env, b"y");
    let x = unsafe { (api.number_value)(env, (api.get_property)(env, obj, key_x)) };
    let y = unsafe { (api.number_value)(env, (api.get_property)(env, obj, key_y)) };
    let d = (x * x + y * y).sqrt();
    unsafe { (*info).ret = (api.number_raw)(env, d) };
    0
}

/// `makeList(n)` → `[0, 1, ..., n-1]` built inside the extension.
unsafe extern "C" fn make_list(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let n = unsafe { (api.number_value)(env, arg_at(env, info, 0)) } as usize;
    let arr = unsafe { (api.new_array)(env, n) };
    for i in 0..n {
        let key = unsafe { (api.number_raw)(env, i as f64) };
        unsafe { (api.array_set)(env, arr, key, (api.number_raw)(env, i as f64)) };
    }
    unsafe { (*info).ret = arr };
    0
}

/// `listLen(a)` → `a.length`.
unsafe extern "C" fn list_len(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let len = unsafe { (api.number_value)(env, (api.array_len)(env, arg_at(env, info, 0))) };
    unsafe { (*info).ret = (api.number_raw)(env, len) };
    0
}

/// `callJs(f, ...args)` → `f(...args)` invoked from inside the extension.
unsafe extern "C" fn call_js(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let info_ref = unsafe { &*info };
    let target = arg_at(env, info_ref, 0);
    let nargs = info_ref.argc.saturating_sub(1);
    let mut call_info = ZqCallbackInfoS {
        target,
        this: unsafe { (api.undefined_raw)(env) },
        args: if nargs > 0 {
            unsafe { info_ref.args.add(1) }
        } else {
            std::ptr::null()
        },
        argc: nargs,
        ret: 0,
        threw: 0,
        magic: 0,
    };
    let ret = unsafe { (api.call_function)(env, &mut call_info) };
    unsafe { (*info).ret = ret };
    0
}

// ──────────────────────────────────────
// Class: Box (payload holds raw JS handles → gc_trace + finalize)
// ──────────────────────────────────────

/// Native payload of a `Box` object. Both fields are JS values the GC must be
/// told about, so we keep the raw handles and report them in `box_trace`.
struct BoxData {
    label: ZqRawValue,
    child: ZqRawValue,
}

/// Constructor: `new Box(label, child?)`. Allocates the payload and wraps it in
/// a class instance via `api.new_instance`.
unsafe extern "C" fn box_new(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let label = arg_at(env, info, 0);
    let child = if unsafe { (*info).argc } > 1 {
        arg_at(env, info, 1)
    } else {
        unsafe { (api.new_object)(env) }
    };
    let data = Box::into_raw(Box::new(BoxData { label, child }));
    let inst = unsafe { (api.new_instance)(env, &BOX_DEF, data.cast::<c_void>()) };
    unsafe { (*info).ret = inst };
    0
}

/// Instance method `label()` → the stored label.
unsafe extern "C" fn box_label(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let ptr = unsafe { (api.get_native_ptr)(env, (*info).this) };
    let label = if ptr.is_null() {
        0
    } else {
        unsafe { (*ptr.cast::<BoxData>()).label }
    };
    unsafe { (*info).ret = label };
    0
}

/// Instance method `setLabel(v)` → replace the stored label.
unsafe extern "C" fn box_set_label(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let ptr = unsafe { (api.get_native_ptr)(env, (*info).this) };
    if !ptr.is_null() {
        unsafe { (*ptr.cast::<BoxData>()).label = arg_at(env, info, 0) };
    }
    unsafe { (*info).ret = (api.undefined_raw)(env) };
    0
}

/// Instance method `child()` → the stored child value.
unsafe extern "C" fn box_child(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let ptr = unsafe { (api.get_native_ptr)(env, (*info).this) };
    let child = if ptr.is_null() {
        0
    } else {
        unsafe { (*ptr.cast::<BoxData>()).child }
    };
    unsafe { (*info).ret = child };
    0
}

/// Instance method `setChild(v)` → replace the stored child value.
unsafe extern "C" fn box_set_child(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let ptr = unsafe { (api.get_native_ptr)(env, (*info).this) };
    if !ptr.is_null() {
        unsafe { (*ptr.cast::<BoxData>()).child = arg_at(env, info, 0) };
    }
    unsafe { (*info).ret = (api.undefined_raw)(env) };
    0
}

/// Static method `Box.isBox(v)` → whether `v` carries a native payload.
unsafe extern "C" fn box_is_box(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let v = arg_at(env, info, 0);
    let native = !unsafe { (api.get_native_ptr)(env, v) }.is_null();
    unsafe { (*info).ret = (api.bool_raw)(env, native) };
    0
}

/// Static accessor `Box.kind` (getter-only) → a constant string.
unsafe extern "C" fn box_kind(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    unsafe { (*info).ret = make_string(env, b"native-box") };
    0
}

/// Class-payload GC trace: tell the host about every JS value held in native
/// storage. Called once per GC for each live `Box` instance.
unsafe extern "C" fn box_trace(trace_ctx: *mut ZqGcTraceCtxS, opaque: *mut c_void) {
    if opaque.is_null() {
        return;
    }
    let data = unsafe { &*opaque.cast::<BoxData>() };
    let api = api();
    if data.label != 0 {
        unsafe { (api.gc_add_trace)(std::ptr::null_mut(), trace_ctx, data.label) };
    }
    if data.child != 0 {
        unsafe { (api.gc_add_trace)(std::ptr::null_mut(), trace_ctx, data.child) };
    }
}

/// How many `Box` payloads have been finalized by the GC so far.
static DROPPED_BOXES: AtomicUsize = AtomicUsize::new(0);

/// Finalizer: drop the payload box when the JS object is collected.
unsafe extern "C" fn box_finalize(_rt: *mut c_void, opaque: *mut c_void) {
    if !opaque.is_null() {
        unsafe { drop(Box::from_raw(opaque.cast::<BoxData>())) };
        DROPPED_BOXES.fetch_add(1, Ordering::Relaxed);
    }
}

/// `droppedBoxes()` → how many Box payloads the GC has finalized (0 initially).
/// Lets the test observe the `finalize` path actually running.
unsafe extern "C" fn dropped_boxes(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let n = DROPPED_BOXES.load(Ordering::Relaxed);
    unsafe { (*info).ret = (api.number_raw)(env, n as f64) };
    0
}

static BOX_PROTO: &[ZqFieldDef] = &[
    // Instance members, methods and accessors interleaved like a JS class body.
    ZqFieldDef::func(c"label".as_ptr(), box_label, 0, 0),
    ZqFieldDef::func(c"setLabel".as_ptr(), box_set_label, 1, 0),
    ZqFieldDef::func(c"child".as_ptr(), box_child, 0, 0),
    ZqFieldDef::func(c"setChild".as_ptr(), box_set_child, 1, 0),
];

static BOX_STATIC: &[ZqFieldDef] = &[
    // Static (constructor) members.
    ZqFieldDef::func(c"isBox".as_ptr(), box_is_box, 1, 0),
    // A getter-only accessor: `Box.kind`.
    ZqFieldDef::getset(c"kind".as_ptr(), Some(box_kind), None, 0),
];

/// The `Box` class definition. Registered by the host when the class export is
/// processed at module load.
static BOX_DEF: ZqClassDef = ZqClassDef {
    name: c"Box".as_ptr(),
    parent_class: std::ptr::null(),
    finalize: Some(box_finalize),
    gc_trace: Some(box_trace),
    call: Some(box_new),
    has_instance: None,
    call_arity: 1,
    proto_entries: BOX_PROTO.as_ptr(),
    proto_entries_len: BOX_PROTO.len(),
    static_entries: BOX_STATIC.as_ptr(),
    static_entries_len: BOX_STATIC.len(),
};

// ──────────────────────────────────────
// thread_local global node + its GC trace
// ──────────────────────────────────────

// A JS value held in native (thread_local) storage. The host never scans a
// `thread_local`, so a global trace callback must keep it alive.
thread_local! {
    static GLOBAL_NODE: RefCell<ZqRawValue> = const { RefCell::new(0) };
}

/// `setGlobal(v)` → store `v` in the thread_local slot.
unsafe extern "C" fn set_global(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let v = arg_at(env, info, 0);
    GLOBAL_NODE.with(|g| *g.borrow_mut() = v);
    unsafe { (*info).ret = (api.undefined_raw)(env) };
    0
}

/// `getGlobal()` → read the value stored by `setGlobal`.
unsafe extern "C" fn get_global(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue {
    let api = api();
    let v = GLOBAL_NODE.with(|g| *g.borrow());
    let ret = if v == 0 {
        unsafe { (api.undefined_raw)(env) }
    } else {
        v
    };
    unsafe { (*info).ret = ret };
    0
}

/// Global GC trace callback: marks the thread_local value at every collection.
unsafe extern "C" fn global_trace(trace_ctx: *mut ZqGcTraceCtxS, _opaque: *mut c_void) {
    let v = GLOBAL_NODE.with(|g| *g.borrow());
    if v == 0 {
        return;
    }
    unsafe { (api().gc_add_trace)(std::ptr::null_mut(), trace_ctx, v) };
}

// ──────────────────────────────────────
// Value export (VALUE, built lazily at module-evaluation time)
// ──────────────────────────────────────

/// `STATS` → an object built with a real env at evaluation time.
unsafe extern "C" fn export_stats(env: ZqEnv, out: *mut ZqRawValue) -> i32 {
    let api = api();
    let obj = unsafe { (api.new_object)(env) };
    let key = make_string(env, b"built");
    unsafe {
        (api.set_property)(env, obj, key, (api.bool_raw)(env, true), 0, 0);
        *out = obj;
    }
    0
}

// ──────────────────────────────────────
// Module registration
// ──────────────────────────────────────

/// Module init hook (evaluation time): seed the thread_local node and register
/// its global GC trace. Nonzero return = init failure.
unsafe extern "C" fn module_init(env: ZqEnv) -> i32 {
    let api = api();
    let seed = make_string(env, b"global-root");
    GLOBAL_NODE.with(|g| *g.borrow_mut() = seed);
    if unsafe { (api.set_gc_trace_func)(env, Some(global_trace), std::ptr::null_mut()) } < 0 {
        -1
    } else {
        0
    }
}

static EXPORTS: &[ZqFieldDef] = &[
    // ── FUNC: basic types ──
    ZqFieldDef::func(c"add".as_ptr(), add, 2, 0),
    ZqFieldDef::func(c"concat".as_ptr(), concat, 2, 0),
    ZqFieldDef::func(c"typeOf".as_ptr(), type_of, 1, 0),
    ZqFieldDef::func(c"id".as_ptr(), id, 1, 0),
    ZqFieldDef::func(c"truthy".as_ptr(), truthy, 1, 0),
    ZqFieldDef::func(c"makeSymbol".as_ptr(), make_symbol, 1, 0),
    ZqFieldDef::func(c"symDesc".as_ptr(), sym_desc, 1, 0),
    // ── FUNC: objects & arrays ──
    ZqFieldDef::func(c"makePoint".as_ptr(), make_point, 2, 0),
    ZqFieldDef::func(c"pointDist".as_ptr(), point_dist, 1, 0),
    ZqFieldDef::func(c"makeList".as_ptr(), make_list, 1, 0),
    ZqFieldDef::func(c"listLen".as_ptr(), list_len, 1, 0),
    ZqFieldDef::func(c"callJs".as_ptr(), call_js, 1, 0),
    // ── FUNC: thread_local global node ──
    ZqFieldDef::func(c"setGlobal".as_ptr(), set_global, 1, 0),
    ZqFieldDef::func(c"getGlobal".as_ptr(), get_global, 0, 0),
    ZqFieldDef::func(c"droppedBoxes".as_ptr(), dropped_boxes, 0, 0),
    // ── CLASS ──
    ZqFieldDef::class(c"Box".as_ptr(), &BOX_DEF),
    // ── VALUE: lazy value builder ──
    ZqFieldDef::value(c"STATS".as_ptr(), export_stats),
    // ── STR: constant string (no trampoline needed) ──
    ZqFieldDef::str(c"VERSION".as_ptr(), c"0.1.0".as_ptr(), 0),
];

/// The module entry point the host resolves via `lib.get("zq_module_register")`.
///
/// Fills the `ZqModuleInfo` with the export manifest and stores the `ZqApi`
/// table for the bodies above. Returns nothing (void) — a failing registration
/// simply leaves `info` untouched and the host errors out.
///
/// # Safety
/// `api_ptr` must point to a host-constructed `ZqApi` table of the current
/// `ZQ_ABI_VERSION`; `info_ptr` must be a writable, zero-initialized
/// `ZqModuleInfo` owned by the host loader.
#[no_mangle]
pub unsafe extern "C" fn zq_module_register(api_ptr: *mut ZqApi, info_ptr: *mut ZqModuleInfo) {
    let api = unsafe { &*api_ptr };
    if api.version != ZQ_ABI_VERSION {
        return;
    }
    API.with(|c| c.set(api_ptr));

    let info = unsafe { &mut *info_ptr };
    info.name = c"zapi_raw".as_ptr();
    info.struct_size = std::mem::size_of::<ZqModuleInfo>() as u32;
    info.abi_version = ZQ_ABI_VERSION;
    info.min_api_version = ZQ_API_VERSION;
    info.exports = EXPORTS.as_ptr();
    info.exports_len = EXPORTS.len();
    info.init = Some(module_init);
}
