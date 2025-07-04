use std::collections::HashMap;

use crate::{
    JS_CLASS_MAP,
    atom::consts::{JS_ATOM_add, JS_ATOM_next, JS_ATOM_set},
    class::JSClassID,
    context::JSContext,
    error::JSResult,
    function::{CallContext, JS_Call},
    iterator::{JS_GetIterator, JS_IteratorClose, JS_IteratorNext},
    map::JSMapState,
    object::JSObjectIsa,
    value::{JSValue, JSValueConst},
};

const MAGIC_SET: i32 = 1 << 0;
const MAGIC_WEAK: i32 = 1 << 1;

pub unsafe fn js_map_constructor(
    ctx: &JSContext,
    new_target: &JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let is_set = (magic & MAGIC_SET) != 0;
    let is_weak = (magic & MAGIC_WEAK) != 0;

    let s = Box::new(JSMapState {
        is_weak,
        items: HashMap::with_capacity(4),
    });

    let isa = match magic {
        0 => JSObjectIsa::MAP(s),
        1 => JSObjectIsa::SET(s),
        2 => JSObjectIsa::WEAKMAP(s),
        3 => JSObjectIsa::WEAKSET(s),
        _ => unreachable!(),
    };

    let obj = match ctx.create_instance_from_ctor(new_target, isa) {
        Ok(o) => o,
        Err(_) => {
            return JSValue::EXCEPTION;
        }
    };

    macro_rules! fail {
        ($iter:expr, $next_method:expr, $adder:expr) => {
            if $iter.is_object() {
                // close the iterator object, preserving pending exception
                let _ = JS_IteratorClose(ctx, $iter, true);
            }
            return JSValue::EXCEPTION;
        };
    }

    let arr = if argc > 0 {
        &*argv.offset(0)
    } else {
        &JSValue::UNDEFINED
    };

    if !arr.is_undefined_or_null() {
        let adder = obj.get_property(ctx, if is_set { JS_ATOM_add } else { JS_ATOM_set });
        if adder.is_exception() {
            return JSValue::EXCEPTION;
        }
        if !adder.is_callable() {
            ctx.JS_ThrowTypeError("set/add is not a function");
            return JSValue::EXCEPTION;
        }

        let iter = JS_GetIterator(ctx, &arr, false);
        if iter.is_exception() {
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                let _ = JS_IteratorClose(ctx, &iter, true);
            }
            return JSValue::EXCEPTION;
        }

        let next_method = iter.get_property(ctx, JS_ATOM_next);
        if next_method.is_exception() {
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                let _ = JS_IteratorClose(ctx, &iter, true);
            }
            return JSValue::EXCEPTION;
        }

        loop {
            let mut done = false;
            let item = JS_IteratorNext(ctx, &iter, &next_method, &[], &mut done);
            if item.is_exception() {
                fail!(&iter, next_method, adder);
            }

            if done {
                break;
            }

            if is_set {
                let r = JS_Call(ctx, &adder, &obj, &[item]);
                if r.is_exception() {
                    fail!(&iter, next_method, adder);
                }
            } else {
                if !item.is_object() {
                    ctx.JS_ThrowTypeErrorNotAnObject();
                    fail!(&iter, next_method, adder);
                }

                let key = item.get_property_u32(ctx, 0);
                if key.is_exception() {
                    fail!(&iter, next_method, adder);
                }

                let value = item.get_property_u32(ctx, 1);
                if value.is_exception() {
                    fail!(&iter, next_method, adder);
                }

                let r = JS_Call(ctx, &adder, &obj, &[key, value]);
                if r.is_exception() {
                    fail!(&iter, next_method, adder);
                }
            }
        }
    }

    obj
}

pub unsafe fn js_map_get(
    ctx: &JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let isa = this_val.object_unchecked().0.isa.borrow();
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
    let mut isa = p.0.isa.borrow_mut();
    if isa.class_id() != JS_CLASS_MAP + cc.magic() as JSClassID {
        return Err(-1);
    }

    let s = isa.map_state_mut().unwrap();

    let key = cc.arg(0);
    if s.is_weak && !key.is_object() {
        ctx.JS_ThrowTypeErrorNotAnObject();
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
