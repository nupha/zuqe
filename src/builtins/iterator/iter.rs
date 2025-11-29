use std::marker::PhantomData;

use gc_lite::Collect;

use crate::{
    atom::Atom,
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState, NativeFunctionType},
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_GPN_SET_ENUM, JS_PROP_C_W_E, JS_PROP_ENUMERABLE,
        JS_PROP_THROW, ObjectPayload, ObjectRef, PropertyKey,
    },
    value::JSValue,
};

use super::JSAsyncFromSyncIteratorData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSIteratorKindEnum {
    JS_ITERATOR_KIND_KEY,
    JS_ITERATOR_KIND_VALUE,
    JS_ITERATOR_KIND_KEY_AND_VALUE,
}

#[derive(Debug)]
pub struct ForInIterPayload {
    pub(crate) obj: Option<ObjectRef>,
    /// obj is an array?
    pub(crate) is_array: bool,

    pub(crate) in_prototype_chain: bool,

    /// current iterate index
    pub(crate) idx: u32,

    /// enumerable props list
    pub(crate) prop_enums: Box<[PropertyKey]>,
    /// for fast array, this equals length of array.
    /// for non fast array, this equals to prop_enums.len()
    pub(crate) prop_count: u32,
}

unsafe impl Collect for ForInIterPayload {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.obj.trace(tracer);
        self.prop_enums.trace(tracer);
    }
}

impl ForInIterPayload {
    /// create new object with ForInIterPayload payload for given value.
    /// the new object is of CLASS_FOR_IN_ITERATOR class.
    pub fn create(ctx: &ContextImpl, val: JSValue) -> Result<ObjectRef, i32> {
        let obj = match val {
            JSValue::Object(o) => Some(o),
            JSValue::NULL | JSValue::UNDEFINED => None,
            _ => Some(val.objectify(ctx)?),
        };
        let mut it = Box::new(ForInIterPayload {
            obj,
            idx: 0,
            prop_count: 0,
            in_prototype_chain: false,
            is_array: false,
            prop_enums: Box::new([]),
        });

        if let Some(ob) = it.obj {
            if ob.flags().is_fast_array() {
                // check that there are no enumerable normal fields
                if !ob
                    .shape()
                    .unwrap()
                    .prop_defs
                    .iter()
                    .any(|prs| prs.flags() & JS_PROP_ENUMERABLE != 0)
                {
                    // no enumerable property, is fast array.
                    // we only store the number of elements
                    it.is_array = true;
                    it.prop_count = ob.fast_array_count();
                }
            }

            if !it.is_array {
                // obj is not an array: query own property names list
                let names = ob.get_own_property_names(ctx, JS_GPN_INDEX_MASK | JS_GPN_SET_ENUM)?;
                it.prop_count = names.len() as _;
                it.prop_enums = names;
            }
        }

        ctx.create_object_with_proto(None, ObjectPayload::FOR_IN_ITERATOR(it))
            .map_err(|_| -1)
    }

    /// build property-enumeration-list for `obj`.
    /// returns true if the enumeration is finished.
    pub(crate) fn prepare_prototype_chain_enum(
        &mut self,
        ctx: &ContextImpl,
        obj: ObjectRef,
    ) -> Result<bool, i32> {
        // fast path: check if there are enumerable properties in shape proto chain
        let mut slow_path = false;

        for proto in obj.shape_proto_iter(true) {
            // count own property on proto
            let mut props_count: u32 = 0;
            proto.own_property_names_foreach(
                ctx,
                JS_GPN_INDEX_MASK | JS_GPN_ENUM_ONLY,
                |_, _| {
                    props_count += 1;
                    Ok(())
                },
            )?;

            if props_count != 0 {
                slow_path = true;
                break;
            }

            // avoid infinite loop
            ctx.poll_interrupts()?;
        }

        if slow_path {
            if self.is_array {
                let names = obj.get_own_property_names(ctx, JS_GPN_INDEX_MASK | JS_GPN_SET_ENUM)?;
                self.is_array = false;
                self.prop_count = names.len() as _;
                self.prop_enums = names;
            }
            Ok(false)
        } else {
            // searched till top of prototype chain, enumeration is done
            Ok(true)
        }
    }
}

#[derive(Clone)]
pub struct ObjectIter<'a> {
    pub(crate) target: ObjectRef,
    pub(crate) next_method: JSValue,
    pub(crate) ctx: ContextRef,
    pub(crate) done: bool,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Iterator for ObjectIter<'a> {
    type Item = JSResult;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else if let Ok((value, done)) =
            Self::fetch_next(&self.ctx, self.target, self.next_method, &[])
        {
            self.done = done;
            if done { None } else { Some(Ok(value)) }
        } else {
            Some(Err(-1))
        }
    }
}

