// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    IntOrFloat, JS_CLASS_MAP, JS_CLASS_SET, ScopedContext,
    array::Array,
    atom::Atom,
    builtins::{
        common::SPECIES_FIELD,
        iterator::{JSIteratorKindEnum, iterator_step},
        mapset::MapSet,
        weakref::can_be_held_weakly,
    },
    class::ClassID,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc::GcHeapExt,
    number::MAX_SAFE_INTEGER,
    object::{JS_PROP_CONFIGURABLE, Object, ObjectPayload},
    value::JSValue,
};

use super::{JSMapIteratorData, JSMapState, MapPair};

/// Get the internal JSMapState from a Map/Set/WeakMap/WeakSet value (throws if not the target class).
fn get_jsmap_state<'s>(
    val: &'s JSValue,
    class_id: ClassID,
    ctx: &mut ScopedContext,
) -> Result<&'s JSMapState, i32> {
    val.class_payload(ctx, class_id)
}

/// Try to get the internal JSMapState (returns None if not the target class).
fn try_get_jsmap_state(val: &JSValue, class_id: ClassID) -> Option<&JSMapState> {
    val.try_class_payload(class_id)
}

/// Get the mutable internal JSMapState from a Map/Set/WeakMap/WeakSet value (throws if not the target class).
fn get_jsmap_state_mut<'s>(
    val: &'s mut JSValue,
    class_id: ClassID,
    ctx: &mut ScopedContext,
) -> Result<&'s mut JSMapState, i32> {
    val.class_payload_mut(ctx, class_id)
}

/// Iterate over all Strong keys in a JSMapState.
fn strong_keys(state: &JSMapState) -> impl Iterator<Item = JSValue> + '_ {
    state.slots.iter().filter_map(|pair| match pair {
        MapPair::Strong { key, .. } => Some(*key),
        _ => None,
    })
}

pub(crate) fn js_map_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let is_set = magic & 1 == 1;
    let is_weak = matches!(magic, 2 | 3);

    let new_target = cc.this();

    let isa = Box::new(JSMapState::new(is_weak, 8));
    let obj = ctx
        .create_instance_with_ctor(
            new_target,
            match magic {
                0 => ObjectPayload::MAP(isa),
                1 => ObjectPayload::SET(isa),
                2 => ObjectPayload::WEAKMAP(isa),
                3 => ObjectPayload::WEAKSET(isa),
                _ => unreachable!(),
            },
        )
        .map(JSValue::from)
        .map_err(|_| -1)?;

    let arr = if cc.length() > 0 {
        cc.arg(0)
    } else {
        JSValue::UNDEFINED
    };

    if !arr.is_undefined_or_null() {
        let adder = obj.get_property(
            ctx,
            if is_set {
                Atom::CONST_add
            } else {
                Atom::CONST_set
            },
        )?;

        if !adder.is_callable() {
            return Err(ctx.throw_type_error("set/add is not a function"));
        }

        for el in arr.iter(ctx)? {
            let item = el?;
            if is_set {
                adder.call(ctx, obj, &[item])?;
            } else if let Some(o) = item.as_object() {
                let key = o.get_property_u32(ctx, 0)?;
                let value = o.get_property_u32(ctx, 1)?;
                adder.call(ctx, obj, &[key, value])?;
            } else {
                return Err(ctx.throw_type_error_not_an_object());
            }
        }
    }

    Ok(obj)
}

pub(crate) fn js_map_get(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let key = cc.arg(0);
    let this = cc.this();
    let state = get_jsmap_state(&this, JS_CLASS_MAP + cc.magic() as ClassID, ctx)?;
    Ok(state.find(ctx, key).unwrap_or(JSValue::UNDEFINED))
}

