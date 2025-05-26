use crate::{
    array::JSObjectArray,
    array_buffer::JSArrayBuffer,
    function::{JSAsyncFunctionState, JSObjectFunc},
    map::JSMapState,
    promise::JSPromiseFunctionData,
    runtime::JSRuntime,
    typed_array::JSTypedArray,
    value::JSValue,
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_ARRAY_BUFFER, JS_CLASS_ARRAY_ITERATOR,
    JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT,
    JS_CLASS_ASYNC_FUNCTION_RESOLVE, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BIG_INT, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_BOOLEAN,
    JS_CLASS_BOUND_FUNCTION, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_C_FUNCTION,
    JS_CLASS_C_FUNCTION_DATA, JS_CLASS_DATAVIEW, JS_CLASS_DATE, JS_CLASS_ERROR,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_FOR_IN_ITERATOR, JS_CLASS_GENERATOR,
    JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY, JS_CLASS_INT8_ARRAY,
    JS_CLASS_MAP, JS_CLASS_MAPPED_ARGUMENTS, JS_CLASS_MAP_ITERATOR, JS_CLASS_MODULE_NS,
    JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_PROMISE, JS_CLASS_PROMISE_REJECT_FUNCTION,
    JS_CLASS_PROMISE_RESOLVE_FUNCTION, JS_CLASS_PROXY, JS_CLASS_REGEXP,
    JS_CLASS_REGEXP_STRING_ITERATOR, JS_CLASS_SET, JS_CLASS_SET_ITERATOR,
    JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_STRING, JS_CLASS_STRING_ITERATOR, JS_CLASS_SYMBOL,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_WEAKMAP, JS_CLASS_WEAKSET,
};

include!(concat!(env!("OUT_DIR"), "/object_isa.rs"));

