// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::Cell;

use crate::{
    Array, JS_CLASS_ITERATOR,
    atom::Atom,
    class::ClassID,
    context::ScopedContext,
    error::{Exception, JSResult},
    function::{CallContext, Callback, FieldDef},
    object::{
        JS_GPN_STR_OR_INT_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE,
        JS_PROP_ENUMERABLE, JS_PROP_WRITABLE, ObjectPayload,
    },
    value::JSValue,
};

use super::{
    IteratorConcatData, IteratorWrapData, IteratorZipData, JSIteratorHelperKind,
    call_iterator_next, close_with_pending_exception, get_proto_from_ctor, iterator_return,
    limit_to_count,
};

// =========================================================================
// Constructor
// =========================================================================

/// Iterator constructor — abstract class, but allows subclassing.
fn js_iterator_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    let is_direct = ctx
        .class_constructor(JS_CLASS_ITERATOR)
        .is_some_and(|ctor| new_target.as_object() == Some(ctor));

    if is_direct {
        return Err(ctx
            .throw_type_error("Iterator is an abstract class and cannot be constructed directly"));
    }

    // Get proto from newTarget's realm for subclass construction
    let proto = get_proto_from_ctor(ctx, new_target)?;
    let obj = ctx.create_base_object_with_proto(proto)?;
    Ok(obj.into())
}

/// Getter for `Iterator.prototype.constructor`
fn js_iterator_constructor_getter(ctx: &mut ScopedContext, _cc: CallContext) -> JSResult {
    let ctor = ctx
        .class_constructor(JS_CLASS_ITERATOR)
        .ok_or_else(|| ctx.throw_type_error("Iterator constructor not initialized"))?;
    Ok(ctor.into())
}

/// Setter for `Iterator.prototype.constructor` — SetterThatIgnoresPrototypeProperties
fn js_iterator_constructor_setter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this = cc.this();
    let v = cc.arg(0);

    if !this.is_object() {
        return Err(ctx.throw_type_error("Cannot set property on non-object"));
    }

    if ctx
        .class_proto(JS_CLASS_ITERATOR)
        .is_some_and(|o| this.as_object() == Some(o))
    {
        return Err(ctx.throw_type_error("Cannot assign to constructor on Iterator.prototype"));
    }

    this.define_property_value(ctx, Atom::CONST_constructor, v, JS_PROP_C_W_E)?;

    Ok(JSValue::UNDEFINED)
}

/// Getter for `[Symbol.toStringTag]` — returns "Iterator"
fn js_iterator_toStringTag_getter(ctx: &mut ScopedContext, _cc: CallContext) -> JSResult {
    let s = Atom::from_str(ctx, "Iterator")?;
    s.force_string(ctx).map(JSValue::make_string)
}

/// Setter for `[Symbol.toStringTag]` — SetterThatIgnoresPrototypeProperties
fn js_iterator_toStringTag_setter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this = cc.this();
    let v = cc.arg(0);

    if !this.is_object() {
        return Err(ctx.throw_type_error("Cannot set property on non-object"));
    }

    if let Some(iter_proto) = ctx.class_proto(JS_CLASS_ITERATOR)
        && let Some(o) = this.as_object()
        && o == iter_proto
    {
        return Err(
            ctx.throw_type_error("Cannot assign to Symbol.toStringTag on Iterator.prototype")
        );
    }

    this.define_property_value(ctx, Atom::CONST_Symbol_toStringTag, v, JS_PROP_C_W_E)?;

    Ok(JSValue::UNDEFINED)
}

// =========================================================================
// Class init
// =========================================================================