pub(crate) fn js_map_getOrInsert(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let computed = (magic & 1) == 1;
    let class_offset = magic & 0b10; // 0 for Map, 2 for WeakMap
    let class_id = JS_CLASS_MAP + class_offset as ClassID;

    let val = cc.arg(1);

    // In computed mode, the second argument must be a function
    if computed {
        val.ensure_callable(ctx)?;
    }

    // Canonicalize key (-0 → +0)  (per CanonicalizeKeyedCollectionKey)
    let key = MapPair::normalize_key(cc.arg(0));

    // For WeakMap, validate key can be held weakly before any callback invocation
    if class_offset == 2 && !can_be_held_weakly(key) {
        return Err(ctx.throw_type_error_not_an_object());
    }

    // Check if key already exists
    {
        let this = cc.this();
        let state = get_jsmap_state(&this, class_id, ctx)?;
        if let Some(existing) = state.find(ctx, key) {
            return Ok(existing);
        }
    }

    // Compute value if needed (may trigger observable side effects via callback)
    let value = if computed {
        val.call(ctx, JSValue::UNDEFINED, &[key])?
    } else {
        val
    };

    // Insert the key-value pair
    let mut this_mut = cc.this();
    let s = get_jsmap_state_mut(&mut this_mut, class_id, ctx)?;

    if s.upsert(ctx, key, value)?
        && s.is_weak()
        && let Some(mut key_obj) = key.as_object()
    {
        key_obj.add_back_ref(cc.this().as_object().unwrap().downgrade());
    }
    let _ = s;

    // Bind GC reference relations
    let this_obj = cc.this().as_object().unwrap();
    let self_node = this_obj.gc_node_ptr();
    this_obj.rt_ptr().gc_heap_mut().bind_value(self_node, &key);
    this_obj
        .rt_ptr()
        .gc_heap_mut()
        .bind_value(self_node, &value);

    Ok(value)
}

pub(crate) fn js_map_set(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();

    let mut this = cc.this();
    let key = cc.arg(0);
    let val = cc.arg(1);

    let s = get_jsmap_state_mut(&mut this, JS_CLASS_MAP + magic as ClassID, ctx)?;

    if s.is_weak() && !can_be_held_weakly(key) {
        return Err(ctx.throw_type_error_not_an_object());
    }

    let map_val = if magic & 1 == 1 {
        JSValue::UNDEFINED // set or weakset
    } else {
        val // map or weakmap
    };

    if s.upsert(ctx, key, map_val)? {
        // inserted pair: make back ref for key object if is weak (symbol
        // keys are never collected, so they need no back ref)
        if s.is_weak()
            && let Some(mut key_obj) = key.as_object()
        {
            key_obj.add_back_ref(cc.this().as_object().unwrap().downgrade());
        }
    }

    // bind reference relation for gc values
    let this_obj = cc.this().as_object().unwrap();
    let self_node = this_obj.gc_node_ptr();
    this_obj.rt_ptr().gc_heap_mut().bind_value(self_node, &key);
    this_obj
        .rt_ptr()
        .gc_heap_mut()
        .bind_value(self_node, &map_val);

    Ok(this)
}

fn js_map_delete(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();

    let mut this = cc.this();
    let key = cc.arg(0);

    let s = get_jsmap_state_mut(&mut this, JS_CLASS_MAP + magic as ClassID, ctx)?;
    if s.find(ctx, key).is_some() {
        s.remove(ctx, key);

        // only object keys have back refs (symbol keys are never collected)
        if s.is_weak()
            && let Some(mut key_obj) = key.as_object()
        {
            key_obj.remove_back_ref(ctx.rt(), this.as_object().unwrap());
        }
        Ok(JSValue::TRUE)
    } else {
        Ok(JSValue::FALSE)
    }
}

fn js_map_has(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let key = cc.arg(0);
    let this = cc.this();
    let state = get_jsmap_state(&this, JS_CLASS_MAP + cc.magic() as ClassID, ctx)?;
    Ok(state.find(ctx, key).is_some().into())
}

fn js_map_clear(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();

    let mut this = cc.this();
    let s = get_jsmap_state_mut(&mut this, JS_CLASS_MAP + magic as ClassID, ctx)?;
    s.clear();
    Ok(JSValue::UNDEFINED)
}

fn js_map_get_size(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let state = get_jsmap_state(&this, JS_CLASS_MAP + cc.magic() as ClassID, ctx)?;
    Ok(JSValue::from(state.len() as i32))
}

