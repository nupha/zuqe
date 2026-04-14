// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{marker::PhantomData, ptr::NonNull};

use gc_lite::{GcHead, GcTrace, GcTraceCtx};

use crate::{
    atom::Atom,
    builtins::js_get_this,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    function::{CallContext, Callback, FieldDef, IteratorState},
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_GPN_SET_ENUM, JS_PROP_C_W_E, JS_PROP_ENUMERABLE,
        JS_PROP_THROW, Object, ObjectPayload, PropertyKey,
    },
    value::JSValue,
};

#[cfg(not(feature = "builtin_symbol"))]
use crate::{
    ClassID, JS_CLASS_ARRAY, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, JS_CLASS_ASYNC_GENERATOR,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_STRING, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    builtins::{iterator::js_create_array_iterator, typed_array::js_create_typed_array_iterator},
    function::CallbackFn,
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
    pub(crate) obj: Option<Object>,
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

impl GcTrace for ForInIterPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Some(o) = self.obj {
            o.trace(tr)
        }
        self.prop_enums.iter().for_each(|x| x.trace(tr));
    }
}

impl ForInIterPayload {
    /// create new object with ForInIterPayload payload for given value.
    /// the new object is of CLASS_FOR_IN_ITERATOR class.
    pub fn create(ctx: &mut ScopedContext, val: JSValue) -> Result<Object, i32> {
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
                    .any(|(_, prs)| prs.flags() & JS_PROP_ENUMERABLE != 0)
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

        ctx.create_instance_with_proto_payload(None, ObjectPayload::FOR_IN_ITERATOR(it))
            .map_err(|_| -1)
    }

    /// build property-enumeration-list for `obj`.
    /// returns true if the enumeration is finished.
    pub(crate) fn prepare_prototype_chain_enum(
        &mut self,
        ctx: &mut ScopedContext,
        obj: Object,
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

pub struct ObjectIter<'a> {
    pub(crate) target: Object,
    pub(crate) next_method: JSValue,
    pub(crate) ctx: ContextPtr,
    pub(crate) done: bool,
    _mark: PhantomData<&'a ()>,
}

impl Drop for ObjectIter<'_> {
    fn drop(&mut self) {
        if !self.done {
            let has_exc = !self.ctx.rt().exception().is_undefined_or_null();
            let _ = self.close(has_exc);
        }
    }
}

impl<'a> Iterator for ObjectIter<'a> {
    type Item = JSResult;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            None
        } else if let Ok((value, done)) = self
            .ctx
            .with_current_scope(|mut scope| {
                let ret = Self::fetch_next(&mut scope, self.target, self.next_method, &[]);
                if let Ok((v, _)) = ret {
                    scope.set_return_value(v);
                }
                ret
            })
            .unwrap()
        {
            self.done = done;
            if done { None } else { Some(Ok(value)) }
        } else {
            Some(Err(-1))
        }
    }
}

impl<'a> From<ObjectIter<'a>> for Object {
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
    pub(crate) fn new(ctx: &mut ScopedContext, target: Object, next: Atom) -> Result<Self, i32> {
        let next_method = if !next.is_null() {
            target.get_property(ctx, next)?
        } else {
            JSValue::UNDEFINED
        };

        // protect target, next_method to avoid being collected while iterating
        {
            debug_assert!(ctx.gc_heap().scope_max_depth() > 0);

            let mut cnt = 0;
            let mut protects: [*mut GcHead; 2] = [std::ptr::null_mut(), std::ptr::null_mut()];

            let n = target.gc_node_ptr();
            let h = unsafe { n.as_ref() };
            if !h.is_local() && !h.is_root() {
                protects[cnt] = n.as_ptr();
                cnt += 1;
            }

            if let Some(n) = next_method.gc_node_ptr() {
                let h = unsafe { n.as_ref() };
                if !h.is_local() && !h.is_root() {
                    protects[cnt] = n.as_ptr();
                    cnt += 1;
                }
            }

            if cnt > 0 {
                ctx.gc_heap_mut().with_current_scope(|s| {
                    for n in protects.into_iter().take(cnt) {
                        s.add_non_local(unsafe { NonNull::new_unchecked(n) });
                    }
                });
            }
        }

        Ok(Self {
            target,
            next_method,
            done: next_method.is_undefined(),
            ctx: ctx.into(),
            _mark: PhantomData,
        })
    }