pub(crate) fn js_iterator_class_init(
    ctx: &mut ScopedContext,
    class_id: ClassID,
) -> Result<(), i32> {
    let mut proto = ctx.iterator_proto()?;
    proto.install_fields(ctx, js_iterator_proto_full_funcs)?;
    ctx.set_class_proto(class_id, proto);

    ctx.init_class_constructor(
        class_id,
        js_iterator_constructor,
        0,
        0,
        proto,
        js_iterator_funcs,
    )?;

    let ctor = ctx
        .class_constructor(class_id)
        .ok_or_else(|| ctx.throw_internal_error("Iterator ctor missing"))?;
    ctx.global_obj().define_property_value(
        ctx,
        Atom::CONST_Iterator,
        ctor.into(),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    )?;

    // Per spec: constructor and @@toStringTag should be accessor properties,
    // overriding value properties set by install_fields.
    {
        let getter_fn: JSValue = ctx
            .create_native_function(
                Callback::Func(js_iterator_constructor_getter),
                "get constructor",
                0,
                0,
            )
            .map_err(|_| -1)?
            .into();
        let setter_fn: JSValue = ctx
            .create_native_function(
                Callback::Func(js_iterator_constructor_setter),
                "set constructor",
                1,
                0,
            )
            .map_err(|_| -1)?
            .into();
        proto
            .define_property_getset(
                ctx,
                Atom::CONST_constructor,
                getter_fn,
                setter_fn,
                JS_PROP_CONFIGURABLE,
            )
            .map(|_| ())?;
    }

    {
        let getter_fn2: JSValue = ctx
            .create_native_function(
                Callback::Func(js_iterator_toStringTag_getter),
                "get [Symbol.toStringTag]",
                0,
                0,
            )
            .map_err(|_| -1)?
            .into();
        let setter_fn2: JSValue = ctx
            .create_native_function(
                Callback::Func(js_iterator_toStringTag_setter),
                "set [Symbol.toStringTag]",
                1,
                0,
            )
            .map_err(|_| -1)?
            .into();
        proto
            .define_property_getset(
                ctx,
                Atom::CONST_Symbol_toStringTag,
                getter_fn2,
                setter_fn2,
                JS_PROP_CONFIGURABLE,
            )
            .map(|_| ())?;
    }

    Ok(())
}

// =========================================================================
// Iterator.concat()
// =========================================================================

fn js_iterator_concat(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let argc = cc.length();
    let mut values = Vec::with_capacity(argc);

    // Validate each argument, collect (object, method) pairs
    for i in 0..argc {
        let item = cc.arg(i);
        if !item.is_object() {
            return Err(
                ctx.throw_type_error("Iterator.concat requires all arguments to be objects")
            );
        }
        // GetMethod(item, %Symbol.iterator%): undefined/null or any other
        // non-callable value is a TypeError.
        let method = item.get_property(ctx, Atom::CONST_Symbol_iterator)?;
        if method.is_undefined_or_null() {
            return Err(ctx.throw_type_error("iterable has no iterator method"));
        }
        method.ensure_callable(ctx)?;
        values.push((item, method));
    }

    let concat = ctx
        .create_instance_with_payload(ObjectPayload::ITERATOR_CONCAT(Box::new(
            IteratorConcatData {
                values,
                index: Cell::new(0),
                iter: Cell::new(None),
                next: Cell::new(JSValue::UNDEFINED),
                running: Cell::new(false),
            },
        )))
        .map_err(|_| -1)?;

    Ok(concat.into())
}

// =========================================================================
// Iterator.from()
// =========================================================================

fn js_iterator_from(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);

    if !obj.is_object() && !obj.is_string() {
        return Err(ctx.throw_type_error("Iterator.from called on non-object"));
    }

    let method = obj.get_property(ctx, Atom::CONST_Symbol_iterator)?;

    let iter = if method.is_undefined_or_null() {
        obj
    } else {
        method.call(ctx, obj, &[])?
    };

    let iter_obj = iter.ensure_object(ctx)?;
    let next = iter_obj.get_property(ctx, Atom::CONST_next)?;

    // Use Iterator constructor for instanceof check
    if let Some(iter_ctor) = ctx.class_constructor(JS_CLASS_ITERATOR)
        && iter_obj.instance_of(ctx, iter_ctor.into())?
    {
        return Ok(iter_obj.into());
    }

    let wrap = ctx
        .create_instance_with_payload(ObjectPayload::ITERATOR_WRAP(Box::new(IteratorWrapData {
            obj: iter_obj,
            next,
        })))
        .map_err(|_| -1)?;

    Ok(wrap.into())
}

// =========================================================================
// Iterator.zip()
// =========================================================================

