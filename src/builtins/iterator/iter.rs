// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::Cell, marker::PhantomData, ptr::NonNull};

use gc_lite::{GcHead, GcTrace, GcTraceCtx};

use crate::{
    Array,
    atom::Atom,
    builtins::js_get_this,
    context::{ContextPtr, ScopedContext},
    error::{Exception, JSResult},
    function::{CallContext, Callback, FieldDef, IteratorState},
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_SET_ENUM, JS_GPN_STR_OR_INT_MASK, JS_PROP_C_W_E,
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_THROW, Object, ObjectPayload,
        PropertyKey,
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
        let obj: Option<Object> = if let Some(o) = val.as_object() {
            Some(o)
        } else if val.is_undefined_or_null() {
            None
        } else {
            Some(val.objectify(ctx)?)
        };

        let mut it = Box::new(ForInIterPayload {
            obj,
            idx: 0,
            prop_count: 0,
            in_prototype_chain: false,
            is_array: false,
            prop_enums: Box::new([]),
        });

        if let Some(mut ob) = it.obj {
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
                let names =
                    ob.get_own_property_names(ctx, JS_GPN_STR_OR_INT_MASK | JS_GPN_SET_ENUM)?;
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
        mut obj: Object,
    ) -> Result<bool, i32> {
        // fast path: check if there are enumerable properties in shape proto chain
        let mut slow_path = false;

        for mut proto in obj.shape_proto_iter(true) {
            // count own property on proto
            let mut props_count: u32 = 0;
            proto.own_property_names_foreach(
                ctx,
                JS_GPN_STR_OR_INT_MASK | JS_GPN_ENUM_ONLY,
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
            if ctx.poll_interrupts() {
                return Err(-1);
            }
        }

        if slow_path {
            if self.is_array {
                let names =
                    obj.get_own_property_names(ctx, JS_GPN_STR_OR_INT_MASK | JS_GPN_SET_ENUM)?;
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
            let has_pending = self.ctx.rt().exception() != Exception::No;
            let _ = self.close();
            // close() via close_iter_internal:
            //   - If there WAS a pending exception: preserved and re-threw it
            //   - If there was NO pending exception and .return() failed: set a new one
            //
            // If there was a pending exception, keep it (caller will handle).
            // Only clear if close() itself generated a NEW error we can't propagate.
            if !has_pending && self.ctx.rt().exception() != Exception::No {
                self.ctx.rt_mut().clear_exception();
            }
        }
    }
}

impl<'a> Iterator for ObjectIter<'a> {
    type Item = JSResult;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }

        self.ctx
            .with_current_scope(|mut scope| {
                Self::fetch_next(&mut scope, self.target, self.next_method, &[])
            })
            .map(|(value, done)| {
                self.done = done;
                if done { None } else { Some(Ok(value)) }
            })
            .unwrap_or_else(|_| Some(Err(-1)))
    }
}

impl<'a> From<ObjectIter<'a>> for Object {
    fn from(mut value: ObjectIter) -> Self {
        value.done = true; // avoid close on drop
        value.target
    }
}

