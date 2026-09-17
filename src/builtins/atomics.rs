// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, OnceLock},
    time::Duration,
};

use crate::{
    JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY, JSResult,
    bigint::from_i64,
    bigint::from_u64,
    bigint::to_bigint64,
    bigint::to_biguint64,
    builtins::{bigint::to_big_int, promise::Promise},
    class::ClassID,
    context::ScopedContext,
    error::ErrCode,
    function::CallContext,
    function::FieldDef,
    object::TypedArray,
    runtime::RuntimeImpl,
    value::JSValue,
};

/// Element metadata: (bit_width, signed, is_bigint)
const fn elem_info(cid: ClassID) -> Option<(u32, bool, bool)> {
    Some(match cid {
        JS_CLASS_INT8_ARRAY => (8, true, false),
        JS_CLASS_UINT8_ARRAY => (8, false, false),
        JS_CLASS_INT16_ARRAY => (16, true, false),
        JS_CLASS_UINT16_ARRAY => (16, false, false),
        JS_CLASS_INT32_ARRAY => (32, true, false),
        JS_CLASS_UINT32_ARRAY => (32, false, false),
        JS_CLASS_BIG_INT64_ARRAY => (64, true, true),
        JS_CLASS_BIG_UINT64_ARRAY => (64, false, true),
        _ => return None,
    })
}

const fn is_waitable(cid: ClassID) -> bool {
    matches!(
        cid,
        JS_CLASS_INT32_ARRAY
            | JS_CLASS_UINT32_ARRAY
            | JS_CLASS_BIG_INT64_ARRAY
            | JS_CLASS_BIG_UINT64_ARRAY
    )
}

fn str_val(ctx: &mut ScopedContext, s: &str) -> JSResult {
    let sr = ctx.create_string_interned(s)?;
    Ok(JSValue::make_string(sr))
}

/// Validate that `arg` is an integer TypedArray.
///
/// This only checks the element type; it deliberately does NOT require a
/// SharedArrayBuffer. Per the spec (`ValidateIntegerTypedArray`), the plain
/// Atomics read/write/RMW operations (`load`, `store`, `add`, `and`, `or`,
/// `xor`, `sub`, `exchange`, `compareExchange`) operate on any integer
/// TypedArray, shared or not. Only `wait`/`waitAsync`/`notify` require shared
/// memory, and that check is applied by the callers.
///
/// `waitable` restricts to the element types accepted by the waiting
/// operations (Int32/Uint32/BigInt64/BigUint64; the signedness filter for
/// Int32/BigInt64-only is applied by the caller).
fn validate_atomic(
    ctx: &mut ScopedContext,
    arg: JSValue,
    waitable: bool,
) -> Result<(TypedArray, ClassID, u32, bool, bool), ErrCode> {
    let ta = arg
        .as_object_TypedArray()
        .ok_or_else(|| ctx.throw_type_error("Atomics: first argument must be a TypedArray"))?;
    let cid = ta.class_id();
    let (width, signed, is_bigint) = elem_info(cid)
        .ok_or_else(|| ctx.throw_type_error("Atomics: a non-integer TypedArray is not allowed"))?;
    if waitable && !is_waitable(cid) {
        return Err(ctx.throw_type_error(
            "Atomics: the TypedArray element type must be Int32, Uint32, BigInt64 or BigUint64",
        ));
    }
    ta.ensure_attached(ctx)?;
    Ok((ta, cid, width, signed, is_bigint))
}

/// `ValidateSharedIntegerTypedArray` shared-memory check: the buffer backing
/// `ta` must be a SharedArrayBuffer.
fn require_shared(ctx: &mut ScopedContext, ta: &TypedArray) -> Result<(), ErrCode> {
    if ta.array_buffer().class_id() != JS_CLASS_SHARED_ARRAY_BUFFER {
        return Err(
            ctx.throw_type_error("Atomics: the typed array must be backed by a SharedArrayBuffer")
        );
    }
    Ok(())
}

