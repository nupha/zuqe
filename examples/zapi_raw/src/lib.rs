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
//! * a `body` **returns the JS result value directly**; on error it returns the
//!   `value_exception` sentinel (quickjs `JS_EXCEPTION` convention);
//! * to throw a specific JS value, write it into `info.thrown` and return
//!   `value_exception` (this demo only shows the bare sentinel in `boom()`);
//! * fixed JS values come from the `ZqApi` table constants (`value_undefined`,
//!   `value_null`, `value_true`, ...), and "not yet set" native slots use Rust's
//!   `Option` instead of a raw `0` sentinel;
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
    /// The host hands us a `*const ZqApi` in `zq_module_register`; bodies reach
    /// the table through `api()`.
    static API: Cell<*const ZqApi> = const { Cell::new(std::ptr::null()) };
}

/// Raw accessor to the host-provided function-pointer table.
fn api() -> &'static ZqApi {
    unsafe { &*API.with(|slot| slot.get()) }
}

// ──────────────────────────────────────
// Thin raw helpers (FFI boilerplate only — not the safe layer)
// ──────────────────────────────────────

/// Read argument `i` of the current call as a raw handle; missing arguments
/// come through as `undefined` (matching JS call semantics).
fn arg_at(_env: ZqEnv, info: *const ZqCallInfoS, i: u32) -> ZqRawValue {
    let info = unsafe { &*info };
    if i < info.argc {
        unsafe { *info.args.add(i as usize) }
    } else {
        api().value_undefined
    }
}

/// Read argument `i` as a JS number (`NaN` for non-numeric values).
fn arg_number(env: ZqEnv, info: *const ZqCallInfoS, i: u32) -> f64 {
    unsafe { (api().value_to_float64)(env, arg_at(env, info, i)) }
}

/// Read a JS string value into UTF-8 bytes ("" for non-strings).
///
/// Uses the quickjs measure-then-copy habit: call `get_string_utf8` with a null
/// buffer to learn the byte count, allocate, then copy. `-1` = not a string.
fn string_bytes(env: ZqEnv, value: ZqRawValue) -> Vec<u8> {
    let api = api();
    let len = unsafe { (api.get_string_utf8)(env, value, std::ptr::null_mut(), 0, 0) };
    if len <= 0 {
        return Vec::new();
    }
    let mut bytes = vec![0u8; len as usize];
    unsafe { (api.get_string_utf8)(env, value, bytes.as_mut_ptr(), len as usize, 0) };
    bytes
}

/// Build a JS string value from UTF-8 bytes.
fn make_string(env: ZqEnv, bytes: &[u8]) -> ZqRawValue {
    unsafe { (api().new_string_utf8)(env, bytes.as_ptr(), bytes.len()) }
}

// ──────────────────────────────────────
// Basic-type exports (FUNC)
// ──────────────────────────────────────

/// `add(a, b)` → the sum of two numbers.
unsafe extern "C" fn add(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let sum = arg_number(env, info, 0) + arg_number(env, info, 1);
    unsafe { (api().new_number)(env, sum) }
}
/// `concat(a, b)` → two strings joined, read and rebuilt through the FFI.
unsafe extern "C" fn concat(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let left = string_bytes(env, arg_at(env, info, 0));
    let right = string_bytes(env, arg_at(env, info, 1));
    let mut joined = Vec::with_capacity(left.len() + right.len());
    joined.extend_from_slice(&left);
    joined.extend_from_slice(&right);
    make_string(env, &joined)
}

/// `typeOf(v)` → the JS `typeof` name of `v`.
unsafe extern "C" fn type_of(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let value_type = unsafe { (api().get_value_type)(env, arg_at(env, info, 0)) };
    // `ZqType::name()` is the typeof string; no manual mapping needed.
    make_string(env, value_type.name().as_bytes())
}

/// `id(v)` → the argument unchanged (round-trip for any value).
unsafe extern "C" fn id(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    arg_at(env, info, 0)
}

/// `truthy(v)` → ToBoolean(v).
unsafe extern "C" fn truthy(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let result = unsafe { (api().value_to_bool)(env, arg_at(env, info, 0)) != 0 };
    // `true`/`false` are table constants, no FFI call.
    if result {
        api().value_true
    } else {
        api().value_false
    }
}
/// `makeSymbol(desc)` → `Symbol(desc)`.
unsafe extern "C" fn make_symbol(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let description = arg_at(env, info, 0);
    unsafe { (api().new_symbol)(env, description) }
}
/// `symDesc(symbol)` → the symbol's description ("" when absent).
unsafe extern "C" fn sym_desc(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let symbol = arg_at(env, info, 0);
    let description = unsafe { (api().get_symbol_description)(env, symbol) };
    let bytes = string_bytes(env, description);
    make_string(env, &bytes)
}