impl JSObjectIsa {
    //     pub fn new(class_id: JSClassID) -> Self {
    //         match class_id {
    //             JS_CLASS_OBJECT => Self::OBJECT,
    //             JS_CLASS_ARRAY => Self::ARRAY(JSObjectArray::default()),
    //             JS_CLASS_ERROR => Self::ERROR,
    //             JS_CLASS_NUMBER => Self::NUMBER(JSValue::ZERO),
    //             JS_CLASS_STRING => Self::STRING(JSValue::UNDEFINED),
    //             JS_CLASS_BOOLEAN => Self::BOOLEAN(JSValue::FALSE),
    //             JS_CLASS_SYMBOL => Self::SYMBOL(JSValue::UNDEFINED),
    //             JS_CLASS_ARGUMENTS => Self::ARGUMENTS(JSObjectArray::default()),
    //             JS_CLASS_MAPPED_ARGUMENTS => Self::MAPPED_ARGUMENTS,
    //             JS_CLASS_DATE => Self::DATE,
    //             JS_CLASS_MODULE_NS => Self::MODULE_NS,
    //             JS_CLASS_C_FUNCTION => Self::C_FUNCTION(todo!()),
    //             JS_CLASS_BYTECODE_FUNCTION => Self::BYTECODE_FUNCTION(JSObjectFunc::default()),
    //             JS_CLASS_BOUND_FUNCTION => Self::BOUND_FUNCTION(std::ptr::null_mut()),
    //             JS_CLASS_C_FUNCTION_DATA => Self::C_FUNCTION_DATA(std::ptr::null_mut()),
    //             JS_CLASS_GENERATOR_FUNCTION => Self::GENERATOR_FUNCTION(JSObjectFunc::default()),
    //             JS_CLASS_FOR_IN_ITERATOR => Self::FOR_IN_ITERATOR(std::ptr::null_mut()),
    //             JS_CLASS_REGEXP => Self::REGEXP(std::ptr::null_mut()),
    //             JS_CLASS_ARRAY_BUFFER => Self::ARRAY_BUFFER(std::ptr::null_mut()),
    //             JS_CLASS_SHARED_ARRAY_BUFFER => Self::SHARED_ARRAY_BUFFER(std::ptr::null_mut()),
    //             JS_CLASS_UINT8C_ARRAY => Self::UINT8C_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_INT8_ARRAY => Self::INT8_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_UINT8_ARRAY => Self::UINT8_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_INT16_ARRAY => Self::INT16_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_UINT16_ARRAY => Self::UINT16_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_INT32_ARRAY => Self::INT32_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_UINT32_ARRAY => Self::UINT32_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_BIG_INT64_ARRAY => Self::BIG_INT64_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_BIG_UINT64_ARRAY => Self::BIG_UINT64_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_FLOAT32_ARRAY => Self::FLOAT32_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_FLOAT64_ARRAY => Self::FLOAT64_ARRAY(std::ptr::null_mut()),
    //             JS_CLASS_DATAVIEW => Self::DATAVIEW(std::ptr::null_mut()),
    //             JS_CLASS_BIG_INT => Self::BIG_INT,
    //             JS_CLASS_MAP => Self::MAP(std::ptr::null_mut()),
    //             JS_CLASS_SET => Self::SET(std::ptr::null_mut()),
    //             JS_CLASS_WEAKMAP => Self::WEAKMAP(std::ptr::null_mut()),
    //             JS_CLASS_WEAKSET => Self::WEAKSET(std::ptr::null_mut()),
    //             JS_CLASS_MAP_ITERATOR => Self::MAP_ITERATOR(std::ptr::null_mut()),
    //             JS_CLASS_SET_ITERATOR => Self::SET_ITERATOR(std::ptr::null_mut()),
    //             JS_CLASS_ARRAY_ITERATOR => Self::ARRAY_ITERATOR(std::ptr::null_mut()),
    //             JS_CLASS_STRING_ITERATOR => Self::STRING_ITERATOR(std::ptr::null_mut()),
    //             JS_CLASS_REGEXP_STRING_ITERATOR => Self::REGEXP_STRING_ITERATOR(std::ptr::null_mut()),
    //             JS_CLASS_GENERATOR => Self::GENERATOR(std::ptr::null_mut()),
    //             JS_CLASS_PROXY => Self::PROXY(std::ptr::null_mut()),
    //             JS_CLASS_PROMISE => Self::PROMISE(std::ptr::null_mut()),
    //             JS_CLASS_PROMISE_RESOLVE_FUNCTION => {
    //                 Self::PROMISE_RESOLVE_FUNCTION(std::ptr::null_mut())
    //             }
    //             JS_CLASS_PROMISE_REJECT_FUNCTION => Self::PROMISE_REJECT_FUNCTION(std::ptr::null_mut()),
    //             JS_CLASS_ASYNC_FUNCTION => Self::ASYNC_FUNCTION(JSObjectFunc::default()),
    //             JS_CLASS_ASYNC_FUNCTION_RESOLVE => Self::ASYNC_FUNCTION_RESOLVE(std::ptr::null_mut()),
    //             JS_CLASS_ASYNC_FUNCTION_REJECT => Self::ASYNC_FUNCTION_REJECT(std::ptr::null_mut()),
    //             JS_CLASS_ASYNC_FROM_SYNC_ITERATOR => {
    //                 Self::ASYNC_FROM_SYNC_ITERATOR(std::ptr::null_mut())
    //             }
    //             JS_CLASS_ASYNC_GENERATOR_FUNCTION => {
    //                 Self::ASYNC_GENERATOR_FUNCTION(JSObjectFunc::default())
    //             }
    //             JS_CLASS_ASYNC_GENERATOR => Self::ASYNC_GENERATOR(std::ptr::null_mut()),
    //             _ => Self::OTHER(class_id),
    //         }
    //     }

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

