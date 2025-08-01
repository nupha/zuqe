use std::ops::Deref;

use crate::{
    JS_CFUNC_NATIVE_DEF,
    atom::{JS_ATOM_NULL, JS_FreeAtomRT, JSAtom, consts::*},
    context::JSContext,
    error::JSResult,
    function::{CallContext, IteratorState, JS_Call, NativeField, NativeFunctionType},
    iterator::JSAsyncFromSyncIteratorData,
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

#[derive(Debug, Clone)]
pub struct ObjectIter {
    pub(crate) target: JSObject,
    pub(crate) next_method: JSValue,
    pub(crate) ctx: JSContext,
    pub(crate) done: bool,
}

impl Iterator for ObjectIter {
    type Item = JSResult;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else if let Ok((value, done)) =
            Self::do_next_with_method(&self.ctx, &self.target, &self.next_method, &[])
        {
            self.done = done;
            if done { None } else { Some(Ok(value)) }
        } else {
            Some(Err(-1))
        }
    }
}

impl Drop for ObjectIter {
    fn drop(&mut self) {
        if !self.done {
            let has_exc = !self.ctx.exception().is_undefined_or_null();
            self.close(has_exc);
        }
    }
}

impl From<ObjectIter> for JSObject {
    fn from(mut value: ObjectIter) -> Self {
        value.done = true; // avoid close on drop
        value.target.clone()
    }
}

impl From<ObjectIter> for JSValue {
    fn from(value: ObjectIter) -> Self {
        Self::Object(value.into())
    }
}

impl ObjectIter {
    /// `next_method_name` is not consumed
    pub fn new(ctx: &JSContext, target: JSObject, next_method_name: JSAtom) -> Result<Self, i32> {
        let next_method = if next_method_name != JS_ATOM_NULL {
            target.get_property(ctx, next_method_name)?
        } else {
            JSValue::UNDEFINED
        };

        Ok(Self {
            target,
            done: next_method.is_undefined(),
            next_method,
            ctx: ctx.clone(),
        })
    }

