// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::Cell;

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    Array, JS_CLASS_ITERATOR_HELPER,
    class::ClassID,
    context::ScopedContext,
    error::{Exception, JSResult},
    function::CallContext,
    value::JSValue,
};

use super::{iterator_return, make_iterator_next_result};

#[derive(Debug)]
pub(crate) struct IteratorZipData {
    /// (inner_iterator, next_method) for each compiled entry
    pub entries: Vec<(JSValue, JSValue)>,
    /// Property keys for zipKeyed (empty for zip)
    pub keys: Vec<crate::atom::Atom>,
    /// Zip mode: 0=shortest, 1=longest, 2=strict
    pub mode: Cell<u8>,
    /// Pre-consumed padding values for longest mode (empty for non-longest)
    pub padding: Vec<JSValue>,
    /// Reentrancy guard
    pub running: Cell<bool>,
    /// Closed state
    pub closed: Cell<bool>,
    /// Per-entry done tracking (for longest mode)
    pub done: Vec<Cell<bool>>,
    /// Whether lengths were unequal (strict mode: throws TypeError on next after close)
    pub unequal_length: Cell<bool>,
    /// Whether any value was ever yielded (distinguishes suspended-start vs suspended-yield)
    pub yielded: Cell<bool>,
}

impl GcTrace for IteratorZipData {
    fn trace(&self, tr: &mut GcTraceCtx) {
        for (obj, method) in &self.entries {
            obj.trace(tr);
            method.trace(tr);
        }
        for key in &self.keys {
            key.trace(tr);
        }
        for v in &self.padding {
            v.trace(tr);
        }
    }
}

// =========================================================================
// Iterator.zip next() / return()
// =========================================================================

/// Close all open iterators except a specific index, in reverse order.
/// Sets their done flag and calls return(). Ignores close errors.
fn close_all_except(ctx: &mut ScopedContext, zd: &IteratorZipData, except: usize) {
    let primary = ctx.take_exception();
    for j in (0..zd.entries.len()).rev() {
        if j != except && !zd.done[j].get() {
            zd.done[j].set(true);
            if let Some(o) = zd.entries[j].0.as_object() {
                let _ = ctx.take_exception();
                let _ = iterator_return(ctx, o);
            }
        }
    }
    // Primary error preserved; close errors ignored
    if primary != Exception::No {
        ctx.throw(primary);
    }
}

/// Close all remaining open iterators in reverse order.
fn close_all_remaining(ctx: &mut ScopedContext, zd: &IteratorZipData) -> Result<(), i32> {
    let primary = ctx.take_exception();
    let mut first_err: Option<Exception> = None;

    for j in (0..zd.entries.len()).rev() {
        if !zd.done[j].get() {
            zd.done[j].set(true);
            if let Some(o) = zd.entries[j].0.as_object() {
                let _ = ctx.take_exception();

                if primary != Exception::No {
                    // Close errors ignored
                    let _ = iterator_return(ctx, o);
                } else {
                    if iterator_return(ctx, o).is_err() {
                        if first_err.is_none() {
                            first_err = Some(ctx.take_exception());
                        }
                    }
                }
            }
        }
    }

    if primary != Exception::No {
        ctx.throw(primary);
        Ok(())
    } else if let Some(exc) = first_err {
        ctx.throw(exc);
        Err(-1)
    } else {
        Ok(())
    }
}