// ──────────────────────────────────────
// Object / array exports (FUNC)
// ──────────────────────────────────────

/// `makePoint(x, y)` → `{x, y}` built inside the extension.
unsafe extern "C" fn make_point(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let api = api();
    let x = arg_number(env, info, 0);
    let y = arg_number(env, info, 1);
    let point = unsafe { (api.new_object)(env) };
    // `set_property(env, obj, key, val, throw, flags)`: no throwing, default flags.
    let key_x = make_string(env, b"x");
    let key_y = make_string(env, b"y");
    unsafe {
        (api.set_property)(env, point, key_x, (api.new_number)(env, x), 0, 0);
        (api.set_property)(env, point, key_y, (api.new_number)(env, y), 0, 0);
    }
    point
}

/// `pointDist(point)` → `sqrt(x² + y²)` — reads object properties via FFI.
unsafe extern "C" fn point_dist(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let api = api();
    let point = arg_at(env, info, 0);
    let key_x = make_string(env, b"x");
    let key_y = make_string(env, b"y");
    let x = unsafe { (api.value_to_float64)(env, (api.get_property)(env, point, key_x)) };
    let y = unsafe { (api.value_to_float64)(env, (api.get_property)(env, point, key_y)) };
    let distance = (x * x + y * y).sqrt();
    unsafe { (api.new_number)(env, distance) }
}
/// `makeList(count)` → `[0, 1, ..., count-1]` built inside the extension.
unsafe extern "C" fn make_list(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let api = api();
    let count = arg_number(env, info, 0) as usize;
    let array = unsafe { (api.new_array)(env, count) };
    for index in 0..count {
        // `array_set` takes the index as a JS number handle.
        let index_value = unsafe { (api.new_number)(env, index as f64) };
        unsafe { (api.array_set)(env, array, index_value, index_value) };
    }
    array
}

/// `listLen(array)` → `array.length`.
unsafe extern "C" fn list_len(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let array = arg_at(env, info, 0);
    let length = unsafe { (api().value_to_float64)(env, (api().array_len)(env, array)) };
    unsafe { (api().new_number)(env, length) }
}
/// `callJs(f, ...args)` → `f(...args)` invoked from inside the extension.
unsafe extern "C" fn call_js(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let this_call = unsafe { &*info };
    let target = arg_at(env, this_call, 0);
    // Forward the remaining arguments (index 1..) to `target` unchanged.
    let forwarded_count = this_call.argc.saturating_sub(1);
    let mut call = ZqCallInfoS {
        target,
        this: api().value_undefined,
        args: if forwarded_count > 0 {
            unsafe { this_call.args.add(1) } // point just past `target`
        } else {
            std::ptr::null()
        },
        argc: forwarded_count,
        thrown: 0,
        magic: 0,
    };
    unsafe { (api().call_function)(env, &mut call) }
}

// ──────────────────────────────────────
// Class: Box (payload holds raw JS handles → gc_trace + finalize)// ──────────────────────────────────────

/// Native payload of a `Box` object. Both fields are JS values the GC must be
/// told about, so we keep the raw handles and report them in `box_trace`.
struct BoxData {
    label: ZqRawValue,
    child: ZqRawValue,
}

/// Raw pointer to the `Box` payload of `instance` (`null` when the object is
/// not a `Box`, e.g. a plain JS object passed to an instance method).
fn box_payload(env: ZqEnv, instance: ZqRawValue) -> *mut BoxData {
    unsafe { (api().get_native_ptr)(env, instance).cast::<BoxData>() }
}

/// Constructor: `new Box(label, child?)`. Allocates the payload and wraps it in
/// a class instance via `api.new_instance`.
///
/// `child` defaults to a fresh object so the trace callback always has a real
/// graph to report. The payload is owned by the instance (`Box::into_raw`);
/// `box_finalize` reclaims it when the JS object is collected.
unsafe extern "C" fn box_new(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let api = api();
    let label = arg_at(env, info, 0);
    let child = if unsafe { (*info).argc } > 1 {
        arg_at(env, info, 1)
    } else {
        unsafe { (api.new_object)(env) }
    };
    let payload = Box::into_raw(Box::new(BoxData { label, child }));
    unsafe { (api.new_instance)(env, &BOX_DEF, payload.cast::<c_void>()) }
}