impl Drop for ObjectIter<'_> {
    fn drop(&mut self) {
        if !self.done {
            let has_exc = !self.ctx.rt().exception().is_undefined_or_null();
            let _ = self.close(has_exc);
        }
    }
}

impl<'a> From<ObjectIter<'a>> for ObjectRef {
    fn from(mut value: ObjectIter) -> Self {
        value.done = true; // avoid close on drop
        value.target
    }
}

impl<'a> From<ObjectIter<'a>> for JSValue {
    fn from(value: ObjectIter) -> Self {
        Self::Object(value.into())
    }
}

impl<'a> ObjectIter<'a> {
    /// `next` is the name of method used to fetch the __next__ value when iterating.
    /// this atom is not consumed.
    pub fn new(ctx: &'a ContextImpl, target: ObjectRef, next: Atom) -> Result<Self, i32> {
        let next_method = if !next.is_null() {
            target.get_property(ctx, next)?
        } else {
            JSValue::UNDEFINED
        };

        Ok(Self {
            target,
            done: next_method.is_undefined(),
            next_method,
            ctx: unsafe { ContextRef::from_ref_unchecked(ctx) },
            _mark: PhantomData,
        })
    }

    /// apply `method` on `obj` to evaluate next iterate value
    pub(super) fn do_apply_next_method(
        ctx: &ContextImpl,
        obj: ObjectRef,
        method: JSValue,
        args: &[JSValue],
    ) -> Result<(JSValue, IteratorState), i32> {
        if let JSValue::Object(p) = method
            && let ObjectPayload::C_FUNCTION(nfun) = p.payload()
        {
            // fast path for the built-in iterators (avoid creating the
            // intermediate result object)
            match nfun.handler {
                NativeFunctionType::IteratorNext(f) => {
                    let mut state = IteratorState::NotDone;
                    let ctx_ptr = unsafe { ContextRef::from_ref_unchecked(ctx) };
                    let cc = CallContext::new(ctx_ptr, obj.into(), args, nfun.magic as _, None);
                    let ret = f(ctx_ptr, cc, &mut state)?;
                    return Ok((ret, state));
                }
                NativeFunctionType::CIteratorNext(f) => {
                    let mut pdone = 0;
                    let ret = f(ctx, obj.into(), args, &mut pdone, nfun.magic as _).err()?;

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

        if let JSValue::Object(o) = method.call(ctx, JSValue::Object(obj), args)? {
            Ok((o.into(), IteratorState::NotParsed))
        } else {
            ctx.throw_type_error("iterator must return an object");
            Err(-1)
        }
    }

    /// get next value of iterator by calling specified `method`.
    /// returns: (next_value, is_done)
    pub(crate) fn fetch_next(
        ctx: &ContextImpl,
        obj: ObjectRef,
        method: JSValue,
        args: &[JSValue],
    ) -> Result<(JSValue, bool), i32> {
        match Self::do_apply_next_method(ctx, obj, method, args)? {
            (ret, state @ (IteratorState::NotDone | IteratorState::Done)) => {
                Ok((ret, state == IteratorState::Done))
            }
            (ret, IteratorState::NotParsed) => {
                let done = ret
                    .get_property(ctx, Atom::CONST_done)?
                    .to_bool()
                    .unwrap_or(false);

                Ok((
                    if !done {
                        ret.get_property(ctx, Atom::CONST_value)?
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
        ctx: &ContextImpl,
        obj: ObjectRef,
        has_exception: bool,
    ) -> Result<(), i32> {
        let pending_exc = if has_exception {
            ctx.rt().take_exception()
        } else {
            JSValue::UNDEFINED
        };

        if let Ok(return_method) = obj.get_property(ctx, Atom::CONST_return) {
            if !return_method.is_undefined_or_null() {
                let ret = return_method.call(ctx, obj.into(), &[])?;
                if pending_exc.is_undefined_or_null() && !ret.is_object() {
                    ctx.throw_type_error_not_an_object();
                    return Err(-1);
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
                ctx.throw(pending_exc);
            }
            Err(-1)
        }
    }

    /// pre-close before iterate is done, calling `return` method if available.
    pub fn close(&mut self, has_exception: bool) -> Result<(), i32> {
        if !self.done {
            self.done = true;
            self.next_method = JSValue::UNDEFINED;
            Self::do_close(&self.ctx, self.target, has_exception)
        } else if has_exception {
            Err(-1)
        } else {
            Ok(())
        }
    }
}

impl JSValue {
    /// Get `self`'s iterator object by calling `[Symbol.iterator]` or `[Symbol.asyncIterator]` method.
    ///
    /// - In case when is_async=true and method `[Symbol.asyncIterator]` is not defined,
    ///   try wrapping `[Symbol.iterator]` iterator (if any) into AsyncFromSyncIterator object.
    ///
    /// - If neither `[Symbol.iterator]` nor `[Symbol.asyncIterator]` are defined,
    ///   returns None indicating that `self` is not iterable.
    pub fn get_iterator(
        &self,
        ctx: &ContextImpl,
        is_async: bool,
    ) -> Result<Option<ObjectRef>, i32> {
        if is_async {
            if let Some(it) = self.invoke_defined(ctx, Atom::CONST_Symbol_asyncIterator, &[])? {
                ObjectRef::try_from(it?)
                    .inspect_err(|_| {
                        ctx.throw_type_error_not_an_object();
                    })
                    .map(Some)
            } else {
                // method [Symbol.asyncIterator] is not defined:
                // get [Symbol.iterator], then wrap it into AsyncFromSyncIterator object.
                if let Some(r) = self.invoke_callable(ctx, Atom::CONST_Symbol_iterator, &[])? {
                    let it = ObjectRef::try_from(r?).inspect_err(|_| {
                        ctx.throw_type_error_not_an_object();
                    })?;
                    let next_method = it.get_property(ctx, Atom::CONST_next)?;

                    ctx.create_object(ObjectPayload::ASYNC_FROM_SYNC_ITERATOR(Box::new(
                        JSAsyncFromSyncIteratorData {
                            obj: it,
                            next_method,
                        },
                    )))
                    .map(Some)
                    .map_err(|_| -1)
                } else {
                    Ok(None)
                }
            }
        } else if let Some(r) = self.invoke_callable(ctx, Atom::CONST_Symbol_iterator, &[])? {
            ObjectRef::try_from(r?)
                .inspect_err(|_| {
                    ctx.throw_type_error_not_an_object();
                })
                .map(Some)
        } else {
            Ok(None)
        }
    }

    /// Same as [get_iterator()], but will raise 'not iterable' error if get_iterator() returns None.
    pub fn get_iterator_must(&self, ctx: &ContextImpl, is_async: bool) -> Result<ObjectRef, i32> {
        self.get_iterator(ctx, is_async)?.ok_or_else(|| {
            ctx.throw_type_error("value is not iterable");
            -1
        })
    }

    /// Return an object that implements `Iterator` trait to iterate JSValue.
    /// The iterator uses `next()` method to retrive next value.
    #[inline]
    pub fn iter<'a>(&self, ctx: &'a ContextImpl) -> Result<ObjectIter<'a>, i32> {
        let target = self.get_iterator_must(ctx, false)?;
        ObjectIter::new(ctx, target, Atom::CONST_next)
    }
}

pub fn JS_IteratorGetCompleteValue(ctx: &ContextImpl, obj: JSValue, pdone: &mut bool) -> JSValue {
    if let Ok(done_val) = obj.get_property(ctx, Atom::CONST_done) {
        let done = done_val.to_bool().unwrap();

        if let Ok(value) = obj.get_property(ctx, Atom::CONST_value) {
            *pdone = done;
            value
        } else {
            *pdone = false;
            JSValue::EXCEPTION
        }
    } else {
        *pdone = false;
        JSValue::EXCEPTION
    }
}

/// create an Object with two properties: `value` and `done`.
pub fn js_create_iterator_result(ctx: &ContextImpl, value: JSValue, done: bool) -> JSResult {
    let mut obj = ctx.create_base_object()?;
    obj.define_property_value(ctx, Atom::CONST_value, value, JS_PROP_C_W_E)?;
    obj.define_property_value(ctx, Atom::CONST_done, JSValue::Bool(done), JS_PROP_C_W_E)?;
    Ok(obj.into())
}

pub fn iterator_to_array(ctx: &ContextImpl, items: &JSValue) -> JSResult {
    let it = items.iter(ctx)?;
    let mut arr = ctx.create_array()?;
    for (i, v) in it.enumerate() {
        arr.define_property_value_i64(ctx, i as i64, v?, JS_PROP_C_W_E | JS_PROP_THROW)?;
    }
    Ok(arr.into())
}

pub fn js_array_from_iterator(
    ctx: &ContextImpl,
    obj: JSValue,
    method: JSValue,
) -> Result<(JSValue, usize), i32> {
    if let JSValue::Object(obj) = method.call(ctx, obj, &[])? {
        let it = ObjectIter::new(ctx, obj, Atom::CONST_next)?;
        let mut arr = ctx.create_array()?;

        let mut k = 0_i64;
        for val in it {
            arr.define_property_value_i64(ctx, k, val?, JS_PROP_C_W_E | JS_PROP_THROW)?;
            k += 1;
        }
        Ok((arr.into(), k as usize))
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_iterator_proto_iterator(_: ContextRef, cc: CallContext) -> JSResult {
    Ok(*cc.this())
}

pub static js_iterator_proto_funcs: [FieldDef; 1] = [FieldDef::FUNCTION(
    "[Symbol.iterator]",
    0,
    js_iterator_proto_iterator,
    0,
)];
