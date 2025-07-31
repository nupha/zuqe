use std::ops::Deref;

use crate::{
    atom::{JS_FreeAtomRT, JSAtom, consts::*},
    context::JSContext,
    error::JSResult,
    function::{CallContext, IteratorState, JS_Call, NativeFunctionType},
    map::MapKV,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_SET_ENUM, JS_GPN_STRING_MASK, JS_PROP_C_W_E, JS_PROP_ENUMERABLE,
        JS_PROP_THROW, JSObject, ObjectDetail, PropertyEnum,
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
    pub prop_enums: Box<[PropertyEnum]>,
}

impl JSForInIterator {
    /// build property-enumeration-list for current prototype.
    /// returns true if the enumeration is finished.
    pub fn prepare_prototype_chain_enum(&mut self, ctx: &JSContext) -> Result<bool, i32> {
        // fast path: check if there are enumerable properties in the prototype chain
        let mut cur_obj = unsafe { self.obj.object_unchecked().clone() };
        loop {
            match cur_obj.prototype(ctx)? {
                None => {
                    // top of prototype chain, enumeration is done
                    return Ok(true);
                }
                Some(up) => {
                    cur_obj = up;

                    // count own property on cur proto
                    let mut props_count: u32 = 0;
                    cur_obj.own_property_names_foreach(
                        ctx,
                        JS_GPN_STRING_MASK | JS_GPN_ENUM_ONLY,
                        |_, _| {
                            props_count += 1;
                            Ok(())
                        },
                    )?;
                    if props_count != 0 {
                        break; // try slow path
                    }

                    // must check for timeout to avoid infinite loop
                    ctx.poll_interrupts()?;

                    // redo on upper prototype
                }
            }
        }

        // slow_path
        if self.is_array {
            let lst = unsafe {
                self.obj
                    .object_unchecked()
                    .query_own_property_names(ctx, JS_GPN_STRING_MASK | JS_GPN_SET_ENUM)
            }?;
            self.is_array = false;
            self.atom_count = lst.len() as _;
            self.prop_enums = lst;
        }

        // for name in it.prop_enums.iter() {
        //     enum_obj.define_property_value(ctx, name.atom, JSValue::NULL, JS_PROP_ENUMERABLE)?;
        // }

        Ok(false)
    }
}

impl JSObject {
    /// get enumerable object of `self`.
    /// call Symbol.iterator or Symbol.asyncIterator method defined on `self`,
    /// and return result of the method - an enumerable object.
    pub fn iterator(&self, ctx: &JSContext, is_async: bool) -> Result<JSObject, i32> {
        let method = if is_async {
            let m = self
                .get_property(ctx, JS_ATOM_Symbol_asyncIterator)
                .check_exception()?;

            if m.is_undefined_or_null() {
                // use Symbol.iterator instead, if Symbol.asyncIterator is not available
                let sync_iter = self.call_iterator_method(
                    ctx,
                    &self
                        .get_property(ctx, JS_ATOM_Symbol_iterator)
                        .check_exception()?,
                )?;
                // wrap sync iterator to async iterartor
                return ctx.create_async_from_sync_iterator(JSValue::Object(sync_iter));
            } else {
                m
            }
        } else {
            self.get_property(ctx, JS_ATOM_Symbol_iterator)
                .check_exception()?
        };

        if !method.is_callable() {
            println!("[O.o] self={self:?}, method={method:?}");
            ctx.throw_type_error("value is not iterable");
            return Err(-1);
        } else {
            self.call_iterator_method(ctx, &method)
        }
    }

