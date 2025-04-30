use crate::{
    atom::consts::{JS_ATOM_add, JS_ATOM_next, JS_ATOM_set},
    class::{js_create_from_ctor, JSClassID},
    context::JSContext,
    function::JS_Call,
    iterator::{JS_GetIterator, JS_IteratorClose, JS_IteratorNext},
    map::{map_find_record, map_normalize_key, JSMapState},
    mem::{js_malloc, js_malloc_typed},
    types::init_list_head,
    value::{JSValue, JSValueConst, JS_FreeValue, JS_IsException, JS_IsObject},
    JS_CLASS_MAP,
};

const MAGIC_SET: i32 = 1 << 0;
const MAGIC_WEAK: i32 = 1 << 1;

pub unsafe fn js_map_constructor(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let is_set = (magic & MAGIC_SET) != 0;
    let is_weak = (magic & MAGIC_WEAK) != 0;

    let obj = js_create_from_ctor(ctx, new_target, JS_CLASS_MAP + magic as u32);
    if JS_IsException(obj) {
        return JSValue::EXCEPTION;
    }

    macro_rules! fail {
        ($iter:expr, $next_method:expr, $adder:expr) => {
            if JS_IsObject($iter) {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, $iter, true);
            }
            JS_FreeValue(ctx, $next_method);
            JS_FreeValue(ctx, $iter);
            JS_FreeValue(ctx, $adder);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        };
    }

    let mut s = if let Some(p) = js_malloc_typed::<JSMapState>(ctx, 1, true) {
        Box::from_raw(p.as_ptr())
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    init_list_head(&mut s.records);
    s.is_weak = is_weak;
    s.record_count_threshold = 4;
    s.hash_size = 1;
    s.hash_table = js_malloc(ctx, std::mem::size_of::<JSValue>() * s.hash_size as usize).cast();
    if s.hash_table.is_null() {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }
    init_list_head(s.hash_table);

    // leak JSMapState ownership
    obj.object_unchecked().set_opaque(Box::into_raw(s).cast());

    let arr = if argc > 0 {
        *argv.offset(0)
    } else {
        JSValue::UNDEFINED
    };

    if !arr.is_undefined_or_null() {
        let adder = obj.get_property(
            ctx.as_mut().unwrap(),
            if is_set { JS_ATOM_add } else { JS_ATOM_set },
        );
        if adder.is_exception() {
            JS_FreeValue(ctx, adder);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
        if !adder.is_callable(ctx.as_ref().unwrap()) {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"set/add is not a function");

            JS_FreeValue(ctx, adder);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        let iter = JS_GetIterator(ctx, arr, false);
        if iter.is_exception() {
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, iter, true);
            }
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, adder);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        let next_method = iter.get_property(ctx.as_mut().unwrap(), JS_ATOM_next);
        if next_method.is_exception() {
            if iter.is_object() {
                // close the iterator object, preserving pending exception
                JS_IteratorClose(ctx, iter, true);
            }
            JS_FreeValue(ctx, next_method);
            JS_FreeValue(ctx, iter);
            JS_FreeValue(ctx, adder);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        loop {
            let mut done = false;
            let item = JS_IteratorNext(ctx, iter, next_method, 0, std::ptr::null_mut(), &mut done);
            if item.is_exception() {
                fail!(iter, next_method, adder);
            }

            if done {
                JS_FreeValue(ctx, item);
                break;
            }

            let ret = if is_set {
                let r = JS_Call(ctx.as_mut().unwrap(), adder, obj, 1, [item].as_mut_ptr());
                if r.is_exception() {
                    JS_FreeValue(ctx, item);
                    fail!(iter, next_method, adder);
                }
                r
            } else {
                if !item.is_object() {
                    (*ctx).JS_ThrowTypeErrorNotAnObject();
                    JS_FreeValue(ctx, item);
                    fail!(iter, next_method, adder);
                }

                let key = item.get_property_u32(ctx.as_mut().unwrap(), 0);
                if key.is_exception() {
                    JS_FreeValue(ctx, item);
                    JS_FreeValue(ctx, key);
                    fail!(iter, next_method, adder);
                }

                let value = item.get_property_u32(ctx.as_mut().unwrap(), 1);
                if value.is_exception() {
                    JS_FreeValue(ctx, item);
                    JS_FreeValue(ctx, key);
                    JS_FreeValue(ctx, value);
                    fail!(iter, next_method, adder);
                }

                let r = JS_Call(
                    ctx.as_mut().unwrap(),
                    adder,
                    obj,
                    2,
                    [key, value].as_mut_ptr(),
                );
                JS_FreeValue(ctx, key);
                JS_FreeValue(ctx, value);
                if r.is_exception() {
                    JS_FreeValue(ctx, item);
                    fail!(iter, next_method, adder);
                }

                r
            };

            JS_FreeValue(ctx, ret);
            JS_FreeValue(ctx, item);
        }

        JS_FreeValue(ctx, next_method);
        JS_FreeValue(ctx, iter);
        JS_FreeValue(ctx, adder);
    }

    obj
}

pub unsafe fn js_map_get(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let s = if let Ok(p) = this_val
        .object_unchecked()
        .opaque_must::<JSMapState>(JS_CLASS_MAP + magic as JSClassID, ctx.as_mut().unwrap())
    {
        p
    } else {
        return JSValue::EXCEPTION;
    };

    let key = map_normalize_key(ctx, *argv.offset(0));
    let mr = map_find_record(ctx, s.as_ptr(), key);
    if mr.is_null() {
        JSValue::UNDEFINED
    } else {
        (*mr).value.dup()
    }
}

pub unsafe fn js_map_set(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    magic: i32,
) -> JSValue {
    let mut s = if let Ok(p) = this_val
        .object_unchecked()
        .opaque_must::<JSMapState>(JS_CLASS_MAP + magic as JSClassID, ctx.as_mut().unwrap())
    {
        p
    } else {
        return JSValue::EXCEPTION;
    };

    let key = map_normalize_key(ctx, *argv.offset(0));
    if s.as_ref().is_weak && !key.is_object() {
        return (*ctx).JS_ThrowTypeErrorNotAnObject();
    }

    let value = if magic & MAGIC_SET != 0 {
        JSValue::UNDEFINED
    } else {
        *argv.offset(1)
    };

    let mut mr = map_find_record(ctx, s.as_ptr(), key);
    if !mr.is_null() {
        JS_FreeValue(ctx, (*mr).value);
    } else {
        mr = if let Ok(r) = s.as_mut().add_record(ctx.as_mut().unwrap(), key) {
            r.as_ptr()
        } else {
            return JSValue::EXCEPTION;
        };
    }

    (*mr).value = value.dup();

    this_val.dup()
}