/// GetIterator (ES 27.1.3.3) — requires [Symbol.iterator]
fn get_iterator(ctx: &mut ScopedContext, item: JSValue) -> Result<(JSValue, JSValue), i32> {
    let method = item.get_property(ctx, Atom::CONST_Symbol_iterator)?;
    if method.is_undefined_or_null() {
        return Err(ctx.throw_type_error("object is not iterable"));
    }
    let iter_val = method.call(ctx, item, &[])?;
    let iter_obj = iter_val.ensure_object(ctx)?;
    let next_method = iter_obj.get_property(ctx, Atom::CONST_next)?;
    Ok((iter_val, next_method))
}

/// GetIteratorFlattenable (reject-strings mode) — for zip inner values
fn get_iterator_flattenable_reject_strings(ctx: &mut ScopedContext, item: JSValue) -> JSResult {
    if !item.is_object() {
        return Err(ctx.throw_type_error("not an object"));
    }
    let o = item.as_object().unwrap();
    let method = o.get_property(ctx, Atom::CONST_Symbol_iterator)?;
    if !method.is_undefined_or_null() {
        return method.call(ctx, item, &[]);
    }
    // Duck-type: use directly if it has .next
    Ok(item)
}

/// Close entries in reverse (IteratorCloseAll semantics). Returns first error, continues closing.
fn close_entries_reverse(ctx: &mut ScopedContext, entries: &[(JSValue, JSValue)]) {
    let primary = ctx.take_exception();
    for &(obj, _) in entries.iter().rev() {
        if let Some(o) = obj.as_object() {
            let _ = ctx.take_exception();
            let _ = iterator_return(ctx, o);
        }
    }
    if primary != Exception::No {
        ctx.throw(primary);
    }
}

/// GetOptionsObject + parse mode/padding from zip options object.
/// `options` must be undefined or an object; `mode` must be exactly one of
/// the three strings (no coercion); `padding` (longest mode) must be
/// undefined or an object.
fn parse_zip_options(ctx: &mut ScopedContext, options: JSValue) -> Result<(u8, JSValue), i32> {
    if !options.is_undefined() && !options.is_object() {
        return Err(ctx.throw_type_error("options must be an object"));
    }

    let obj = match options.as_object() {
        Some(o) => o,
        None => return Ok((0, JSValue::UNDEFINED)),
    };

    let mode_val = obj.get_property_str(ctx, "mode")?;
    if mode_val.is_undefined() {
        return Ok((0, JSValue::UNDEFINED));
    }

    // Primitive string comparison only: a String wrapper or an object with
    // toString/valueOf/Symbol.toPrimitive is a TypeError without coercion.
    let mode = if let Some(s) = mode_val.as_string(false) {
        if s.eq_str("shortest") {
            0
        } else if s.eq_str("longest") {
            1
        } else if s.eq_str("strict") {
            2
        } else {
            return Err(ctx.throw_type_error(
                "Iterator.zip mode must be 'shortest', 'strict', or 'longest'",
            ));
        }
    } else {
        return Err(ctx.throw_type_error(
            "Iterator.zip mode must be 'shortest', 'strict', or 'longest'",
        ));
    };

    let padding = if mode == 1 {
        let p = obj.get_property_str(ctx, "padding")?;
        if !p.is_undefined() && !p.is_object() {
            return Err(ctx.throw_type_error("padding must be an object"));
        }
        p
    } else {
        JSValue::UNDEFINED
    };
    Ok((mode, padding))
}