    /// call Symbol.iterator() or Symbol.asyncIterator() method, returns result enumerable object.
    fn call_iterator_method(&self, ctx: &JSContext, method: &JSValue) -> Result<JSObject, i32> {
        if let JSValue::Object(o) =
            JS_Call(ctx, method, &JSValue::Object(self.clone()), &[]).check_exception()?
        {
            Ok(o)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }
}

impl JSValue {
    /// get enumerable object of `self`.
    /// call Symbol.iterator or Symbol.asyncIterator method defined on `self`,
    /// and return result of the method - an enumerable object.
    pub fn iterator(&self, ctx: &JSContext, is_async: bool) -> Result<JSObject, i32> {
        if let Self::Object(o) = self {
            o.iterator(ctx, is_async)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }
}

/// create an Object with two properties: `value` and `done`.
pub fn js_create_iterator_result(ctx: &JSContext, value: JSValue, done: bool) -> JSResult {
    let obj = ctx.create_base_object()?;
    obj.define_property_value(ctx, JS_ATOM_value, value, JS_PROP_C_W_E)?;
    obj.define_property_value(ctx, JS_ATOM_done, JSValue::Bool(done), JS_PROP_C_W_E)?;
    Ok(obj.into())
}

/// return *pdone: 0-not done, 1-done, 2-the iterator object is not parsed
pub fn JS_IteratorNext2(
    ctx: &JSContext,
    enum_obj: &JSValue,
    method: &JSValue,
    args: &[JSValue],
    pdone: &mut i32,
) -> JSValue {
    if let JSValue::Object(p) = method
        && let ObjectDetail::C_FUNCTION(nfunc) = p.0.payload.borrow().deref()
    {
        // fast path for the built-in iterators (avoid creating the
        // intermediate result object)
        match nfunc.handler {
            NativeFunctionType::IteratorNext(f) => {
                let cc = CallContext::new(ctx, enum_obj.clone(), args, nfunc.magic as _, None);
                let mut state = IteratorState::HasMore;
                let ret = f(ctx, cc, &mut state).unwrap_or(JSValue::EXCEPTION);
                *pdone = state as i32;
                return ret;
            }
            NativeFunctionType::CIteratorNext(f) => {
                let ret = f(ctx, enum_obj.clone(), args, pdone, nfunc.magic as _);
                return ret;
            }
            _ => {}
        }
    }

    let obj = JS_Call(ctx, method, enum_obj, args);
    if obj.is_exception() {
        *pdone = 0;
        return JSValue::EXCEPTION;
    }
    if !obj.is_object() {
        ctx.throw_type_error("iterator must return an object");
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

/// TODO: migrate to Iterator::close()
/// or replace enum_obj: &JSValue with enum_obj: &JSObject
pub fn JS_IteratorClose(
    ctx: &JSContext,
    enum_obj: &JSValue,
    has_exception: bool,
) -> Result<(), i32> {
    let (exc, mut has_err) = if has_exception {
        (ctx.take_exception(), true)
    } else {
        (JSValue::UNDEFINED, false)
    };

    let method = enum_obj.get_property(ctx, JS_ATOM_return);
    if method.is_exception() {
        has_err = true;
    } else if !method.is_undefined_or_null() {
        let ret = JS_Call(ctx, &method, enum_obj, &[]);
        if !has_exception {
            if ret.is_exception() {
                has_err = true;
            } else if !ret.is_object() {
                ctx.throw_type_error_not_an_object();
                has_err = true;
            }
        }
    }

    if has_exception {
        ctx.throw(exc);
    }

    if has_err { Err(-1) } else { Ok(()) }
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

pub fn js_for_in_iterator_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, _id: usize) {
    if let ObjectDetail::FOR_IN_ITERATOR(it) = isa {
        if !it.is_array {
            for x in it.prop_enums.iter() {
                JS_FreeAtomRT(rt, x.atom);
            }
        }
    }
}

pub fn js_for_in_iterator_mark(val: JSValue) {
    let p = unsafe { val.object_unchecked() };
    if let ObjectDetail::FOR_IN_ITERATOR(it) = p.0.payload.borrow().deref() {
        it.obj.gc_mark();
    }
}

/// `obj` is consumed.
pub unsafe fn build_for_in_iterator(ctx: &JSContext, obj: JSValue) -> JSValue {
    let obj = if !matches!(obj, JSValue::Object(_) | JSValue::NULL | JSValue::UNDEFINED) {
        obj.coerce_to_object(ctx)
            .map_or(JSValue::EXCEPTION, JSValue::Object)
    } else {
        obj
    };

    let mut it = Box::new(JSForInIterator {
        obj: obj.clone(),
        idx: 0,
        atom_count: 0,
        in_prototype_chain: false,
        is_array: false,
        prop_enums: Box::new([]),
    });

    if let JSValue::Object(ob) = obj {
        if ob.flags().is_fast_array() {
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
                ob.query_own_property_names(ctx, JS_GPN_STRING_MASK | JS_GPN_SET_ENUM)
            {
                it.atom_count = names.len() as _;
                it.prop_enums = names;
            } else {
                return JSValue::EXCEPTION;
            }
        }
    }

    if let Ok(o) = ctx.create_object_from_proto(JSValue::NULL, ObjectDetail::FOR_IN_ITERATOR(it)) {
        o.into()
    } else {
        JSValue::EXCEPTION
    }
}

pub fn iterator_to_array(ctx: &JSContext, items: &JSValue) -> JSResult {
    let iter = JSValue::Object(items.iterator(ctx, false)?);
    let next_method = iter.get_property(ctx, JS_ATOM_next).check_exception()?;

    let r = ctx.create_array()?;
    let mut k = 0;
    loop {
        let mut done = false;
        let v = JS_IteratorNext(ctx, &iter, &next_method, &[], &mut done);
        if v.is_exception() {
            unsafe { JS_IteratorClose(ctx, &iter, true) };
            return Err(-1);
        }

        if done {
            break;
        }

        if r.define_property_value(ctx, JSAtom(k), v, JS_PROP_C_W_E | JS_PROP_THROW)
            .is_err()
        {
            unsafe { JS_IteratorClose(ctx, &iter, true) };
            return Err(-1);
        }

        k += 1;
    }

    Ok(r)
}

pub unsafe fn js_array_from_iterator(
    ctx: &JSContext,
    plen: *mut u32,
    obj: JSValue,
    method: JSValue,
) -> JSValue {
    *plen = 0;

    let arr = if let Ok(a) = ctx.create_array() {
        a
    } else {
        return JSValue::EXCEPTION;
    };

    let iter = obj
        .object_unchecked()
        .call_iterator_method(ctx, &method)
        .map_or(JSValue::EXCEPTION, JSValue::Object);
    // let iter = JS_GetIterator2(ctx, &obj, &method);

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
