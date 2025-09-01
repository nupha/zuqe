use std::ops::DerefMut;

use fallible_collections::{FallibleVec, TryCollect};
use smallvec::SmallVec;

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_PROXY,
    atom::{
        JS_FreeAtom, JSAtom,
        consts::{JS_ATOM_empty_string, JS_ATOM_length},
    },
    context::JSContext,
    number::MAX_SAFE_INTEGER,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LENGTH,
        JS_PROP_THROW, JS_PROP_WRITABLE, JSObject, JSProperty, ObjectPayload, PropertyKeyKind,
    },
    object_cast,
    proxy::js_proxy_isArray,
    runtime::JSRuntime,
    string::StringContent,
    value::JSValue,
};

#[derive(Debug, Default)]
pub struct ArrayObjPayload {
    values: Vec<JSValue>,
}

impl ArrayObjPayload {
    pub fn new(data: Vec<JSValue>) -> Self {
        Self { values: data }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct Array(pub(crate) JSObject);

object_cast!(Array, JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS);

impl Array {
    /// set values list to Array, make `arr` a fast array.
    /// Note: for Array and Arguments class objects only.
    fn set_elements(ctx: &JSContext, obj: &JSObject, values: Vec<JSValue>) -> Result<(), i32> {
        matches!(obj.class_id(), JS_CLASS_ARGUMENTS | JS_CLASS_ARRAY);

        if !obj.flags().is_fast_array() {
            // if `self` is sparse array, delete all index properties
            let mut del = SmallVec::<[u32; 64]>::new();
            obj.own_property_names_foreach(ctx, JS_GPN_INDEX_MASK | JS_GPN_ENUM_ONLY, |_, p| {
                if let PropertyKeyKind::ArrayIndex(i) = p.isa {
                    del.push(i);
                }
                Ok(())
            })?;

            for i in del {
                obj.delete_property_i64(ctx, i as _, JS_PROP_THROW)?;
            }
        }

        let len = values.len();
        // use fast array list
        match obj.0.payload.borrow_mut().deref_mut() {
            ObjectPayload::ARRAY(p) | ObjectPayload::ARGUMENTS(p) => {
                p.values = values;
            }
            _ => unreachable!(),
        }

        // mark self is fast array
        unsafe {
            (&mut *obj.0.obj_flags.as_ptr()).set_fast_array(true);
        }

        // set array length, can't fail
        debug_assert!(
            obj.shape().unwrap().0.borrow().prop_defs[0].flags() & JS_PROP_LENGTH != 0,
            "Array.length property must be index 0"
        );
        obj.0.prop_values.borrow_mut()[0] = JSProperty::Normal(JSValue::from(len as u32));

        Ok(())
    }

    /// sort array-like object `obj`.
    pub fn sort(ctx: &JSContext, obj: &JSObject, cmp: Option<&JSValue>) -> Result<(), i32> {
        if let Some(f) = cmp {
            f.ensure_callable(ctx)?;
        }

        let arr_len = obj.get_length_prop(ctx)?;
        if arr_len <= 1 {
            return Ok(());
        }

        //
        // fast path
        //
        if let Some(fa) = obj
            .with_fast_array_values_mut(|lst| {
                if lst.len() == arr_len as usize {
                    if let Some(cmp) = cmp {
                        let mut slots = Vec::<JSValue>::new();
                        if let Err(e) = slots.try_reserve_exact(arr_len as usize).map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        }) {
                            return Some(Err(e));
                        }
                        slots.extend(lst.iter().filter(|x| !x.is_undefined()).cloned());

                        if sort_values_by(ctx, &mut slots, cmp).is_err() {
                            return Some(Err(-1));
                        }

                        let slots_len = slots.len();
                        for (i, v) in slots.into_iter().enumerate() {
                            lst[i] = v;
                        }
                        for i in slots_len..lst.len() {
                            lst[i] = JSValue::UNDEFINED;
                        }

                        Some(Ok(()))
                    } else {
                        let mut slots = Vec::<(JSValue, StringContent)>::new();
                        if let Err(e) = slots.try_reserve_exact(arr_len as usize).map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        }) {
                            return Some(Err(e));
                        }

                        for v in lst.iter().filter(|v| !v.is_undefined()) {
                            let v = v.clone();
                            if let Ok(s) = v.to_string(ctx) {
                                slots.push((v, s.content().clone()));
                            } else {
                                return Some(Err(-1));
                            }
                        }

                        sort_values_default(&mut slots);

                        let slots_len = slots.len();
                        for (i, (v, _)) in slots.into_iter().enumerate() {
                            lst[i] = v;
                        }
                        for i in slots_len..lst.len() {
                            lst[i] = JSValue::UNDEFINED;
                        }

                        Some(Ok(()))
                    }
                } else {
                    // must be an empty list with pre-defined length.
                    // for example, created by `Array(10)`
                    debug_assert!(lst.is_empty(), "{lst:?}, len={arr_len}");
                    None
                }
            })
            .flatten()
        {
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
            for i in (slots_len + undefined_count) as i64..arr_len {
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
            for i in (slots_len + undefined_count) as i64..arr_len {
                obj.delete_property_i64(ctx, i, JS_PROP_THROW)?;
            }
        }

        Ok(())
    }
}

