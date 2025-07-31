use std::ops::Deref;

use crate::{
    JS_CFUNC_NATIVE_DEF,
    atom::{JS_FreeAtomRT, JSAtom, consts::*},
    context::JSContext,
    error::JSResult,
    function::{CallContext, IteratorState, JS_Call, NativeField, NativeFunctionType},
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

#[derive(Clone)]
#[repr(transparent)]
pub struct IteratorObject(pub(crate) JSObject);

impl From<IteratorObject> for JSObject {
    fn from(value: IteratorObject) -> Self {
        value.0
    }
}

impl From<IteratorObject> for JSValue {
    fn from(value: IteratorObject) -> Self {
        Self::Object(value.0)
    }
}

impl TryFrom<JSValue> for IteratorObject {
    type Error = i32;

    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        if let JSValue::Object(o) = value {
            Ok(Self(o))
        } else {
            Err(-1)
        }
    }
}

impl IteratorObject {
    /// return *pdone: 0-not done, 1-done, 2-the iterator object is not parsed
    fn apply_next_method(
        &self,
        ctx: &JSContext,
        method: &JSValue,
        args: &[JSValue],
    ) -> Result<(JSValue, IteratorState), i32> {
        if let JSValue::Object(p) = method
            && let ObjectDetail::C_FUNCTION(nfunc) = p.0.payload.borrow().deref()
        {
            // fast path for the built-in iterators (avoid creating the
            // intermediate result object)
            match nfunc.handler {
                NativeFunctionType::IteratorNext(f) => {
                    let mut state = IteratorState::HasMore;
                    let cc =
                        CallContext::new(ctx, self.0.clone().into(), args, nfunc.magic as _, None);
                    let ret = f(ctx, cc, &mut state)?;
                    return Ok((ret, state));
                }
                NativeFunctionType::CIteratorNext(f) => {
                    let mut pdone = 0;
                    let ret = f(
                        ctx,
                        self.0.clone().into(),
                        args,
                        &mut pdone,
                        nfunc.magic as _,
                    )
                    .check_exception()?;

                    return if let JSValue::Object(ret) = ret {
                        Ok((
                            ret.into(),
                            match pdone {
                                0 => IteratorState::HasMore,
                                1 => IteratorState::Done,
                                2 => IteratorState::NotParsed,
                                _ => unreachable!(),
                            },
                        ))
                    } else {
                        ctx.throw_type_error("iterator must return an object");
                        Err(-1)
                    };
                }
                _ => {}
            }
        }

        if let JSValue::Object(obj) =
            JS_Call(ctx, method, &JSValue::Object(self.0.clone()), args).check_exception()?
        {
            Ok((obj.into(), IteratorState::NotParsed))
        } else {
            ctx.throw_type_error("iterator must return an object");
            Err(-1)
        }
    }

    /// get next value of iterator by calling specified `method`.
    /// returns: (next_value, is_iterator_done)
    pub fn next_with_method(
        &self,
        ctx: &JSContext,
        method: &JSValue,
        args: &[JSValue],
    ) -> Result<(JSValue, bool), i32> {
        let (obj, state) = self.apply_next_method(ctx, method, args)?;

        match state {
            IteratorState::HasMore | IteratorState::Done => {
                Ok((obj.into(), state == IteratorState::Done))
            }
            IteratorState::NotParsed => {
                let done = obj
                    .get_property(ctx, JS_ATOM_done)?
                    .coerce_to_bool(ctx)
                    .unwrap_or(false);
                let ret = if !done {
                    obj.get_property(ctx, JS_ATOM_value)?
                } else {
                    JSValue::UNDEFINED
                };
                Ok((ret, done))
            }
        }
    }

    /// get next value of iterator by calling default `next` method.
    pub fn next(&self, ctx: &JSContext, args: &[JSValue]) -> Result<(JSValue, bool), i32> {
        let next = self.0.get_property(ctx, JS_ATOM_next)?;
        self.next_with_method(ctx, &next, args)
    }

    /// close iterator (pre-abort), calling `return` method if available.
    pub fn close(&self, ctx: &JSContext, has_exception: bool) -> Result<(), i32> {
        let pending_exc = if has_exception {
            ctx.take_exception()
        } else {
            JSValue::UNDEFINED
        };

        if let Ok(return_method) = self.0.get_property(ctx, JS_ATOM_return) {
            if !return_method.is_undefined_or_null() {
                let ret = JS_Call(ctx, &return_method, &self.clone().into(), &[]);

                if pending_exc.is_undefined_or_null() {
                    if !ret.check_exception()?.is_object() {
                        ctx.throw_type_error_not_an_object();
                        return Err(-1);
                    }
                }
            }

            if !pending_exc.is_undefined_or_null() {
                ctx.throw(pending_exc);
                Err(-1)
            } else {
                Ok(())
            }
        } else {
            if !pending_exc.is_undefined_or_null() {
                ctx.throw(pending_exc.clone());
            }
            Err(-1)
        }
    }
}