/// Instance method `label()` → the stored label.
unsafe extern "C" fn box_label(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let payload = box_payload(env, unsafe { (*info).this });
    // `value_undefined` is the table constant for JS `undefined` — no FFI call.
    if payload.is_null() {
        api().value_undefined
    } else {
        unsafe { (*payload).label }
    }
}

/// Instance method `setLabel(newLabel)` → replace the stored label.
unsafe extern "C" fn box_set_label(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let payload = box_payload(env, unsafe { (*info).this });
    if !payload.is_null() {
        unsafe { (*payload).label = arg_at(env, info, 0) };
    }
    api().value_undefined
}

/// Instance method `child()` → the stored child value.
unsafe extern "C" fn box_child(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let payload = box_payload(env, unsafe { (*info).this });
    if payload.is_null() {
        api().value_undefined
    } else {
        unsafe { (*payload).child }
    }
}

/// Instance method `setChild(newChild)` → replace the stored child value.
unsafe extern "C" fn box_set_child(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let payload = box_payload(env, unsafe { (*info).this });
    if !payload.is_null() {
        unsafe { (*payload).child = arg_at(env, info, 0) };
    }
    api().value_undefined
}

/// Static method `Box.isBox(value)` → whether `value` carries a native payload.
unsafe extern "C" fn box_is_box(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let value = arg_at(env, info, 0);
    let has_payload = !unsafe { (api().get_native_ptr)(env, value) }.is_null();
    if has_payload {
        api().value_true
    } else {
        api().value_false
    }
}
/// Static accessor `Box.kind` (getter-only) → a constant string.
unsafe extern "C" fn box_kind(env: ZqEnv, _info: *mut ZqCallInfoS) -> ZqRawValue {
    make_string(env, b"native-box")
}

/// Class-payload GC trace: tell the host about every JS value held in native
/// storage. Called once per GC for each live `Box` instance. The env argument
/// to `gc_mark` is ignored by the host (only `trace_ctx` matters).
unsafe extern "C" fn box_trace(trace_ctx: *mut ZqGcTraceCtxS, opaque: *mut c_void) {
    if opaque.is_null() {
        return;
    }
    let payload = unsafe { &*opaque.cast::<BoxData>() };
    let api = api();
    // Both fields are always valid handles (`box_new` sets them from args or
    // creates fresh values), so trace them unconditionally.
    unsafe { (api.gc_mark)(trace_ctx, payload.label) };
    unsafe { (api.gc_mark)(trace_ctx, payload.child) };
}

/// How many `Box` payloads have been finalized by the GC so far.
static DROPPED_BOXES: AtomicUsize = AtomicUsize::new(0);

/// Finalizer: reclaim the payload box when the JS object is collected.
unsafe extern "C" fn box_finalize(_rt: *mut c_void, opaque: *mut c_void) {
    if !opaque.is_null() {
        let payload = unsafe { Box::from_raw(opaque.cast::<BoxData>()) };
        drop(payload);
        DROPPED_BOXES.fetch_add(1, Ordering::Relaxed);
    }
}

