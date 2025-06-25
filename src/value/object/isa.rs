use std::ops::DerefMut;

use crate::{
    array::JSObjectArray,
    array_buffer::JSArrayBuffer,
    function::{JSAsyncFunctionState, JSObjectFunc},
    map::JSMapState,
    promise::JSPromiseFunctionData,
    runtime::JSRuntime,
    typed_array::JSTypedArray,
    value::JSValue,
};

include!(concat!(env!("OUT_DIR"), "/object_isa.rs"));

impl JSObjectIsa {
    pub const fn array(&self) -> Option<&JSObjectArray> {
        match self {
            Self::ARRAY(a) | Self::ARGUMENTS(a) => Some(a),
            _ => None,
        }
    }
    pub const fn array_mut(&mut self) -> Option<&mut JSObjectArray> {
        match self {
            Self::ARRAY(a) | Self::ARGUMENTS(a) => Some(a),
            _ => None,
        }
    }
    pub fn with_array<R>(&self, f: impl FnOnce(&JSObjectArray) -> R) -> Option<R> {
        self.array().map(f)
    }
    pub fn with_array_mut<R>(&mut self, f: impl FnOnce(&mut JSObjectArray) -> R) -> Option<R> {
        self.array_mut().map(f)
    }

    pub fn array_buffer(&self) -> Option<&JSArrayBuffer> {
        match self {
            Self::ARRAY_BUFFER(a) | Self::SHARED_ARRAY_BUFFER(a) => Some(a),
            _ => None,
        }
    }
    pub const fn array_buffer_mut(&mut self) -> Option<&mut JSArrayBuffer> {
        match self {
            Self::ARRAY_BUFFER(a) | Self::SHARED_ARRAY_BUFFER(a) => Some(a),
            _ => None,
        }
    }

    pub fn typed_array(&self) -> Option<&JSTypedArray> {
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
    pub fn typed_array_mut(&mut self) -> Option<&mut JSTypedArray> {
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
    pub fn with_typed_array<R>(&self, f: impl FnOnce(&JSTypedArray) -> R) -> Option<R> {
        self.typed_array().map(f)
    }
    pub fn with_typed_array_mut<R>(&mut self, f: impl FnOnce(&mut JSTypedArray) -> R) -> Option<R> {
        self.typed_array_mut().map(f)
    }

    // pub const fn typed_array_replace(
    //     &mut self,
    //     ta: Option<Box<JSTypedArray>>,
    // ) -> Result<Option<Box<JSTypedArray>>, i32> {
    //     match self {
    //         Self::UINT8C_ARRAY(a)
    //         | Self::INT8_ARRAY(a)
    //         | Self::UINT8_ARRAY(a)
    //         | Self::INT16_ARRAY(a)
    //         | Self::UINT16_ARRAY(a)
    //         | Self::INT32_ARRAY(a)
    //         | Self::UINT32_ARRAY(a)
    //         | Self::BIG_INT64_ARRAY(a)
    //         | Self::BIG_UINT64_ARRAY(a)
    //         | Self::FLOAT32_ARRAY(a)
    //         | Self::FLOAT64_ARRAY(a)
    //         | Self::DATAVIEW(a) => Ok(a.replace(ta)),
    //         _ => Err(-1),
    //     }
    // }
    pub const fn promise_func_data_mut(&mut self) -> Option<&mut JSPromiseFunctionData> {
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

    pub const fn func(&self) -> Option<&JSObjectFunc> {
        match self {
            Self::BYTECODE_FUNCTION(f)
            | Self::GENERATOR_FUNCTION(f)
            | Self::ASYNC_FUNCTION(f)
            | Self::ASYNC_GENERATOR_FUNCTION(f) => Some(f),
            _ => None,
        }
    }
    pub const fn func_mut(&mut self) -> Option<&mut JSObjectFunc> {
        match self {
            Self::BYTECODE_FUNCTION(f)
            | Self::GENERATOR_FUNCTION(f)
            | Self::ASYNC_FUNCTION(f)
            | Self::ASYNC_GENERATOR_FUNCTION(f) => Some(f),
            _ => None,
        }
    }

    pub const fn async_func_state(&self) -> Option<&JSAsyncFunctionState> {
        match self {
            Self::ASYNC_FUNCTION_RESOLVE(a) | Self::ASYNC_FUNCTION_REJECT(a) => Some(a),
            _ => None,
        }
    }

    /// returned value is not duplicated.
    /// caller should do duplicate if want to reference it.
    pub const fn value(&self) -> Option<&JSValue> {
        match self {
            Self::NUMBER(a) | Self::STRING(a) | Self::BOOLEAN(a) | Self::SYMBOL(a) => Some(a),
            _ => None,
        }
    }
    /// `val` is consumed
    pub fn value_replace(&mut self, rt: &mut JSRuntime, val: JSValue) -> Result<JSValue, i32> {
        match self {
            Self::NUMBER(a) | Self::STRING(a) | Self::BOOLEAN(a) | Self::SYMBOL(a) => {
                Ok(a.replace(val))
            }
            _ => unsafe {
                crate::value::JS_FreeValueRT(rt, &val);
                Err(-1)
            },
        }
    }
}