/// `ToIntegerOrInfinity` applied to an integer-element value.
fn to_integer_or_infinity(ctx: &mut ScopedContext, val: JSValue) -> Result<i64, ErrCode> {
    let n = val.to_number(ctx)?;
    let d = n.to_f64();
    if d.is_nan() {
        Ok(0)
    } else if d.is_infinite() {
        Err(ctx.throw_range_error("Atomics: value must not be ±Infinity"))
    } else {
        Ok(d.trunc() as i64)
    }
}

/// Truncate an integer to the element width, returning its raw bit pattern.
fn truncate_to_bits(v: i64, signed: bool, width: u32) -> u64 {
    if signed {
        let t = match width {
            8 => v as i8 as i64,
            16 => v as i16 as i64,
            32 => v as i32 as i64,
            64 => v,
            _ => unreachable!(),
        };
        t as u64
    } else {
        match width {
            8 => (v as u8) as u64,
            16 => (v as u16) as u64,
            32 => (v as u32) as u64,
            64 => v as u64,
            _ => unreachable!(),
        }
    }
}

/// Compute the raw bit pattern of an atomic value.
fn value_to_bits(
    ctx: &mut ScopedContext,
    val: JSValue,
    signed: bool,
    is_bigint: bool,
    width: u32,
) -> Result<u64, ErrCode> {
    if is_bigint {
        let bi_val = to_big_int(ctx, val)?;
        let bi_ref = bi_val
            .as_bigint()
            .ok_or_else(|| ctx.throw_type_error("Atomics: expected a BigInt"))?;
        if signed {
            Ok(to_bigint64(&bi_ref) as u64)
        } else {
            Ok(to_biguint64(&bi_ref))
        }
    } else {
        let v = to_integer_or_infinity(ctx, val)?;
        Ok(truncate_to_bits(v, signed, width))
    }
}

/// Sign-extend raw bits into the element's signed integer value.
#[inline(always)]
const fn bits_to_signed(bits: u64, width: u32) -> i64 {
    let shift = 64 - width;
    ((bits << shift) as i64) >> shift
}

/// Convert the raw bit pattern of a read element back into a JSValue.
fn bits_to_value(
    ctx: &mut ScopedContext,
    bits: u64,
    signed: bool,
    is_bigint: bool,
    width: u32,
) -> JSResult {
    if is_bigint {
        if signed {
            Ok(JSValue::make_bigint(from_i64(
                ctx,
                bits_to_signed(bits, 64),
            )?))
        } else {
            Ok(JSValue::make_bigint(from_u64(ctx, bits)?))
        }
    } else if signed {
        Ok(JSValue::from(bits_to_signed(bits, width)))
    } else {
        Ok(JSValue::from(bits as i64))
    }
}

fn read_bits(buf: &[u8], off: usize, width: u32) -> u64 {
    match width {
        8 => buf[off] as u64,
        16 => u16::from_ne_bytes([buf[off], buf[off + 1]]) as u64,
        32 => u32::from_ne_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]]) as u64,
        64 => u64::from_ne_bytes(buf[off..off + 8].try_into().unwrap()),
        _ => unreachable!(),
    }
}

fn write_bits(buf: &mut [u8], off: usize, width: u32, val: u64) {
    match width {
        8 => buf[off] = val as u8,
        16 => buf[off..off + 2].copy_from_slice(&(val as u16).to_ne_bytes()),
        32 => buf[off..off + 4].copy_from_slice(&(val as u32).to_ne_bytes()),
        64 => buf[off..off + 8].copy_from_slice(&val.to_ne_bytes()),
        _ => unreachable!(),
    }
}

