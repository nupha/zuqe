use std::ops::DerefMut;

use crate::{
    array::ArrayObjPayload,
    array_buffer::ObjArrayBuffer,
    function::{ObjAsyncState, ObjFunc},
    map::JSMapState,
    promise::ObjPromiseResolvingState,
    typed_array::ObjTypedArray,
    value::JSValue,
};

include!(concat!(env!("OUT_DIR"), "/class_payload.rs"));

impl ObjectDetail {
    pub const fn array(&self) -> Option<&ArrayObjPayload> {
        match self {
            Self::ARRAY(a) | Self::ARGUMENTS(a) => Some(a),
            _ => None,
        }
    }
    pub const fn array_mut(&mut self) -> Option<&mut ArrayObjPayload> {
        match self {
            Self::ARRAY(a) | Self::ARGUMENTS(a) => Some(a),
            _ => None,
        }
    }
    pub fn with_array<R>(&self, f: impl FnOnce(&ArrayObjPayload) -> R) -> Option<R> {
        self.array().map(f)
    }
    pub fn with_array_mut<R>(&mut self, f: impl FnOnce(&mut ArrayObjPayload) -> R) -> Option<R> {
        self.array_mut().map(f)
    }

    pub const fn array_buffer(&self) -> Option<&ObjArrayBuffer> {
        match self {
            Self::ARRAY_BUFFER(a) | Self::SHARED_ARRAY_BUFFER(a) => Some(a),
            _ => None,
        }
    }
    pub const fn array_buffer_mut(&mut self) -> Option<&mut ObjArrayBuffer> {
        match self {
            Self::ARRAY_BUFFER(a) | Self::SHARED_ARRAY_BUFFER(a) => Some(a),
            _ => None,
        }
    }

    pub fn typed_array(&self) -> Option<&ObjTypedArray> {
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
            | Self::DATAVIEW(a) => Some(a.as_ref()),
            _ => None,
        }
    }
    pub fn typed_array_mut(&mut self) -> Option<&mut ObjTypedArray> {
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
            | Self::DATAVIEW(a) => Some(a.deref_mut()),
            _ => None,
        }
    }
    pub fn with_typed_array<R>(&self, f: impl FnOnce(&ObjTypedArray) -> R) -> Option<R> {
        self.typed_array().map(f)
    }
    pub fn with_typed_array_mut<R>(
        &mut self,
        f: impl FnOnce(&mut ObjTypedArray) -> R,
    ) -> Option<R> {
        self.typed_array_mut().map(f)
    }

    /// get detail for promise resolving function
    pub const fn promise_func_data(&self) -> Option<&ObjPromiseResolvingState> {
        match self {
            Self::PROMISE_RESOLVE_FUNCTION(a) | Self::PROMISE_REJECT_FUNCTION(a) => Some(a),
            _ => None,
        }
    }

    pub fn map_state(&self) -> Option<&JSMapState> {
        match self {
            Self::MAP(a) | Self::SET(a) | Self::WEAKMAP(a) | Self::WEAKSET(a) => Some(a.as_ref()),
            _ => None,
        }
    }
    pub fn map_state_mut(&mut self) -> Option<&mut JSMapState> {
        match self {
            Self::MAP(a) | Self::SET(a) | Self::WEAKMAP(a) | Self::WEAKSET(a) => {
                Some(a.deref_mut())
            }
            _ => None,
        }
    }

    pub const fn func(&self) -> Option<&ObjFunc> {
        match self {
            Self::BYTECODE_FUNCTION(f)
            | Self::GENERATOR_FUNCTION(f)
            | Self::ASYNC_FUNCTION(f)
            | Self::ASYNC_GENERATOR_FUNCTION(f) => Some(f),
            _ => None,
        }
    }
    pub const fn func_mut(&mut self) -> Option<&mut ObjFunc> {
        match self {
            Self::BYTECODE_FUNCTION(f)
            | Self::GENERATOR_FUNCTION(f)
            | Self::ASYNC_FUNCTION(f)
            | Self::ASYNC_GENERATOR_FUNCTION(f) => Some(f),
            _ => None,
        }
    }

    pub const fn async_func_state(&self) -> Option<&ObjAsyncState> {
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