    /// calling `method` on `enum_obj` to iterate next value.
    pub(super) fn apply_next(
        ctx: &mut ScopedContext,
        enum_obj: Object,
        method: JSValue,
        args: &[JSValue],
    ) -> Result<(JSValue, IteratorState), i32> {
        if let Some(ObjectPayload::C_FUNCTION(nfun)) = method.as_object().map(|o| o.payload())
            && let Callback::IteratorNext(f) = nfun.callback
        {
            // fast path for the built-in iterators,
            // avoid creating the intermediate result object
            let mut state = IteratorState::NotDone;
            let ret = f(
                ctx,
                CallContext::new(enum_obj.into(), args, nfun.magic as _, None),
                &mut state,
            )?;

            return Ok((ret, state));
        }

        // generic case: call `method` - must return an object.
        if let JSValue::Object(o) = method.call(ctx, enum_obj.into(), args)? {
            Ok((o.into(), IteratorState::NotParsed))
        } else {
            ctx.throw_type_error("iterator must return an object");
            Err(-1)
        }
    }

    /// get next value of iterator by calling specified `method`.
    /// returns: (next_value, is_done)
    pub(crate) fn fetch_next(
        ctx: &mut ScopedContext,
        obj: Object,
        method: JSValue,
        args: &[JSValue],
    ) -> Result<(JSValue, bool), i32> {
        match Self::apply_next(ctx, obj, method, args)? {
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
    pub(crate) fn close_iter_internal(
        ctx: &mut ScopedContext,
        obj: Object,
        has_exception: bool,
    ) -> Result<(), i32> {
        debug_assert!(ctx.gc_heap().scope_max_depth() > 0);

        let pending_exc = if has_exception {
            ctx.rt_mut().take_exception()
        } else {
            JSValue::UNDEFINED
        };

        // prevent pending_exc from being collected
        {
            if let Some(n) = pending_exc.gc_node_ptr()
                && unsafe { !n.as_ref().is_root_or_local() }
            {
                // add to current scope to protect it.
                ctx.gc_heap_mut().with_current_scope(|s| {
                    s.add_non_local(n);
                });
            }
        }

        if let Ok(method) = obj.get_property(ctx, Atom::CONST_return) {
            if !method.is_undefined_or_null() {
                let ret = method.call(ctx, obj.into(), &[]);
                if !has_exception && !ret?.is_object() {
                    ctx.throw_type_error_not_an_object();
                    return Err(-1);
                }
            }
        } else if !has_exception {
            return Err(-1);
        }

        if has_exception {
            ctx.throw(pending_exc);
            Err(-1)
        } else {
            Ok(())
        }
    }

    /// pre-close before iterate is done, calling `return` method if available.
    pub fn close(&mut self, has_exception: bool) -> Result<(), i32> {
        if !self.done {
            self.done = true;
            self.next_method = JSValue::UNDEFINED;
            self.ctx
                .with_current_scope(|mut scope| {
                    Self::close_iter_internal(&mut scope, self.target, has_exception)
                })
                .unwrap()
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
    #[cfg(feature = "builtin_symbol")]
    pub fn get_iterator(
        &self,
        ctx: &mut ScopedContext,
        is_async: bool,
    ) -> Result<Option<Object>, i32> {
        if is_async {
            if let Some(it) = self.invoke_defined(ctx, Atom::CONST_Symbol_asyncIterator, &[])? {
                Object::try_from(it?)
                    .inspect_err(|_| {
                        ctx.throw_type_error_not_an_object();
                    })
                    .map(Some)
            } else {
                // method [Symbol.asyncIterator] is not defined:
                // get [Symbol.iterator], then wrap it into AsyncFromSyncIterator object.
                if let Some(r) = self.invoke_callable(ctx, Atom::CONST_Symbol_iterator, &[])? {
                    let it = Object::try_from(r?).inspect_err(|_| {
                        ctx.throw_type_error_not_an_object();
                    })?;
                    let next_method = it.get_property(ctx, Atom::CONST_next)?;

                    ctx.create_instance_with_payload(ObjectPayload::ASYNC_FROM_SYNC_ITERATOR(
                        Box::new(JSAsyncFromSyncIteratorData {
                            obj: it,
                            next_method,
                        }),
                    ))
                    .map(Some)
                    .map_err(|_| -1)
                } else {
                    Ok(None)
                }
            }
        } else if let Some(r) = self.invoke_callable(ctx, Atom::CONST_Symbol_iterator, &[])? {
            Object::try_from(r?)
                .inspect_err(|_| {
                    ctx.throw_type_error_not_an_object();
                })
                .map(Some)
        } else {
            Ok(None)
        }
    }

    /// Get `self`'s iterator object
    #[cfg(not(feature = "builtin_symbol"))]
    pub fn get_iterator(
        &self,
        ctx: &mut ContextImpl,
        is_async: bool,
    ) -> Result<Option<Object>, i32> {
        let it: Option<Object> = if self.is_string() {
            let r = js_create_array_iterator(CallContext::new(
                unsafe { ContextPtr::from_ref_unchecked(ctx) },
                *self,
                &[],
                (JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i32) | 4,
                None,
            ))?;
            Some(Object::try_from(r).inspect_err(|_| {
                ctx.throw_type_error_not_an_object();
            })?)
        } else if let Some(o) = self.as_object() {
            let class_id = o.class_id();

            if is_async
                && let Some((_, f, m)) = CLASS_ASYNC_ITERATOR_HANDLER
                    .iter()
                    .find(|(c, _, _)| *c == class_id)
            {
                let r = f(CallContext::new(
                    unsafe { ContextPtr::from_ref_unchecked(ctx) },
                    *self,
                    &[],
                    *m as _,
                    None,
                ))?;
                return Object::try_from(r)
                    .inspect_err(|_| {
                        ctx.throw_type_error_not_an_object();
                    })
                    .map(Some);
            }

            if let Some((_, f, m)) = CLASS_ITERATOR_HANDLER
                .iter()
                .find(|(c, _, _)| *c == class_id)
            {
                let r = f(CallContext::new(
                    unsafe { ContextPtr::from_ref_unchecked(ctx) },
                    *self,
                    &[],
                    *m as _,
                    None,
                ))?;
                Some(Object::try_from(r).inspect_err(|_| {
                    ctx.throw_type_error_not_an_object();
                })?)
            } else {
                None
            }
        } else {
            None
        };

        if let Some(it) = it {
            if is_async {
                // make AsyncFromSyncIterator
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
                Ok(Some(it))
            }
        } else {
            Ok(None)
        }
    }

    /// Same as [get_iterator()], but will raise 'not iterable' error if get_iterator() returns None.
    pub fn get_iterator_must(
        &self,
        ctx: &mut ScopedContext,
        is_async: bool,
    ) -> Result<Object, i32> {
        self.get_iterator(ctx, is_async)?.ok_or_else(|| {
            ctx.throw_type_error("value is not iterable");
            -1
        })
    }

    /// Return ObjectIter to iterate JSValue.
    pub(crate) fn iter(&self, ctx: &mut ScopedContext) -> Result<ObjectIter<'_>, i32> {
        let target = self.get_iterator_must(ctx, false)?;
        ObjectIter::new(ctx, target, Atom::CONST_next)
    }
}

pub fn JS_IteratorGetCompleteValue(
    ctx: &mut ScopedContext,
    obj: JSValue,
    pdone: &mut bool,
) -> JSValue {
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
pub(crate) fn js_create_iterator_result(
    ctx: &mut ScopedContext,
    value: JSValue,
    done: bool,
) -> JSResult {
    let mut obj = ctx.create_base_object()?;
    obj.define_property_value(ctx, Atom::CONST_value, value, JS_PROP_C_W_E)?;
    obj.define_property_value(ctx, Atom::CONST_done, JSValue::Bool(done), JS_PROP_C_W_E)?;
    Ok(obj.into())
}

pub(crate) fn iterator_to_array(ctx: &mut ScopedContext, items: JSValue) -> JSResult {
    let it = items.iter(ctx)?;
    let mut arr = ctx.create_array()?;
    for (i, v) in it.enumerate() {
        arr.define_property_value_i64(ctx, i as i64, v?, JS_PROP_C_W_E | JS_PROP_THROW)?;
    }
    Ok(arr.into())
}

pub(crate) fn js_array_from_iterator(
    ctx: &mut ScopedContext,
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

pub(crate) const js_iterator_proto_funcs: &[FieldDef] = &[
    #[cfg(feature = "builtin_symbol")]
    FieldDef::CALL("[Symbol.iterator]", 0, js_get_this, 0),
];

#[cfg(not(feature = "builtin_symbol"))]
const CLASS_ITERATOR_HANDLER: &[(ClassID, CallbackFn, i16)] = &[
    (
        JS_CLASS_STRING,
        js_create_array_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) | 4,
    ),
    (
        JS_CLASS_ARRAY,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    #[cfg(feature = "builtin_mapset")]
    (
        crate::JS_CLASS_MAP,
        crate::builtins::mapset::js_create_map_iterator,
        (JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16) << 2,
    ),
    #[cfg(feature = "builtin_mapset")]
    (
        crate::JS_CLASS_SET,
        crate::builtins::mapset::js_create_map_iterator,
        ((JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16) << 2) | 1,
    ),
    (
        JS_CLASS_UINT8C_ARRAY,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    (
        JS_CLASS_INT8_ARRAY,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    (
        JS_CLASS_UINT8_ARRAY,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    (
        JS_CLASS_INT16_ARRAY,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    (
        JS_CLASS_UINT16_ARRAY,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    (
        JS_CLASS_INT32_ARRAY,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    (
        JS_CLASS_UINT32_ARRAY,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    (
        JS_CLASS_FLOAT32_ARRAY,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    (
        JS_CLASS_FLOAT64_ARRAY,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
];

#[cfg(not(feature = "builtin_symbol"))]
const CLASS_ASYNC_ITERATOR_HANDLER: &[(ClassID, CallbackFn, i16)] = &[
    (JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, js_get_this, 0),
    (JS_CLASS_ASYNC_GENERATOR, js_get_this, 0),
];