#[inline(always)]
const fn mask_for(width: u32) -> u64 {
    if width == 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// Read a single element, applying `op` to produce the new value. Returns the
/// previous element bit pattern as a JSValue.
fn atomic_rmw(
    ctx: &mut ScopedContext,
    ta: &mut TypedArray,
    cid: ClassID,
    nth: usize,
    value_bits: u64,
    op: impl Fn(u64, u64) -> u64,
) -> JSResult {
    let (width, signed, is_bigint) = elem_info(cid).unwrap();
    let mask = mask_for(width);
    let old_bits = ta.with_bytes_mut(|buf| {
        let off = nth * (width as usize / 8);
        let old = read_bits(buf, off, width);
        let new = op(old & mask, value_bits & mask) & mask;
        write_bits(buf, off, width, new);
        old
    });
    bits_to_value(ctx, old_bits & mask, signed, is_bigint, width)
}

fn validate_index(
    ctx: &mut ScopedContext,
    ta: &TypedArray,
    index_val: JSValue,
) -> Result<usize, ErrCode> {
    // `ValidateAtomicAccess`: retrieve the length BEFORE coercing the index, so
    // an index `valueOf` that resizes/grows the buffer cannot change the
    // recorded bounds.
    let length = ta.length();
    let idx = index_val.to_index(ctx)? as usize;
    if idx >= length {
        return Err(ctx.throw_range_error("Atomics: index out of range"));
    }
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Atomics.load
// ---------------------------------------------------------------------------

pub fn js_atomics_load(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (ta, _cid, width, signed, is_bigint) = validate_atomic(ctx, cc.arg(0), false)?;
    let nth = validate_index(ctx, &ta, cc.arg(1))?;
    let mask = mask_for(width);
    let old_bits = ta.with_bytes(|buf| {
        let off = nth * (width as usize / 8);
        read_bits(buf, off, width)
    });
    bits_to_value(ctx, old_bits & mask, signed, is_bigint, width)
}

// ---------------------------------------------------------------------------
// Atomics.store
// ---------------------------------------------------------------------------

/// The value `Atomics.store` must return: `ToBigInt(value)` for a bigint
/// element type, or `ToNumber(value)` truncated to an integer for a numeric
/// element type. This is the *converted* value, NOT truncated to the element
/// width (the width-truncated bit pattern is what gets written to memory).
fn store_return_value(
    ctx: &mut ScopedContext,
    val: JSValue,
    is_bigint: bool,
) -> Result<JSValue, ErrCode> {
    if is_bigint {
        // `ToBigInt(value)` — return it as-is.
        to_big_int(ctx, val)
    } else {
        // `ToNumber(value)` then truncate toward zero (NaN → 0, ±Infinity
        // kept). Note: truncation must also normalize `-0` to `+0`, since
        // `ToInteger` normalizes -0 to +0.
        let n = val.to_number(ctx)?;
        let d = n.to_f64();
        let t = if d.is_nan() { 0.0 } else { d.trunc() };
        Ok(JSValue::from(if t == 0.0 { 0.0 } else { t }))
    }
}

pub fn js_atomics_store(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (mut ta, _cid, width, signed, is_bigint) = validate_atomic(ctx, cc.arg(0), false)?;
    let nth = validate_index(ctx, &ta, cc.arg(1))?;
    // `ToBigInt`/`ToNumber` conversion happens before the element-width
    // truncation, and the *converted* value is what store returns.
    let ret = store_return_value(ctx, cc.arg(2), is_bigint)?;
    let value_bits = value_to_bits(ctx, cc.arg(2), signed, is_bigint, width)?;
    // Atomics.store returns the stored (converted) value, not the previous value.
    ta.with_bytes_mut(|buf| {
        let off = nth * (width as usize / 8);
        write_bits(buf, off, width, value_bits);
    });
    Ok(ret)
}

// ---------------------------------------------------------------------------
// Atomics.exchange
// ---------------------------------------------------------------------------

pub fn js_atomics_exchange(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (mut ta, cid, width, signed, is_bigint) = validate_atomic(ctx, cc.arg(0), false)?;
    let nth = validate_index(ctx, &ta, cc.arg(1))?;
    let value_bits = value_to_bits(ctx, cc.arg(2), signed, is_bigint, width)?;
    atomic_rmw(ctx, &mut ta, cid, nth, value_bits, |_old, v| v)
}

// ---------------------------------------------------------------------------
// Atomics.compareExchange
// ---------------------------------------------------------------------------

pub fn js_atomics_compare_exchange(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (mut ta, _cid, width, signed, is_bigint) = validate_atomic(ctx, cc.arg(0), false)?;
    let nth = validate_index(ctx, &ta, cc.arg(1))?;
    let expected = value_to_bits(ctx, cc.arg(2), signed, is_bigint, width)?;
    let replacement = value_to_bits(ctx, cc.arg(3), signed, is_bigint, width)?;
    let mask = mask_for(width);
    let old_bits = ta.with_bytes_mut(|buf| {
        let off = nth * (width as usize / 8);
        let old = read_bits(buf, off, width);
        if old & mask == expected & mask {
            write_bits(buf, off, width, replacement & mask);
        }
        old
    });
    bits_to_value(ctx, old_bits & mask, signed, is_bigint, width)
}

// ---------------------------------------------------------------------------
// Atomics.add / sub / and / or / xor
// ---------------------------------------------------------------------------

pub fn js_atomics_add(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    rmw_binary(ctx, cc, |o, v| o.wrapping_add(v))
}
pub fn js_atomics_sub(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    rmw_binary(ctx, cc, |o, v| o.wrapping_sub(v))
}
pub fn js_atomics_and(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    rmw_binary(ctx, cc, |o, v| o & v)
}
pub fn js_atomics_or(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    rmw_binary(ctx, cc, |o, v| o | v)
}
pub fn js_atomics_xor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    rmw_binary(ctx, cc, |o, v| o ^ v)
}

fn rmw_binary(ctx: &mut ScopedContext, cc: CallContext, op: impl Fn(u64, u64) -> u64) -> JSResult {
    let (mut ta, cid, width, signed, is_bigint) = validate_atomic(ctx, cc.arg(0), false)?;
    let nth = validate_index(ctx, &ta, cc.arg(1))?;
    let value_bits = value_to_bits(ctx, cc.arg(2), signed, is_bigint, width)?;
    atomic_rmw(ctx, &mut ta, cid, nth, value_bits, op)
}

// ---------------------------------------------------------------------------
// Atomics.isLockFree
// ---------------------------------------------------------------------------

pub fn js_atomics_is_lock_free(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let size = if cc.arguments().is_empty() {
        return Ok(JSValue::from(false));
    } else {
        to_integer_or_infinity(ctx, cc.arg(0))?
    };
    Ok(JSValue::from(matches!(size, 1 | 2 | 4 | 8)))
}

// ---------------------------------------------------------------------------
// Atomics.pause
// ---------------------------------------------------------------------------

pub const fn js_atomics_pause(_ctx: &mut ScopedContext, _cc: CallContext) -> JSResult {
    // Spin-wait hint; no observable effect.
    Ok(JSValue::UNDEFINED)
}

// ---------------------------------------------------------------------------
// Atomics.notify
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Cross-Runtime wait/notify queue
// ---------------------------------------------------------------------------
//
// Each `Atomics.wait` registers a waiter keyed by the shared buffer identity
// (the `Arc` backing the `SharedArrayBuffer`) and the element's byte offset.
// `Atomics.notify` wakes the waiters sharing that key. This allows agents
// running in separate `Runtime`s (and therefore separate OS threads) to
// synchronize through a `SharedArrayBuffer`.
//
// The registry itself is deliberately process-global: `Atomics.notify` in one
// runtime/agent must reach waiters registered by another (same address space,
// shared `Arc` backing — the ES spec's cross-agent wake semantics, exercised
// by the test262 agent tests). Making it per-runtime would break that.
//
// What IS per-runtime is the *outstanding `waitAsync` count*
// (`RuntimeImpl::atomics_wait_async_count`), so a `waitAsync` registered by
// runtime A does not keep runtime B's event loop ticking. Waiters are
// therefore associated with their owning runtime only via that shared count.

type Waiter = Arc<(Mutex<bool>, Condvar)>;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct WaitKey {
    sab: usize,
    off: usize,
}

impl std::fmt::Debug for WaitKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WaitKey(sab={:#x}, off={})", self.sab, self.off)
    }
}

struct WaitRegistry {
    map: Mutex<HashMap<WaitKey, Vec<Waiter>>>,
}

static WAIT_REGISTRY: OnceLock<WaitRegistry> = OnceLock::new();

fn wait_registry() -> &'static WaitRegistry {
    WAIT_REGISTRY.get_or_init(|| WaitRegistry {
        map: Mutex::new(HashMap::new()),
    })
}

