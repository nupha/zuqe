use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
};

use fallible_collections::{FallibleVec, TryCollect};
use smallvec::SmallVec;

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_PROXY,
    atom::Atom,
    branch_hints::unlikely,
    context::JSContext,
    number::MAX_SAFE_INTEGER,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LENGTH,
        JS_PROP_THROW, JS_PROP_WRITABLE, JSObject, JSProperty, ObjectPayload, PropertyKeyKind,
    },
    object_cast,
    runtime::JSRuntime,
    string::StringContent,
    value::JSValue,
};

#[derive(Debug, Default)]
pub struct ArrayObjPayload<'a> {
    values: RefCell<Vec<JSValue<'a>>>,
}

impl<'a> ArrayObjPayload<'a> {
    pub const fn new(data: Vec<JSValue<'a>>) -> Self {
        Self {
            values: RefCell::new(data),
        }
    }
}

/// Object representing an Array, Arguments instance.
#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct Array<'a>(JSObject<'a>);

object_cast!(Array, JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS);

impl<'a> Array<'a> {
    #[inline(always)]
    pub fn is_fast_array(&self) -> bool {
        self.flags().is_fast_array()
    }

    /// get `length` property value
    pub fn get_length_prop(&self, ctx: &JSContext<'a>) -> Result<i64, i32> {
        if let JSProperty::Normal(v) = &self.0.0.prop_values.borrow()[0] {
            v.to_length(ctx)
        } else {
            #[cfg(debug_assertions)]
            unreachable!();
            #[cfg(not(debug_assertions))]
            unsafe {
                std::hint::unreachable_unchecked();
            }
        }
    }

    /// Update `length` property of an array-like object.
    ///
    /// Returns:
    /// - true if length is updated,
    /// - false if can't update length property and JS_THROW flag is not set,
    /// - Err if can't update length property ahd JS_THOW flag is set.
    pub fn set_length(&self, ctx: &JSContext<'a>, length: u32, throw: bool) -> Result<bool, i32> {
        #[cfg(debug_assertions)]
        debug_assert!(
            self.shape().unwrap().0.borrow().prop_defs[0].is_length(),
            "missing length property at index 0"
        );

        if !self.shape().unwrap().0.borrow().prop_defs[0].is_writable() {
            return if throw {
                ctx.throw_type_error("'length' is read-only");
                Err(-1)
            } else {
                Ok(false)
            };
        }

        if self.is_fast_array() {
            // fast array
            self.with_fast_array_values_mut(|lst| {
                let old_len = lst.len() as u32;
                if length < old_len {
                    lst.drain(length as usize..old_len as usize);
                }
            })
            .unwrap();
            self.0.0.prop_values.borrow_mut()[0] = JSProperty::Normal(JSValue::from(length));
        } else {
            // sparse array
            // Note: prop `length` must be less than u32::MAX
            let mut cur_len = self.get_length_prop(ctx)? as u32;

            if length < cur_len {
                // delete index property
                let mut sh = self.shape().unwrap();
                let d = cur_len - length;
                let propdef_len = sh.0.borrow().prop_defs.len() as u32;

                if d <= propdef_len {
                    // faster to iterate
                    while cur_len > length {
                        let atom = Atom::from_i64(ctx.rt().deref(), cur_len as i64 - 1)?;
                        let ret = self.delete_property_internal(ctx, &atom);
                        if let Ok(false) = ret {
                            break; // property is not configurable
                        }
                        cur_len -= 1;
                    }
                } else {
                    // faster to iterate thru all the properties.
                    // Need two passes in case one of the property is not  configurable
                    cur_len = length;
                    sh.with_mut(|sh| {
                        for pr in sh.prop_defs.values() {
                            if let Some(i) = pr.atom.to_array_index()
                                && i >= cur_len
                                && (pr.flags() & JS_PROP_CONFIGURABLE == 0)
                            {
                                cur_len = i + 1;
                            }
                        }
                    });

                    // Hint: to avoid index conflict, do remove from tail to head
                    for i in (0..propdef_len).rev() {
                        let prop =
                            sh.0.borrow()
                                .prop_defs
                                .get_index(i as usize)
                                .map(|(_, prs)| prs.atom.to_ref())
                                .unwrap();

                        if let Some(idx) = prop.to_array_index()
                            && idx >= cur_len
                        {
                            let _r = self.delete_property_internal(ctx, prop.deref());
                            debug_assert!(_r.is_ok());

                            // WARNING: the shape may have been modified
                            sh = self.shape().unwrap();
                        }
                    }
                }
            } else {
                cur_len = length;
            }

            unsafe {
                *self.0.0.prop_values.borrow_mut().get_unchecked_mut(0) =
                    JSProperty::Normal(JSValue::from(cur_len));
            }

            if cur_len > length {
                return if throw {
                    ctx.throw_type_error("not configurable");
                    Err(-1)
                } else {
                    Ok(false)
                };
            }
        }

        Ok(true)
    }

