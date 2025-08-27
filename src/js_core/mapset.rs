use std::collections::HashMap;

use crate::{
    JS_CLASS_MAP,
    atom::consts::{JS_ATOM_add, JS_ATOM_set},
    class::JSClassID,
    context::JSContext,
    error::JSResult,
    function::CallContext,
    map::JSMapState,
    object::ObjectPayload,
    value::{JSValue, JSValueConst},
};

const MAGIC_SET: i32 = 1 << 0;
const MAGIC_WEAK: i32 = 1 << 1;

pub fn js_map_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let is_set = (magic & MAGIC_SET) != 0;
    let is_weak = (magic & MAGIC_WEAK) != 0;
    let new_target = cc.this();

    let s = Box::new(JSMapState {
        is_weak,
        items: HashMap::with_capacity(4),
    });
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

pub unsafe fn js_map_get(
    ctx: &JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let isa = this_val.object_unchecked().0.payload.borrow();
    if isa.class_id() != JS_CLASS_MAP + magic as JSClassID {
        return JSValue::EXCEPTION;
    }
    let s = isa.map_state().unwrap();
    let key = &*argv.offset(0);
    s.find(ctx, key).unwrap_or(JSValue::UNDEFINED)
}

/// insert (key, value) pair to a map or set object
pub fn js_map_set(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = unsafe { cc.this().object_unchecked() };
    let mut isa = p.0.payload.borrow_mut();
    if isa.class_id() != JS_CLASS_MAP + cc.magic() as JSClassID {
        return Err(-1);
    }

    let s = isa.map_state_mut().unwrap();

    let key = cc.arg(0);
    if s.is_weak && !key.is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let value = if cc.magic() & MAGIC_SET != 0 {
        &JSValue::UNDEFINED
    } else {
        cc.arg(1)
    };

    s.add(ctx, key.clone(), value.clone())?;

    if s.is_weak {
        let ob = unsafe { key.object_unchecked() };
        ob.0.weak_refs.borrow_mut().push(p.downgrade());
    }

    Ok(cc.this().clone())
}