/// Wake up to `count` waiters (or all when `count == usize::MAX`) for `key`.
/// Returns the number of waiters actually woken.
fn wake_waiters(key: WaitKey, count: usize) -> usize {
    let mut map = wait_registry().map.lock().unwrap();
    let mut woken = 0;
    if let Some(vec) = map.get_mut(&key) {
        let take = if count == usize::MAX {
            vec.len()
        } else {
            count.min(vec.len())
        };
        for w in vec.drain(0..take) {
            if let Ok(mut g) = w.0.lock() {
                *g = true;
                w.1.notify_one();
            }
            woken += 1;
        }
        if vec.is_empty() {
            map.remove(&key);
        }
    }
    woken
}

/// Remove a single waiter from its queue (best-effort, on wake-up or timeout).
fn drop_waiter(key: WaitKey, waiter: &Waiter) {
    let mut map = wait_registry().map.lock().unwrap();
    if let Some(vec) = map.get_mut(&key) {
        vec.retain(|w| !Arc::ptr_eq(w, waiter));
        if vec.is_empty() {
            map.remove(&key);
        }
    }
}

impl RuntimeImpl {
    /// Override whether the current agent may suspend (block) in `Atomics.wait`.
    /// The test262 harness sets this to `false` when a test declares
    /// `CanBlockIsFalse`, and restores it afterwards.
    pub const fn set_agent_can_suspend(&mut self, v: bool) {
        self.agent_can_suspend = v;
    }
}