fn sort_values_default(values: &mut [(JSValue, StringContent)]) {
    values.sort_by(|a, b| {
        // compare by string
        a.1.as_ref().cmp(b.1.as_ref())
    });
}

fn sort_values_by(ctx: &JSContext, values: &mut [JSValue], cmpfn: &JSValue) -> Result<(), i32> {
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

impl JSContext {
    /// create an Array with element values
    pub fn create_array_with_values(&self, values: Vec<JSValue>) -> Result<Array, i32> {
        let len = values.len();

        let fa = self
            .create_object_with_shape(
                self.0.borrow().array_shape.clone().unwrap(),
                ObjectPayload::ARRAY(ArrayObjPayload { values }),
            )
            .map(Array)
            .map_err(|_| -1)?;

        debug_assert!(fa.0.flags().is_exotic());
        debug_assert!(fa.0.flags().is_fast_array());

        // set array length
        if len > 0 {
            fa.0.set_array_length(self, len as u32, true)?;
        }

        Ok(fa)
    }

    #[inline]
    pub fn create_array_from_slice(&self, values: &[JSValue]) -> Result<Array, i32> {
        self.create_array_with_values(values.iter().cloned().try_collect::<Vec<_>>().map_err(
            |_| {
                self.throw_out_of_memory();
                -1
            },
        )?)
    }

    /// create an Array object with pre-allocated capacity.
    /// `cap` is allowed no greater than i32::MAX
    pub(super) fn create_array_with_capacity(&self, cap: usize) -> Result<Array, i32> {
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
                fa.0.set_array_length(self, cap as u32, true)?;
            }
            Ok(fa)
        } else {
            self.throw_range_error("invalid array length");
            Err(-1)
        }
    }

    /// create an empty Array
    #[inline(always)]
    pub fn create_array(&self) -> Result<Array, i32> {
        self.create_array_with_values(Vec::new())
    }
}