    /// set values list to Array, make `arr` a fast array.
    /// Note: for Array and Arguments class objects only.
    pub(crate) fn reset_fast_array(
        &self,
        ctx: &JSContext<'a>,
        values: Vec<JSValue<'a>>,
    ) -> Result<(), i32> {
        if !self.is_fast_array() {
            // if `self` is sparse array, delete all index properties
            let mut del = SmallVec::<[u32; 64]>::new();
            self.own_property_names_foreach(ctx, JS_GPN_INDEX_MASK | JS_GPN_ENUM_ONLY, |_, p| {
                if let PropertyKeyKind::ArrayIndex(i) = p.kind {
                    del.push(i);
                }
                Ok(())
            })?;

            for i in del {
                self.delete_property_i64(ctx, i as _, JS_PROP_THROW)?;
            }
        }

        let len = values.len();

        // use fast array values
        *self.payload().array().unwrap().values.borrow_mut() = values;

        // set fast array flag
        self.set_flag_fast_array(true);

        // set array length, can't fail
        debug_assert!(
            self.shape().unwrap().0.borrow().prop_defs[0].flags() & JS_PROP_LENGTH != 0,
            "Array.length property must be index 0"
        );
        self.0.0.prop_values.borrow_mut()[0] = JSProperty::Normal(JSValue::from(len as u32));

        Ok(())
    }

    /// sort array-like object `obj`.
    pub fn sort(
        ctx: &JSContext<'a>,
        obj: &JSObject<'a>,
        cmp: Option<&JSValue<'a>>,
    ) -> Result<(), i32> {
        if let Some(f) = cmp {
            f.ensure_function(ctx)?;
        }

        let arr_len = obj.get_length_prop(ctx)?;
        if unlikely(arr_len <= 1) {
            return Ok(());
        }

        // fast path
        if let Some(fa) = obj.with_fast_array_values_mut(|lst| -> Result<(), i32> {
            debug_assert!(lst.len() <= arr_len as usize);

            if let Some(cmp) = cmp {
                let mut slots = Vec::<JSValue>::new();
                slots.try_reserve_exact(lst.len()).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
                slots.extend(lst.iter().filter(|x| !x.is_undefined()).cloned());

                sort_values_by(ctx, &mut slots, cmp)?;

                let slots_len = slots.len();
                for (i, v) in slots.into_iter().enumerate() {
                    lst[i] = v;
                }

                lst.iter_mut()
                    .skip(slots_len)
                    .for_each(|x| *x = JSValue::UNDEFINED);
            } else {
                let mut slots = Vec::<(JSValue, StringContent)>::new();
                slots.try_reserve_exact(lst.len()).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

                for v in lst.iter().filter(|v| !v.is_undefined()) {
                    let v = v.clone();
                    let s = v.to_string(ctx)?;
                    slots.push((v, s.content().clone()));
                }

                sort_values_default(&mut slots);

                let slots_len = slots.len();
                for (i, (v, _)) in slots.into_iter().enumerate() {
                    lst[i] = v;
                }

                lst.iter_mut()
                    .skip(slots_len)
                    .for_each(|x| *x = JSValue::UNDEFINED);
            }

            Ok(())
        }) {
            return fa; // early exit for fast array
        }

        //
        // slow path: `obj` is sparse array.
        //
        let mut undefined_count = 0;
        let mut first_missing: Option<usize> = None;

        if let Some(cmp) = cmp {
            let mut slots = Vec::<JSValue>::new();
            slots.try_reserve_exact(arr_len as usize).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            for i in 0..arr_len {
                if let Some(v) = obj.try_get_property_i64(ctx, i)? {
                    if v.is_undefined() {
                        undefined_count += 1;
                    } else {
                        slots.push(v);
                    }
                } else if first_missing.is_none() {
                    first_missing.replace(i as usize);
                }
            }

            sort_values_by(ctx, slots.as_mut_slice(), cmp)?;

            let slots_len = slots.len() as i64;
            for (i, v) in slots.into_iter().enumerate() {
                obj.set_property_u32(ctx, i as u32, v)?;
            }
            if undefined_count > 0 {
                for i in slots_len..(slots_len + undefined_count) {
                    obj.set_property_u32(ctx, i as u32, JSValue::UNDEFINED)?;
                }
            }
            for i in (slots_len + undefined_count)..arr_len {
                obj.delete_property_i64(ctx, i, JS_PROP_THROW)?;
            }
        } else {
            let mut slots = Vec::<(JSValue, StringContent)>::with_capacity(16);
            slots.try_reserve_exact(arr_len as usize).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            for i in 0..arr_len {
                if let Some(v) = obj.try_get_property_i64(ctx, i)? {
                    if v.is_undefined() {
                        undefined_count += 1;
                    } else {
                        let s = v.to_string(ctx)?;
                        slots.push((v, s.content().clone()));
                    }
                } else if first_missing.is_none() {
                    first_missing.replace(i as usize);
                }
            }

            sort_values_default(slots.as_mut_slice());

            let slots_len = slots.len() as i64;
            for (i, (v, _)) in slots.into_iter().enumerate() {
                obj.set_property_u32(ctx, i as u32, v)?;
            }
            if undefined_count > 0 {
                for i in slots_len..(slots_len + undefined_count) {
                    obj.set_property_u32(ctx, i as u32, JSValue::UNDEFINED)?;
                }
            }
            for i in (slots_len + undefined_count)..arr_len {
                obj.delete_property_i64(ctx, i, JS_PROP_THROW)?;
            }
        }

        Ok(())
    }
}

