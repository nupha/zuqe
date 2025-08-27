use fallible_collections::{FallibleVec, TryCollect};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_PROXY,
    atom::{JS_FreeAtom, JSAtom, consts::JS_ATOM_length},
    context::JSContext,
    number::MAX_SAFE_INTEGER,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LENGTH, JS_PROP_THROW, JS_PROP_WRITABLE,
        JSObject, JSProperty, ObjectPayload,
    },
    object_cast,
    proxy::js_proxy_isArray,
    runtime::JSRuntime,
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

object_cast!(Array, JS_CLASS_ARRAY);

impl JSContext {
    /// create an Array with element values
    pub fn create_array_with_values(&self, values: Vec<JSValue>) -> Result<Array, i32> {
        let len = values.len();
        debug_assert!(len <= u32::MAX as usize);

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

    /// create an empty Array
    #[inline(always)]
    pub fn create_array(&self) -> Result<Array, i32> {
        self.create_array_with_values(Vec::new())
    }

    // /// create a sparse Array object with capacity.
    // pub(super) fn create_sparse_array(&self, capacity: usize) -> Result<Array, i32> {
    //     debug_assert!(capacity > 0);
    //     if capacity <= MAX_SAFE_INTEGER as usize {
    //         let fa = self.create_array_with_values(Vec::new())?;
    //         if capacity > 0 {
    //             fa.0.set_array_length(self, capacity as u32, true)?;
    //         }
    //         Ok(fa)
    //     } else {
    //         self.throw_range_error("invalid array length");
    //         Err(-1)
    //     }
    // }
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
                    JSAtom::new_small_int(i as _), // int as property name
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
    pub(crate) fn array_elements_count(&self) -> usize {
        if let Some(ap) = self.0.payload.borrow().array() {
            // ARRAY or ARGUMENT
            ap.values.len()
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
                            if let Some(i) = pr.atom.to_index(ctx.rt()) {
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

                        if let Some(idx) = prop.to_index(ctx.rt()) {
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