fn js_iterator_zip(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let iterables = cc.arg(0);
    let options = if cc.length() > 1 {
        cc.arg(1)
    } else {
        JSValue::UNDEFINED
    };

    // Step 1: iterables must be an object (checked before reading options).
    if !iterables.is_object() {
        return Err(ctx.throw_type_error("iterables must be an object"));
    }

    let (mode, padding) = parse_zip_options(ctx, options)?;

    // GetIteratorFlattenable (reject-strings) over iterables
    let outer = get_iterator_flattenable_reject_strings(ctx, iterables)?;
    let outer_obj = outer.ensure_object(ctx)?;
    let outer_next = outer_obj.get_property(ctx, Atom::CONST_next)?;

    // Collect inner iterators, close on error. Track whether outer yielded a value
    // before error (determines if outer should be closed).
    let mut entries: Vec<(JSValue, JSValue)> = Vec::new();
    let mut need_close_outer = false;
    let collect_result = (|| -> Result<(), i32> {
        loop {
            need_close_outer = false;
            let result = outer_next.call(ctx, outer, &[])?;
            let result_obj = result.ensure_object(ctx)?;
            let done = result_obj.get_property(ctx, Atom::CONST_done)?;
            if done.to_bool().unwrap_or(false) {
                break Ok(());
            }
            let value = result_obj.get_property(ctx, Atom::CONST_value)?;
            need_close_outer = true;

            // reject-strings for inner iterables; error here includes inputIter in close set
            let inner = get_iterator_flattenable_reject_strings(ctx, value)?;
            let inner_obj = inner.ensure_object(ctx)?;
            let inner_next = inner_obj.get_property(ctx, Atom::CONST_next)?;
            entries.push((inner, inner_next));
        }
    })();

    if collect_result.is_err() {
        // Close entries in reverse; close outer only if error was NOT from IteratorStepValue
        close_entries_reverse(ctx, &entries);
        if need_close_outer {
            let primary = ctx.take_exception();
            if let Some(o) = outer.as_object() {
                let _ = iterator_return(ctx, o);
            }
            if primary != Exception::No {
                ctx.throw(primary);
            }
        }
        return Err(-1);
    }

    // Pre-consume padding values for longest mode
    let padding_values = if mode == 1 {
        if padding.is_undefined() {
            vec![JSValue::UNDEFINED; entries.len()]
        } else {
            let pad_entries_len = entries.len(); // capture before move
            let pad_result = (|| -> Result<Vec<JSValue>, i32> {
                let (pad_iter, pad_next) = get_iterator(ctx, padding)?;
                let mut values = Vec::with_capacity(pad_entries_len);
                let mut exhausted = false;
                for _ in 0..pad_entries_len {
                    if exhausted {
                        values.push(JSValue::UNDEFINED);
                        continue;
                    }
                    let result = pad_next.call(ctx, pad_iter, &[])?;
                    let result_obj = result.ensure_object(ctx)?;
                    let pad_done = result_obj
                        .get_property(ctx, Atom::CONST_done)?
                        .to_bool()
                        .unwrap_or(false);
                    if pad_done {
                        exhausted = true;
                        values.push(JSValue::UNDEFINED);
                    } else {
                        let v = result_obj.get_property(ctx, Atom::CONST_value)?;
                        values.push(v);
                    }
                }
                // Close padding iterator if not exhausted
                if !exhausted && let Some(pad_obj) = pad_iter.as_object() {
                    iterator_return(ctx, pad_obj)?;
                }

                Ok(values)
            })();

            match pad_result {
                Ok(v) => v,
                Err(_) => {
                    close_entries_reverse(ctx, &entries);
                    // Close outer iterables iterator
                    let primary = ctx.take_exception();
                    if let Some(o) = outer.as_object() {
                        let _ = iterator_return(ctx, o);
                    }
                    if primary != Exception::No {
                        ctx.throw(primary);
                    }
                    return Err(-1);
                }
            }
        }
    } else {
        Vec::new()
    };

    let done_states = (0..entries.len()).map(|_| Cell::new(false)).collect();

    let zip_data = IteratorZipData {
        entries,
        keys: Vec::new(),
        mode: Cell::new(mode),
        padding: padding_values,
        running: Cell::new(false),
        closed: Cell::new(false),
        done: done_states,
        unequal_length: Cell::new(false),
        yielded: Cell::new(false),
    };

    let obj = ctx
        .create_instance_with_payload(ObjectPayload::ITERATOR_ZIP(Box::new(zip_data)))
        .map_err(|_| -1)?;

    Ok(obj.into())
}

// =========================================================================
// Iterator.zipKeyed()
// =========================================================================

