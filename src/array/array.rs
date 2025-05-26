use std::{ffi::c_void, mem::ManuallyDrop};

use crate::{
    atom::{consts::JS_ATOM_length, JSAtom, JS_AtomIsArrayIndex, JS_FreeAtom, JS_ATOM_NULL},
    context::JSContext,
    error::{JSErrorEnum, JSResult, JS_ThrowRangeError},
    gc::{JS_MarkFunc, JS_MarkValue},
    proxy::js_proxy_isArray,
    runtime::JSRuntime,
    typed_array::JSTypedArray,
    types::unlikely,
    value::{
        object::{
            JSObject, JSObjectIsa, JSProperty, JSPropertyUnion, JSShapeHandle,
            JS_PROP_CONFIGURABLE, JS_PROP_C_W_E, JS_PROP_LENGTH, JS_PROP_THROW, JS_PROP_WRITABLE,
        },
        set_value, JSTag, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT, JS_ToUint32,
        JS_TAG_INT, JS_TAG_OBJECT, JS_VALUE_GET_FLOAT64, JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG,
        JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_PROXY,
};

#[derive(Debug, Default)]
pub struct JSObjectArray {
    pub size: u32,
    pub values: Vec<JSValue>,
}

// TODO: replace with this
// pub enum JSObjectArray {
//     Empty,
//     Owned(Vec<JSValue>),
//     Borrowed {
//         ptr: NonNull<c_void>,
//         len: usize,
//         typed_array: *const JSTypedArray,
//     },
// }

#[derive(Clone, Copy)]
pub union JSObjectArrayU1 {
    /// for JS_CLASS_ARRAY, JS_CLASS_ARGUMENTS
    pub size: u32,

    /// for JS_CLASS_UINT8C_ARRAY..JS_CLASS_FLOAT64_ARRAY
    #[deprecated]
    pub typed_array: *mut JSTypedArray,
}

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
    #[inline(always)]
    pub fn create_array(&mut self) -> JSResult {
        let arr_sh: JSShapeHandle = unsafe { (*self.array_shape).dup().into() };

        self.create_object_from_shape_isa(arr_sh, || {
            Ok(JSObjectIsa::ARRAY(JSObjectArray {
                size: 0,
                values: Vec::new(),
            }))
        })
    }

    /// create a fast array value. `capacity` indicates reserved space.
    /// returns EXCEPTION value when fail, otherwise returns Array value
    pub fn create_fast_array(&mut self, capacity: usize) -> JSValue {
        if capacity > i32::MAX as usize {
            return self.JS_ThrowError(JSErrorEnum::Range, c"invalid array length");
        }

        let arr = if let Ok(v) = self.create_array() {
            v
        } else {
            return JSValue::EXCEPTION;
        };

        unsafe {
            // ensure is a fast array
            debug_assert!(arr.object_unchecked().detailed.fast_array());
        }

        if capacity > 0 {
            unsafe {
                if arr
                    .object_unchecked()
                    .fast_array_values_mut_unchecked()
                    .try_reserve_exact(capacity)
                    .is_err()
                {
                    JS_FreeValue(self, arr);
                    return JSValue::EXCEPTION;
                }
            }
        }

        arr
    }
}

impl JSObject {
    /// if object is Array or Argument class, and is fast array, returns values slice.
    pub fn fast_array_values(&self) -> Option<&[JSValue]> {
        if self.detailed.fast_array() {
            match self.isa() {
                JSObjectIsa::ARGUMENTS(a) | JSObjectIsa::ARRAY(a) => Some(a.values.as_slice()),
                _ => None,
            }
        } else {
            None
        }
    }

    /// if object is Array or Argument class, and is fast array, returns values mut.
    pub fn fast_array_values_mut(&mut self) -> Option<&mut Vec<JSValue>> {
        if self.detailed.fast_array() {
            match self.isa_mut() {
                JSObjectIsa::ARGUMENTS(a) | JSObjectIsa::ARRAY(a) => Some(&mut a.values),
                _ => None,
            }
        } else {
            None
        }
    }

    /// if object is Array or Argument class, and is fast array, returns values slice.
    /// SAFETY: must ensure `self` conforms precondition described above, otherwise behavior is undefined.
    pub unsafe fn fast_array_values_unchecked(&self) -> &[JSValue] {
        debug_assert!(self.detailed.fast_array());
        match self.isa() {
            JSObjectIsa::ARGUMENTS(a) | JSObjectIsa::ARRAY(a) => a.values.as_slice(),
            _ => unreachable!(),
        }
    }