/// Wake every registered waiter regardless of key. Used on agent-harness
/// teardown so agent threads blocked in `Atomics.wait` are released instead of
/// leaking across tests.
pub fn wake_all_waiters() {
    let mut map = wait_registry().map.lock().unwrap();
    for (_, vec) in map.drain() {
        for w in vec {
            if let Ok(mut g) = w.0.lock() {
                *g = true;
                w.1.notify_one();
            }
        }
    }
}

/// Build the wait key for a typed array index, returning `None` if the buffer
/// is not a `SharedArrayBuffer` (already validated by the caller).
fn wait_key(ta: &TypedArray, nth: usize, width: u32) -> WaitKey {
    let off = ta.array_buffer_byte_offset() + nth * (width as usize / 8);
    let sab = ta
        .array_buffer()
        .shared_data()
        .map(|a| Arc::as_ptr(&a) as usize)
        .unwrap_or(0);
    WaitKey { sab, off }
}

// ---------------------------------------------------------------------------
// Atomics.notify
// ---------------------------------------------------------------------------

pub fn js_atomics_notify(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // Per `Atomics.notify`, the array is validated as an *integer* TypedArray
    // of a waitable element type (Int32 or BigInt64); the SharedArrayBuffer
    // check is deferred until AFTER the index and count have been coerced, at
    // which point a non-shared buffer makes the call return 0.
    let (ta, _cid, width, signed, _is_bigint) = validate_atomic(ctx, cc.arg(0), true)?;
    if !signed {
        return Err(ctx.throw_type_error(
            "Atomics.notify: typed array must be an Int32Array or BigInt64Array",
        ));
    }

    // `ValidateAtomicAccess`: capture the length before coercing the index.
    let length = ta.length();
    let nth = cc.arg(1).to_index(ctx)? as usize;
    if nth >= length {
        return Err(ctx.throw_range_error("Atomics: index out of range"));
    }

    // Resolve the count argument (default: +Infinity => wake everyone).
    let count = if cc.arguments().len() <= 2 || cc.arg(2).is_undefined() {
        usize::MAX
    } else {
        let c = cc.arg(2).to_number(ctx)?.to_f64();
        if c.is_nan() || c < 0.0 {
            0
        } else if !c.is_finite() {
            usize::MAX
        } else {
            c as usize
        }
    };

    // The shared-memory check comes last: a non-shared buffer returns 0 (it has
    // no waiters), it does not throw.
    if ta.array_buffer().class_id() != JS_CLASS_SHARED_ARRAY_BUFFER {
        return Ok(JSValue::from(0));
    }
    ta.ensure_attached(ctx)?;

    let key = wait_key(&ta, nth, width);
    let n = wake_waiters(key, count);
    Ok(JSValue::from(n as i64))
}

// ---------------------------------------------------------------------------
// Atomics.wait
// ---------------------------------------------------------------------------