impl JSObject {
    // return Err if exception (proxy case) or TRUE/FALSE
    pub fn is_array(&self, ctx: &JSContext) -> Result<bool, i32> {
        let class_id = self.class_id();
        if class_id == JS_CLASS_PROXY {
            js_proxy_isArray(ctx, &JSValue::Object(self.clone()))
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
    pub(crate) fn destruct_fast_array(&self, ctx: &JSContext) -> Result<(), i32> {
        debug_assert!(matches!(
            self.class_id(),
            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS
        ));

        if self.is_fast_array() {
            self.shape_prepare_update(ctx)?;

            // take array list out
            let items = self
                .with_fast_array_values_mut(|lst| std::mem::replace(lst, Vec::new()))
                .unwrap();

            for (i, v) in items.into_iter().enumerate() {
                // add_property cannot fail here
                // but __JS_AtomFromUInt32(i) fails for i > i32::MAX
                //
                // Note: special JSValue::UNINITIALIZED is used to indicate not adding this as property
                if !v.is_uninitialized() {
                    self.add_own_property(
                        ctx,
                        JSAtom::new_small_int(i as _),
                        JS_PROP_C_W_E,
                        JSProperty::Normal(v),
                    )?;
                }
            }

            unsafe {
                (*self.0.obj_flags.as_ptr()).set_fast_array(false);
            }
        }

        Ok(())
    }

    /// if object is Array or Argument class, and is fast array, returns values slice.
    pub fn with_fast_array_values<R>(&self, f: impl FnOnce(&[JSValue]) -> R) -> Option<R> {
        if self.flags().is_fast_array() {
            self.0
                .payload
                .borrow()
                .array()
                .map(|a| f(a.values.as_slice()))
        } else {
            None
        }
    }

    /// if object is Array or Argument class, and is fast array, returns values slice.
    pub fn with_fast_array_values_mut<R>(
        &self,
        f: impl FnOnce(&mut Vec<JSValue>) -> R,
    ) -> Option<R> {
        if self.flags().is_fast_array() {
            self.0
                .payload
                .borrow_mut()
                .array_mut()
                .map(|a| f(&mut a.values))
        } else {
            None
        }
    }

    /// Preconditions: 'self' must be of class JS_CLASS_ARRAY, self is fast_array and is extensible.
    /// returns Err if fail, true if append successfully, false if can't append due to read-only but flags indicates don't throw exception.
    pub fn add_fast_array_element(
        &self,
        ctx: &JSContext,
        val: JSValue,
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
        let len = if let JSProperty::Normal(JSValue::Int(n)) = &self.0.prop_values.borrow()[0] {
            Some(*n)
        } else {
            // unreachable!();
            None
        };
        if let Some(arr_len) = len {
            if new_len > arr_len as usize {
                if self
                    .shape()
                    .and_then(|sh| sh.0.borrow().prop_defs.get_index(0).map(|(_, p)| p.flags()))
                    .unwrap()
                    & JS_PROP_WRITABLE
                    == 0
                {
                    return if ctx.JS_ThrowTypeErrorReadOnly(flags, JS_ATOM_length) {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                }

                // update `length`
                self.0.prop_values.borrow_mut()[0] =
                    JSProperty::Normal(JSValue::Int(new_len as i32));
            }
        }

        // add new value to list
        self.with_fast_array_values_mut(|lst| -> Result<bool, i32> {
            lst.try_push(val).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
            Ok(true)
        })
        .unwrap()
    }

    /// Get array length:
    /// - for Array or Argument instance, return elements count (not property `length`);
    /// - for TypedArray or DataView instance, return elements count;
    /// - __Panic__ when object is not an Array, Argument, TypedArray or DataView.
    pub(crate) fn fast_array_count(&self) -> u32 {
        if let Some(ap) = self.0.payload.borrow().array() {
            // ARRAY or ARGUMENT
            ap.values.len() as u32
        } else if let Some(ta) = self.0.payload.borrow().typed_array() {
            // typed array or dataview
            ta.count
        } else {
            unreachable!("expect argument, array, typed array or dataview")
        }
    }

    /// Update `length` property of an array-like object.
    ///
    /// Returns:
    /// - true if length is updated,
    /// - false if can't update length property and JS_THROW flag is not set,
    /// - Err if can't update length property ahd JS_THOW flag is set.
    pub fn set_array_length(&self, ctx: &JSContext, length: u32, throw: bool) -> Result<bool, i32> {
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

        if self.flags().is_fast_array() {
            // fast array
            self.with_fast_array_values_mut(|lst| {
                let old_len = lst.len() as u32;
                if length < old_len {
                    lst.drain(length as usize..old_len as usize);
                }
            })
            .unwrap();
            self.0.prop_values.borrow_mut()[0] = JSProperty::Normal(JSValue::from(length));
        } else {
            // sparse array
            // Note: prop `length` must be less than u32::MAX
            let mut old_len = self
                .0
                .prop_values
                .borrow()
                .first()
                .map(|pr| pr.value(ctx, &JSValue::UNDEFINED))
                .unwrap() // `length` property must be defined in slot #0
                .and_then(|v| v.to_u32(ctx))?;

            if length < old_len {
                // delete index property
                let mut sh = self.shape().unwrap();
                let d = old_len - length;
                let propdef_len = sh.0.borrow().prop_defs.len() as u32;

                if d <= propdef_len {
                    // faster to iterate
                    while old_len > length {
                        let atom = JSAtom::from_u32(ctx.rt_mut(), old_len as u32 - 1)?;
                        let ret = self.delete_property_internal(ctx, atom);
                        JS_FreeAtom(ctx, atom);
                        if let Ok(false) = ret {
                            break; // property is not configurable
                        }
                        old_len -= 1;
                    }
                } else {
                    // faster to iterate thru all the properties.
                    // Need two passes in case one of the property is not  configurable
                    old_len = length;
                    sh.with_mut(|sh| {
                        for pr in sh.prop_defs.values() {
                            if let Some(i) = pr.atom.to_array_index(ctx.rt()) {
                                if i >= old_len && (pr.flags() & JS_PROP_CONFIGURABLE == 0) {
                                    old_len = i + 1;
                                }
                            }
                        }
                    });

                    // Hint: to avoid index conflict, do remove from tail to head
                    for i in (0..propdef_len).rev() {
                        let prop =
                            sh.0.borrow()
                                .prop_defs
                                .get_index(i as usize)
                                .map(|(_, prs)| prs.atom)
                                .unwrap();

                        if let Some(idx) = prop.to_array_index(ctx.rt()) {
                            if idx >= old_len {
                                let _r = self.delete_property_internal(ctx, prop);
                                debug_assert!(_r.is_ok());

                                // WARNING: the shape may have been modified
                                sh = self.shape().unwrap();
                            }
                        }
                    }
                }
            } else {
                old_len = length;
            }

            self.0.prop_values.borrow_mut()[0] = JSProperty::Normal(JSValue::from(old_len as u32));

            if old_len > length {
                return if throw {
                    ctx.throw_type_error("'length' is not configurable");
                    Err(-1)
                } else {
                    Ok(false)
                };
            }
        }

        Ok(true)
    }
}

impl JSValue {
    /// check if `self` is an Array object
    /// return Err if exception (proxy case) or TRUE/FALSE
    pub fn is_array(&self, ctx: &JSContext) -> Result<bool, i32> {
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
    pub fn with_fast_array_values<R>(&self, f: impl FnOnce(&[JSValue]) -> R) -> Option<R> {
        self.object().and_then(|o| o.with_fast_array_values(f))
    }

    /// if value is Array or Argument class, and is a fast array,
    /// run closure with values slice,
    /// otherwise returns None.
    #[inline(always)]
    pub fn with_fast_array_values_mut<R>(
        &self,
        f: impl FnOnce(&mut Vec<JSValue>) -> R,
    ) -> Option<R> {
        self.object().and_then(|o| o.with_fast_array_values_mut(f))
    }

    /// coerce value to array length, which is in range \[0, 2^32-1\].
    pub fn to_array_length(&self, ctx: &JSContext, is_array_ctor: bool) -> Result<u32, i32> {
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
    pub fn to_array_index(&self, ctx: &JSContext) -> Result<i64, i32> {
        let i: i64 = self.to_i64_saturating(ctx)?;
        if (0..=MAX_SAFE_INTEGER).contains(&i) {
            Ok(i)
        } else {
            ctx.throw_range_error("invalid array index");
            Err(-1)
        }
    }
}

pub fn js_array_finalizer(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {}

pub fn js_array_mark(val: JSValue) {
    unsafe {
        val.object_unchecked().with_fast_array_values(|lst| {
            for v in lst {
                v.gc_mark();
            }
        });
    }
}
