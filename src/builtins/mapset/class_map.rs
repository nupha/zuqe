use crate::{
    JS_CLASS_MAP,
    atom::Atom,
    builtins::{iterator::JSIteratorKindEnum, js_get_this},
    class::ClassID,
    error::JSResult,
    function::{CallContext, FieldDef},
    number::MAX_SAFE_INTEGER,
    object::{JS_PROP_CONFIGURABLE, Object, ObjectPayload},
    value::JSValue,
};

use super::{JSMapIteratorData, JSMapState, MapPair};

pub fn js_map_constructor(cc: CallContext) -> JSResult {
    let cx = cc.ctx();
    let magic = cc.magic();
    let is_set = magic & 1 == 1;
    let is_weak = matches!(magic, 2 | 3);

    let new_target = *cc.this();

    let isa = Box::new(JSMapState::new(is_weak, 8));
    let obj = cx
        .create_object_from_ctor(
            new_target,
            match magic {
                0 => ObjectPayload::MAP(isa),
                1 => ObjectPayload::SET(isa),
                2 => ObjectPayload::WEAKMAP(isa),
                3 => ObjectPayload::WEAKSET(isa),
                _ => unreachable!(),
            },
        )
        .map(JSValue::Object)
        .map_err(|_| -1)?;

    let arr = if cc.length() > 0 {
        cc.arg(0)
    } else {
        &JSValue::UNDEFINED
    };

    if !arr.is_undefined_or_null() {
        let adder = obj.get_property(
            cx,
            if is_set {
                Atom::CONST_add
            } else {
                Atom::CONST_set
            },
        )?;

        if !adder.is_function() {
            cx.throw_type_error("set/add is not a function");
            return Err(-1);
        }

        for el in arr.iter(cx)? {
            let item = el?;
            if is_set {
                adder.call(cx, obj, &[item])?;
            } else if let Some(o) = item.as_object() {
                let key = o.get_property_u32(cx, 0)?;
                let value = o.get_property_u32(cx, 1)?;
                adder.call(cx, obj, &[key, value])?;
            } else {
                cx.throw_type_error_not_an_object();
                return Err(-1);
            }
        }
    }

    Ok(obj)
}

pub(crate) fn js_map_get(cc: CallContext) -> JSResult {
    let key = *cc.arg(0);
    let ctx = cc.ctx();

    cc.this()
        .retrive_object_payload(ctx, JS_CLASS_MAP + cc.magic() as ClassID)
        .map(|r| r.mapset().unwrap().find(key).unwrap_or(JSValue::UNDEFINED))
}

pub(crate) fn js_map_set(cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let ctx = cc.ctx();

    let mut this = *cc.this();
    let mut key = *cc.arg(0);
    let val = *cc.arg(1);

    let s = this
        .retrive_object_payload_mut(ctx, JS_CLASS_MAP + magic as ClassID)?
        .mapset_mut()
        .unwrap();

    if s.is_weak() && !key.is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    if s.upsert(
        ctx,
        key,
        if magic & 1 == 1 {
            JSValue::UNDEFINED // set or weakset
        } else {
            val // map or weakmap
        },
    )? {
        // inserted pair
        // make backref for key object if is weak
        if s.is_weak() {
            key.as_object_mut()
                .unwrap()
                .weak_refs
                .push(cc.this().as_object().unwrap().downgrade());
        }
    }

    Ok(this)
}

fn js_map_delete(cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let ctx = cc.ctx();

    let this = *cc.this();
    let mut key = *cc.arg(0);

    let mut this2 = this;
    this2
        .retrive_object_payload_mut(ctx, JS_CLASS_MAP + magic as ClassID)
        .map(|isa| {
            let s = isa.mapset_mut().unwrap();

            if s.find(key).is_some() {
                s.remove(ctx, key);

                if s.is_weak() {
                    let this_obj = this.as_object().unwrap();
                    let key_obj = key.as_object_mut().unwrap();

                    key_obj.weak_refs.retain(|w| {
                        w.upgrade(ctx.rt_ptr().gc_heap())
                            .is_some_and(|o| !Object::from(o).eq(this_obj))
                    });
                }

                JSValue::Bool(true)
            } else {
                JSValue::Bool(false)
            }
        })
}

fn js_map_has(cc: CallContext) -> JSResult {
    let key = *cc.arg(0);
    cc.this()
        .retrive_object_payload(cc.ctx(), JS_CLASS_MAP + cc.magic() as ClassID)
        .map(|r| r.mapset().unwrap().find(key).is_some().into())
}

fn js_map_clear(cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let mut this = *cc.this();

    this.retrive_object_payload_mut(cc.ctx(), JS_CLASS_MAP + magic as ClassID)
        .map(|isa| {
            isa.mapset_mut().unwrap().clear();
            JSValue::UNDEFINED
        })
}

fn js_map_get_size(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    cc.this()
        .retrive_object_payload(ctx, JS_CLASS_MAP + cc.magic() as ClassID)
        .map(|r| JSValue::Int(r.mapset().unwrap().len() as i32))
}