pub(crate) fn js_iterator_zip_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let zd = this_obj.payload().iterator_zip().ok_or_else(|| {
        ctx.throw_type_error("not an Iterator Zip");
        -1
    })?;

    // Reentrancy guard
    if zd.running.get() {
        ctx.throw_type_error("Iterator Zip is already executing");
        return Err(-1);
    }

    if zd.closed.get() {
        // Strict: throw if lengths were unequal
        if zd.mode.get() == 2 && zd.unequal_length.get() {
            ctx.throw_type_error("Iterator Zip already completed with unequal lengths");
            return Err(-1);
        }
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }

    zd.running.set(true);

    let mode = zd.mode.get();
    let count = zd.entries.len();

    if count == 0 {
        zd.closed.set(true);
        zd.running.set(false);
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }

    let mut values = Vec::with_capacity(count);

    for (i, (obj, next_fn)) in zd.entries.iter().enumerate() {
        if mode == 1 && zd.done[i].get() {
            // Longest: use pre-consumed padding for done entries
            values.push(if i < zd.padding.len() {
                zd.padding[i]
            } else {
                JSValue::UNDEFINED
            });
            continue;
        }

        // Step 1: Call next()
        let next_result = match next_fn.call(ctx, *obj, &[]) {
            Ok(r) => r,
            Err(e) => {
                // Close this iter, then close all others in reverse
                zd.done[i].set(true);
                close_all_except(ctx, zd, i);
                zd.closed.set(true);
                zd.running.set(false);
                return Err(e);
            }
        };

        // Step 2: Ensure result is an object
        let next_obj = match next_result.ensure_object(ctx) {
            Ok(o) => o,
            Err(e) => {
                zd.done[i].set(true);
                close_all_except(ctx, zd, i);
                zd.closed.set(true);
                zd.running.set(false);
                return Err(e);
            }
        };

        // Step 3: Check .done
        let done = match next_obj.get_property(ctx, crate::atom::Atom::CONST_done) {
            Ok(v) => v.to_bool().unwrap_or(false),
            Err(e) => {
                zd.done[i].set(true);
                close_all_except(ctx, zd, i);
                zd.closed.set(true);
                zd.running.set(false);
                return Err(e);
            }
        };

        if done {
            zd.done[i].set(true);

            if mode == 1 {
                // Longest: use padding
                values.push(if i < zd.padding.len() {
                    zd.padding[i]
                } else {
                    JSValue::UNDEFINED
                });
                continue;
            }

            if mode == 0 {
                // Shortest: close all remaining open iterators, return done
                zd.closed.set(true);
                zd.running.set(false);
                close_all_remaining(ctx, zd)?;
                return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
            }

            // Strict (2): i != 0, close all with TypeError
            if i != 0 {
                ctx.throw_type_error("Iterator Zip already completed with unequal lengths");
                close_all_remaining(ctx, zd)?;
                zd.closed.set(true);
                zd.unequal_length.set(true);
                zd.running.set(false);
                return Err(-1);
            }

            // i == 0: step remaining iterators; check if all are simultaneously done
            let len = zd.entries.len();
            for k in 1..len {
                if zd.done[k].get() {
                    continue;
                }
                let (obj, next_fn) = &zd.entries[k];
                let step_result = match next_fn.call(ctx, *obj, &[]) {
                    Ok(r) => r,
                    Err(e) => {
                        // k-th iterator threw; close all
                        zd.done[k].set(true);
                        close_all_except(ctx, zd, k);
                        zd.closed.set(true);
                        zd.running.set(false);
                        return Err(e);
                    }
                };
                let step_obj = match step_result.ensure_object(ctx) {
                    Ok(o) => o,
                    Err(e) => {
                        zd.done[k].set(true);
                        close_all_except(ctx, zd, k);
                        zd.closed.set(true);
                        zd.running.set(false);
                        return Err(e);
                    }
                };
                let step_done = match step_obj.get_property(ctx, crate::atom::Atom::CONST_done) {
                    Ok(v) => v.to_bool().unwrap_or(false),
                    Err(e) => {
                        zd.done[k].set(true);
                        close_all_except(ctx, zd, k);
                        zd.closed.set(true);
                        zd.running.set(false);
                        return Err(e);
                    }
                };
                if step_done {
                    zd.done[k].set(true);
                } else {
                    // Not done → unequal lengths
                    ctx.throw_type_error("Iterator Zip already completed with unequal lengths");
                    close_all_remaining(ctx, zd)?;
                    zd.closed.set(true);
                    zd.unequal_length.set(true);
                    zd.running.set(false);
                    return Err(-1);
                }
            }

            // All remaining iterators are also done simultaneously
            zd.running.set(false);
            return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
        }

        // Step 4: Read .value
        let value = match next_obj.get_property(ctx, crate::atom::Atom::CONST_value) {
            Ok(v) => v,
            Err(e) => {
                zd.done[i].set(true);
                close_all_except(ctx, zd, i);
                zd.closed.set(true);
                zd.running.set(false);
                return Err(e);
            }
        };
        values.push(value);
    }

    // After loop (no done in shortest/strict):
    let all_done = zd.done.iter().all(|d| d.get());
    if mode == 1 && all_done {
        zd.closed.set(true);
        zd.running.set(false);
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }

    let result_val = if zd.keys.is_empty() {
        // zip: yield array
        let arr = Array::from_values(ctx, values)?;
        arr.into()
    } else {
        // zipKeyed: yield null-prototype object with keys
        let mut obj = ctx.create_base_object_with_proto(None)?;
        for (i, key) in zd.keys.iter().enumerate() {
            if i < values.len() {
                obj.define_property_value(ctx, *key, values[i], crate::object::JS_PROP_C_W_E)?;
            }
        }
        obj.into()
    };

    zd.yielded.set(true);
    zd.running.set(false);
    make_iterator_next_result(ctx, result_val, false)
}

pub(crate) fn js_iterator_zip_return(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let zd = this_obj.payload().iterator_zip().ok_or_else(|| {
        ctx.throw_type_error("not an Iterator Zip");
        -1
    })?;

    // Reentrancy guard
    if zd.running.get() {
        ctx.throw_type_error("Iterator Zip is already executing");
        return Err(-1);
    }

    if zd.closed.get() {
        return make_iterator_next_result(ctx, cc.arg(0), true);
    }

    // suspended-start: set closed but NOT running; suspended-yield: set running so reentrant calls throw
    if !zd.yielded.get() {
        // next() during close sees closed → done
        zd.closed.set(true);
    } else {
        // next()/return() during close throws TypeError
        zd.running.set(true);
    }

    // Close inner iterators in reverse; the first close error becomes final error
    let mut first_err: Option<Exception> = None;
    for j in (0..zd.entries.len()).rev() {
        if !zd.done[j].get() {
            zd.done[j].set(true);
            if let Some(o) = zd.entries[j].0.as_object() {
                let _ = ctx.take_exception();
                if iterator_return(ctx, o).is_err() && first_err.is_none() {
                    first_err = Some(ctx.take_exception());
                }
            }
        }
    }

    zd.running.set(false);
    zd.closed.set(true);

    if let Some(exc) = first_err {
        ctx.throw(exc);
        return Err(-1);
    }

    make_iterator_next_result(ctx, cc.arg(0), true)
}

pub(crate) fn js_iterator_zip_class_init(
    ctx: &mut ScopedContext,
    class_id: ClassID,
) -> Result<(), i32> {
    // Zip results share IteratorHelper proto so next/return dispatch based on payload
    let helper_proto = ctx.class_proto(JS_CLASS_ITERATOR_HELPER).ok_or_else(|| {
        ctx.throw_internal_error("IteratorHelper proto not initialized");
        -1
    })?;
    ctx.set_class_proto(class_id, helper_proto);
    Ok(())
}
