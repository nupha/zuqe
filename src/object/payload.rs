use std::{
    cell::{Ref, RefCell, RefMut},
    ops::Deref,
};

use crate::{
    array::ArrayObjPayload,
    array_buffer::ArrayBufferPayload,
    context::JSContext,
    function::{AsyncFuncState, BCFuncPayload},
    mapset::{JSMapIteratorData, JSMapState},
    object::JSObject,
    promise::PromiseResolvePayload,
    typed_array::TypedArrayPayload,
    value::JSValue,
};

include!(concat!(env!("OUT_DIR"), "/class_payload.rs"));

impl ObjectPayload {
    pub const fn array(&self) -> Option<&ArrayObjPayload> {
        match self {
            Self::ARRAY(a) | Self::ARGUMENTS(a) => Some(a),
            _ => None,
        }
    }
    pub fn with_array<R>(&self, f: impl FnOnce(&ArrayObjPayload) -> R) -> Option<R> {
        self.array().map(f)
    }

    pub const fn array_buffer(&self) -> Option<&ArrayBufferPayload> {
        match self {
            Self::ARRAY_BUFFER(a) | Self::SHARED_ARRAY_BUFFER(a) => Some(a),
            _ => None,
        }
    }

    pub fn typed_array(&self) -> Option<&RefCell<Box<TypedArrayPayload>>> {
        match self {
            Self::UINT8C_ARRAY(a)
            | Self::INT8_ARRAY(a)
            | Self::UINT8_ARRAY(a)
            | Self::INT16_ARRAY(a)
            | Self::UINT16_ARRAY(a)
            | Self::INT32_ARRAY(a)
            | Self::UINT32_ARRAY(a)
            | Self::BIG_INT64_ARRAY(a)
            | Self::BIG_UINT64_ARRAY(a)
            | Self::FLOAT32_ARRAY(a)
            | Self::FLOAT64_ARRAY(a)
            | Self::DATAVIEW(a) => Some(a),
            _ => None,
        }
    }
    pub fn with_typed_array<R>(&self, f: impl FnOnce(&TypedArrayPayload) -> R) -> Option<R> {
        self.typed_array().map(|x| f(x.borrow().deref()))
    }

    /// get detail for promise resolving function
    pub const fn promise_func_data(&self) -> Option<&PromiseResolvePayload> {
        match self {
            Self::PROMISE_RESOLVE_FUNCTION(a) | Self::PROMISE_REJECT_FUNCTION(a) => Some(a),
            _ => None,
        }
    }

    pub fn map_state(&self) -> Option<Ref<'_, Box<JSMapState>>> {
        match self {
            Self::MAP(a) | Self::SET(a) | Self::WEAKMAP(a) | Self::WEAKSET(a) => Some(a.borrow()),
            _ => None,
        }
    }
    pub fn map_state_mut(&self) -> Option<RefMut<'_, Box<JSMapState>>> {
        match self {
            Self::MAP(a) | Self::SET(a) | Self::WEAKMAP(a) | Self::WEAKSET(a) => {
                Some(a.borrow_mut())
            }
            _ => None,
        }
    }

    pub fn map_iterator_state(&self) -> Option<Ref<'_, Box<JSMapIteratorData>>> {
        match self {
            Self::MAP_ITERATOR(a) | Self::SET_ITERATOR(a) => Some(a.borrow()),
            _ => None,
        }
    }
    pub fn map_iterator_state_mut(&self) -> Option<RefMut<'_, Box<JSMapIteratorData>>> {
        match self {
            Self::MAP_ITERATOR(a) | Self::SET_ITERATOR(a) => Some(a.borrow_mut()),
            _ => None,
        }
    }

    pub fn func(&self) -> Option<Ref<'_, Box<BCFuncPayload>>> {
        match self {
            Self::BYTECODE_FUNCTION(f)
            | Self::GENERATOR_FUNCTION(f)
            | Self::ASYNC_FUNCTION(f)
            | Self::ASYNC_GENERATOR_FUNCTION(f) => Some(f.borrow()),
            _ => None,
        }
    }
    pub fn func_mut(&self) -> Option<RefMut<'_, Box<BCFuncPayload>>> {
        match self {
            Self::BYTECODE_FUNCTION(f)
            | Self::GENERATOR_FUNCTION(f)
            | Self::ASYNC_FUNCTION(f)
            | Self::ASYNC_GENERATOR_FUNCTION(f) => Some(f.borrow_mut()),
            _ => None,
        }
    }

    pub const fn async_func_state(&self) -> Option<&AsyncFuncState> {
        match self {
            Self::ASYNC_FUNCTION_RESOLVE(a) | Self::ASYNC_FUNCTION_REJECT(a) => Some(a),
            _ => None,
        }
    }

    /// returned value is not duplicated.
    /// caller should do duplicate if you want to reference it.
    pub fn value(&self) -> Option<JSValue> {
        match self {
            Self::NUMBER(a) | Self::BOOLEAN(a) => Some(a.clone()),
            Self::STRING(a) => Some(JSValue::String(a.clone())),
            Self::SYMBOL(a) => Some(JSValue::Symbol(a.clone())),
            _ => None,
        }
    }

    pub fn replace_value(&mut self, val: JSValue) -> Result<JSValue, i32> {
        match (self, val) {
            (Self::NUMBER(a), v @ (JSValue::Int(_) | JSValue::Float64(_))) => {
                Ok(std::mem::replace(a, v))
            }
            (Self::BOOLEAN(a), v @ JSValue::Bool(_)) => Ok(std::mem::replace(a, v)),
            (Self::STRING(a), JSValue::String(s)) => Ok(std::mem::replace(a, s).into()),
            (Self::SYMBOL(a), JSValue::Symbol(s)) => Ok(std::mem::replace(a, s).into()),
            _ => Err(-1),
        }
    }
}

impl JSObject {
    /// get object's payload.
    /// if the object is not the required class_id (optional),
    /// returns None
    pub(crate) fn retrive_payload0(&self, class_id: JSClassID) -> Option<&ObjectPayload> {
        if class_id == self.class_id() {
            Some(self.payload())
        } else {
            None
        }
    }

    /// get object's payload borrow ref.
    /// if the object is not the required class_id (optional),
    /// throws type error.
    pub(crate) fn retrive_payload(
        &self,
        ctx: &JSContext,
        class_id: JSClassID,
    ) -> Result<&ObjectPayload, i32> {
        self.retrive_payload0(class_id).ok_or_else(|| {
            ctx.throw_type_error_invalid_class(class_id);
            -1
        })
    }
}

impl JSValue {
    /// get object value's payload.
    /// if the value is not an object, or optionally is not the required class_id,
    /// throws type error.
    pub(crate) fn retrive_object_payload0(&self, class_id: JSClassID) -> Option<&ObjectPayload> {
        if let Some(o) = self.as_object() {
            o.retrive_payload0(class_id)
        } else {
            None
        }
    }

    /// get object value's payload borrow ref.
    /// if the value is not an object, or optionally is not the required class_id,
    /// throws type error.
    pub(crate) fn retrive_object_payload(
        &self,
        ctx: &JSContext,
        class_id: JSClassID,
    ) -> Result<&ObjectPayload, i32> {
        if let Some(o) = self.as_object() {
            o.retrive_payload(ctx, class_id)
        } else {
            ctx.throw_type_error_invalid_class(class_id);
            Err(-1)
        }
    }
}