pub fn js_atomics_wait(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // `ValidateSharedIntegerTypedArray(typedArray, true)`: an integer
    // TypedArray that is Int32/BigInt64 AND backed by a SharedArrayBuffer. The
    // shared check happens before any argument coercion.
    let (ta, _cid, width, signed, is_bigint) = validate_atomic(ctx, cc.arg(0), true)?;
    if !signed {
        return Err(ctx
            .throw_type_error("Atomics.wait: typed array must be an Int32Array or BigInt64Array"));
    }
    require_shared(ctx, &ta)?;

    // `ValidateAtomicAccess`: capture the length before coercing the index.
    let length = ta.length();
    let nth = cc.arg(1).to_index(ctx)? as usize;
    if nth >= length {
        return Err(ctx.throw_range_error("Atomics: index out of range"));
    }
    let mask = mask_for(width);
    let elem_off = nth * (width as usize / 8);

    let current = ta.with_bytes(|buf| read_bits(buf, elem_off, width) & mask);
    let expected_bits = value_to_bits(ctx, cc.arg(2), signed, is_bigint, width)?;

    if current != (expected_bits & mask) {
        return str_val(ctx, "not-equal");
    }

    // Compute timeout in nanoseconds.
    let timeout = if cc.arguments().len() <= 3 || cc.arg(3).is_undefined() {
        f64::INFINITY
    } else {
        cc.arg(3).to_number(ctx)?.to_f64()
    };
    let timeout_ns: i64 = if timeout.is_nan() {
        // Per spec: "If q is NaN, let t be +∞".
        i64::MAX
    } else if timeout < 0.0 {
        // Negative timeouts resolve to an immediate time-out.
        0
    } else if !timeout.is_finite() {
        i64::MAX
    } else {
        (timeout * 1e6) as i64
    };

    // `AgentCanSuspend`: a host where the agent cannot be suspended (e.g. the
    // main host thread under test262's `CanBlockIsFalse`) throws a TypeError
    // instead of blocking.
    if !ctx.rt().agent_can_suspend {
        return Err(ctx
            .throw_type_error("Atomics.wait: cannot be called from a thread that cannot suspend"));
    }

    let key = wait_key(&ta, nth, width);
    let waiter: Waiter = Arc::new((Mutex::new(false), Condvar::new()));

    // Register before checking the value again, so a `notify` that arrives
    // (possibly after a `store`) either sets our flag or we observe the change
    // on the next loop iteration.
    {
        let mut map = wait_registry().map.lock().unwrap();
        map.entry(key).or_default().push(waiter.clone());
    }

    let mut woken = false;
    let mut guard = waiter.0.lock().unwrap();
    // A deadline ensures the wait is *at least* `timeout`, never shorter. If a
    // `wait_timeout` wakes early (e.g. spurious wakeup or coarse OS timer
    // resolution), we re-wait for the remaining time instead of giving up, so a
    // positive timeout cannot resolve "timed-out" before the requested duration
    // — test262's `no-spurious-wakeup-*` tests assert the observed wait is >=
    // the timeout.
    // `timeout_ns <= 0` (negative / zero timeout) must time out immediately,
    // NOT wait forever — so give it a deadline already in the past. Only
    // `+Infinity` (`i64::MAX`) waits indefinitely (until a `notify`).
    let deadline = if timeout_ns == i64::MAX {
        None
    } else {
        Some(std::time::Instant::now() + Duration::from_nanos(timeout_ns.max(0) as u64))
    };
    loop {
        // Only an explicit `Atomics.notify` (which sets our flag and signals the
        // condvar) wakes a waiter. A plain `store`/`add` must NOT spuriously
        // wake the waiter, so we never treat a changed memory value as a wakeup.
        if *guard {
            woken = true;
            break;
        }
        match deadline {
            Some(dl) => {
                let now = std::time::Instant::now();
                if now >= dl {
                    break;
                }
                let remaining = dl - now;
                let (g, _res) = waiter.1.wait_timeout(guard, remaining).unwrap();
                guard = g;
                // Loop re-checks `*guard` and the deadline so the total wait is
                // at least `timeout` even if this wait returned early.
            }
            None => {
                guard = match waiter.1.wait(guard) {
                    Ok(g) => g,
                    Err(p) => p.into_inner(),
                };
            }
        }
    }

    drop_waiter(key, &waiter);

    str_val(ctx, if woken { "ok" } else { "timed-out" })
}