fn js_map_forEach(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let is_set = magic & 1 == 1;

    let func = cc.arg(0);
    func.ensure_callable(ctx)?;

    let this_arg = cc.arg(1);
    let mut this = cc.this();

    // lock map, and ensure `this` is an object of expected class_id.
    get_jsmap_state_mut(&mut this, JS_CLASS_MAP + magic as ClassID, ctx)?.lock();

    let mut this_obj = this.as_object().unwrap();
    let mut has_err = false;
    let mut i = 0;

    loop {
        // get index `i`
        let kv = this_obj.payload().mapset().unwrap().slots.get(i).cloned();

        if let Some(kv) = kv {
            let pair = match kv {
                MapPair::Strong { key, value } => Some((key, value)),
                MapPair::Weak { key, value } => key
                    .upgrade(ctx.rt().gc_heap())
                    .map(|k| (JSValue::from(Object(k.into_raw())), value)),
                MapPair::Removed => None,
            };
            if let Some((key, val)) = pair
                && func
                    .call(
                        ctx,
                        this_arg,
                        &if is_set {
                            [key, key, this]
                        } else {
                            [val, key, this]
                        },
                    )
                    .is_err()
            {
                has_err = true;
                break;
            }
            i += 1;
        } else {
            // end of slot
            break;
        }
    }

    // unlock map
    this_obj.payload_mut().mapset_mut().unwrap().unlock();

    if has_err {
        Err(-1)
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub(crate) fn js_create_map_iterator(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    debug_assert!(matches!(cc.magic() >> 2, 0..=2));

    let kind = unsafe { std::mem::transmute::<u8, JSIteratorKindEnum>(cc.magic() as u8 >> 2) };

    // ensure `this` is an object of expected class_id
    let this = cc.this();
    let _: &JSMapState = this.class_payload(ctx, JS_CLASS_MAP + (cc.magic() & 3) as ClassID)?;

    // create iterator object
    let isa = Box::new(JSMapIteratorData {
        obj: this.as_object(),
        kind,
        index: 0,
    });
    ctx.create_instance_with_payload(match cc.magic() & 1 {
        0 => ObjectPayload::MAP_ITERATOR(isa),
        1 => ObjectPayload::SET_ITERATOR(isa),
        _ => unreachable!(),
    })
    .map_err(|_| -1)
    .map(JSValue::from)
}

pub const js_map_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("set", 2, js_map_set, 0),
    FieldDef::FUNC("get", 1, js_map_get, 0),
    FieldDef::FUNC("getOrInsert", 2, js_map_getOrInsert, 0),
    FieldDef::FUNC("getOrInsertComputed", 2, js_map_getOrInsert, 1),
    FieldDef::FUNC("has", 1, js_map_has, 0),
    FieldDef::FUNC("delete", 1, js_map_delete, 0),
    FieldDef::FUNC("clear", 0, js_map_clear, 0),
    FieldDef::GETSET("size", Some(js_map_get_size), None, 0),
    FieldDef::FUNC("forEach", 1, js_map_forEach, 0),
    FieldDef::FUNC(
        "values",
        0,
        js_create_map_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) << 2,
    ),
    FieldDef::FUNC(
        "keys",
        0,
        js_create_map_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16) << 2,
    ),
    FieldDef::FUNC(
        "entries",
        0,
        js_create_map_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16) << 2,
    ),
    FieldDef::ALIAS("[Symbol.iterator]", "entries"),
    FieldDef::STR("[Symbol.toStringTag]", "Map", JS_PROP_CONFIGURABLE),
];

