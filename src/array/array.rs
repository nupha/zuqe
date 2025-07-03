use std::{ffi::c_void, mem::ManuallyDrop};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_PROXY,
    atom::{JS_FreeAtom, JSAtom, consts::JS_ATOM_length},
    context::JSContext,
    error::{JS_ThrowRangeError, JSErrorEnum, JSResult},
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LENGTH, JS_PROP_THROW, JS_PROP_WRITABLE,
        JSObject, JSObjectIsa, JSProperty,
    },
    proxy::js_proxy_isArray,
    runtime::JSRuntime,
    typed_array::JSTypedArray,
    value::{JS_ToUint32, JSValue, JSValueConst},
};

#[derive(Debug, Default)]
pub struct JSObjectArray {
    pub size: u32,
    pub values: Vec<JSValue>,
}

#[deprecated]
#[derive(Clone, Copy)]
pub union JSObjectArrayU1 {
    /// for JS_CLASS_ARRAY, JS_CLASS_ARGUMENTS
    pub size: u32,

    /// for JS_CLASS_UINT8C_ARRAY..JS_CLASS_FLOAT64_ARRAY
    #[deprecated]
    pub typed_array: *mut JSTypedArray,
}

#[deprecated]
pub union JSObjectArrayDataUnion {
    /// for JS_CLASS_ARRAY, JS_CLASS_ARGUMENTS. store element values.
    pub values: ManuallyDrop<Vec<JSValue>>,

    //
    // following are deprecated fields
    //
    /// for JS_CLASS_UINT8C_ARRAY..JS_CLASS_FLOAT64_ARRAY.
    /// pointer to start of ArrayBuffer buffer
    #[deprecated(note = "don't save TypedArray buf pointer, this will be removed")]
    pub ptr: *mut c_void,

    pub int8_ptr: *mut i8,    /* JS_CLASS_INT8_ARRAY */
    pub uint8_ptr: *mut u8,   /* JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY */
    pub int16_ptr: *mut i16,  /* JS_CLASS_INT16_ARRAY */
    pub uint16_ptr: *mut u16, /* JS_CLASS_UINT16_ARRAY */
    pub int32_ptr: *mut i32,  /* JS_CLASS_INT32_ARRAY */
    pub uint32_ptr: *mut u32, /* JS_CLASS_UINT32_ARRAY */
    pub int64_ptr: *mut i64,  /* JS_CLASS_INT64_ARRAY */
    pub uint64_ptr: *mut u64, /* JS_CLASS_UINT64_ARRAY */
    pub float_ptr: *mut f32,  /* JS_CLASS_FLOAT32_ARRAY */
    pub double_ptr: *mut f64, /* JS_CLASS_FLOAT64_ARRAY */
}

impl JSContext {
    /// create an array value.
    pub fn create_array(&self) -> JSResult {
        self.create_instance_from_shape(
            self.0.borrow().array_shape.clone().unwrap(),
            JSObjectIsa::ARRAY(JSObjectArray {
                size: 0,
                values: Vec::new(),
            }),
        )
        .map_err(|_| -1)
    }

    /// create a fast array value. `capacity` indicates reserved space.
    /// returns Array value
    pub fn create_fast_array(&self, capacity: usize) -> JSResult {
        if capacity > i32::MAX as usize {
            self.JS_ThrowError(JSErrorEnum::Range, "invalid array length");
            return Err(-1);
        }

        let arr = self.create_array()?;

        unsafe {
            // ensure it is a fast array
            debug_assert!(arr.object_unchecked().flags().fast_array());
        }

        if capacity > 0 {
            unsafe {
                arr.object_unchecked()
                    .with_fast_array_values_mut(|lst| lst.try_reserve_exact(capacity))
                    .unwrap()
                    .map_err(|_| -1)?;
            }
        }

        Ok(arr)
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
        self.flags().fast_array()
    }

    /// if object is Array or Argument class, and is fast array, returns values slice.
    pub fn with_fast_array_values<R>(&self, f: impl FnOnce(&[JSValue]) -> R) -> Option<R> {
        if self.flags().fast_array() {
            self.0.isa.borrow().array().map(|a| f(a.values.as_slice()))
        } else {
            None
        }
    }

    /// if object is Array or Argument class, and is fast array, returns values slice.
    pub fn with_fast_array_values_mut<R>(
        &self,
        f: impl FnOnce(&mut Vec<JSValue>) -> R,
    ) -> Option<R> {
        if self.flags().fast_array() {
            self.0
                .isa
                .borrow_mut()
                .array_mut()
                .map(|a| f(&mut a.values))
        } else {
            None
        }
    }