    pub const fn array_buffer_ptr(&self) -> *mut JSArrayBuffer {
        match self {
            Self::ARRAY_BUFFER(a) | Self::SHARED_ARRAY_BUFFER(a) => *a,
            _ => std::ptr::null_mut(),
        }
    }
    pub const unsafe fn array_buffer_replace(
        &mut self,
        ptr: *mut JSArrayBuffer,
    ) -> Result<*mut JSArrayBuffer, i32> {
        match self {
            Self::ARRAY_BUFFER(a) | Self::SHARED_ARRAY_BUFFER(a) => Ok(std::ptr::replace(a, ptr)),
            _ => Err(-1),
        }
    }
    pub const fn array_buffer(&self) -> Option<&JSArrayBuffer> {
        match self {
            Self::ARRAY_BUFFER(a) | Self::SHARED_ARRAY_BUFFER(a) => unsafe { (*a).as_ref() },
            _ => None,
        }
    }
    pub const fn array_buffer_mut(&mut self) -> Option<&mut JSArrayBuffer> {
        match self {
            Self::ARRAY_BUFFER(a) | Self::SHARED_ARRAY_BUFFER(a) => unsafe { (*a).as_mut() },
            _ => None,
        }
    }
    pub const fn typed_array_ptr(&self) -> *mut JSTypedArray {
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
            | Self::DATAVIEW(a) => *a,
            _ => std::ptr::null_mut(),
        }
    }
    pub const fn typed_array(&self) -> Option<&JSTypedArray> {
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
            | Self::DATAVIEW(a) => unsafe { (*a).as_ref() },
            _ => None,
        }
    }
    pub const fn typed_array_mut(&mut self) -> Option<&mut JSTypedArray> {
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
            | Self::DATAVIEW(a) => unsafe { (*a).as_mut() },
            _ => None,
        }
    }
    pub const fn typed_array_replace(
        &mut self,
        ta: *mut JSTypedArray,
    ) -> Result<*mut JSTypedArray, i32> {
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
            | Self::DATAVIEW(a) => unsafe { Ok(std::ptr::replace(a, ta)) },
            _ => Err(-1),
        }
    }
    pub const fn promise_func_data_ptr(&self) -> *mut JSPromiseFunctionData {
        match self {
            Self::PROMISE_RESOLVE_FUNCTION(a) | Self::PROMISE_REJECT_FUNCTION(a) => *a,
            _ => std::ptr::null_mut(),
        }
    }
    pub const fn promise_func_data_replace(
        &mut self,
        ta: *mut JSPromiseFunctionData,
    ) -> Result<*mut JSPromiseFunctionData, i32> {
        match self {
            Self::PROMISE_RESOLVE_FUNCTION(a) | Self::PROMISE_REJECT_FUNCTION(a) => unsafe {
                Ok(std::ptr::replace(a, ta))
            },
            _ => Err(-1),
        }
    }
    pub const fn map_state_ptr(&self) -> *mut JSMapState {
        match self {
            Self::MAP(a) | Self::SET(a) | Self::WEAKMAP(a) | Self::WEAKSET(a) => *a,
            _ => std::ptr::null_mut(),
        }
    }
    pub const fn map_state_replace(&mut self, ta: *mut JSMapState) -> Result<*mut JSMapState, i32> {
        match self {
            Self::MAP(a) | Self::SET(a) | Self::WEAKMAP(a) | Self::WEAKSET(a) => unsafe {
                Ok(std::ptr::replace(a, ta))
            },
            _ => Err(-1),
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

    pub const fn async_func_state_ptr(&self) -> *mut JSAsyncFunctionState {
        match self {
            Self::ASYNC_FUNCTION_RESOLVE(a) | Self::ASYNC_FUNCTION_REJECT(a) => *a,
            _ => std::ptr::null_mut(),
        }
    }
    pub const unsafe fn async_func_state_replace(
        &mut self,
        ptr: *mut JSAsyncFunctionState,
    ) -> Result<*mut JSAsyncFunctionState, i32> {
        match self {
            Self::ASYNC_FUNCTION_RESOLVE(a) | Self::ASYNC_FUNCTION_REJECT(a) => {
                Ok(std::ptr::replace(a, ptr))
            }
            _ => Err(-1),
        }
    }

    /// returned value is not duplicated.
    /// caller should do duplicate if want to reference it.
    pub const fn value(&self) -> Option<JSValue> {
        match self {
            Self::NUMBER(a) | Self::STRING(a) | Self::BOOLEAN(a) | Self::SYMBOL(a) => Some(*a),
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
                crate::value::JS_FreeValueRT(rt, val);
                Err(-1)
            },
        }
    }
}