/// `droppedBoxes()` → how many Box payloads the GC has finalized (0 initially).
/// Lets the test observe the `finalize` path actually running.
unsafe extern "C" fn dropped_boxes(env: ZqEnv, _info: *mut ZqCallInfoS) -> ZqRawValue {
    let count = DROPPED_BOXES.load(Ordering::Relaxed);
    unsafe { (api().new_number)(env, count as f64) }
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
// `thread_local`, so a global trace callback must keep it alive. `None` = not
// yet set (no sentinel needed — idiomatic Rust).
thread_local! {
    static GLOBAL_NODE: RefCell<Option<ZqRawValue>> = const { RefCell::new(None) };
}

/// `setGlobal(value)` → store `value` in the thread_local slot.
unsafe extern "C" fn set_global(env: ZqEnv, info: *mut ZqCallInfoS) -> ZqRawValue {
    let value = arg_at(env, info, 0);
    GLOBAL_NODE.with(|slot| *slot.borrow_mut() = Some(value));
    api().value_undefined
}

/// `getGlobal()` → read the value stored by `setGlobal`.
unsafe extern "C" fn get_global(_env: ZqEnv, _info: *mut ZqCallInfoS) -> ZqRawValue {
    let value = GLOBAL_NODE.with(|slot| *slot.borrow());
    // Not-yet-set reads back as JS `undefined`.
    match value {
        Some(v) => v,
        None => api().value_undefined,
    }
}

/// Global GC trace callback: marks the thread_local value at every collection.
unsafe extern "C" fn global_trace(trace_ctx: *mut ZqGcTraceCtxS, _opaque: *mut c_void) {
    let value = GLOBAL_NODE.with(|slot| *slot.borrow());
    if let Some(v) = value {
        unsafe { (api().gc_mark)(trace_ctx, v) };
    }
}

// ──────────────────────────────────────
// Value export (VALUE, built lazily at module-evaluation time)
// ──────────────────────────────────────

/// `STATS` → an object built with a real env at evaluation time.
unsafe extern "C" fn export_stats(env: ZqEnv, out: *mut ZqRawValue) -> i32 {
    let api = api();
    let stats = unsafe { (api.new_object)(env) };
    let key = make_string(env, b"built");
    unsafe {
        // `value_true` is the table constant for JS `true` — no FFI call.
        (api.set_property)(env, stats, key, api.value_true, 0, 0);
        *out = stats;
    }
    0 // success
}

// ──────────────────────────────────────
// Host constant values (no FFI calls — read straight from the ZqApi table)
// ──────────────────────────────────────

/// `constants()` → an object exposing the host-provided constant value handles.
/// They are plain constants, so they need no `env` and no FFI call.
unsafe extern "C" fn constants(env: ZqEnv, _info: *mut ZqCallInfoS) -> ZqRawValue {
    let api = api();
    let obj = unsafe { (api.new_object)(env) };
    let key_undef = make_string(env, b"undef");
    let key_null = make_string(env, b"null");
    let key_true = make_string(env, b"true");
    let key_false = make_string(env, b"false");
    let key_nan = make_string(env, b"nan");
    let key_zero = make_string(env, b"zero");
    let key_one = make_string(env, b"one");
    let key_neg_one = make_string(env, b"negOne");
    let key_inf = make_string(env, b"inf");
    let key_neg_inf = make_string(env, b"negInf");
    unsafe {
        (api.set_property)(env, obj, key_undef, api.value_undefined, 0, 0);
        (api.set_property)(env, obj, key_null, api.value_null, 0, 0);
        (api.set_property)(env, obj, key_true, api.value_true, 0, 0);
        (api.set_property)(env, obj, key_false, api.value_false, 0, 0);
        (api.set_property)(env, obj, key_nan, api.value_nan, 0, 0);
        (api.set_property)(env, obj, key_zero, api.value_zero, 0, 0);
        (api.set_property)(env, obj, key_one, api.value_one, 0, 0);
        (api.set_property)(env, obj, key_neg_one, api.value_negative_one, 0, 0);
        (api.set_property)(env, obj, key_inf, api.value_infinity, 0, 0);
        (api.set_property)(env, obj, key_neg_inf, api.value_minus_infinity, 0, 0);
    }
    obj
}

/// `boom()` → always signals an error by returning the `value_exception`
/// sentinel (quickjs `JS_EXCEPTION` convention). The host turns it into a
/// thrown JS exception.
unsafe extern "C" fn boom(_env: ZqEnv, _info: *mut ZqCallInfoS) -> ZqRawValue {
    api().value_exception
}

// ──────────────────────────────────────
// Module registration
// ──────────────────────────────────────

/// Module init hook (evaluation time): seed the thread_local node and register
/// its global GC trace. Nonzero return = init failure.
unsafe extern "C" fn module_init(env: ZqEnv) -> i32 {
    let api = api();
    let seed = make_string(env, b"global-root");
    GLOBAL_NODE.with(|slot| *slot.borrow_mut() = Some(seed));
    let ok = unsafe { (api.set_gc_trace_func)(env, Some(global_trace), std::ptr::null_mut()) } == 0;
    // 0 = success; nonzero = init failure
    if ok { 0 } else { -1 }
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
    // ── FUNC: host constant values ──
    ZqFieldDef::func(c"constants".as_ptr(), constants, 0, 0),
    // ── FUNC: error signaling via the exception sentinel ──
    ZqFieldDef::func(c"boom".as_ptr(), boom, 0, 0),
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
    API.with(|slot| slot.set(api_ptr));

    let module = unsafe { &mut *info_ptr };
    module.name = c"zapi_raw".as_ptr();
    module.struct_size = std::mem::size_of::<ZqModuleInfo>() as u32;
    module.version = ZQ_VERSION_PACKED;
    module.exports = EXPORTS.as_ptr();
    module.exports_len = EXPORTS.len();
    module.init = Some(module_init);
}