// ---------------------------------------------------------------------------
// Atomics.waitAsync
// ---------------------------------------------------------------------------

pub fn js_atomics_wait_async(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let (ta, _cid, width, signed, is_bigint) = validate_atomic(ctx, cc.arg(0), true)?;
    if !signed {
        return Err(ctx.throw_type_error(
            "Atomics.waitAsync: typed array must be an Int32Array or BigInt64Array",
        ));
    }
    require_shared(ctx, &ta)?;

    // DoWait step 4 (ValidateAtomicAccess): capture the typed array length
    // BEFORE coercing the index, so a `valueOf` that grows the buffer cannot
    // change the recorded bounds.
    let length = ta.length();
    let nth = cc.arg(1).to_index(ctx)?;
    if nth as usize >= length {
        return Err(ctx.throw_range_error("Atomics.waitAsync: index out of range"));
    }

    // DoWait step 6-ish: coerce the expected value (after index validation).
    let expected_bits = value_to_bits(ctx, cc.arg(2), signed, is_bigint, width)?;

    // DoWait step 6: ToNumber(timeout).
    let timeout = cc.arg(3);
    let q = if timeout.is_undefined() {
        f64::INFINITY
    } else {
        timeout.to_number(ctx)?.to_f64()
    };

    // Read current value and compare with the expected value.
    let mask = mask_for(width);
    let off = nth as usize * (width as usize / 8);
    let current = ta.with_bytes(|buf| read_bits(buf, off, width) & mask);

    let mut result = ctx.create_base_object()?;

    if current != (expected_bits & mask) {
        // Synchronous non-wait path: { async: false, value: "not-equal" }.
        let not_eq = str_val(ctx, "not-equal")?;
        result.define_property_value_str(
            ctx,
            "async",
            JSValue::from(false),
            crate::JS_PROP_WRITABLE | crate::JS_PROP_CONFIGURABLE,
        )?;
        result.define_property_value_str(
            ctx,
            "value",
            not_eq,
            crate::JS_PROP_WRITABLE | crate::JS_PROP_CONFIGURABLE,
        )?;
        return Ok(JSValue::from(result));
    }

    // Value matches. Per spec, a +0 timeout (including `false`, `null`, `0`)
    // resolves synchronously to "timed-out" with `async: false`.
    if q == 0.0 || (q.is_finite() && q < 0.0) {
        let timed_out = str_val(ctx, "timed-out")?;
        result.define_property_value_str(
            ctx,
            "async",
            JSValue::from(false),
            crate::JS_PROP_WRITABLE | crate::JS_PROP_CONFIGURABLE,
        )?;
        result.define_property_value_str(
            ctx,
            "value",
            timed_out,
            crate::JS_PROP_WRITABLE | crate::JS_PROP_CONFIGURABLE,
        )?;
        return Ok(JSValue::from(result));
    }

    // Equal: register a waiter and return a Promise ({ async: true, value:
    // Promise }). The Promise settles to "ok" when woken by `Atomics.notify`,
    // or "timed-out" on timeout. A worker thread performs the blocking wait so
    // the calling (agent) thread is never blocked.
    let key = wait_key(&ta, nth as usize, width);
    let waiter: Waiter = Arc::new((Mutex::new(false), Condvar::new()));
    wait_registry()
        .map
        .lock()
        .unwrap()
        .entry(key)
        .or_default()
        .push(waiter.clone());

    // Per-runtime count (see the module comment): register this waiter on the
    // owning runtime so only ITS tick handler reports pending work.
    ctx.rt()
        .atomics_wait_async_count
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // q is already coerced via ToNumber(timeout). +0 / negative were handled
    // above (synchronous "timed-out"); here q > 0 or +Infinity.
    let timeout_ns: i64 = if q.is_nan() || !q.is_finite() {
        i64::MAX // +Infinity: wait forever (only `notify` can wake it)
    } else {
        (q.max(0.0) * 1e6) as i64
    };

    let (promise, tx) = Promise::create_with_channel::<&'static str>(ctx, str_val)?;

    let worker_key = key;
    let worker_waiter = waiter.clone();
    // The worker runs on a detached OS thread and must decrement the count of
    // the runtime that registered the waiter (it may outlive the runtime).
    let wait_async_count = ctx.rt().atomics_wait_async_count.clone();

    std::thread::spawn(move || {
        let (lock, cvar) = &*worker_waiter;
        let mut guard = lock.lock().unwrap();
        let mut woken = false;
        // A deadline ensures the wait is *at least* `timeout`, never shorter. If a
        // `wait_timeout` wakes early (e.g. spurious or coarse OS timer resolution),
        // we re-wait for the remaining time instead of giving up, so `Atomics.waitAsync`
        // with a positive timeout cannot resolve "timed-out" before the requested
        // duration — test262's `no-spurious-wakeup-*` tests assert the observed
        // wait is >= the timeout.
        let deadline = if timeout_ns > 0 && timeout_ns != i64::MAX {
            Some(std::time::Instant::now() + Duration::from_nanos(timeout_ns as u64))
        } else {
            None // +Infinity: only `notify` can wake it.
        };
        loop {
            if *guard {
                woken = true;
                break;
            }
            match deadline {
                Some(dl) => {
                    let now = std::time::Instant::now();
                    if now >= dl {
                        break;
                    }
                    let remaining = dl - now;
                    let (g, _res) = cvar.wait_timeout(guard, remaining).unwrap();
                    guard = g;
                    // Loop re-checks `*guard` and the deadline so the total wait is
                    // at least `timeout` even if this wait returned early.
                }
                None => {
                    guard = match cvar.wait(guard) {
                        Ok(g) => g,
                        Err(p) => p.into_inner(),
                    };
                }
            }
        }
        drop(guard);
        drop_waiter(worker_key, &worker_waiter);
        // Deliver the value to the channel *first*, then drop the waiter count.
        // The count must remain > 0 while the value is in the channel so a tick
        // handler sees real pending work and drives the polling job that resolves
        // the Promise. Decrementing before the send could let the event loop stop
        // in the (tiny) window before the value arrives, leaving the Promise
        // unresolved and the test hanging on `$DONE() not called`.
        let val = if woken { "ok" } else { "timed-out" };
        let _ = tx.send(val);
        wait_async_count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    });

    result.define_property_value_str(
        ctx,
        "async",
        JSValue::from(true),
        crate::JS_PROP_WRITABLE | crate::JS_PROP_CONFIGURABLE,
    )?;
    result.define_property_value_str(
        ctx,
        "value",
        JSValue::from(promise),
        crate::JS_PROP_WRITABLE | crate::JS_PROP_CONFIGURABLE,
    )?;
    Ok(JSValue::from(result))
}

