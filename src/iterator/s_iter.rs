use std::ops::{Deref, DerefMut};

use crate::{
    atom::{JS_FreeAtomRT, JSAtom, consts::*},
    context::JSContext,
    error::JSResult,
    function::{JS_Call, JSCFunctionEnum},
    map::MapKV,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_SET_ENUM, JS_GPN_STRING_MASK, JS_PROP_C_W_E, JS_PROP_ENUMERABLE,
        JS_PROP_THROW, JSObjectIsa, PropertyEnum,
    },
    runtime::JSRuntime,
    value::{JSValue, JSValueConst},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSIteratorKindEnum {
    JS_ITERATOR_KIND_KEY,
    JS_ITERATOR_KIND_VALUE,
    JS_ITERATOR_KIND_KEY_AND_VALUE,
}

#[derive(Debug)]
pub struct JSMapIteratorData {
    pub obj: JSValue,
    pub kind: JSIteratorKindEnum,
    pub cur_record: MapKV,
}

#[derive(Debug)]
pub struct JSForInIterator {
    pub obj: JSValue,
    pub idx: u32,
    pub atom_count: u32,
    pub in_prototype_chain: bool,
    pub is_array: bool,
    pub tab_atom: Box<[PropertyEnum]>,
}

/// create an Object with two properties: `value` and `done`.
pub fn js_create_iterator_result(ctx: &JSContext, value: JSValue, done: bool) -> JSResult {
    let obj = ctx.create_base_object()?;
    obj.define_property_value(ctx, JS_ATOM_value, value, JS_PROP_C_W_E)?;
    obj.define_property_value(ctx, JS_ATOM_done, JSValue::Bool(done), JS_PROP_C_W_E)?;
    Ok(obj)
}

pub fn JS_GetIterator2(ctx: &JSContext, obj: &JSValueConst, method: &JSValueConst) -> JSValue {
    let enum_obj = JS_Call(ctx, method, obj, &[]);
    if enum_obj.is_exception() {
        return enum_obj;
    }

    if !enum_obj.is_object() {
        ctx.JS_ThrowTypeErrorNotAnObject();
        return JSValue::EXCEPTION;
    }

    return enum_obj;
}

pub fn JS_GetIterator(ctx: &JSContext, obj: &JSValueConst, is_async: bool) -> JSValue {
    let method = if is_async {
        let mut val = obj.get_property(ctx, JS_ATOM_Symbol_asyncIterator);
        if val.is_exception() {
            return JSValue::EXCEPTION;
        }

        if val.is_undefined_or_null() {
            val = obj.get_property(ctx, JS_ATOM_Symbol_iterator);
            if val.is_exception() {
                return val;
            }

            let sync_iter = JS_GetIterator2(ctx, obj, &val);
            if sync_iter.is_exception() {
                return JSValue::EXCEPTION;
            }

            let ret = ctx
                .create_async_from_sync_iterator(sync_iter)
                .unwrap_or(JSValue::EXCEPTION);

            return ret;
        }

        val
    } else {
        let v = obj.get_property(ctx, JS_ATOM_Symbol_iterator);
        if v.is_exception() {
            return JSValue::EXCEPTION;
        }
        v
    };

    if !method.is_callable() {
        ctx.JS_ThrowTypeError("value is not iterable");
        return JSValue::EXCEPTION;
    }

    JS_GetIterator2(ctx, obj, &method)
}

/// return *pdone: 0-not done, 1-done, 2-the iterator object is not parsed
pub fn JS_IteratorNext2(
    ctx: &JSContext,
    enum_obj: &JSValue,
    method: &JSValue,
    args: &[JSValue],
    pdone: &mut i32,
) -> JSValue {
    // fast path for the built-in iterators (avoid creating the
    // intermediate result object)
    if let JSValue::Object(p) = method {
        if let JSObjectIsa::C_FUNCTION(cfunc) = p.0.isa.borrow().deref() {
            if cfunc.cproto == JSCFunctionEnum::IteratorNext {
                let ret = unsafe {
                    (cfunc.c_function.iterator_next)(
                        ctx,
                        enum_obj.clone(),
                        args,
                        pdone,
                        cfunc.magic as _,
                    )
                };
                return ret;
            }
        }
    }

    let obj = JS_Call(ctx, method, enum_obj, args);
    if obj.is_exception() {
        *pdone = 0;
        return JSValue::EXCEPTION;
    }
    if !obj.is_object() {
        ctx.JS_ThrowTypeError("iterator must return an object");
        *pdone = 0;
        return JSValue::EXCEPTION;
    }

    *pdone = 2;
    obj
}