    /// if object is Array or Argument class, and is fast array, returns values mut.
    /// SAFETY: must ensure `self` conforms precondition described above, otherwise behavior is undefined.
    #[inline(always)]
    pub unsafe fn fast_array_values_mut_unchecked(&mut self) -> &mut Vec<JSValue> {
        debug_assert!(self.detailed.fast_array());

        match self.isa_mut() {
            JSObjectIsa::ARGUMENTS(a) | JSObjectIsa::ARRAY(a) => &mut a.values,
            _ => unreachable!(),
        }
    }

    /// get array object length.
    /// will panic if `self` is not an Array, Argument, or TypedArray.
    pub unsafe fn array_length_unchecked(&self) -> usize {
        if let Some(array) = self.isa().array() {
            // ARRAY or ARGUMENT
            array.values.len()
        } else if let Some(ta) = self.isa().typed_array() {
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
    pub fn set_array_length(
        &mut self,
        ctx: &mut JSContext,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let len = unsafe { JS_ToArrayLengthFree(ctx, val, false)? as usize };

        // check if length property is writable?
        if self
            .shape()
            .and_then(|sh| {
                //prop `length` is #0
                debug_assert!(
                    sh.props[0].flags() & JS_PROP_LENGTH != 0,
                    "Object don't have length property"
                );
                sh.props.get_index(0)
            })
            .map(|(_, p)| p.flags())
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

        if self.detailed.fast_array() {
            let lst = unsafe { self.fast_array_values_mut_unchecked() };
            let old_len = lst.len();
            if len < old_len {
                for el in lst.drain(len..old_len) {
                    JS_FreeValue(ctx, el);
                }
            }
            self.prop_values[0] = JSProperty {
                u: JSPropertyUnion {
                    value: JSValue::from_u32(len as _),
                },
            };
        } else {
            // Note: length is always a uint32 because the object is an array
            let mut cur_len = self
                .prop_values
                .first()
                .map(|pr| unsafe { pr.u.value })
                .ok_or(-1)
                .and_then(|v| unsafe { JS_ToUint32(ctx, v) })
                .unwrap() as usize;

            if len < cur_len {
                let mut sh = self.shape().unwrap();
                let d = cur_len - len;

                if d <= sh.props.len() {
                    // faster to iterate
                    while cur_len > len {
                        let atom = ctx.rt_mut().add_atom_u32(cur_len as u32 - 1);
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

                    for pr in sh.props.values() {
                        let idx = 0_usize;
                        if pr.atom != JS_ATOM_NULL
                            && unsafe { JS_AtomIsArrayIndex(ctx, &mut (idx as u32), pr.atom) }
                        {
                            if idx >= cur_len && pr.flags() & JS_PROP_CONFIGURABLE == 0 {
                                cur_len = idx + 1;
                            }
                        }
                    }

                    for i in 0..sh.props.len() {
                        let (_, pr) = sh.props.get_index(i).unwrap();
                        let mut idx = 0_u32;
                        if pr.atom != JS_ATOM_NULL
                            && unsafe { JS_AtomIsArrayIndex(ctx, &mut idx, pr.atom) }
                        {
                            if idx as usize >= cur_len {
                                self.delete_property(ctx, pr.atom).unwrap();
                                // WARNING: the shape may have been modified
                                sh = self.shape().unwrap();
                            }
                        }
                    }
                }
            } else {
                cur_len = len;
            }

            unsafe {
                set_value(
                    ctx,
                    &mut self.prop_values.first_mut().unwrap().u.value, // #0 is the `length`
                    JSValue::from(cur_len as u32),
                );
            }

            if cur_len > len {
                return if ctx.JS_ThrowTypeErrorOrFalse(flags as _, c"not configurable") {
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
    /// test if value is fast array (could be one of Array, Argument, TypedArray fast array).
    pub fn is_fast_array(self) -> bool {
        if JS_VALUE_GET_TAG(self) == JS_TAG_OBJECT {
            unsafe { self.object_unchecked().detailed.fast_array() }
        } else {
            false
        }
    }

    /// if value is Array or Argument class, and is fast array, returns values slice.
    #[inline]
    pub fn fast_array_values<'a>(self) -> Option<&'a [JSValue]> {
        if JS_VALUE_GET_TAG(self) == JS_TAG_OBJECT {
            unsafe { self.object_unchecked().fast_array_values() }
        } else {
            None
        }
    }

    /// if value is Array or Argument class, and is fast array, returns values vec mut.
    #[inline]
    pub fn fast_array_values_mut<'a>(self) -> Option<&'a mut Vec<JSValue>> {
        if JS_VALUE_GET_TAG(self) == JS_TAG_OBJECT {
            unsafe { self.object_unchecked().fast_array_values_mut() }
        } else {
            None
        }
    }
}

// return Err if exception (proxy case) or TRUE/FALSE
pub unsafe fn JS_IsArray(ctx: *mut JSContext, val: JSValueConst) -> Result<bool, i32> {
    if JS_VALUE_GET_TAG(val) == JS_TAG_OBJECT {
        let p = JS_VALUE_GET_OBJ(val);
        if unlikely((*p).class_id() == JS_CLASS_PROXY) {
            js_proxy_isArray(ctx, val)
        } else {
            Ok((*p).class_id() == JS_CLASS_ARRAY)
        }
    } else {
        Ok(false)
    }
}

/// set the array length and remove the array elements if necessary.
/// returns 0,1,-1
#[deprecated(note = "use JSObject::set_array_length() instead")]
pub unsafe fn set_array_length(
    ctx: *mut JSContext,
    p: *mut JSObject,
    val: JSValue,
    flags: u32,
) -> i32 {
    (*p).set_array_length(ctx.as_mut().unwrap(), val, flags)
        .map_or(-1, |b| b as i32)
}

/// can be called on Array or Arguments objects. return < 0 if memory alloc error.
/// returns -1 if fail.
pub unsafe fn convert_fast_array_to_array(ctx: *mut JSContext, p: *mut JSObject) -> i32 {
    debug_assert!(
        (*p).detailed.fast_array()
            && matches!((*p).class_id(), JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS)
    );

    if (*p).shape_prepare_update(ctx, std::ptr::null_mut()) != 0 {
        return -1;
    }

    // /* TODO: resize the properties once to simplify the error handling */
    // let sh: *mut JSShape = (*p).shape;
    // let new_count = (*sh).prop_count as usize + len as usize;
    // if new_count > (*sh).prop_size as usize {
    //     if resize_properties(ctx.as_mut().unwrap(), &mut (*p).shape, p, new_count as u32) != 0 {
    //         return -1;
    //     }
    // }

    for (i, v) in (*p)
        .fast_array_values_mut_unchecked()
        .drain(..)
        .into_iter()
        .enumerate()
    {
        // add_property cannot fail here
        // but __JS_AtomFromUInt32(i) fails for i > i32::MAX
        let pr = (*p)
            .add_property(
                ctx.as_mut().unwrap(),
                JSAtom::from_u32(i as _), // int as property name
                JS_PROP_C_W_E,
            )
            .expect("FIXME: add_property error");
        (*pr).u.value = v;
    }

    (*p).detailed.set_fast_array(false);

    0
}

/// Preconditions: 'p' must be of class JS_CLASS_ARRAY, p is fast_array and p is extensible.
/// returns Err if fail, true if append successfully, false if can't append due to read-only but flags indicates don't throw exception.
/// Note: `val` is consumed.
pub unsafe fn add_fast_array_element(
    ctx: *mut JSContext,
    p: *mut JSObject,
    val: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    let lst = (*p).fast_array_values_mut_unchecked();

    // extend the array by one
    // XXX: convert to slow array if new_len > u32::MAX elements
    let new_len = lst.len() + 1;

    // first element in `prop_values` is the array length.
    // update the length if necessary. We assume that if the length is
    // not an integer, then if it >= 2^31.
    let pr0 = (*p).prop_values.first().unwrap();
    if JS_VALUE_GET_TAG(pr0.u.value) == JS_TAG_INT {
        let array_len = JS_VALUE_GET_INT(pr0.u.value) as usize;
        if new_len > array_len {
            if (*p)
                .shape()
                .and_then(|sh| sh.props.get_index(0))
                .map(|(_, p)| p.flags())
                .unwrap()
                & JS_PROP_WRITABLE
                == 0
            {
                JS_FreeValue(ctx, val);
                return if (*ctx).JS_ThrowTypeErrorReadOnly(flags, JS_ATOM_length) {
                    Err(-1)
                } else {
                    Ok(false)
                };
            }
            (*p).prop_values[0].u.value = JSValue::from(new_len as i32);
        }
    }

    if lst.len() == lst.capacity() {
        if lst.try_reserve(1).is_err() {
            JS_FreeValue(ctx, val);
            return Err(-1);
        }
    }

    lst.push(val);

    return Ok(true);
}

pub unsafe fn JS_ToArrayLengthFree(
    ctx: *mut JSContext,
    val: JSValue,
    is_array_ctor: bool,
) -> Result<u32, i32> {
    let len: u32;

    match JS_VALUE_GET_NORM_TAG(val) {
        JSTag::Int | JSTag::Bool | JSTag::Null => {
            let v = JS_VALUE_GET_INT(val);
            if v < 0 {
                // C2RS: goto fail replaced with direct return
                JS_ThrowRangeError(ctx, c"invalid array length");
                return Err(-1);
            }
            len = v as u32;
        }

        JSTag::Float64 => {
            let d = JS_VALUE_GET_FLOAT64(val);
            len = d as u32;
            if len as f64 != d {
                // f64 can't convert to u32
                JS_ThrowRangeError(ctx, c"invalid array length");
                return Err(-1);
            }
        }

        JSTag::BigInt => {
            unimplemented!()
        }
        _ if is_array_ctor => {
            let v = val.coerce_to_number_free(ctx.as_mut().unwrap())?;
            // won't recurse because v is a number
            len = JS_ToArrayLengthFree(ctx, v, true)?;
        }
        _ => {
            // legacy behavior: must do the conversion twice and compare
            match JS_ToUint32(ctx, val) {
                Ok(n) => {
                    len = n;
                }
                Err(_) => {
                    JS_FreeValue(ctx, val);
                    return Err(-1);
                }
            }

            let v = val.coerce_to_number_free(ctx.as_mut().unwrap())?;
            if len != JS_ToArrayLengthFree(ctx, v, false)? {
                JS_ThrowRangeError(ctx, c"invalid array length");
                return Err(-1);
            }
        }
    }

    Ok(len)
}

/// 将数组 obj 中从 from_pos 开始的 count 个元素复制到 to_pos 开始的位置。
/// - 正向复制（dir > 0）：从 from_pos 到 from_pos + count - 1 的元素依次复制到 to_pos 到 to_pos + count - 1。
/// - 反向复制（dir < 0）：从 from_pos + count - 1 到 from_pos 的元素依次复制到 to_pos + count - 1 到 to_pos。
pub unsafe fn JS_CopySubArray(
    ctx: *mut JSContext,
    obj: JSValueConst,
    to_pos: usize,
    from_pos: usize,
    count: usize,
    dir: i32,
) -> Result<(), i32> {
    let fast_array = if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
        let p0 = JS_VALUE_GET_OBJ(obj);
        if (*p0).class_id() != JS_CLASS_ARRAY || !(*p0).detailed.fast_array() {
            None
        } else {
            Some(p0)
        }
    } else {
        None
    };

    if let Some(fa) = fast_array {
        // `obj` is fast array
        let lst = (*fa).fast_array_values_mut_unchecked();
        let len = lst.len();

        // calc actual copy count
        let cnt1 = if from_pos + count > len {
            if len > from_pos {
                len - from_pos
            } else {
                0
            }
        } else {
            count
        };
        let cnt2 = if to_pos + count > len {
            if len > to_pos {
                len - to_pos
            } else {
                0
            }
        } else {
            count
        };
        let cnt = std::cmp::min(cnt1, cnt2);

        if cnt > 0 {
            if dir > 0 {
                for i in 0..cnt {
                    let v1 = lst[from_pos + i];
                    let v0 = lst[to_pos + i].replace(v1.dup());
                    JS_FreeValue(ctx, v0); // free old value
                }
            } else {
                for i in (0..cnt).rev() {
                    let v1 = lst[from_pos + i];
                    let v0 = lst[to_pos + i].replace(v1.dup());
                    JS_FreeValue(ctx, v0); // free old value
                }
            }
        }
    } else {
        // slow array
        for i in 0..count {
            match obj.try_get_property_i64(ctx.as_mut().unwrap(), (from_pos + i) as i64)? {
                Some(val) => {
                    obj.set_property_i64(ctx.as_mut().unwrap(), (to_pos + i) as _, val)?;
                }
                None => {
                    obj.delete_property_i64(
                        ctx.as_mut().unwrap(),
                        (to_pos + 1) as _,
                        JS_PROP_THROW,
                    )?;
                }
            }
        }
    }

    // let p = if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
    //     let p0 = JS_VALUE_GET_OBJ(obj);
    //     if (*p0).class_id() != JS_CLASS_ARRAY || !(*p0).detailed.fast_array() {
    //         std::ptr::null_mut()
    //     } else {
    //         p0
    //     }
    // } else {
    //     std::ptr::null_mut()
    // };

    // let mut from: i64;
    // let mut to: i64;
    // let mut len: i64;

    // let mut i = 0;
    // while i < count {
    //     if dir < 0 {
    //         from = from_pos + count - i - 1;
    //         to = to_pos + count - i - 1;
    //     } else {
    //         from = from_pos + i;
    //         to = to_pos + i;
    //     }

    //     len = (*p).u.array.count as i64;

    //     if !p.is_null()
    //         && (*p).detailed.fast_array()
    //         && from >= 0
    //         && from < len
    //         && to >= 0
    //         && to < len
    //     {
    //         // Fast path for fast arrays. Since we don't look at the
    //         // prototype chain, we can optimize only the cases where
    //         // all the elements are present in the array.

    //         let mut j: i64 = 0;
    //         let mut l = count - i;

    //         if dir < 0 {
    //             l = std::cmp::min(l, from + 1);
    //             l = std::cmp::min(l, to + 1);
    //             j = 0;
    //             while j < l {
    //                 set_value(
    //                     ctx,
    //                     (*p).u.array.data.values.offset((to - j) as isize),
    //                     JS_DupValue(ctx, *(*p).u.array.data.values.offset((from - j) as isize)),
    //                 );
    //                 j += 1;
    //             }
    //         } else {
    //             l = std::cmp::min(l, len - from);
    //             l = std::cmp::min(l, len - to);
    //             j = 0;
    //             while j < l {
    //                 set_value(
    //                     ctx,
    //                     (*p).u.array.data.values.offset((to + j) as isize),
    //                     JS_DupValue(ctx, *(*p).u.array.data.values.offset((from + j) as isize)),
    //                 );
    //                 j += 1;
    //             }
    //         }
    //         i += l;
    //     } else {
    //         match JS_TryGetPropertyInt64(ctx, obj, from)? {
    //             Some(val) => {
    //                 if JS_SetPropertyInt64(ctx, obj, to, val) < 0 {
    //                     return Err(-1);
    //                 }
    //             }
    //             None => {
    //                 JS_DeletePropertyInt64(ctx, obj, to, JS_PROP_THROW)?;
    //             }
    //         }
    //         i += 1;
    //     }
    // }

    Ok(())
}

pub unsafe fn js_create_array(ctx: *mut JSContext, len: i32, tab: *const JSValueConst) -> JSValue {
    if let Ok(obj) = (*ctx).create_array() {
        for i in 0..len {
            if obj
                .create_data_property_u32(
                    ctx.as_mut().unwrap(),
                    i as _,
                    (*tab.offset(i as isize)).dup(),
                    0,
                )
                .is_err()
            {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }

        obj
    } else {
        JSValue::EXCEPTION
    }
}

/// Access an Array's internal JSValue array if available
#[deprecated(note = "use JSValue::fast_array_mut() instead")]
pub unsafe fn js_get_fast_array(
    ctx: *const JSContext,
    mut obj: JSValueConst,
    arrpp: *mut *mut JSValue,
    countp: *mut u32,
) -> bool {
    if let Some(lst) = obj.fast_array_values_mut() {
        *countp = lst.len() as _;
        *arrpp = lst.as_mut_ptr();
        true
    } else {
        false
    }
}

#[deprecated(note = "use JSValue::is_fast_array() instead")]
pub unsafe fn js_is_fast_array(ctx: *const JSContext, obj: JSValueConst) -> bool {
    obj.is_fast_array()
}

pub unsafe fn js_array_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let p = JS_VALUE_GET_OBJ(val);
    if let Some(lst) = (*p).fast_array_values_mut() {
        for v in lst.drain(..) {
            JS_FreeValueRT(rt, v);
        }
    }
}

pub unsafe fn js_array_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    let p = JS_VALUE_GET_OBJ(val);
    if let Some(lst) = (*p).fast_array_values() {
        for v in lst {
            JS_MarkValue(rt, *v, mark_func);
        }
    }
}