impl<'a> From<ObjectIter<'a>> for JSValue {
    fn from(iter: ObjectIter) -> Self {
        Self::from(Object::from(iter))
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
            let mut cnt = 0;
            let mut protects: [*mut GcHead; 2] = [std::ptr::null_mut(), std::ptr::null_mut()];

            let n = target.gc_node_ptr();
            let h = unsafe { n.as_ref() };
            if !h.is_local() && !h.is_root() {
                protects[cnt] = n.as_ptr();
                cnt += 1;
            }

            if let Some(n) = next_method.as_gc_node() {
                let h = unsafe { n.as_ref() };
                if !h.is_local() && !h.is_root() {
                    protects[cnt] = n.as_ptr();
                    cnt += 1;
                }
            }

            if cnt > 0 {
                let scope_stack_id = ctx.scope_stack_id();
                ctx.gc_heap_mut()
                    .with_current_scope_of(scope_stack_id, |s| {
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
        if let Some(o) = method.as_object()
            && let ObjectPayload::C_FUNCTION(nfun) = o.payload()
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
        let call_result = method.call(ctx, enum_obj.into(), args)?;
        if let Some(o) = call_result.as_object() {
            Ok((o.into(), IteratorState::NotParsed))
        } else {
            Err(ctx.throw_type_error("iterator must return an object"))
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
                let done_val = ret.get_property(ctx, Atom::CONST_done)?;
                let done = done_val.to_bool().unwrap_or(false);
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
    pub(crate) fn close_iter_internal(ctx: &mut ScopedContext, obj: Object) -> Result<(), i32> {
        debug_assert!(ctx.gc_heap().scope_max_depth(ctx.ctx_ptr.scope_stack_id()) > 0);

        let pending_exc = ctx.take_exception();

        // `ScopedContext::take_exception()` already added the error value
        // to current scope for GC protection. Assert it is indeed local.
        #[cfg(debug_assertions)]
        if let Exception::Error(v) = &pending_exc {
            debug_assert!(
                v.as_gc_node()
                    .is_none_or(|n| unsafe { n.as_ref().is_root_or_local() })
            );
        }

        let has_exception = pending_exc != Exception::No;

        if let Ok(method) = obj.get_property(ctx, Atom::CONST_return) {
            if !method.is_undefined_or_null() {
                let ret = method.call(ctx, obj.into(), &[]);
                if !has_exception && !ret?.is_object() {
                    return Err(ctx.throw_type_error_not_an_object());
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
    pub fn close(&mut self) -> Result<(), i32> {
        if !self.done {
            self.done = true;
            self.next_method = JSValue::UNDEFINED;
            self.ctx
                .with_current_scope(|mut scope| Self::close_iter_internal(&mut scope, self.target))
        } else if self.ctx.rt().exception() != Exception::No {
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
        ctx: &mut ScopedContext,
        is_async: bool,
    ) -> Result<Option<Object>, i32> {
        if is_async
            && let Some(r) = self.invoke_defined(ctx, Atom::CONST_Symbol_asyncIterator, &[])?
        {
            r.and_then(|v| v.ensure_object(ctx)).map(Some)
        } else if let Some(r) = self.invoke_defined(ctx, Atom::CONST_Symbol_iterator, &[])? {
            let sync_it = r.and_then(|v| v.ensure_object(ctx))?;
            if is_async {
                // method [Symbol.asyncIterator] is not defined:
                // wrap [Symbol.iterator]() object into an AsyncFromSyncIterator
                let next_method = sync_it.get_property(ctx, Atom::CONST_next)?;
                ctx.create_instance_with_payload(ObjectPayload::ASYNC_FROM_SYNC_ITERATOR(Box::new(
                    JSAsyncFromSyncIteratorData {
                        obj: sync_it,
                        next_method,
                        closed: Cell::new(false),
                    },
                )))
                .map(Some)
                .map_err(|_| -1)
            } else {
                Ok(Some(sync_it))
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
        self.get_iterator(ctx, is_async)?.ok_or_else(|| ctx.throw_type_error(if is_async {
                "value has neither [Symbol.asyncIterator] nor [Symbol.iterator] property"
            } else {
                "value has no [Symbol.iterator] property"
            }))
    }

    /// Return ObjectIter used to iterate, if `self` value has [Symbol.iterator] method.
    pub(crate) fn iter_maybe(
        &self,
        ctx: &mut ScopedContext,
    ) -> Result<Option<ObjectIter<'_>>, i32> {
        if let Some(target) = self.get_iterator(ctx, false)? {
            ObjectIter::new(ctx, target, Atom::CONST_next).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Return ObjectIter used to iterate. Assume `self` must have [Symbol.iterator] method.
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
pub(crate) fn make_iterator_next_result(
    ctx: &mut ScopedContext,
    value: JSValue,
    done: bool,
) -> JSResult {
    let mut obj = ctx.create_base_object()?;
    obj.define_property_value(ctx, Atom::CONST_value, value, JS_PROP_C_W_E)?;
    obj.define_property_value(ctx, Atom::CONST_done, JSValue::from(done), JS_PROP_C_W_E)?;
    Ok(obj.into())
}

pub(crate) fn iterator_to_array(ctx: &mut ScopedContext, items: JSValue) -> JSResult {
    let it = items.iter(ctx)?;
    let mut arr = Array::create(ctx)?;
    for (i, v) in it.enumerate() {
        arr.define_property_value_i64(ctx, i as i64, v?, JS_PROP_C_W_E | JS_PROP_THROW)?;
    }
    Ok(arr.into())
}

impl Array {
    /// create Array with values retreived from an iterable object.
    /// returns Array and length
    ///
    /// # Arguments
    /// * `ctx` - The context in which the array is created.
    /// * `iterable` - The iterable object.
    /// * `method` - The method to get the iterator from `iterable`.
    pub(crate) fn from_iterator(
        ctx: &mut ScopedContext,
        iterable: Object,
        method: JSValue,
    ) -> Result<(Self, usize), i32> {
        let target = method
            .call(ctx, iterable.into(), &[])
            .and_then(|v| v.ensure_object(ctx))?;

        let it = ObjectIter::new(ctx, target, Atom::CONST_next)?;

        let mut lst = Vec::<JSValue>::new();
        for val in it {
            lst.push(val?);
        }

        let len = lst.len();
        Ok((Array::from_values(ctx, lst)?, len))
    }
}

pub(crate) const js_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("[Symbol.iterator]", 0, js_get_this, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Iterator", JS_PROP_CONFIGURABLE),
];