fn sort_values_default(values: &mut [(JSValue, StringContent)]) {
    values.sort_by(|a, b| {
        // compare string content
        a.1.cmp(&b.1)
    });
}

fn sort_values_by<'a>(
    ctx: &JSContext<'a>,
    values: &mut [JSValue<'a>],
    cmpfn: &JSValue<'a>,
) -> Result<(), i32> {
    let mut has_err = false;

    values.sort_by(|a, b| {
        if !has_err {
            match cmpfn.call(ctx, &JSValue::UNDEFINED, &[a.clone(), b.clone()]) {
                Ok(r) => match r {
                    JSValue::Int(n) => {
                        if n == 0 {
                            std::cmp::Ordering::Equal
                        } else if n > 0 {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Less
                        }
                    }
                    r => {
                        if let Ok(f) = r.to_f64(ctx) {
                            if f == 0.0 {
                                std::cmp::Ordering::Equal
                            } else if f > 0.0 {
                                std::cmp::Ordering::Greater
                            } else {
                                std::cmp::Ordering::Less
                            }
                        } else {
                            has_err = true;
                            std::cmp::Ordering::Equal
                        }
                    }
                },
                Err(_) => {
                    has_err = true;
                    std::cmp::Ordering::Equal
                }
            }
        } else {
            std::cmp::Ordering::Equal
        }
    });

    if has_err { Err(-1) } else { Ok(()) }
}

impl<'a> JSContext<'a> {
    /// create an Array with element values
    pub fn create_array_with_values(&self, values: Vec<JSValue<'a>>) -> Result<Array<'a>, i32> {
        let len = values.len();

        let fa = self
            .create_object_with_shape(
                self.0.borrow().array_shape.clone().unwrap(),
                ObjectPayload::ARRAY(ArrayObjPayload {
                    values: RefCell::new(values),
                }),
            )
            .map(Array)
            .map_err(|_| -1)?;

        debug_assert!(fa.0.flags().is_exotic());
        debug_assert!(fa.0.flags().is_fast_array());

        // set array length
        if len > 0 {
            fa.set_length(self, len as u32, true)?;
        }

        Ok(fa)
    }

    #[inline]
    pub fn create_array_from_slice(&self, values: &[JSValue<'a>]) -> Result<Array<'a>, i32> {
        self.create_array_with_values(values.iter().cloned().try_collect::<Vec<_>>().map_err(
            |_| {
                self.throw_out_of_memory();
                -1
            },
        )?)
    }

    /// create an Array object with pre-allocated capacity.
    /// `cap` is allowed no greater than i32::MAX
    pub(super) fn create_array_with_capacity(&self, cap: usize) -> Result<Array<'a>, i32> {
        if cap <= i32::MAX as usize {
            let mut lst = Vec::<JSValue>::new();
            if cap > 0 {
                lst.try_reserve_exact(cap).map_err(|_| {
                    self.throw_out_of_memory();
                    -1
                })?;
            }
            let fa = self.create_array_with_values(lst)?;
            if cap > 0 {
                fa.set_length(self, cap as u32, true)?;
            }
            Ok(fa)
        } else {
            self.throw_range_error("invalid array length");
            Err(-1)
        }
    }

    /// create an empty Array
    #[inline(always)]
    pub fn create_array(&self) -> Result<Array<'a>, i32> {
        self.create_array_with_values(Vec::new())
    }
}

impl<'a> JSObject<'a> {
    /// return Err if exception (proxy case) or TRUE/FALSE
    pub fn is_array(&self, ctx: &JSContext<'a>) -> Result<bool, i32> {
        let class_id = self.class_id();
        if class_id == JS_CLASS_PROXY {
            unsafe { self.as_Proxy_unchecked().is_array(ctx) }
        } else {
            Ok(class_id == JS_CLASS_ARRAY)
        }
    }