pub fn JS_IteratorNext(
    ctx: &JSContext,
    enum_obj: &JSValue,
    method: &JSValue,
    args: &[JSValue],
    pdone: &mut bool,
) -> JSValue {
    let mut done: i32 = 0;
    let obj = JS_IteratorNext2(ctx, enum_obj, method, args, &mut done);
    if obj.is_exception() {
        *pdone = false;
        return JSValue::EXCEPTION;
    }

    if done != 2 {
        *pdone = done != 0;
        return obj;
    }

    let done_val = obj.get_property(ctx, JS_ATOM_done);
    if done_val.is_exception() {
        *pdone = false;
        return JSValue::EXCEPTION;
    }

    *pdone = done_val.coerce_to_bool(ctx).unwrap_or(true);
    if !*pdone {
        obj.get_property(ctx, JS_ATOM_value)
    } else {
        JSValue::UNDEFINED
    }
}

pub fn JS_IteratorClose(
    ctx: &JSContext,
    enum_obj: &JSValueConst,
    is_exception_pending: bool,
) -> Result<(), i32> {
    let (exc_obj, mut res) = if is_exception_pending {
        let exc = ctx.take_exception();
        (exc, -1)
    } else {
        (JSValue::UNDEFINED, 0)
    };

    let method = enum_obj.get_property(ctx, JS_ATOM_return);
    if method.is_exception() {
        res = -1;
    } else {
        if !method.is_undefined_or_null() {
            let ret = JS_Call(ctx, &method, enum_obj, &[]);
            if !is_exception_pending {
                if ret.is_exception() {
                    res = -1;
                } else if !ret.is_object() {
                    ctx.JS_ThrowTypeErrorNotAnObject();
                    res = -1;
                }
            }
        }
    }

    if is_exception_pending {
        ctx.throw(exc_obj);
    }

    if res == -1 { Err(-1) } else { Ok(()) }
}

pub unsafe fn JS_IteratorGetCompleteValue(
    ctx: &JSContext,
    obj: &JSValueConst,
    pdone: *mut bool,
) -> JSValue {
    let done_val = obj.get_property(ctx, JS_ATOM_done);
    if done_val.is_exception() {
        *pdone = false;
        return JSValue::EXCEPTION;
    }
    let done = done_val.coerce_to_bool(ctx).unwrap();

    let value = obj.get_property(ctx, JS_ATOM_value);
    if value.is_exception() {
        *pdone = false;
        return JSValue::EXCEPTION;
    }
    *pdone = done;
    value
}

pub fn js_for_in_iterator_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, _id: usize) {
    if let JSObjectIsa::FOR_IN_ITERATOR(it) = isa {
        if !it.is_array {
            for x in it.tab_atom.iter() {
                JS_FreeAtomRT(rt, x.atom);
            }
        }
    }
}

pub fn js_for_in_iterator_mark(val: JSValue) {
    let p = unsafe { val.object_unchecked() };
    if let JSObjectIsa::FOR_IN_ITERATOR(it) = p.0.isa.borrow().deref() {
        it.obj.gc_mark();
    }
}

/// `obj` is consumed.
pub unsafe fn build_for_in_iterator(ctx: &JSContext, obj: JSValue) -> JSValue {
    let obj = if !matches!(obj, JSValue::Object(_) | JSValue::NULL | JSValue::UNDEFINED) {
        obj.coerce_to_object(ctx).unwrap_or(JSValue::EXCEPTION)
    } else {
        obj
    };

    let mut it = Box::new(JSForInIterator {
        obj: obj.clone(),
        idx: 0,
        atom_count: 0,
        in_prototype_chain: false,
        is_array: false,
        tab_atom: Box::new([]),
    });

    if let JSValue::Object(ob) = obj {
        if ob.flags().fast_array() {
            // check that there are no enumerable normal fields
            if !ob
                .shape()
                .unwrap()
                .0
                .borrow()
                .prop_defs
                .iter()
                .any(|(_, prs)| prs.flags() & JS_PROP_ENUMERABLE != 0)
            {
                // no enumerable property, is fast array.
                // we only store the number of elements
                it.is_array = true;
                it.atom_count = ob.array_length_unchecked() as u32;
            }
        }

        if !it.is_array {
            // not an array - get own property names list
            if let Ok(names) =
                ob.get_own_property_names_internal(ctx, JS_GPN_STRING_MASK | JS_GPN_SET_ENUM)
            {
                it.atom_count = names.len() as _;
                it.tab_atom = names;
            } else {
                return JSValue::EXCEPTION;
            }
        }
    }

    if let Ok(o) = ctx.create_instance_from_proto(&JSValue::NULL, JSObjectIsa::FOR_IN_ITERATOR(it))
    {
        o
    } else {
        JSValue::EXCEPTION
    }
}