    /// Preconditions: 'p' must be of class JS_CLASS_ARRAY, p is fast_array and p is extensible.
    /// returns Err if fail, true if append successfully, false if can't append due to read-only but flags indicates don't throw exception.
    /// Note: `val` is consumed.
    pub fn add_fast_array_element(
        &self,
        ctx: &JSContext,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        // extend the array by one
        // XXX: convert to slow array if new_len > u32::MAX elements
        let new_len = self.with_fast_array_values(|lst| lst.len() + 1).unwrap();

        // first element in `prop_values` is the array length.
        // update the length if necessary. We assume that if the length is
        // not an integer, then if it >= 2^31.
        // Note: check array length to int in advance, to avoid prop_values borrow conflict.
        let len = if let JSProperty::Normal(JSValue::Int(len)) = &self.0.prop_values.borrow()[0] {
            Some(*len)
        } else {
            // unreachable!();
            None
        };
        if let Some(cur_len) = len {
            if new_len > cur_len as usize {
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
                // update to new length
                self.0.prop_values.borrow_mut()[0] =
                    JSProperty::Normal(JSValue::Int(new_len as i32));
            }
        }

        // add to value list
        self.with_fast_array_values_mut(|lst| -> Result<bool, i32> {
            if lst.len() == lst.capacity() {
                lst.try_reserve(1).map_err(|_| -1)?;
            }
            lst.push(val);
            Ok(true)
        })
        .unwrap()
    }

    /// get array object length.
    /// will panic if `self` is not an Array, Argument, or TypedArray.
    pub fn array_length_unchecked(&self) -> usize {
        if let Some(array) = self.0.isa.borrow().array() {
            // ARRAY or ARGUMENT
            array.values.len()
        } else if let Some(ta) = self.0.isa.borrow().typed_array() {
            // typed array or dataview
            ta.count as usize
        } else {
            unreachable!("expect argument, array, typed array or dataview")
        }
    }