    /// test if object is fast array (could be one of Array, Argument, TypedArray fast array).
    pub fn is_fast_array(&self) -> bool {
        self.flags().is_fast_array()
    }

    /// if `self` is a fast array, convert it to sparse array,
    /// valid elements (non UNINITIALIZED) will be added as property with integral name.
    ///
    /// Note: for Array or Arguments object only.
    pub(crate) fn destruct_fast_array(&self, ctx: &JSContext<'a>) -> Result<(), i32> {
        debug_assert!(matches!(
            self.class_id(),
            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS
        ));

        if self.is_fast_array() {
            self.shape_prepare_update(ctx)?;

            // take array list out
            let items = self.with_fast_array_values_mut(std::mem::take).unwrap();

            for (i, v) in items.into_iter().enumerate() {
                // add_property cannot fail here
                // but __JS_AtomFromUInt32(i) fails for i > i32::MAX
                //
                // Note: special JSValue::UNINITIALIZED is used to indicate not adding this as property
                if !v.is_uninitialized() {
                    self.add_own_property(
                        ctx,
                        Atom::new_small_int(i as _),
                        JS_PROP_C_W_E,
                        JSProperty::Normal(v),
                    )?;
                }
            }

            self.set_flag_fast_array(false);
        }

        Ok(())
    }

