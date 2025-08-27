use std::collections::TryReserveError;

use fallible_collections::{FallibleVec, TryCollect};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_PROXY,
    atom::{JS_FreeAtom, JSAtom, consts::JS_ATOM_length},
    context::JSContext,
    error::ErrorKind,
    number::MAX_SAFE_INTEGER,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LENGTH, JS_PROP_THROW, JS_PROP_WRITABLE,
        JSObject, JSProperty, ObjectDetail,
    },
    object_cast,
    proxy::js_proxy_isArray,
    runtime::JSRuntime,
    value::JSValue,
};

#[derive(Debug, Default)]
pub struct ArrayObjPayload {
    slots: Vec<JSValue>,
}

impl ArrayObjPayload {
    pub fn with_capacity(cap: usize) -> Result<Self, TryReserveError> {
        Ok(Self {
            slots: Vec::<JSValue>::new(),
        })
    }

    pub fn new(data: Vec<JSValue>) -> Self {
        Self { slots: data }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct Array(pub(crate) JSObject);

object_cast!(Array, JS_CLASS_ARRAY);

impl JSContext {
    /// create an Array with element values
    pub fn create_array_with_values(&self, values: Vec<JSValue>) -> Result<Array, i32> {
        let len = values.len();
        debug_assert!(len <= MAX_SAFE_INTEGER as usize);

        let fa = self
            .create_object_with_shape(
                self.0.borrow().array_shape.clone().unwrap(),
                ObjectDetail::ARRAY(ArrayObjPayload { slots: values }),
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

    /// create an Array object with capacity.
    pub fn create_array(&self, capacity: usize) -> Result<Array, i32> {
        if capacity > u32::MAX as usize {
            self.throw_error(ErrorKind::Range, "invalid array length");
            return Err(-1);
        }
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

    /// if object `self` is fast array, convert it to sparse array.
    /// can be called on Array or Arguments objects only.
    pub(crate) fn destruct_fast_array(&self, ctx: &JSContext) -> Result<(), i32> {
        debug_assert!(matches!(
            self.class_id(),
            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS
        ));

        if self.is_fast_array() {
            self.shape_prepare_update(ctx)?;

            // clear dense array list, take values out
            let items = self
                .with_fast_array_values_mut(|lst| std::mem::replace(lst, Vec::new()))
                .unwrap();

            for (i, v) in items.into_iter().enumerate() {
                // add_property cannot fail here
                // but __JS_AtomFromUInt32(i) fails for i > i32::MAX
                self.add_own_property(
                    ctx,
                    JSAtom::new_u32(i as _), // int as property name
                    JS_PROP_C_W_E,
                    JSProperty::Normal(v),
                )?;
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
                .map(|a| f(a.slots.as_slice()))
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
                .map(|a| f(&mut a.slots))
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

    /// get array object length.
    /// will panic if `self` is not an Array, Argument, or TypedArray.
    pub fn array_length_unchecked(&self) -> usize {
        if let Some(array) = self.0.payload.borrow().array() {
            // ARRAY or ARGUMENT
            array.slots.len()
        } else if let Some(ta) = self.0.payload.borrow().typed_array() {
            // typed array or dataview
            ta.count as usize
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
    pub fn set_array_length(
        &self,
        ctx: &JSContext,
        len: u32,
        throw_error: bool,
    ) -> Result<bool, i32> {
        let len = len as usize;

        if self
            .shape()
            .and_then(|sh| {
                // prop `length` must be index 0
                debug_assert!(
                    sh.0.borrow().prop_defs[0].flags() & JS_PROP_LENGTH != 0,
                    "Object don't have length property"
                );
                sh.0.borrow().prop_defs.get_index(0).map(|(_, p)| p.flags())
            })
            .unwrap()
            & JS_PROP_WRITABLE
            == 0
        {
            // prop `length` is not writable
            return if ctx.throw_type_error_or_false(
                if throw_error { JS_PROP_THROW } else { 0 },
                "'length' is read-only",
            ) {
                Err(-1)
            } else {
                Ok(false)
            };
        }

        if self.flags().is_fast_array() {
            // fast array
            self.with_fast_array_values_mut(|lst| {
                let old_len = lst.len();
                if len < old_len {
                    lst.drain(len..old_len);
                }
            })
            .unwrap();
            self.0.prop_values.borrow_mut()[0] = JSProperty::Normal(JSValue::from(len as i64));
        } else {
            // Note: length is always a uint32 because the object is an array
            let mut arr_len = self
                .0
                .prop_values
                .borrow()
                .first()
                .map(|pr| pr.value(ctx, &JSValue::UNDEFINED))
                .unwrap() // `length` property must be defined in slot #0
                .and_then(|v| v.to_u32(ctx))? as usize;

            if len < arr_len {
                // delete index property
                let mut sh = self.shape().unwrap();
                let d = arr_len - len;
                let propdef_len = sh.0.borrow().prop_defs.len();

                if d <= propdef_len {
                    // faster to iterate
                    while arr_len > len {
                        let atom = JSAtom::from_u32(ctx.rt_mut(), arr_len as u32 - 1)?;
                        let ret = self.delete_property(ctx, atom);
                        JS_FreeAtom(ctx, atom);
                        if let Ok(false) = ret {
                            break; // property is not configurable
                        }
                        arr_len -= 1;
                    }
                } else {
                    // faster to iterate thru all the properties.
                    // Need two passes in case one of the property is not  configurable
                    arr_len = len;
                    sh.with_mut(|sh| {
                        for pr in sh.prop_defs.values() {
                            if let Some(i) = pr.atom.to_array_index(ctx.rt()) {
                                if i >= arr_len && (pr.flags() & JS_PROP_CONFIGURABLE == 0) {
                                    arr_len = i + 1;
                                }
                            }
                        }
                    });

                    // Hint: to avoid index conflict, do remove from tail to head
                    for i in (0..propdef_len).rev() {
                        let prop =
                            sh.0.borrow()
                                .prop_defs
                                .get_index(i)
                                .map(|(_, prs)| prs.atom)
                                .unwrap();

                        if let Some(idx) = prop.to_array_index(ctx.rt()) {
                            if idx >= arr_len {
                                self.delete_property(ctx, prop).unwrap();
                                // WARNING: the shape may have been modified
                                sh = self.shape().unwrap();
                            }
                        }
                    }
                }
            } else {
                arr_len = len;
            }

            // #0 is the `length`
            self.0.prop_values.borrow_mut()[0] = JSProperty::Normal(JSValue::from(arr_len as u32));

            if arr_len > len {
                return if ctx.throw_type_error_or_false(
                    if throw_error { JS_PROP_THROW } else { 0 },
                    "not configurable",
                ) {
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
    // return Err if exception (proxy case) or TRUE/FALSE
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
    #[inline]
    pub fn with_fast_array_values<R>(&self, f: impl FnOnce(&[JSValue]) -> R) -> Option<R> {
        self.object().and_then(|o| o.with_fast_array_values(f))
    }

    /// if value is Array or Argument class, and is a fast array,
    /// run closure with values slice,
    /// otherwise returns None.
    #[inline]
    pub fn with_fast_array_values_mut<R>(
        &self,
        f: impl FnOnce(&mut Vec<JSValue>) -> R,
    ) -> Option<R> {
        self.object().and_then(|o| o.with_fast_array_values_mut(f))
    }

    /// coerce value to valid array length.
    /// array length can't be greater than 2^32-1 (u32::MAX) per js specification.
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

    /// coerce value to valid array index.
    pub fn to_array_index(&self, ctx: &JSContext) -> Result<usize, i32> {
        // FIXME: array index no greater than 2^32-2, fix to use u32 type.
        // XXX
        let v: i64 = self.coerce_to_i64_saturating(ctx)?;
        if !(0..=MAX_SAFE_INTEGER).contains(&v) {
            ctx.throw_range_error("invalid array index");
            return Err(-1);
        }
        Ok(v as usize)
    }
}

/// 将数组 obj 中从 from_pos 开始的 count 个元素复制到 to_pos 开始的位置。
/// - 正向复制（dir > 0）：从 from_pos 到 from_pos + count - 1 的元素依次复制到 to_pos 到 to_pos + count - 1。
/// - 反向复制（dir < 0）：从 from_pos + count - 1 到 from_pos 的元素依次复制到 to_pos + count - 1 到 to_pos。
pub fn JS_CopySubArray(
    ctx: &JSContext,
    obj: &JSValue,
    to_pos: usize,
    from_pos: usize,
    count: usize,
    dir: i32,
) -> Result<(), i32> {
    let fast_array = if let JSValue::Object(p0) = obj {
        if p0.class_id() == JS_CLASS_ARRAY && p0.flags().is_fast_array() {
            Some(p0)
        } else {
            None
        }
    } else {
        None
    };

    if let Some(fast) = fast_array {
        // fast array
        fast.with_fast_array_values_mut(|lst| {
            let len = lst.len();

            // calc actual copy count
            let cnt1 = if from_pos + count > len {
                if len > from_pos { len - from_pos } else { 0 }
            } else {
                count
            };
            let cnt2 = if to_pos + count > len {
                if len > to_pos { len - to_pos } else { 0 }
            } else {
                count
            };
            let cnt = std::cmp::min(cnt1, cnt2);

            if cnt > 0 {
                if dir > 0 {
                    for i in 0..cnt {
                        let v1 = lst[from_pos + i].clone();
                        lst[to_pos + i] = v1;
                    }
                } else {
                    for i in (0..cnt).rev() {
                        let v1 = lst[from_pos + i].clone();
                        lst[to_pos + i] = v1;
                    }
                }
            }
        })
        .unwrap();
    } else {
        // slow array
        for i in 0..count {
            match obj.try_get_property_i64(ctx, (from_pos + i) as i64)? {
                Some(val) => {
                    obj.set_property_i64(ctx, (to_pos + i) as _, val)?;
                }
                None => {
                    obj.delete_property_i64(ctx, (to_pos + 1) as _, JS_PROP_THROW)?;
                }
            }
        }
    }

    Ok(())
}

pub fn js_array_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {}

pub fn js_array_mark(val: JSValue) {
    unsafe {
        val.object_unchecked().with_fast_array_values(|lst| {
            for v in lst {
                v.gc_mark();
            }
        });
    }
}