fn js_iterator_zipKeyed(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let iterables = cc.arg(0);
    let options = if cc.length() > 1 {
        cc.arg(1)
    } else {
        JSValue::UNDEFINED
    };

    // Step 1: iterables must be an object (checked before reading options).
    if !iterables.is_object() {
        return Err(ctx.throw_type_error("iterables must be an object"));
    }

    let (mode, padding) = parse_zip_options(ctx, options)?;

    let mut iterables_obj = iterables.ensure_object(ctx)?;

    // Get all own property keys, check enumerable per key
    let all_keys =
        iterables_obj.get_own_property_names(ctx, JS_GPN_STR_OR_INT_MASK | JS_GPN_SYMBOL_MASK)?;

    // Collect entries with per-key enumerable check
    let mut entries: Vec<(JSValue, JSValue)> = Vec::new();
    let mut keys: Vec<Atom> = Vec::new();

    let collect_result = (|| -> Result<(), i32> {
        for pk in all_keys.iter() {
            let key = pk.atom;
            // Check enumerability
            if let Some(desc) = iterables_obj.get_own_property(ctx, key)? {
                if desc.flags & JS_PROP_ENUMERABLE == 0 {
                    continue;
                }
            } else {
                continue;
            }
            let value = iterables_obj.get_property(ctx, key)?;
            // Skip undefined per spec
            if value.is_undefined() {
                continue;
            }
            let inner = get_iterator_flattenable_reject_strings(ctx, value)?;
            let inner_obj = inner.ensure_object(ctx)?;
            let inner_next = inner_obj.get_property(ctx, Atom::CONST_next)?;
            entries.push((inner, inner_next));
            keys.push(key);
        }
        Ok(())
    })();

    if collect_result.is_err() {
        close_entries_reverse(ctx, &entries);
        return Err(-1);
    }

    // Pre-consume padding for longest mode; for zipKeyed, padding is read per-key
    let padding_values = if mode == 1 {
        if padding.is_undefined() {
            vec![JSValue::UNDEFINED; entries.len()]
        } else if let Some(pad_obj) = padding.as_object() {
            let mut values = Vec::with_capacity(entries.len());
            let pad_result = (|| -> Result<(), i32> {
                for key in &keys {
                    let v = pad_obj.get_property(ctx, *key)?;
                    values.push(v);
                }
                Ok(())
            })();
            if let Err(e) = pad_result {
                close_entries_reverse(ctx, &entries);
                return Err(e);
            }
            values
        } else {
            let e = ctx.throw_type_error("padding must be an object");
            close_entries_reverse(ctx, &entries);
            return Err(e);
        }
    } else {
        Vec::new()
    };

    let done_states = (0..entries.len()).map(|_| Cell::new(false)).collect();

    let zip_data = IteratorZipData {
        entries,
        keys,
        mode: Cell::new(mode),
        padding: padding_values,
        running: Cell::new(false),
        closed: Cell::new(false),
        done: done_states,
        unequal_length: Cell::new(false),
        yielded: Cell::new(false),
    };

    let obj = ctx
        .create_instance_with_payload(ObjectPayload::ITERATOR_ZIP(Box::new(zip_data)))
        .map_err(|_| -1)?;

    Ok(obj.into())
}

// =========================================================================
// Lazy helper factory
// =========================================================================