pub const js_set_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("add", 1, js_map_set, 1),
    FieldDef::FUNC("has", 1, js_map_has, 1),
    FieldDef::FUNC("delete", 1, js_map_delete, 1),
    FieldDef::FUNC("clear", 0, js_map_clear, 1),
    FieldDef::GETSET("size", Some(js_map_get_size), None, 1),
    FieldDef::FUNC("forEach", 1, js_map_forEach, 1),
    FieldDef::FUNC(
        "values",
        0,
        js_create_map_iterator,
        ((JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) << 2) | 1,
    ),
    FieldDef::ALIAS("keys", "values"),
    FieldDef::ALIAS("[Symbol.iterator]", "values"),
    FieldDef::FUNC(
        "entries",
        0,
        js_create_map_iterator,
        ((JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16) << 2) | 1,
    ),
    FieldDef::FUNC("isDisjointFrom", 1, js_set_isDisjointFrom, 0),
    FieldDef::FUNC("isSubsetOf", 1, js_set_isSubsetOf, 0),
    FieldDef::FUNC("isSupersetOf", 1, js_set_isSupersetOf, 0),
    FieldDef::FUNC("intersection", 1, js_set_intersection, 0),
    FieldDef::FUNC("difference", 1, js_set_difference, 0),
    FieldDef::FUNC("symmetricDifference", 1, js_set_symmetricDifference, 0),
    FieldDef::FUNC("union", 1, js_set_union, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Set", JS_PROP_CONFIGURABLE),
];

pub const js_weakmap_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("set", 2, js_map_set, 2),
    FieldDef::FUNC("get", 1, js_map_get, 2),
    FieldDef::FUNC("getOrInsert", 2, js_map_getOrInsert, 2),
    FieldDef::FUNC("getOrInsertComputed", 2, js_map_getOrInsert, 3),
    FieldDef::FUNC("has", 1, js_map_has, 2),
    FieldDef::FUNC("delete", 1, js_map_delete, 2),
    FieldDef::STR("[Symbol.toStringTag]", "WeakMap", JS_PROP_CONFIGURABLE),
];

pub const js_weakset_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("add", 1, js_map_set, 3),
    FieldDef::FUNC("has", 1, js_map_has, 3),
    FieldDef::FUNC("delete", 1, js_map_delete, 3),
    FieldDef::STR("[Symbol.toStringTag]", "WeakSet", JS_PROP_CONFIGURABLE),
];

/// SetRecord: holds the validated GetSetRecord result.
struct SetRecord {
    size: f64,
    has: Object,
    keys: Object,
}

/// GetSetRecord: extracts size (ToNumber'd), has method, and keys method.
/// Throws TypeError if `has` is missing/not-callable, or `keys` is undefined/null/not-callable.
fn get_set_record(ctx: &mut ScopedContext, val: JSValue) -> Result<SetRecord, i32> {
    // 1. Get size → ToNumber(size)
    let size_str = Atom::from_str(ctx, "size")?;
    let size_val = val.get_property(ctx, size_str)?;
    let size = match size_val.to_number(ctx)? {
        IntOrFloat::Int64(i) => i as f64,
        IntOrFloat::Float64(f) => f,
    };

    // 2. Get has (GetMethod: required, throws if missing or not callable)
    let has_val = val.get_property(ctx, Atom::CONST_has)?;
    let has_obj = match has_val.ensure_callable(ctx) {
        Ok(o) => o,
        Err(_) => {
            return Err(ctx.throw_type_error("Set.prototype.has is not a function"));
        }
    };

    // 3. Get keys via GetMethod: undefined/null → undefined, not-callable → throw
    let keys_str = Atom::from_str(ctx, "keys")?;
    let keys_val = val.get_property(ctx, keys_str)?;
    // GetMethod returns undefined for null/undefined
    if keys_val.is_undefined_or_null() {
        return Err(ctx.throw_type_error("Set.prototype.keys is not a function"));
    }
    let keys_obj = match keys_val.ensure_callable(ctx) {
        Ok(o) => o,
        Err(_) => {
            return Err(ctx.throw_type_error("Set.prototype.keys is not a function"));
        }
    };

    Ok(SetRecord {
        size,
        has: has_obj,
        keys: keys_obj,
    })
}

/// create an empty Set instance with the given capacity.
fn create_empty_set(ctx: &mut ScopedContext, capacity: usize) -> Result<JSValue, i32> {
    let state = Box::new(JSMapState::new(false, capacity));
    ctx.create_instance_with_payload(ObjectPayload::SET(state))
        .map_err(|_| -1)
        .map(JSValue::from)
}