// return -1 if exception, 0 if slow case, 1 if the enumeration is finished
pub unsafe fn js_for_in_prepare_prototype_chain_enum(
    ctx: &JSContext,
    enum_obj: &JSValueConst,
) -> i32 {
    let mut isa = enum_obj.object_unchecked().0.isa.borrow_mut();
    let it = if let JSObjectIsa::FOR_IN_ITERATOR(p) = isa.deref_mut() {
        p
    } else {
        unreachable!()
    };

    // check if there are enumerable properties in the prototype chain (fast path)
    let mut obj1 = it.obj.clone();

    loop {
        match obj1.prototype(ctx) {
            Ok(JSValue::NULL) => {
                break;
            }
            Ok(v) => {
                obj1 = v;

                // count own property names
                let mut props_count: u32 = 0;
                let r = obj1.object_unchecked().own_property_names_foreach(
                    ctx,
                    JS_GPN_STRING_MASK | JS_GPN_ENUM_ONLY,
                    |_, _| {
                        props_count += 1;
                        Ok(())
                    },
                );
                if r.is_err() {
                    return -1;
                }

                if props_count != 0 {
                    break;
                }

                // must check for timeout to avoid infinite loop
                if ctx.poll_interrupts().is_err() {
                    return -1;
                }
            }
            Err(_) => {
                return -1;
            }
        }
    } // loop

    // slow_path
    if it.is_array {
        if let Ok(names) = it
            .obj
            .object_unchecked()
            .get_own_property_names_internal(ctx, JS_GPN_STRING_MASK | JS_GPN_SET_ENUM)
        {
            it.is_array = false;
            it.atom_count = names.len() as _;
            it.tab_atom = names;
        } else {
            return -1;
        }
    }

    for name in (*it).tab_atom.iter() {
        if enum_obj
            .define_property_value(ctx, name.atom, JSValue::NULL, JS_PROP_ENUMERABLE)
            .is_err()
        {
            return -1;
        }
    }

    0
}

pub unsafe fn iterator_to_array(ctx: &JSContext, items: &JSValueConst) -> JSValue {
    let iter = JS_GetIterator(ctx, items, false);
    if iter.is_exception() {
        return JSValue::EXCEPTION;
    }

    let next_method = iter.get_property(ctx, JS_ATOM_next);
    if next_method.is_exception() {
        return JSValue::EXCEPTION;
    }

    if let Ok(r) = ctx.create_array() {
        let mut k = 0;
        loop {
            let mut done = false;
            let v = JS_IteratorNext(ctx, &iter, &next_method, &[], &mut done);
            if v.is_exception() {
                let _ = JS_IteratorClose(ctx, &iter, true);
                return JSValue::EXCEPTION;
            }

            if done {
                break;
            }

            if r.define_property_value(ctx, JSAtom(k), v, JS_PROP_C_W_E | JS_PROP_THROW)
                .is_err()
            {
                let _ = JS_IteratorClose(ctx, &iter, true);
                return JSValue::EXCEPTION;
            }

            k += 1;
        }

        r
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_array_from_iterator(
    ctx: &JSContext,
    plen: *mut u32,
    obj: JSValueConst,
    method: JSValueConst,
) -> JSValue {
    *plen = 0;

    let arr = if let Ok(a) = ctx.create_array() {
        a
    } else {
        return JSValue::EXCEPTION;
    };

    let iter = JS_GetIterator2(ctx, &obj, &method);
    if iter.is_exception() {
        return JSValue::EXCEPTION;
    }

    let next_method = iter.get_property(ctx, JS_ATOM_next);
    if next_method.is_exception() {
        return JSValue::EXCEPTION;
    }

    let mut k = 0;

    loop {
        let mut done = false;
        let val = JS_IteratorNext(ctx, &iter, &next_method, &[], &mut done);
        if val.is_exception() {
            return JSValue::EXCEPTION;
        }

        if done {
            break;
        }

        if arr
            .create_data_property_u32(ctx, k, val, JS_PROP_THROW)
            .is_err()
        {
            return JSValue::EXCEPTION;
        }

        k += 1;
    }

    *plen = k as u32;
    arr
}