fn js_create_iterator_helper(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let kind = match cc.magic() as usize {
        0 => JSIteratorHelperKind::Drop,
        1 => JSIteratorHelperKind::Filter,
        2 => JSIteratorHelperKind::FlatMap,
        3 => JSIteratorHelperKind::Map,
        4 => JSIteratorHelperKind::Take,
        _ => unreachable!(),
    };

    let this_val = cc.this();

    let this_obj = this_val
        .as_object()
        .ok_or_else(|| ctx.throw_type_error_not_an_object())?;

    // Validate arguments BEFORE reading .next (per spec).
    // On failure, close the underlying iterator first.
    let validate_result = match kind {
        JSIteratorHelperKind::Drop | JSIteratorHelperKind::Take => {
            let name = if kind == JSIteratorHelperKind::Drop {
                "drop"
            } else {
                "take"
            };
            limit_to_count(ctx, cc.arg(0), name).map(|c| (JSValue::UNDEFINED, c))
        }
        JSIteratorHelperKind::Filter
        | JSIteratorHelperKind::FlatMap
        | JSIteratorHelperKind::Map => {
            let func = cc.arg(0);
            let what = match kind {
                JSIteratorHelperKind::Filter => "filter",
                JSIteratorHelperKind::FlatMap => "flatMap",
                _ => "map",
            };
            if func.is_callable() {
                Ok((func, 0))
            } else {
                Err(ctx.throw_type_error(&format!("{what} argument must be a function")))
            }
        }
    };

    let (func, count) = match validate_result {
        Ok(v) => v,
        Err(_) => {
            // Validation failed: save exception, close underlying, restore exception
            let pending_exc = ctx.take_exception();
            let _ = iterator_return(ctx, this_obj);
            if pending_exc != crate::error::Exception::No {
                ctx.throw(pending_exc);
            }
            return Err(-1);
        }
    };

    // Read .next (GetIteratorDirect)
    let next = this_obj.get_property(ctx, Atom::CONST_next)?;

    let helper = ctx
        .create_instance_with_payload(ObjectPayload::ITERATOR_HELPER(Box::new(
            super::IteratorHelperData {
                obj: this_obj,
                next,
                func,
                inner: Cell::new(JSValue::UNDEFINED),
                inner_next: Cell::new(JSValue::UNDEFINED),
                has_inner: Cell::new(false),
                count: Cell::new(count),
                kind,
                executing: Cell::new(false),
                done: Cell::new(false),
                source_closed: Cell::new(false),
            },
        )))
        .map_err(|_| -1)?;

    Ok(helper.into())
}

// =========================================================================
// Eager methods: every, find, forEach, some
// =========================================================================

fn eager_iterate(ctx: &mut ScopedContext, this_val: JSValue, func: JSValue, mode: i32) -> JSResult {
    let this_obj = this_val
        .as_object()
        .ok_or_else(|| ctx.throw_type_error_not_an_object())?;
    let next = this_obj.get_property(ctx, Atom::CONST_next)?;
    let mut index: i64 = 0;

    loop {
        let (value, done) = call_iterator_next(ctx, this_obj, next)?;
        if done {
            return Ok(match mode {
                0 => JSValue::TRUE,
                2 => JSValue::UNDEFINED,
                3 => JSValue::FALSE,
                _ => JSValue::UNDEFINED,
            });
        }
        let result = func.call(ctx, JSValue::UNDEFINED, &[value, JSValue::from(index)]);
        index += 1;
        let val = match result {
            Ok(r) => r,
            Err(_) => {
                // Callback threw: close underlying, preserve error
                close_with_pending_exception(ctx, this_obj);
                return Err(-1);
            }
        };
        let truthy = val.to_bool().unwrap_or(false);

        match mode {
            0 => {
                if !truthy {
                    iterator_return(ctx, this_obj)?;
                    return Ok(JSValue::FALSE);
                }
            }
            1 => {
                if truthy {
                    iterator_return(ctx, this_obj)?;
                    return Ok(value);
                }
            }
            2 => {}
            3 => {
                if truthy {
                    iterator_return(ctx, this_obj)?;
                    return Ok(JSValue::TRUE);
                }
            }
            _ => unreachable!(),
        }
    }
}

fn validate_iterate_arg_and_close_on_fail(
    ctx: &mut ScopedContext,
    this_val: JSValue,
    func: JSValue,
    name: &str,
) -> Result<(), i32> {
    if !func.is_callable() {
        let this_obj = this_val.as_object();
        let e = ctx.throw_type_error(&format!("{name} argument must be a function"));
        if let Some(o) = this_obj {
            close_with_pending_exception(ctx, o);
        }
        return Err(e);
    }
    Ok(())
}

fn js_iterator_proto_every(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let func = cc.arg(0);
    validate_iterate_arg_and_close_on_fail(ctx, this, func, "every")?;
    eager_iterate(ctx, this, func, 0)
}

fn js_iterator_proto_find(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let func = cc.arg(0);
    validate_iterate_arg_and_close_on_fail(ctx, this, func, "find")?;
    eager_iterate(ctx, this, func, 1)
}

fn js_iterator_proto_forEach(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let func = cc.arg(0);
    validate_iterate_arg_and_close_on_fail(ctx, this, func, "forEach")?;
    eager_iterate(ctx, this, func, 2)
}