/// Call iterator.return() if it exists.
fn iterator_return(ctx: &mut ScopedContext, iterator: JSValue) -> Result<(), i32> {
    let return_str = Atom::from_str(ctx, "return")?;
    let return_val = iterator.get_property(ctx, return_str)?;
    if return_val.is_callable() {
        let return_obj = return_val.as_object().unwrap();
        return_obj.call(ctx, iterator, &[])?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Set.prototype methods (set-methods proposal)
// ---------------------------------------------------------------------------

/// Set.prototype.isDisjointFrom(other)
fn js_set_isDisjointFrom(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let other = cc.arg(0);

    let this_state = get_jsmap_state(&this, JS_CLASS_SET, ctx)?;

    // Fast path: other is an internal Set
    if let Some(other_state) = try_get_jsmap_state(&other, JS_CLASS_SET) {
        // iterate over the smaller set for efficiency
        let (iter_set, check_set) = if this_state.len() <= other_state.len() {
            (this_state, other_state)
        } else {
            (other_state, this_state)
        };
        for key in strong_keys(iter_set) {
            if check_set.find(ctx, key).is_some() {
                return Ok(JSValue::from(false));
            }
        }
        return Ok(JSValue::from(true));
    }

    // Slow path (GetSetRecord)
    let rec = get_set_record(ctx, other)?;

    let this_len = this_state.len() as f64;
    if this_len > rec.size {
        // Strategy: iterate other's keys, check this.has (internal)
        let iterator = rec.keys.call(ctx, other, &[])?;
        let next_method = iterator.get_property(ctx, Atom::CONST_next)?;
        let next_obj = next_method.ensure_callable(ctx)?;

        while let Some(value) = iterator_step(ctx, iterator, next_obj)? {
            if this_state.find(ctx, value).is_some() {
                iterator_return(ctx, iterator)?;
                return Ok(JSValue::from(false));
            }
        }

        return Ok(JSValue::TRUE);
    }

    // Strategy: iterate this, check other.has
    for key in strong_keys(this_state) {
        let found = rec.has.call(ctx, other, &[key])?;
        if found.to_bool()? {
            return Ok(JSValue::FALSE);
        }
    }
    Ok(JSValue::TRUE)
}

/// Set.prototype.isSubsetOf(other)
fn js_set_isSubsetOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let other = cc.arg(0);

    let this_state = get_jsmap_state(&this, JS_CLASS_SET, ctx)?;

    // Fast path: other is an internal Set
    if let Some(other_state) = try_get_jsmap_state(&other, JS_CLASS_SET) {
        if this_state.len() > other_state.len() {
            return Ok(JSValue::FALSE);
        }
        for key in strong_keys(this_state) {
            if other_state.find(ctx, key).is_none() {
                return Ok(JSValue::FALSE);
            }
        }
        return Ok(JSValue::TRUE);
    }

    // Slow path (GetSetRecord)
    let rec = get_set_record(ctx, other)?;

    // Optimization: if this is larger than other, it can't be a subset
    if (this_state.len() as f64) > rec.size {
        return Ok(JSValue::FALSE);
    }

    // Iterate this, check other.has
    for key in strong_keys(this_state) {
        let found = rec.has.call(ctx, other, &[key])?;
        if !found.to_bool()? {
            return Ok(JSValue::FALSE);
        }
    }
    Ok(JSValue::TRUE)
}

/// Set.prototype.isSupersetOf(other)
fn js_set_isSupersetOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let other = cc.arg(0);

    let this_state = get_jsmap_state(&this, JS_CLASS_SET, ctx)?;

    // Fast path: other is an internal Set
    if let Some(other_state) = try_get_jsmap_state(&other, JS_CLASS_SET) {
        if other_state.len() > this_state.len() {
            return Ok(JSValue::FALSE);
        }
        for key in strong_keys(other_state) {
            if this_state.find(ctx, key).is_none() {
                return Ok(JSValue::FALSE);
            }
        }
        return Ok(JSValue::TRUE);
    }

    // Slow path (GetSetRecord)
    let rec = get_set_record(ctx, other)?;

    // Optimization: if other is larger, this can't be a superset
    if rec.size > this_state.len() as f64 {
        return Ok(JSValue::FALSE);
    }

    // Iterate other's keys, check this.has (internal)
    let iterator = rec.keys.call(ctx, other, &[])?;
    let next_method = iterator.get_property(ctx, Atom::CONST_next)?;
    let next_obj = next_method.ensure_callable(ctx)?;

    while let Some(value) = iterator_step(ctx, iterator, next_obj)? {
        if this_state.find(ctx, value).is_none() {
            iterator_return(ctx, iterator)?;
            return Ok(JSValue::FALSE);
        }
    }

    Ok(JSValue::TRUE)
}