    /// set the length property of an array-like object.
    /// returns
    /// - true if length is updated,
    /// - false if can't update length property and JS_THROW flag is not set,
    /// - Err if can't update length property ahd JS_THOW flag is set.
    pub fn set_array_length(&self, ctx: &JSContext, val: JSValue, flags: u32) -> Result<bool, i32> {
        let len = unsafe { JS_ToArrayLengthFree(ctx, val, false)? as usize };

        // check if length property is writable?
        if self
            .shape()
            .and_then(|sh| {
                //prop `length` is #0
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
            // not writable
            return if ctx.JS_ThrowTypeErrorReadOnly(flags, JS_ATOM_length) {
                Err(-1)
            } else {
                Ok(false)
            };
        }

        if self.flags().fast_array() {
            // is fast array.
            self.with_fast_array_values_mut(|lst| {
                let old_len = lst.len();
                if len < old_len {
                    lst.drain(len..old_len);
                }
            })
            .unwrap();
            self.0.prop_values.borrow_mut()[0] = JSProperty::Normal(JSValue::from_u32(len as _));
        } else {
            // Note: length is always a uint32 because the object is an array
            let mut cur_len = self
                .0
                .prop_values
                .borrow()
                .first()
                .map(|pr| pr.value(ctx, &JSValue::UNDEFINED))
                .ok_or(-1)
                .and_then(|v| unsafe { JS_ToUint32(ctx, &v) })
                .unwrap() as usize;

            if len < cur_len {
                // shrink
                let mut sh = self.shape().unwrap();
                let d = cur_len - len;

                if d <= sh.0.borrow().prop_defs.len() {
                    // faster to iterate
                    while cur_len > len {
                        let atom = ctx.rt_mut().register_atom_u32(cur_len as u32 - 1);
                        let ret = self.delete_property(ctx, atom);
                        JS_FreeAtom(ctx, atom);
                        if let Ok(false) = ret {
                            break; // property is not configurable
                        }
                        cur_len -= 1;
                    }
                } else {
                    // faster to iterate thru all the properties.
                    // Need two passes in case one of the property is not  configurable
                    cur_len = len;

                    sh.with_mut(|sh| {
                        for pr in sh.prop_defs.values() {
                            if let Some(idx) = pr.atom.to_array_index(ctx.rt()) {
                                if idx >= cur_len && pr.flags() & JS_PROP_CONFIGURABLE == 0 {
                                    cur_len = idx + 1;
                                }
                            }
                        }
                    });

                    let num_props = sh.0.borrow().prop_defs.len();
                    for i in 0..num_props {
                        let prop =
                            sh.0.borrow()
                                .prop_defs
                                .get_index(i)
                                .map(|(_, prs)| prs.atom)
                                .unwrap();

                        if let Some(idx) = prop.to_array_index(ctx.rt()) {
                            if idx >= cur_len {
                                self.delete_property(ctx, prop).unwrap();
                                // WARNING: the shape may have been modified
                                sh = self.shape().unwrap();
                            }
                        }
                    }
                }
            } else {
                cur_len = len;
            }

            // #0 is the `length`
            self.0.prop_values.borrow_mut()[0] = JSProperty::Normal(JSValue::from(cur_len as u32));

            if cur_len > len {
                return if ctx.JS_ThrowTypeErrorOrFalse(flags as _, "not configurable") {
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
}

/// can be called on Array or Arguments objects. return < 0 if memory alloc error.
/// returns -1 if fail.
pub unsafe fn convert_fast_array_to_array(ctx: &JSContext, obj: &JSObject) -> i32 {
    debug_assert!(obj.with(|p| {
        p.detailed.get().fast_array() && matches!(p.class_id(), JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS)
    }));

    if obj.shape_prepare_update(ctx).is_err() {
        return -1;
    }

    // clear fast array list, take values out as `items`
    let items = obj
        .with_fast_array_values_mut(|lst| std::mem::replace(lst, Vec::new()))
        .unwrap();

    for (i, v) in items.into_iter().enumerate() {
        // add_property cannot fail here
        // but __JS_AtomFromUInt32(i) fails for i > i32::MAX
        obj.add_own_property_then(
            ctx,
            JSAtom::new_u32(i as _), // int as property name
            JS_PROP_C_W_E,
            |pr| {
                *pr = JSProperty::Normal(v);
            },
        )
        .expect("FIXME: add_property error");
    }

    unsafe {
        (*obj.0.detailed.as_ptr()).set_fast_array(false);
    }
    0
}

pub unsafe fn JS_ToArrayLengthFree(
    ctx: &JSContext,
    val: JSValue,
    is_array_ctor: bool,
) -> Result<u32, i32> {
    let len: u32;

    match val {
        JSValue::NULL => {
            len = 0;
        }
        JSValue::Int(v) => {
            if v < 0 {
                JS_ThrowRangeError(ctx, "invalid array length");
                return Err(-1);
            }
            len = v as u32;
        }
        JSValue::Bool(b) => {
            len = b as u32;
        }
        JSValue::Float64(d) => {
            len = d as u32;
            if len as f64 != d {
                // f64 can't convert to u32
                JS_ThrowRangeError(ctx, "invalid array length");
                return Err(-1);
            }
        }

        _ if is_array_ctor => {
            let v = val.coerce_to_number_free(ctx)?;
            // won't recurse because v is a number
            len = JS_ToArrayLengthFree(ctx, v, true)?;
        }
        _ => {
            // legacy behavior: must do the conversion twice and compare
            len = JS_ToUint32(ctx, &val)?;
            let v = val.coerce_to_number_free(ctx)?;
            if len != JS_ToArrayLengthFree(ctx, v, false)? {
                JS_ThrowRangeError(ctx, "invalid array length");
                return Err(-1);
            }
        }
    }

    Ok(len)
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
        if p0.class_id() == JS_CLASS_ARRAY && p0.flags().fast_array() {
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
                        lst[to_pos + i].replace(v1);
                    }
                } else {
                    for i in (0..cnt).rev() {
                        let v1 = lst[from_pos + i].clone();
                        lst[to_pos + i].replace(v1);
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

pub unsafe fn js_create_array(ctx: &JSContext, len: i32, tab: *const JSValueConst) -> JSValue {
    if let Ok(obj) = ctx.create_array() {
        for i in 0..len {
            if obj
                .create_data_property_u32(ctx, i as _, (*tab.offset(i as isize)).clone(), 0)
                .is_err()
            {
                return JSValue::EXCEPTION;
            }
        }

        obj
    } else {
        JSValue::EXCEPTION
    }
}

pub fn js_array_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {}

pub fn js_array_mark(val: JSValue) {
    unsafe {
        val.object_unchecked().with_fast_array_values(|lst| {
            for v in lst {
                v.gc_mark();
            }
        });
    }
}
