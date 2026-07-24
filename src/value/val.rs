// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcHead, GcTrace, GcTraceCtx};

use crate::{JS_CLASS_ERROR, JSValue, ScopedContext, object::Object, utils::unlikely};

impl GcTrace for JSValue {
    #[inline(always)]
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Some(p) = self.as_gc_node() {
            tr.add_node(p);
        }
    }
}

impl Eq for JSValue {}

impl std::hash::Hash for JSValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u32(self.hash());
    }
}

impl JSValue {
    pub fn is_htmldda(&self) -> bool {
        self.as_object().is_some_and(|o| o.flags().is_htmldda())
    }

    /// Test if value `self` is a callable object
    #[inline]
    pub fn is_callable(&self) -> bool {
        self.as_object().is_some_and(|o| o.is_callable())
    }

    pub fn is_error(&self) -> bool {
        self.as_object()
            .is_some_and(|o| o.class_id() == JS_CLASS_ERROR)
    }

    #[inline(always)]
    pub fn gc_node(&self) -> Option<&GcHead> {
        self.as_gc_node().map(|n| unsafe { n.as_ref() })
    }

    /// Convert value `self` to object, raise exception if value is not an object.
    pub fn ensure_object(&self, ctx: &mut ScopedContext) -> Result<Object, i32> {
        if let Some(o) = self.as_object() {
            Ok(o)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    // Convert value `self` to callable object, raise exception if value is not a callable object.
    pub fn ensure_callable(&self, ctx: &mut ScopedContext) -> Result<Object, i32> {
        self.ensure_object(ctx).and_then(|o| {
            if o.is_callable() {
                Ok(o)
            } else {
                ctx.throw_type_error("not a function");
                Err(-1)
            }
        })
    }

    /// if `self` is an exception, returns Err if yes; Otherwise returns Ok(self)
    #[inline]
    pub const fn err(self) -> Result<Self, i32> {
        if unlikely(self.is_exception()) {
            Err(-1)
        } else {
            Ok(self)
        }
    }
}

// ============================================================================
// serde_json support
// ============================================================================
#[cfg(feature = "serde_json_support")]
mod serde_json_support {
    use smallvec::SmallVec;

    use crate::{
        Array, IntOrFloat, JS_CLASS_BOOLEAN, JS_CLASS_NUMBER, JS_CLASS_STRING, ScopedContext,
        builtins::iterator::JSIteratorKindEnum,
        object::{JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, Object},
        value::JSValue,
    };

    impl JSValue {
        /// Convert a serde_json::Value to a JSValue.
        pub fn from_serde_json(
            ctx: &mut ScopedContext,
            json: &serde_json::Value,
        ) -> Result<Self, i32> {
            match json {
                serde_json::Value::Null => Ok(JSValue::NULL),
                serde_json::Value::Bool(b) => Ok(JSValue::from(*b)),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        Ok(JSValue::from(i))
                    } else {
                        Ok(n.as_f64().unwrap_or(0.0).into())
                    }
                }
                serde_json::Value::String(s) => ctx
                    .create_string_from_utf8(s.clone().into_bytes())
                    .map(JSValue::make_string),
                serde_json::Value::Array(arr) => {
                    let mut values = Vec::with_capacity(arr.len());
                    for item in arr {
                        values.push(Self::from_serde_json(ctx, item)?);
                    }
                    Array::from_values(ctx, values).map(JSValue::from)
                }
                serde_json::Value::Object(map) => {
                    let mut obj = ctx.create_base_object()?;
                    for (k, v) in map {
                        let js_val = Self::from_serde_json(ctx, v)?;
                        obj.set_property_str(ctx, k, js_val)?;
                    }
                    Ok(obj.into())
                }
            }
        }

        /// Convert this JSValue to a serde_json::Value.
        pub fn to_serde_json(&self, ctx: &mut ScopedContext) -> Result<serde_json::Value, i32> {
            Self::jsvalue_to_serde_json(ctx, self, &mut SmallVec::new())
        }

        /// Shallow JSValue -> serde_json::Value, considering only scalar
        /// values (string/number/bool/null/undefined). Arrays, objects,
        /// functions and symbols all become `serde_json::Value::Null`.
        ///
        /// Unlike [`to_serde_json`], this does not recurse into nested
        /// structures and never fails.
        pub fn to_serde_json_primitive(&self, ctx: &mut ScopedContext) -> serde_json::Value {
            if let Some(s) = self.as_string(true) {
                serde_json::Value::String(s.content().encode_utf8(false).to_string())
            } else if let Some(n) = self.to_number(ctx).ok() {
                match n {
                    IntOrFloat::Int64(i) => serde_json::Value::from(i),
                    IntOrFloat::Float64(f) => serde_json::Value::from(f),
                }
            } else if self.is_bool() {
                serde_json::Value::Bool(self.to_bool().unwrap_or(false))
            } else {
                // null, undefined, and every non-scalar (array/object/function/symbol)
                serde_json::Value::Null
            }
        }

        /// Shallow serde_json::Value -> JSValue, considering only scalar
        /// values (null/bool/number/string). Arrays and objects become
        /// `JSValue::NULL` — no nested structure is built.
        ///
        /// Unlike [`from_serde_json`], this does not recurse and never fails.
        pub fn from_serde_json_primitive(
            ctx: &mut ScopedContext,
            json: &serde_json::Value,
        ) -> Self {
            match json {
                serde_json::Value::Null => JSValue::NULL,
                serde_json::Value::Bool(b) => JSValue::from(*b),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        JSValue::from(i)
                    } else {
                        JSValue::from(n.as_f64().unwrap_or(0.0))
                    }
                }
                serde_json::Value::String(s) => ctx
                    .create_string_from_utf8(s.clone().into_bytes())
                    .map(JSValue::make_string)
                    .unwrap_or(JSValue::NULL),
                serde_json::Value::Array(_) | serde_json::Value::Object(_) => JSValue::NULL,
            }
        }

        /// Convert JS Object => serde_json::Value.
        pub(super) fn object_to_serde_json(
            ctx: &mut ScopedContext,
            obj: &Object,
            stack: &mut SmallVec<[Object; 16]>,
        ) -> Result<serde_json::Value, i32> {
            match obj.class_id() {
                JS_CLASS_STRING => {
                    if let Some(s) = obj.payload().string() {
                        let rust_str = s.content().encode_utf8(false).into_owned();
                        return Ok(serde_json::Value::String(rust_str));
                    }
                }
                JS_CLASS_NUMBER => {
                    if let Some(n) = obj.payload().value() {
                        return if let Some(num) = n.as_number() {
                            match num {
                                IntOrFloat::Int64(i) => Ok(serde_json::Value::Number(i.into())),
                                IntOrFloat::Float64(f) => {
                                    if f.is_finite() {
                                        Ok(serde_json::Number::from_f64(f)
                                            .map(serde_json::Value::Number)
                                            .unwrap_or(serde_json::Value::Null))
                                    } else {
                                        Ok(serde_json::Value::Null)
                                    }
                                }
                            }
                        } else {
                            Ok(serde_json::Value::Null)
                        };
                    }
                }
                JS_CLASS_BOOLEAN => {
                    if let Some(b) = obj.payload().bool() {
                        return Ok(serde_json::Value::Bool(*b));
                    }
                }
                _ => {}
            }

            // Functions are not JSON-serializable → null
            if obj.is_callable() {
                return Ok(serde_json::Value::Null);
            }

            // Check for circular reference
            if stack.iter().any(|v| obj.eq(v)) {
                ctx.throw_type_error("circular reference");
                return Err(-1);
            }

            stack.push(*obj);

            let result = if obj.is_array(ctx)? {
                let len = obj.get_length_prop(ctx)?;
                let mut arr = Vec::with_capacity(len as usize);
                for i in 0..len {
                    let prop_val = obj.get_property_i64(ctx, i)?;
                    arr.push(Self::jsvalue_to_serde_json(ctx, &prop_val, stack)?);
                }
                serde_json::Value::Array(arr)
            } else {
                let mut o = *obj;
                let kvs = o.get_own_property_kvs(
                    ctx,
                    JS_GPN_ENUM_ONLY | JS_GPN_STR_OR_INT_MASK,
                    JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE,
                )?;

                let mut map = serde_json::Map::new();
                for (key, val) in kvs {
                    if !val.is_undefined()
                        && let Some(k) = key.as_string(false)
                    {
                        let json_val = Self::jsvalue_to_serde_json(ctx, &val, stack)?;
                        map.insert(k.content().encode_utf8(false).into_owned(), json_val);
                    }
                }
                serde_json::Value::Object(map)
            };

            stack.pop();
            Ok(result)
        }
    }

    impl Object {
        /// Convert a JS Object to a serde_json::Value.
        #[inline(always)]
        pub fn to_serde_json(&self, ctx: &mut ScopedContext) -> Result<serde_json::Value, i32> {
            JSValue::object_to_serde_json(ctx, self, &mut SmallVec::new())
        }
    }
}