/// Set.prototype.intersection(other)
fn js_set_intersection(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let other = cc.arg(0);

    let this_state = get_jsmap_state(&this, JS_CLASS_SET, ctx)?;

    let mut result = create_empty_set(ctx, this_state.len())?;
    let result_state = get_jsmap_state_mut(&mut result, JS_CLASS_SET, ctx)?;

    // Fast path: other is an internal Set
    if let Some(other_state) = try_get_jsmap_state(&other, JS_CLASS_SET) {
        let (iter_set, check_set) = if this_state.len() <= other_state.len() {
            (this_state, other_state)
        } else {
            (other_state, this_state)
        };
        for key in strong_keys(iter_set) {
            if check_set.find(ctx, key).is_some() {
                result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
            }
        }
        return Ok(result);
    }

    // Slow path (GetSetRecord)
    let rec = get_set_record(ctx, other)?;

    let this_len = this_state.len() as f64;
    if this_len > rec.size {
        // Strategy: iterate other's keys, check this.has (internal) → add to result
        let iterator = rec.keys.call(ctx, other, &[])?;
        let next_method = iterator.get_property(ctx, Atom::CONST_next)?;
        let next_obj = next_method.ensure_callable(ctx)?;

        while let Some(value) = iterator_step(ctx, iterator, next_obj)? {
            if this_state.find(ctx, value).is_some() {
                result_state.upsert(ctx, value, JSValue::UNDEFINED)?;
            }
        }

        return Ok(result);
    }

    // Strategy: iterate this, check other.has → add to result
    for key in strong_keys(this_state) {
        let found = rec.has.call(ctx, other, &[key])?;
        if found.to_bool()? {
            result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
        }
    }

    Ok(result)
}

/// Set.prototype.difference(other)
fn js_set_difference(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let other = cc.arg(0);

    let this_state = get_jsmap_state(&this, JS_CLASS_SET, ctx)?;

    let mut result = create_empty_set(ctx, this_state.len())?;
    let result_state = get_jsmap_state_mut(&mut result, JS_CLASS_SET, ctx)?;

    // Fast path: other is an internal Set
    if let Some(other_state) = try_get_jsmap_state(&other, JS_CLASS_SET) {
        for key in strong_keys(this_state) {
            if other_state.find(ctx, key).is_none() {
                result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
            }
        }
        return Ok(result);
    }

    // Slow path (GetSetRecord)
    let rec = get_set_record(ctx, other)?;

    let this_len = this_state.len() as f64;
    if this_len > rec.size {
        // Strategy: copy this to result, iterate other's keys, remove matching
        for key in strong_keys(this_state) {
            result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
        }

        let iterator = rec.keys.call(ctx, other, &[])?;
        let next_method = iterator.get_property(ctx, Atom::CONST_next)?;
        let next_obj = next_method.ensure_callable(ctx)?;

        while let Some(value) = iterator_step(ctx, iterator, next_obj)? {
            if this_state.find(ctx, value).is_some() {
                result_state.remove(ctx, value);
            }
        }
        return Ok(result);
    }

    // Strategy: iterate this, check !other.has → add to result
    for key in strong_keys(this_state) {
        let found = rec.has.call(ctx, other, &[key])?;
        if !found.to_bool()? {
            result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
        }
    }

    Ok(result)
}