pub const js_atomics_funcs: &[FieldDef] = &[
    FieldDef::FUNC("load", 2, js_atomics_load, 0),
    FieldDef::FUNC("store", 3, js_atomics_store, 0),
    FieldDef::FUNC("exchange", 3, js_atomics_exchange, 0),
    FieldDef::FUNC("compareExchange", 4, js_atomics_compare_exchange, 0),
    FieldDef::FUNC("add", 3, js_atomics_add, 0),
    FieldDef::FUNC("sub", 3, js_atomics_sub, 0),
    FieldDef::FUNC("and", 3, js_atomics_and, 0),
    FieldDef::FUNC("or", 3, js_atomics_or, 0),
    FieldDef::FUNC("xor", 3, js_atomics_xor, 0),
    FieldDef::FUNC("isLockFree", 1, js_atomics_is_lock_free, 0),
    FieldDef::FUNC("notify", 3, js_atomics_notify, 0),
    FieldDef::FUNC("wait", 4, js_atomics_wait, 0),
    FieldDef::FUNC("waitAsync", 4, js_atomics_wait_async, 0),
    FieldDef::FUNC("pause", 0, js_atomics_pause, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Atomics",
        crate::JS_PROP_CONFIGURABLE,
    ),
];

/// Lazy-initialized `Atomics` namespace object, registered as a global.
pub const js_atomics_obj: &[FieldDef] = &[FieldDef::OBJECT(
    "Atomics",
    js_atomics_funcs,
    crate::JS_PROP_WRITABLE | crate::JS_PROP_CONFIGURABLE,
)];
