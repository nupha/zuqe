use std::cell::RefCell;

use crate::{
    JS_ALIAS_DEF, JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_MAP, JS_PROP_STRING_DEF,
    atom::consts::{JS_ATOM_add, JS_ATOM_set},
    class::JSClassID,
    context::{JSContext, is_strict_mode},
    error::JSResult,
    function::{CallContext, FieldDef},
    iterator::JSIteratorKindEnum,
    js_core::js_get_this,
    mapset::{JSMapState, MapPair},
    number::MAX_SAFE_INTEGER,
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::iter::JSMapIteratorData;

pub fn js_map_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let is_set = magic & 1 == 1;
    let is_weak = matches!(magic, 2 | 3);
    let new_target = cc.this();

    let isa = RefCell::new(Box::new(JSMapState::new(is_weak, 16)));
    let obj = ctx
        .create_object_apply_ctor(
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
        let adder = obj.get_property(ctx, if is_set { JS_ATOM_add } else { JS_ATOM_set })?;
        if !adder.is_function() {
            ctx.throw_type_error("set/add is not a function");
            return Err(-1);
        }

        for el in arr.iter(ctx)? {
            let item = el?;
            if is_set {
                adder.call(ctx, &obj, &[item])?;
            } else {
                if let Some(o) = item.as_object() {
                    let key = o.get_property_u32(ctx, 0)?;
                    let value = o.get_property_u32(ctx, 1)?;
                    adder.call(ctx, &obj, &[key, value])?;
                } else {
                    ctx.throw_type_error_not_an_object();
                    return Err(-1);
                }
            }
        }
    }

    Ok(obj)
}

pub fn js_map_get(ctx: &JSContext, cc: CallContext) -> JSResult {
    let key = cc.arg(0);

    cc.this()
        .retrive_object_payload(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))
        .map(|r| {
            r.map_state()
                .unwrap()
                .find(key)
                .unwrap_or(JSValue::UNDEFINED)
        })
}

pub fn js_map_set(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let key = cc.arg(0);

    let isa = this.retrive_object_payload(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))?;
    let mut s = isa.map_state_mut().unwrap();

    if s.is_weak() && !key.is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    if s.upsert(
        ctx,
        key.clone(),
        if cc.magic() & 1 == 1 {
            JSValue::UNDEFINED // set or weakset
        } else {
            cc.arg(1).clone() // map or weakmap
        },
    )? {
        // inserted pair
        // make backref for key object if is weak
        if s.is_weak() {
            key.as_object()
                .unwrap()
                .0
                .weak_refs
                .borrow_mut()
                .push(this.as_object().unwrap().downgrade());
        }
    }

    Ok(this.clone())
}

pub fn js_map_delete(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let key = cc.arg(0);

    this.retrive_object_payload(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))
        .map(|isa| {
            let mut s = isa.map_state_mut().unwrap();
            if s.find(key).is_some() {
                s.remove(key);

                if s.is_weak() {
                    let this_obj = this.as_object().unwrap();
                    let kobj = key.as_object().unwrap();

                    kobj.0
                        .weak_refs
                        .borrow_mut()
                        .retain(|w| w.upgrade().map_or(false, |o| !o.eq(this_obj)));
                }

                JSValue::Bool(true)
            } else {
                JSValue::Bool(false)
            }
        })
}

pub fn js_map_has(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this()
        .retrive_object_payload(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))
        .map(|r| r.map_state().unwrap().find(cc.arg(0)).is_some().into())
}

pub fn js_map_clear(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this()
        .retrive_object_payload(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))
        .map(|isa| {
            isa.map_state_mut().unwrap().clear();
            JSValue::UNDEFINED
        })
}

pub fn js_map_get_size(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this()
        .retrive_object_payload(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))
        .map(|r| JSValue::Int(r.map_state().unwrap().len() as i32))
}