/// Set.prototype.symmetricDifference(other)
fn js_set_symmetricDifference(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let other = cc.arg(0);

    let this_state = get_jsmap_state(&this, JS_CLASS_SET, ctx)?;

    let mut result = create_empty_set(ctx, this_state.len() + 8)?;
    let result_state = get_jsmap_state_mut(&mut result, JS_CLASS_SET, ctx)?;

    // Fast path: other is an internal Set
    if let Some(other_state) = try_get_jsmap_state(&other, JS_CLASS_SET) {
        for key in strong_keys(this_state) {
            if other_state.find(ctx, key).is_none() {
                result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
            }
        }
        for key in strong_keys(other_state) {
            if this_state.find(ctx, key).is_none() {
                result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
            }
        }
        return Ok(result);
    }

    // Slow path: QuickJS copy-this-first algorithm
    let rec = get_set_record(ctx, other)?;

    // Copy this to result (preserves insertion order)
    for key in strong_keys(this_state) {
        result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
    }

    // Iterate other's keys, remove from/add to result
    let iterator = rec.keys.call(ctx, other, &[])?;
    let next_method = iterator.get_property(ctx, Atom::CONST_next)?;
    let next_obj = next_method.ensure_callable(ctx)?;

    loop {
        let Some(value) = iterator_step(ctx, iterator, next_obj)? else {
            break;
        };
        if this_state.find(ctx, value).is_some() {
            result_state.remove(ctx, value);
        } else {
            result_state.upsert(ctx, value, JSValue::UNDEFINED)?;
        }
    }

    Ok(result)
}

/// Set.prototype.union(other)
fn js_set_union(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let other = cc.arg(0);

    let this_state = get_jsmap_state(&this, JS_CLASS_SET, ctx)?;

    let mut result = create_empty_set(ctx, this_state.len() + 8)?;
    let result_state = get_jsmap_state_mut(&mut result, JS_CLASS_SET, ctx)?;

    // Fast path: other is an internal Set
    if let Some(other_state) = try_get_jsmap_state(&other, JS_CLASS_SET) {
        for key in strong_keys(this_state) {
            result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
        }
        for key in strong_keys(other_state) {
            result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
        }
        return Ok(result);
    }

    // Slow path:
    // 1. GetSetRecord first — may trigger side effects that mutate this_state
    let rec = get_set_record(ctx, other)?;

    // 2. Copy all elements from this Set (after any mutations from step 1)
    for key in strong_keys(this_state) {
        result_state.upsert(ctx, key, JSValue::UNDEFINED)?;
    }

    // 3. Iterate other's keys and add directly
    let iterator = rec.keys.call(ctx, other, &[])?;
    let next_method = iterator.get_property(ctx, Atom::CONST_next)?;
    let next_obj = next_method.ensure_callable(ctx)?;

    loop {
        let Some(value) = iterator_step(ctx, iterator, next_obj)? else {
            break;
        };
        result_state.upsert(ctx, value, JSValue::UNDEFINED)?;
    }

    Ok(result)
}

fn js_map_groupBy(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // check must be observed before argv[0] is accessed

    let func = cc.arg(1);
    func.ensure_callable(ctx)?;

    let this_val = if ctx.rt().is_strict_mode() {
        JSValue::UNDEFINED
    } else {
        ctx.global_obj().into()
    };

    let mut groups = Vec::<(JSValue, Vec<JSValue>)>::with_capacity(16);

    let obj = cc.arg(0);
    let it = obj.iter(ctx)?;

    for (idx, val) in it.enumerate() {
        if idx as i64 >= MAX_SAFE_INTEGER {
            return Err(ctx.throw_type_error("too many elements"));
        }

        let val = val?;
        let key = func.call(ctx, this_val, &[val, JSValue::from(idx as u32)])?;
        let key = MapPair::normalize_key(key);

        if let Some(i) = groups.iter().position(|(k, _)| k == &key) {
            groups[i].1.push(val);
        } else {
            let mut lst = Vec::<JSValue>::with_capacity(8);
            lst.push(val);
            groups.push((key, lst));
        }
    }

    let mut ms = JSMapState::new(false, groups.len());
    for (key, mut lst) in groups {
        lst.shrink_to_fit();
        let arr = Array::from_values(ctx, lst)?;
        ms.upsert(ctx, key, arr.into())?;
    }

    MapSet::from_entries(ctx, ms).map(JSValue::from)
}

pub const js_map_funcs: &[FieldDef] = &[
    FieldDef::FUNC("groupBy", 2, js_map_groupBy, 0),
    SPECIES_FIELD,
];

pub const js_set_funcs: &[FieldDef] = &[SPECIES_FIELD];
