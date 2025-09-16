use crate::{
    JS_ALIAS_DEF, JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_MAP, JS_PROP_STRING_DEF,
    atom::consts::{JS_ATOM_add, JS_ATOM_set},
    class::JSClassID,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    iterator::{JSIteratorKindEnum, JSMapIteratorData},
    js_core::{js_get_this, object::js_object_groupBy},
    mapset::{JSMapState, MapPair},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

const MAGIC_SET: i32 = 1 << 0;
const MAGIC_WEAK: i32 = 1 << 1;

pub fn js_map_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let is_set = (magic & MAGIC_SET) != 0;
    let is_weak = (magic & MAGIC_WEAK) != 0;
    let new_target = cc.this();

    let s = Box::new(JSMapState::new(is_weak, 16));
    let obj = ctx
        .create_object_apply_ctor(
            new_target,
            match magic {
                0 => ObjectPayload::MAP(s),
                1 => ObjectPayload::SET(s),
                2 => ObjectPayload::WEAKMAP(s),
                3 => ObjectPayload::WEAKSET(s),
                _ => unreachable!(),
            },
        )
        .map_err(|_| -1)?;

    let arr = if cc.length() > 0 {
        cc.arg(0)
    } else {
        &JSValue::UNDEFINED
    };

    let obj: JSValue = obj.into();

    if !arr.is_undefined_or_null() {
        let adder = obj.get_property(ctx, if is_set { JS_ATOM_add } else { JS_ATOM_set })?;
        if !adder.is_callable() {
            ctx.throw_type_error("set/add is not a function");
            return Err(-1);
        }

        for item in arr.iter(ctx)? {
            let item = item?;

            if is_set {
                adder.call(ctx, &obj, &[item])?;
            } else {
                if !item.is_object() {
                    ctx.throw_type_error_not_an_object();
                    return Err(-1);
                }
                let key = item.get_property_u32(ctx, 0)?;
                let value = item.get_property_u32(ctx, 1)?;
                adder.call(ctx, &obj, &[key, value])?;
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

    let mut r =
        this.retrive_object_payload_mut(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))?;

    let s = r.map_state_mut().unwrap();
    if s.is_weak() && !key.is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let value = if cc.magic() & MAGIC_SET != 0 {
        &JSValue::UNDEFINED
    } else {
        cc.arg(1)
    };

    s.add(ctx, key.clone(), value.clone())?;

    if s.is_weak() {
        key.as_object()
            .unwrap()
            .0
            .weak_refs
            .borrow_mut()
            .push(this.as_object().unwrap().downgrade());
    }

    Ok(this.clone())
}

pub fn js_map_delete(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let key = cc.arg(0);

    this.retrive_object_payload_mut(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))
        .map(|mut r| {
            let s = r.map_state_mut().unwrap();

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
        .map(|r| {
            r.map_state()
                .unwrap()
                .find(cc.arg(0))
                .unwrap_or(JSValue::UNDEFINED)
        })
}

pub fn js_map_clear(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this()
        .retrive_object_payload_mut(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))
        .map(|mut r| {
            r.map_state_mut().unwrap().clear();
            JSValue::UNDEFINED
        })
}

pub fn js_map_get_size(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this()
        .retrive_object_payload(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))
        .map(|r| JSValue::Int(r.map_state().unwrap().len() as i32))
}

pub fn js_map_forEach(ctx: &JSContext, cc: CallContext) -> JSResult {
    let func = cc.arg(0);
    let this_arg = cc.arg(1);
    func.ensure_callable(ctx)?;

    // lock map, and ensure this is an object of expected class_id.
    {
        let mut pl = cc
            .this()
            .retrive_object_payload_mut(ctx, Some(JS_CLASS_MAP + cc.magic() as JSClassID))?;
        pl.map_state_mut().unwrap().lock();
    }

    let this = cc.this();
    let obj = this.as_object().unwrap();
    let mut i = 0;

    loop {
        let pair = {
            let isa = obj.0.payload.borrow();
            let s = isa.map_state().unwrap();
            if let Some(kv) = s.slots.get(i) {
                match kv {
                    MapPair::Strong { key, value } => Some((key.clone(), value.clone())),
                    MapPair::Weak { key, value } => {
                        key.upgrade().map(|k| (JSValue::Object(k), value.clone()))
                    }
                    _ => None,
                }
            } else {
                None
            }
        };

        if let Some((key, val)) = pair {
            if func.call(ctx, this_arg, &[key, val, this.clone()]).is_err() {
                break;
            }

            // if ctx.poll_interrupts().is_err() {
            //     break;
            // }

            i += 1;
        } else {
            break;
        }
    }

    // unlock map
    obj.0.payload.borrow_mut().map_state_mut().unwrap().unlock();

    Ok(JSValue::UNDEFINED)
}

fn js_create_map_iterator(ctx: &JSContext, cc: CallContext) -> JSResult {
    // ensure `this` is an object of expected class_id
    let this = cc.this();
    let _ = this.retrive_object_payload(ctx, Some(JS_CLASS_MAP + (cc.magic() & 3) as JSClassID))?;

    debug_assert!(matches!(cc.magic() >> 2, 0 | 1 | 2));
    let kind = unsafe { std::mem::transmute::<u8, JSIteratorKindEnum>(cc.magic() as u8 >> 2) };

    // create iterator object
    let it = Box::new(JSMapIteratorData {
        obj: this.clone(),
        kind,
        cur_record: MapPair::Strong {
            key: JSValue::UNDEFINED,
            value: JSValue::UNDEFINED,
        },
    });
    ctx.create_object(match cc.magic() & 1 {
        0 => ObjectPayload::MAP_ITERATOR(it),
        1 => ObjectPayload::SET_ITERATOR(it),
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

pub static js_map_funcs: [FieldDef; 2] = [
    JS_CFUNC_NATIVE_DEF!("groupBy", 2, js_object_groupBy, 1),
    JS_CGETSET_NATIVE_DEF!("[Symbol.species]", js_get_this, NONE, 0),
];