    /// apply `method` on `obj` to evaluate next iterate value
    pub(super) fn do_apply_next_method(
        ctx: &JSContext,
        obj: &JSObject,
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
                    let mut state = IteratorState::NotDone;
                    let cc =
                        CallContext::new(ctx, obj.clone().into(), args, nfunc.magic as _, None);
                    let ret = f(ctx, cc, &mut state)?;
                    return Ok((ret, state));
                }
                NativeFunctionType::CIteratorNext(f) => {
                    let mut pdone = 0;
                    let ret = f(ctx, obj.clone().into(), args, &mut pdone, nfunc.magic as _)
                        .check_exception()?;

                    return if let JSValue::Object(ret) = ret {
                        Ok((
                            ret.into(),
                            match pdone {
                                0 => IteratorState::NotDone,
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

        if let JSValue::Object(o) =
            JS_Call(ctx, method, &JSValue::Object(obj.clone()), args).check_exception()?
        {
            Ok((o.into(), IteratorState::NotParsed))
        } else {
            // bytecode function must return an object
            ctx.throw_type_error("iterator must return an object");
            Err(-1)
        }
    }

    /// get next value of iterator by calling specified `method`.
    /// returns: (next_value, is_iterator_done)
    pub(crate) fn do_next_with_method(
        ctx: &JSContext,
        obj: &JSObject,
        method: &JSValue,
        args: &[JSValue],
    ) -> Result<(JSValue, bool), i32> {
        match Self::do_apply_next_method(ctx, obj, method, args)? {
            (ret, state @ (IteratorState::NotDone | IteratorState::Done)) => {
                Ok((ret.into(), state == IteratorState::Done))
            }
            (ret, IteratorState::NotParsed) => {
                let done = ret
                    .get_property(ctx, JS_ATOM_done)?
                    .coerce_to_bool(ctx)
                    .unwrap_or(false);

                Ok((
                    if !done {
                        ret.get_property(ctx, JS_ATOM_value)?
                    } else {
                        JSValue::UNDEFINED
                    },
                    done,
                ))
            }
        }
    }

    /// pre-close before iterate is done, calling `return` method if available.
    pub(crate) fn do_close(
        ctx: &JSContext,
        obj: &JSObject,
        has_exception: bool,
    ) -> Result<(), i32> {
        let pending_exc = if has_exception {
            ctx.take_exception()
        } else {
            JSValue::UNDEFINED
        };

        if let Ok(return_method) = obj.get_property(ctx, JS_ATOM_return) {
            if !return_method.is_undefined_or_null() {
                let ret = JS_Call(ctx, &return_method, &obj.clone().into(), &[]);
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

    /// pre-close before iterate is done, calling `return` method if available.
    pub fn close(&mut self, has_exception: bool) -> Result<(), i32> {
        if !self.done {
            self.done = true;
            self.next_method = JSValue::UNDEFINED;
            Self::do_close(&self.ctx, &self.target, has_exception)
        } else if has_exception {
            Err(-1)
        } else {
            Ok(())
        }
    }
}

impl JSObject {
    /// get object iterator.
    pub fn iterator_obj(&self, ctx: &JSContext, is_async: bool) -> Result<JSObject, i32> {
        let method = if is_async {
            let m = self.get_property(ctx, JS_ATOM_Symbol_asyncIterator)?;
            if m.is_undefined_or_null() {
                let obj = self.apply_iterator_method(
                    ctx,
                    &self.get_property(ctx, JS_ATOM_Symbol_iterator)?,
                )?;
                let next_method = obj.get_property(ctx, JS_ATOM_next)?;
                return ctx
                    .create_object(ObjectDetail::ASYNC_FROM_SYNC_ITERATOR(
                        JSAsyncFromSyncIteratorData { obj, next_method },
                    ))
                    .map_err(|_| -1);
            } else {
                m
            }
        } else {
            self.get_property(ctx, JS_ATOM_Symbol_iterator)?
        };

        if !method.is_callable() {
            ctx.throw_type_error("value is not iterable");
            Err(-1)
        } else {
            self.apply_iterator_method(ctx, &method)
        }
    }

    pub fn iter(&self, ctx: &JSContext) -> Result<ObjectIter, i32> {
        ObjectIter::new(ctx, self.iterator_obj(ctx, false)?, JS_ATOM_next)
    }

    /// call Symbol.iterator() or Symbol.asyncIterator() method to get object iterator.
    fn apply_iterator_method(
        &self,
        ctx: &JSContext,
        iterator_method: &JSValue,
    ) -> Result<JSObject, i32> {
        if let JSValue::Object(o) =
            JS_Call(ctx, iterator_method, &JSValue::Object(self.clone()), &[]).check_exception()?
        {
            Ok(o)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }
}

impl JSValue {
    /// get object iterator.
    pub fn iterator_obj(&self, ctx: &JSContext, is_async: bool) -> Result<JSObject, i32> {
        if let Self::Object(o) = self {
            o.iterator_obj(ctx, is_async)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    pub fn iter(&self, ctx: &JSContext) -> Result<ObjectIter, i32> {
        if let Self::Object(o) = self {
            o.iter(ctx)
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
#[deprecated(note = "use IteratorObject::apply_next_method() instead")]
pub fn JS_IteratorNext2(
    ctx: &JSContext,
    enum_obj: &JSValue,
    method: &JSValue,
    args: &[JSValue],
    pdone: &mut i32,
) -> JSValue {
    unreachable!("unused");

    // if let JSValue::Object(p) = method
    //     && let ObjectDetail::C_FUNCTION(nfunc) = p.0.payload.borrow().deref()
    // {
    //     // fast path for the built-in iterators (avoid creating the
    //     // intermediate result object)
    //     match nfunc.handler {
    //         NativeFunctionType::IteratorNext(f) => {
    //             let cc = CallContext::new(ctx, enum_obj.clone(), args, nfunc.magic as _, None);
    //             let mut state = IteratorState::HasMore;
    //             let ret = f(ctx, cc, &mut state).unwrap_or(JSValue::EXCEPTION);
    //             *pdone = state as i32;
    //             return ret;
    //         }
    //         NativeFunctionType::CIteratorNext(f) => {
    //             let ret = f(ctx, enum_obj.clone(), args, pdone, nfunc.magic as _);
    //             return ret;
    //         }
    //         _ => {}
    //     }
    // }

    // let obj = JS_Call(ctx, method, enum_obj, args);
    // if obj.is_exception() {
    //     *pdone = 0;
    //     return JSValue::EXCEPTION;
    // }
    // if !obj.is_object() {
    //     ctx.throw_type_error("iterator must return an object");
    //     *pdone = 0;
    //     return JSValue::EXCEPTION;
    // }

    // *pdone = 2;
    // obj
}

#[deprecated(note = "use IteratorObject::next() instead")]
pub fn JS_IteratorNext(
    ctx: &JSContext,
    enum_obj: &JSValue,
    method: &JSValue,
    args: &[JSValue],
    pdone: &mut bool,
) -> JSValue {
    unreachable!("unused");
    // let mut done: i32 = 0;
    // let obj = JS_IteratorNext2(ctx, enum_obj, method, args, &mut done);
    // if obj.is_exception() {
    //     *pdone = false;
    //     return JSValue::EXCEPTION;
    // }

    // if done != 2 {
    //     *pdone = done != 0;
    //     return obj;
    // }

    // if let Ok(done_val) = obj.get_property(ctx, JS_ATOM_done) {
    //     *pdone = done_val.coerce_to_bool(ctx).unwrap_or(true);
    //     if !*pdone {
    //         obj.get_property(ctx, JS_ATOM_value)
    //             .unwrap_or(JSValue::EXCEPTION)
    //     } else {
    //         JSValue::UNDEFINED
    //     }
    // } else {
    //     *pdone = false;
    //     return JSValue::EXCEPTION;
    // }
}

/// TODO: migrate to Iterator::close()
/// or replace enum_obj: &JSValue with enum_obj: &JSObject
#[deprecated(note = "use IteratorObject::close() instead")]
pub fn JS_IteratorClose(
    ctx: &JSContext,
    enum_obj: &JSValue,
    has_exception: bool,
) -> Result<(), i32> {
    unreachable!("unused");
    // let pending_exc = if has_exception {
    //     ctx.take_exception()
    // } else {
    //     JSValue::UNDEFINED
    // };

    // let method = enum_obj
    //     .get_property(ctx, JS_ATOM_return)
    //     .inspect_err(|_| {
    //         // restore original error
    //         ctx.throw(pending_exc.clone());
    //     })?;

    // if !method.is_undefined_or_null() {
    //     let ret = JS_Call(ctx, &method, enum_obj, &[]);
    //     if !has_exception {
    //         if !ret.check_exception()?.is_object() {
    //             ctx.throw_type_error_not_an_object();
    //             return Err(-1);
    //         }
    //     }
    // }

    // if has_exception {
    //     ctx.throw(pending_exc);
    //     Err(-1)
    // } else {
    //     Ok(())
    // }
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
    let it = items.iter(ctx)?;
    let arr = ctx.create_array()?;
    for (i, v) in it.enumerate() {
        arr.define_property_value(ctx, JSAtom(i as u32), v?, JS_PROP_C_W_E | JS_PROP_THROW)?;
    }
    Ok(arr)
}

pub fn js_array_from_iterator(
    ctx: &JSContext,
    obj: &JSValue,
    method: &JSValue,
) -> Result<(JSValue, usize), i32> {
    let iter = ObjectIter::new(
        ctx,
        unsafe { obj.object_unchecked().apply_iterator_method(ctx, method)? },
        JS_ATOM_next,
    )?;

    let arr = ctx.create_array()?;
    let mut k = 0_i64;

    for val in iter {
        arr.create_data_property_u32(ctx, k, val?, JS_PROP_THROW)?;
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
