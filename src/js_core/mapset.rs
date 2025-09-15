use crate::{
    JS_ALIAS_DEF, JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_MAP, JS_PROP_STRING_DEF,
    atom::consts::{JS_ATOM_add, JS_ATOM_set},
    class::JSClassID,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    iterator::{JSIteratorKindEnum, JSMapIteratorData},
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
    let p = cc.this().object().unwrap();
    if p.class_id() != JS_CLASS_MAP + cc.magic() as JSClassID {
        ctx.throw_type_error_invalid_class(JS_CLASS_MAP + cc.magic() as JSClassID);
        return Err(-1);
    }

    Ok(p.0
        .payload
        .borrow()
        .map_state()
        .unwrap()
        .find(cc.arg(0))
        .unwrap_or(JSValue::UNDEFINED))
}

pub fn js_map_set(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().object().unwrap();
    if p.class_id() != JS_CLASS_MAP + cc.magic() as JSClassID {
        ctx.throw_type_error_invalid_class(JS_CLASS_MAP + cc.magic() as JSClassID);
        return Err(-1);
    }

    let mut isa = p.0.payload.borrow_mut();
    let s = isa.map_state_mut().unwrap();

    let key = cc.arg(0);
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
        let kob = key.object().unwrap();
        kob.0.weak_refs.borrow_mut().push(p.downgrade());
    }

    Ok(cc.this().clone())
}

pub fn js_map_delete(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().object().unwrap();
    if p.class_id() != JS_CLASS_MAP + cc.magic() as JSClassID {
        ctx.throw_type_error_invalid_class(JS_CLASS_MAP + cc.magic() as JSClassID);
        return Err(-1);
    }

    let mut isa = p.0.payload.borrow_mut();
    let s = isa.map_state_mut().unwrap();
    let key = cc.arg(0);
    if s.find(key).is_some() {
        s.remove(cc.arg(0));

        if s.is_weak() {
            let kob = key.object().unwrap();
            kob.0
                .weak_refs
                .borrow_mut()
                .retain(|w| w.upgrade().map_or(false, |o| !o.eq(p)));
        }

        Ok(JSValue::Bool(true))
    } else {
        Ok(JSValue::Bool(false))
    }
}

pub fn js_map_has(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().object().unwrap();
    if p.class_id() != JS_CLASS_MAP + cc.magic() as JSClassID {
        ctx.throw_type_error_invalid_class(JS_CLASS_MAP + cc.magic() as JSClassID);
        return Err(-1);
    }

    Ok(p.0
        .payload
        .borrow()
        .map_state()
        .unwrap()
        .find(cc.arg(0))
        .unwrap_or(JSValue::UNDEFINED))
}

pub fn js_map_clear(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().object().unwrap();
    if p.class_id() != JS_CLASS_MAP + cc.magic() as JSClassID {
        ctx.throw_type_error_invalid_class(JS_CLASS_MAP + cc.magic() as JSClassID);
        return Err(-1);
    }

    p.0.payload.borrow_mut().map_state_mut().unwrap().clear();
    Ok(JSValue::UNDEFINED)
}

pub fn js_map_get_size(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().object().unwrap();
    if p.class_id() != JS_CLASS_MAP + cc.magic() as JSClassID {
        ctx.throw_type_error_invalid_class(JS_CLASS_MAP + cc.magic() as JSClassID);
        return Err(-1);
    }
    Ok(JSValue::Int(
        p.0.payload.borrow().map_state().unwrap().len() as i32,
    ))
}

pub fn js_map_forEach(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let p = this.object().unwrap();
    if p.class_id() != JS_CLASS_MAP + cc.magic() as JSClassID {
        ctx.throw_type_error_invalid_class(JS_CLASS_MAP + cc.magic() as JSClassID);
        return Err(-1);
    }

    let func = cc.arg(0);
    let this_arg = cc.arg(1);
    func.ensure_callable(ctx)?;

    p.0.payload.borrow_mut().map_state_mut().unwrap().lock();

    let mut i = 0;
    loop {
        let pair = {
            let isa = p.0.payload.borrow();
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

    p.0.payload.borrow_mut().map_state_mut().unwrap().unlock();

    Ok(JSValue::UNDEFINED)
}

fn js_create_map_iterator(ctx: &JSContext, cc: CallContext) -> JSResult {
    debug_assert!(matches!(cc.magic() >> 2, 0 | 1 | 2));
    let kind = unsafe { std::mem::transmute::<u8, JSIteratorKindEnum>(cc.magic() as u8 >> 2) };
    let class_id = JS_CLASS_MAP + (cc.magic() & 3) as JSClassID; // map,set,weakmap,weakset

    let p = cc.this().object().unwrap();
    if p.class_id() != class_id {
        ctx.throw_type_error_invalid_class(class_id);
        return Err(-1);
    }

    let s = p.0.payload.borrow().map_state().unwrap();

    let it = Box::new(JSMapIteratorData {
        obj: cc.this().clone(),
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