fn js_map_forEach(cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let is_set = magic & 1 == 1;
    let ctx = cc.ctx();

    let func = *cc.arg(0);
    func.ensure_function(ctx)?;

    let this_arg = *cc.arg(1);
    let mut this = *cc.this();

    // lock map, and ensure `this` is an object of expected class_id.
    this.retrive_object_payload_mut(ctx, JS_CLASS_MAP + magic as ClassID)?
        .mapset_mut()
        .unwrap()
        .lock();

    let mut this_obj = *this.as_object().unwrap();
    let mut has_err = false;
    let mut i = 0;

    loop {
        // get index `i`
        let kv = this_obj.payload().mapset().unwrap().slots.get(i).cloned();

        if let Some(kv) = kv {
            let pair = match kv {
                MapPair::Strong { key, value } => Some((key, value)),
                MapPair::Weak { key, value } => key
                    .upgrade(ctx.rt_ptr().gc_heap())
                    .map(|k| (JSValue::from(k), value)),
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

fn js_create_map_iterator(cc: CallContext) -> JSResult {
    debug_assert!(matches!(cc.magic() >> 2, 0..=2));

    let kind = unsafe { std::mem::transmute::<u8, JSIteratorKindEnum>(cc.magic() as u8 >> 2) };
    let ctx = cc.ctx();

    // ensure `this` is an object of expected class_id
    let this = *cc.this();
    let _ = this.retrive_object_payload(ctx, JS_CLASS_MAP + (cc.magic() & 3) as ClassID)?;

    // create iterator object
    let isa = Box::new(JSMapIteratorData {
        obj: this.as_object().copied(),
        kind,
        index: 0,
    });
    ctx.create_object(match cc.magic() & 1 {
        0 => ObjectPayload::MAP_ITERATOR(isa),
        1 => ObjectPayload::SET_ITERATOR(isa),
        _ => unreachable!(),
    })
    .map_err(|_| -1)
    .map(JSValue::Object)
}

pub const js_map_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("set", 2, js_map_set, 0),
    FieldDef::CALL("get", 1, js_map_get, 0),
    FieldDef::CALL("has", 1, js_map_has, 0),
    FieldDef::CALL("delete", 1, js_map_delete, 0),
    FieldDef::CALL("clear", 0, js_map_clear, 0),
    FieldDef::GETSET("size", Some(js_map_get_size), None, 0),
    FieldDef::CALL("forEach", 1, js_map_forEach, 0),
    FieldDef::CALL(
        "values",
        0,
        js_create_map_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) << 2,
    ),
    FieldDef::CALL(
        "keys",
        0,
        js_create_map_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16) << 2,
    ),
    FieldDef::CALL(
        "entries",
        0,
        js_create_map_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16) << 2,
    ),
    FieldDef::ALIAS("[Symbol.iterator]", "entries"),
    FieldDef::STR("[Symbol.toStringTag]", "Map", JS_PROP_CONFIGURABLE),
];

pub const js_set_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("add", 1, js_map_set, 1),
    FieldDef::CALL("has", 1, js_map_has, 1),
    FieldDef::CALL("delete", 1, js_map_delete, 1),
    FieldDef::CALL("clear", 0, js_map_clear, 1),
    FieldDef::GETSET("size", Some(js_map_get_size), None, 1),
    FieldDef::CALL("forEach", 1, js_map_forEach, 1),
    FieldDef::CALL(
        "values",
        0,
        js_create_map_iterator,
        ((JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) << 2) | 1,
    ),
    FieldDef::ALIAS("keys", "values"),
    FieldDef::ALIAS("[Symbol.iterator]", "values"),
    FieldDef::CALL(
        "entries",
        0,
        js_create_map_iterator,
        ((JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16) << 2) | 1,
    ),
    FieldDef::STR("[Symbol.toStringTag]", "Set", JS_PROP_CONFIGURABLE),
];

pub const js_weakmap_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("set", 2, js_map_set, 2),
    FieldDef::CALL("get", 1, js_map_get, 2),
    FieldDef::CALL("has", 1, js_map_has, 2),
    FieldDef::CALL("delete", 1, js_map_delete, 2),
    FieldDef::STR("[Symbol.toStringTag]", "WeakMap", JS_PROP_CONFIGURABLE),
];

pub const js_weakset_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("add", 1, js_map_set, 3),
    FieldDef::CALL("has", 1, js_map_has, 3),
    FieldDef::CALL("delete", 1, js_map_delete, 3),
    FieldDef::STR("[Symbol.toStringTag]", "WeakSet", JS_PROP_CONFIGURABLE),
];

fn js_map_groupBy(cc: CallContext) -> JSResult {
    // check must be observed before argv[0] is accessed
    let cx = cc.ctx();

    let func = cc.arg(1);
    func.ensure_function(cx)?;

    let this_val = if cx.is_strict_mode() {
        JSValue::UNDEFINED
    } else {
        cx.global_obj().into()
    };

    let mut groups = Vec::<(JSValue, Vec<JSValue>)>::with_capacity(16);

    for (idx, val) in cc.arg(0).iter(cx)?.enumerate() {
        if idx as i64 >= MAX_SAFE_INTEGER {
            cx.throw_type_error("too many elements");
            return Err(-1);
        }

        let val = val?;
        let key = func.call(cx, this_val, &[val, JSValue::from(idx as u32)])?;
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
        let arr = cx.create_array_with_values(lst)?;
        ms.upsert(cx, key, arr.into())?;
    }

    cx.create_map_with_entries(ms).map(JSValue::Object)
}

pub const js_map_funcs: &[FieldDef] = &[
    FieldDef::CALL("groupBy", 2, js_map_groupBy, 0),
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0),
];

pub const js_set_funcs: &[FieldDef] = &[FieldDef::GETSET(
    "[Symbol.species]",
    Some(js_get_this),
    None,
    0,
)];