    /// if object is Array or Argument class, and is fast array, returns values slice.
    pub fn with_fast_array_values<R>(&self, f: impl FnOnce(&[JSValue<'a>]) -> R) -> Option<R> {
        if self.flags().is_fast_array() {
            self.payload()
                .array()
                .map(|a| f(a.values.borrow().as_slice()))
        } else {
            None
        }
    }

    /// if object is Array or Argument class, and is fast array, returns values slice.
    pub fn with_fast_array_values_mut<R>(
        &self,
        f: impl FnOnce(&mut Vec<JSValue<'a>>) -> R,
    ) -> Option<R> {
        if self.flags().is_fast_array() {
            self.payload()
                .array()
                .map(|a| f(a.values.borrow_mut().deref_mut()))
        } else {
            None
        }
    }

    /// Preconditions: 'self' must be of class JS_CLASS_ARRAY, self is fast_array and is extensible.
    /// returns Err if fail, true if append successfully, false if can't append due to read-only but flags indicates don't throw exception.
    pub fn add_fast_array_element(
        &self,
        ctx: &JSContext<'a>,
        val: JSValue<'a>,
        flags: u32,
    ) -> Result<bool, i32> {
        debug_assert!(self.flags().is_fast_array(), "must be a fast array");

        // extend the array by one
        // XXX: convert to slow array if new_len > u32::MAX elements
        let new_len = self.with_fast_array_values(|lst| lst.len() + 1).unwrap();

        // first element in `prop_values` is the array length.
        // update the length if necessary. We assume that if the length is
        // not an integer, then if it >= 2^31.
        // Note: check array length to int in advance, to avoid prop_values borrow conflict.
        let len: Option<i32> =
            if let JSProperty::Normal(JSValue::Int(n)) = &self.0.prop_values.borrow()[0] {
                Some(*n)
            } else {
                #[cfg(debug_assertions)]
                unreachable!();
                #[cfg(not(debug_assertions))]
                None
            };

        if let Some(arr_len) = len
            && new_len > arr_len as usize
        {
            let r = self
                .shape()
                .and_then(|sh| sh.0.borrow().prop_defs.get_index(0).map(|(_, p)| p.flags()))
                .unwrap();

            if unlikely(r & JS_PROP_WRITABLE == 0) {
                return if ctx.can_throw(flags) {
                    ctx.throw_type_error("'length' is read-only");
                    Err(-1)
                } else {
                    Ok(false)
                };
            }

            // update `length`
            self.0.prop_values.borrow_mut()[0] = JSProperty::Normal(JSValue::Int(new_len as i32));
        }

        // add new element to list
        self.with_fast_array_values_mut(|lst| -> Result<bool, i32> {
            lst.try_push(val).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            Ok(true)
        })
        .unwrap()
    }

    /// Get fast array list length:
    /// - for Array or Argument instance, return elements count (not property `length`);
    /// - for TypedArray or DataView instance, return elements count;
    /// - __Panic__ when object is not an Array, Argument, TypedArray or DataView.
    pub(crate) fn fast_array_count(&self) -> u32 {
        if let Some(ap) = self.payload().array() {
            // ARRAY or ARGUMENT
            ap.values.borrow().len() as u32
        } else if let Some(ta) = self.payload().typed_array() {
            // typed array or dataview
            ta.count() as _
        } else {
            unreachable!("expect argument, array, typed array or dataview")
        }
    }
}

impl<'a> JSValue<'a> {
    /// check if `self` is an Array object
    /// return Err if exception (proxy case) or TRUE/FALSE
    pub fn is_array(&self, ctx: &JSContext<'a>) -> Result<bool, i32> {
        if let JSValue::Object(p) = self {
            p.is_array(ctx)
        } else {
            Ok(false)
        }
    }

    /// test if value is fast array (could be one of Array, Argument, TypedArray fast array).
    pub fn is_fast_array(&self) -> bool {
        if let JSValue::Object(p) = self {
            p.is_fast_array()
        } else {
            false
        }
    }

    /// if value is Array or Argument class, and is fast array, returns values slice.
    #[inline(always)]
    pub fn with_fast_array_values<R>(&self, f: impl FnOnce(&[JSValue<'a>]) -> R) -> Option<R> {
        self.as_object().and_then(|o| o.with_fast_array_values(f))
    }

    /// if value is Array or Argument class, and is a fast array,
    /// run closure with values slice,
    /// otherwise returns None.
    #[inline(always)]
    pub fn with_fast_array_values_mut<R>(
        &self,
        f: impl FnOnce(&mut Vec<JSValue<'a>>) -> R,
    ) -> Option<R> {
        self.as_object()
            .and_then(|o| o.with_fast_array_values_mut(f))
    }

    /// coerce value to array length, which is in range \[0, 2^32-1\].
    pub fn to_array_length(&self, ctx: &JSContext<'a>, is_array_ctor: bool) -> Result<u32, i32> {
        let len: u32;

        match self {
            Self::NULL => {
                len = 0;
            }
            Self::Int(v) => {
                let n = *v;
                if n < 0 {
                    ctx.throw_range_error("invalid array length");
                    return Err(-1);
                }
                len = n as u32;
            }
            Self::Bool(b) => {
                len = *b as u32;
            }
            Self::Float64(d) => {
                len = *d as u32;
                if len as f64 != *d {
                    // f64 can't convert to u32
                    ctx.throw_range_error("invalid array length");
                    return Err(-1);
                }
            }

            _ => {
                if is_array_ctor {
                    let v = self.to_number(ctx)?;
                    // won't recurse because v is a number
                    len = v.to_array_length(ctx, true)?;
                } else {
                    // legacy behavior: must do the conversion twice and compare
                    len = self.to_u32(ctx)?;
                    let v = self.to_number(ctx)?;
                    if len != v.to_array_length(ctx, false)? {
                        ctx.throw_range_error("invalid array length");
                        return Err(-1);
                    }
                }
            }
        }

        Ok(len)
    }

    /// coerce value to array index, which is in range \[0, MAX_SAFE_INTEGER\].
    pub fn to_array_index(&self, ctx: &JSContext<'a>) -> Result<i64, i32> {
        let i: i64 = self.to_i64_saturating(ctx)?;
        if (0..=MAX_SAFE_INTEGER).contains(&i) {
            Ok(i)
        } else {
            ctx.throw_range_error("invalid array index");
            Err(-1)
        }
    }
}

pub const fn js_array_finalizer(_rt: &mut JSRuntime, _isa: &ObjectPayload, _id: usize) {}

pub fn js_array_mark(val: JSValue) {
    unsafe {
        val.as_object_unchecked().with_fast_array_values(|lst| {
            for v in lst {
                v.gc_mark();
            }
        });
    }
}