pub fn js_map_forEach(ctx: &JSContext, cc: CallContext) -> JSResult {
    let is_set = cc.magic() & 1 == 1;

    let func = cc.arg(0);
    func.ensure_function(ctx)?;
    let this_arg = cc.arg(1);

    // lock map, and ensure `this` is an object of expected class_id.
    cc.this()
        .retrive_object_payload(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))?
        .map_state_mut()
        .unwrap()
        .lock();

    let this = cc.this();
    let mut has_err = false;
    let map_obj = this.as_object().unwrap();
    let mut i = 0;

    loop {
        // get index `i`
        let kv = map_obj
            .0
            .payload
            .borrow()
            .map_state()
            .unwrap()
            .slots
            .get(i)
            .cloned();

        if let Some(kv) = kv {
            let pair = match kv {
                MapPair::Strong { key, value } => Some((key, value)),
                MapPair::Weak { key, value } => key.upgrade().map(|k| (JSValue::Object(k), value)),
                MapPair::Removed => None,
            };
            if let Some((key, val)) = pair {
                if func
                    .call(
                        ctx,
                        this_arg,
                        &if is_set {
                            [key.clone(), key, this.clone()]
                        } else {
                            [val, key, this.clone()]
                        },
                    )
                    .is_err()
                {
                    has_err = true;
                    break;
                }
            }
            i += 1;
        } else {
            // end of slot
            break;
        }
    }

    // unlock map
    map_obj
        .0
        .payload
        .borrow_mut()
        .map_state_mut()
        .unwrap()
        .unlock();

    if has_err {
        Err(-1)
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

fn js_create_map_iterator(ctx: &JSContext, cc: CallContext) -> JSResult {
    debug_assert!(matches!(cc.magic() >> 2, 0 | 1 | 2));
    let kind = unsafe { std::mem::transmute::<u8, JSIteratorKindEnum>(cc.magic() as u8 >> 2) };

    // ensure `this` is an object of expected class_id
    let this = cc.this();
    let _ = this.retrive_object_payload(ctx, Some(JS_CLASS_MAP + (cc.magic() & 3) as JSClassID))?;

    // create iterator object
    let isa = RefCell::new(Box::new(JSMapIteratorData {
        obj: this.as_object().cloned(),
        kind,
        index: 0,
    }));
    ctx.create_object(match cc.magic() & 1 {
        0 => ObjectPayload::MAP_ITERATOR(isa),
        1 => ObjectPayload::SET_ITERATOR(isa),
        _ => unreachable!(),
    })
    .map_err(|_| -1)
    .map(JSValue::Object)
}

pub static js_map_proto_funcs: [FieldDef; 12] = [
    JS_CFUNC_NATIVE_DEF!("set", 2, js_map_set, 0),
    JS_CFUNC_NATIVE_DEF!("get", 1, js_map_get, 0),
    JS_CFUNC_NATIVE_DEF!("has", 1, js_map_has, 0),
    JS_CFUNC_NATIVE_DEF!("delete", 1, js_map_delete, 0),
    JS_CFUNC_NATIVE_DEF!("clear", 0, js_map_clear, 0),
    JS_CGETSET_NATIVE_DEF!("size", js_map_get_size, NONE, 0),
    JS_CFUNC_NATIVE_DEF!("forEach", 1, js_map_forEach, 0),
    JS_CFUNC_NATIVE_DEF!(
        "values",
        0,
        js_create_map_iterator,
        ((JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) << 2) | 0
    ),
    JS_CFUNC_NATIVE_DEF!(
        "keys",
        0,
        js_create_map_iterator,
        ((JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16) << 2) | 0
    ),
    JS_CFUNC_NATIVE_DEF!(
        "entries",
        0,
        js_create_map_iterator,
        ((JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16) << 2) | 0
    ),
    JS_ALIAS_DEF!("[Symbol.iterator]", "entries"),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "Map", JS_PROP_CONFIGURABLE as i16),
];

pub static js_set_proto_funcs: [FieldDef; 11] = [
    JS_CFUNC_NATIVE_DEF!("add", 1, js_map_set, 1),
    JS_CFUNC_NATIVE_DEF!("has", 1, js_map_has, 1),
    JS_CFUNC_NATIVE_DEF!("delete", 1, js_map_delete, 1),
    JS_CFUNC_NATIVE_DEF!("clear", 0, js_map_clear, 1),
    JS_CGETSET_NATIVE_DEF!("size", js_map_get_size, NONE, 1),
    JS_CFUNC_NATIVE_DEF!("forEach", 1, js_map_forEach, 1),
    JS_CFUNC_NATIVE_DEF!(
        "values",
        0,
        js_create_map_iterator,
        ((JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) << 2) | 1
    ),
    JS_ALIAS_DEF!("keys", "values"),
    JS_ALIAS_DEF!("[Symbol.iterator]", "values"),
    JS_CFUNC_NATIVE_DEF!(
        "entries",
        0,
        js_create_map_iterator,
        ((JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16) << 2) | 1
    ),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "Set", JS_PROP_CONFIGURABLE as i16),
];

pub static js_weakmap_proto_funcs: [FieldDef; 5] = [
    JS_CFUNC_NATIVE_DEF!("set", 2, js_map_set, 2),
    JS_CFUNC_NATIVE_DEF!("get", 1, js_map_get, 2),
    JS_CFUNC_NATIVE_DEF!("has", 1, js_map_has, 2),
    JS_CFUNC_NATIVE_DEF!("delete", 1, js_map_delete, 2),
    JS_PROP_STRING_DEF!(
        "[Symbol.toStringTag]",
        "WeakMap",
        JS_PROP_CONFIGURABLE as i16
    ),
];

pub static js_weakset_proto_funcs: [FieldDef; 4] = [
    JS_CFUNC_NATIVE_DEF!("add", 1, js_map_set, 3),
    JS_CFUNC_NATIVE_DEF!("has", 1, js_map_has, 3),
    JS_CFUNC_NATIVE_DEF!("delete", 1, js_map_delete, 3),
    JS_PROP_STRING_DEF!(
        "[Symbol.toStringTag]",
        "WeakSet",
        JS_PROP_CONFIGURABLE as i16
    ),
];

fn js_map_groupBy(ctx: &JSContext, cc: CallContext) -> JSResult {
    // check must be observed before argv[0] is accessed
    let func = cc.arg(1);
    func.ensure_function(ctx)?;

    let this_val = if is_strict_mode(ctx) {
        JSValue::UNDEFINED
    } else {
        ctx.0.borrow().global_obj.clone() // clone to avoid borrow too long
    };

    let mut groups = Vec::<(JSValue, Vec<JSValue>)>::with_capacity(16);

    for (idx, val) in cc.arg(0).iter(ctx)?.enumerate() {
        if idx as i64 >= MAX_SAFE_INTEGER {
            ctx.throw_type_error("too many elements");
            return Err(-1);
        }

        let val = val?;
        let key = func.call(ctx, &this_val, &[val.clone(), JSValue::from(idx as u32)])?;
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
        let arr = ctx.create_array_with_values(lst)?;
        ms.upsert(ctx, key, arr.into())?;
    }

    ctx.create_map_with_entries(ms).map(JSValue::Object)
}

pub static js_map_funcs: [FieldDef; 2] = [
    JS_CFUNC_NATIVE_DEF!("groupBy", 2, js_map_groupBy, 0),
    JS_CGETSET_NATIVE_DEF!("[Symbol.species]", js_get_this, NONE, 0),
];

pub static js_set_funcs: [FieldDef; 1] = [JS_CGETSET_NATIVE_DEF!(
    "[Symbol.species]",
    js_get_this,
    NONE,
    0
)];