fn js_iterator_proto_some(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let func = cc.arg(0);
    validate_iterate_arg_and_close_on_fail(ctx, this, func, "some")?;
    eager_iterate(ctx, this, func, 3)
}

// =========================================================================
// reduce, toArray
// =========================================================================

fn js_iterator_proto_reduce(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this
        .as_object()
        .ok_or_else(|| ctx.throw_type_error_not_an_object())?;

    let func = cc.arg(0);
    if !func.is_callable() {
        let e = ctx.throw_type_error("reduce argument must be a function");
        close_with_pending_exception(ctx, this_obj);
        return Err(e);
    }

    let next = this_obj.get_property(ctx, Atom::CONST_next)?;
    let mut accumulator: JSValue;
    let mut index: i64;

    if cc.length() >= 2 {
        accumulator = cc.arg(1);
        index = 0;
    } else {
        let (value, done) = call_iterator_next(ctx, this_obj, next)?;
        if done {
            return Err(ctx.throw_type_error("reduce of empty iterator with no initial value"));
        }
        accumulator = value;
        index = 1;
    }

    loop {
        let (value, done) = call_iterator_next(ctx, this_obj, next)?;
        if done {
            return Ok(accumulator);
        }
        let result = func.call(
            ctx,
            JSValue::UNDEFINED,
            &[accumulator, value, JSValue::from(index)],
        );
        match result {
            Ok(r) => {
                accumulator = r;
            }
            Err(_) => {
                close_with_pending_exception(ctx, this_obj);
                return Err(-1);
            }
        }
        index += 1;
    }
}

fn js_iterator_proto_toArray(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this
        .as_object()
        .ok_or_else(|| ctx.throw_type_error_not_an_object())?;

    let next = this_obj.get_property(ctx, Atom::CONST_next)?;
    let mut arr = Array::create(ctx)?;
    let mut i: u32 = 0;

    loop {
        let (value, done) = call_iterator_next(ctx, this_obj, next)?;
        if done {
            return Ok(arr.into());
        }
        arr.define_property_value_i64(ctx, i as i64, value, JS_PROP_C_W_E)?;
        i += 1;
    }
}

// =========================================================================
// [Symbol.iterator]()
// =========================================================================

pub(crate) const fn js_iterator_proto_Symbol_iterator(
    _: &mut ScopedContext,
    cc: CallContext,
) -> JSResult {
    Ok(cc.this())
}

// =========================================================================
// Field defs
// =========================================================================

pub(crate) const js_iterator_funcs: &[FieldDef] = &[
    FieldDef::FUNC("from", 1, js_iterator_from, 0),
    FieldDef::FUNC("concat", 0, js_iterator_concat, 0),
    FieldDef::FUNC("zip", 1, js_iterator_zip, 0),
    FieldDef::FUNC("zipKeyed", 1, js_iterator_zipKeyed, 0),
];

pub(crate) const js_iterator_proto_full_funcs: &[FieldDef] = &[
    FieldDef::FUNC("every", 1, js_iterator_proto_every, 0),
    FieldDef::FUNC("find", 1, js_iterator_proto_find, 0),
    FieldDef::FUNC("forEach", 1, js_iterator_proto_forEach, 0),
    FieldDef::FUNC("some", 1, js_iterator_proto_some, 0),
    FieldDef::FUNC("reduce", 1, js_iterator_proto_reduce, 0),
    FieldDef::FUNC("toArray", 0, js_iterator_proto_toArray, 0),
    FieldDef::FUNC(
        "drop",
        1,
        js_create_iterator_helper,
        JSIteratorHelperKind::Drop as i16,
    ),
    FieldDef::FUNC(
        "filter",
        1,
        js_create_iterator_helper,
        JSIteratorHelperKind::Filter as i16,
    ),
    FieldDef::FUNC(
        "flatMap",
        1,
        js_create_iterator_helper,
        JSIteratorHelperKind::FlatMap as i16,
    ),
    FieldDef::FUNC(
        "map",
        1,
        js_create_iterator_helper,
        JSIteratorHelperKind::Map as i16,
    ),
    FieldDef::FUNC(
        "take",
        1,
        js_create_iterator_helper,
        JSIteratorHelperKind::Take as i16,
    ),
];