impl JSObject {
    /// get enumerable object of `self`.
    /// call Symbol.iterator or Symbol.asyncIterator method defined on `self`,
    /// and return result of the method - an enumerable object.
    pub fn iterator(&self, ctx: &JSContext, is_async: bool) -> Result<IteratorObject, i32> {
        let method = if is_async {
            let m = self.get_property(ctx, JS_ATOM_Symbol_asyncIterator)?;
            if m.is_undefined_or_null() {
                // use Symbol.iterator instead, if Symbol.asyncIterator is not available
                let sync_iter = self.apply_iterator_method(
                    ctx,
                    &self.get_property(ctx, JS_ATOM_Symbol_iterator)?,
                )?;
                // wrap sync iterator to async iterartor
                return ctx.create_async_from_sync_iterator(sync_iter.into());
            } else {
                m
            }
        } else {
            self.get_property(ctx, JS_ATOM_Symbol_iterator)?
        };

        if !method.is_callable() {
            ctx.throw_type_error("value is not iterable");
            return Err(-1);
        } else {
            self.apply_iterator_method(ctx, &method)
        }
    }

    /// call Symbol.iterator() or Symbol.asyncIterator() method, returns result enumerable object.
    fn apply_iterator_method(
        &self,
        ctx: &JSContext,
        method: &JSValue,
    ) -> Result<IteratorObject, i32> {
        if let JSValue::Object(o) =
            JS_Call(ctx, method, &JSValue::Object(self.clone()), &[]).check_exception()?
        {
            Ok(IteratorObject(o))
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
    pub fn iterator(&self, ctx: &JSContext, is_async: bool) -> Result<IteratorObject, i32> {
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
#[deprecated(note = "use IteratorObject::call_next_method() instead")]
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

#[deprecated(note = "use IteratorObject::next() instead")]
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

    if let Ok(done_val) = obj.get_property(ctx, JS_ATOM_done) {
        *pdone = done_val.coerce_to_bool(ctx).unwrap_or(true);
        if !*pdone {
            obj.get_property(ctx, JS_ATOM_value)
                .unwrap_or(JSValue::EXCEPTION)
        } else {
            JSValue::UNDEFINED
        }
    } else {
        *pdone = false;
        return JSValue::EXCEPTION;
    }
}

/// TODO: migrate to Iterator::close()
/// or replace enum_obj: &JSValue with enum_obj: &JSObject
#[deprecated(note = "use IteratorObject::close() instead")]
pub fn JS_IteratorClose(
    ctx: &JSContext,
    enum_obj: &JSValue,
    has_exception: bool,
) -> Result<(), i32> {
    let pending_exc = if has_exception {
        ctx.take_exception()
    } else {
        JSValue::UNDEFINED
    };

    let method = enum_obj
        .get_property(ctx, JS_ATOM_return)
        .inspect_err(|_| {
            // restore original error
            ctx.throw(pending_exc.clone());
        })?;

    if !method.is_undefined_or_null() {
        let ret = JS_Call(ctx, &method, enum_obj, &[]);
        if !has_exception {
            if !ret.check_exception()?.is_object() {
                ctx.throw_type_error_not_an_object();
                return Err(-1);
            }
        }
    }

    if has_exception {
        ctx.throw(pending_exc);
        Err(-1)
    } else {
        Ok(())
    }
}

pub unsafe fn JS_IteratorGetCompleteValue(
    ctx: &JSContext,
    obj: &JSValueConst,
    pdone: *mut bool,
) -> JSValue {
    if let Ok(done_val) = obj.get_property(ctx, JS_ATOM_done) {
        let done = done_val.coerce_to_bool(ctx).unwrap();

        if let Ok(value) = obj.get_property(ctx, JS_ATOM_value) {
            *pdone = done;
            value
        } else {
            *pdone = false;
            return JSValue::EXCEPTION;
        }
    } else {
        *pdone = false;
        JSValue::EXCEPTION
    }
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
    let iter_obj = items.iterator(ctx, false)?;
    let next_method = iter_obj.0.get_property(ctx, JS_ATOM_next)?;

    let arr = ctx.create_array()?;
    let mut k = 0;
    loop {
        let (v, done) = iter_obj
            .next_with_method(ctx, &next_method, &[])
            .inspect_err(|_| {
                let _ = iter_obj.close(ctx, true);
            })?;

        if done {
            break;
        }

        arr.define_property_value(ctx, JSAtom(k), v, JS_PROP_C_W_E | JS_PROP_THROW)
            .inspect_err(|_| {
                let _ = iter_obj.close(ctx, true);
            })?;

        k += 1;
    }

    Ok(arr)
}

pub fn js_array_from_iterator(
    ctx: &JSContext,
    obj: &JSValue,
    method: &JSValue,
) -> Result<(JSValue, usize), i32> {
    let iter_obj = unsafe { obj.object_unchecked().apply_iterator_method(ctx, method)? };
    let next_method = iter_obj.0.get_property(ctx, JS_ATOM_next)?;

    let arr = ctx.create_array()?;
    let mut k = 0_i64;

    loop {
        let (val, done) = iter_obj.next_with_method(ctx, &next_method, &[])?;
        if done {
            break;
        }
        arr.create_data_property_u32(ctx, k, val, JS_PROP_THROW)?;
        k += 1;
    }

    Ok((arr, k as usize))
}

pub fn js_iterator_proto_iterator(_: &JSContext, cc: CallContext) -> JSResult {
    Ok(cc.this().clone())
}

pub const js_iterator_proto_funcs: [NativeField; 1] = [JS_CFUNC_NATIVE_DEF!(
    "[Symbol.iterator]",
    0,
    js_iterator_proto_iterator,
    0
)];
