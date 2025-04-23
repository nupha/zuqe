use std::ptr::NonNull;

use smallvec::SmallVec;

use crate::{
    atom::consts::JS_ATOM_empty_string,
    context::JSContext,
    error::JSResult,
    function::{js_function_set_properties, CallArgs, JSNativeCallback},
    gc::{JS_MarkFunc, JS_MarkValue},
    mem::js_malloc_typed,
    runtime::JSRuntime,
    value::{
        object::{JS_GetOpaque, JS_NewObjectProtoClass, JS_SetOpaque},
        JSValue, JSValueConst, JS_FreeValueRT, JS_TAG_OBJECT, JS_VALUE_GET_TAG,
    },
    JS_CLASS_C_FUNCTION_DATA,
};

pub struct JSCFunctionDataRecord {
    pub func: JSNativeCallback,
    pub length: u8,
    pub magic: u16,
    pub data: SmallVec<[JSValue; 8]>,
}

impl JSCFunctionDataRecord {
    /// `data` values are not consumed.
    pub fn new(
        ctx: &mut JSContext,
        callback: JSNativeCallback,
        length: u8,
        magic: i32,
        data: &[JSValue],
    ) -> Result<Box<Self>, i32> {
        if let Some(s) = unsafe { js_malloc_typed::<JSCFunctionDataRecord>(ctx, 1, false) } {
            let mut rec = unsafe { Box::from_raw(s.as_ptr()) };

            rec.func = callback;
            rec.length = length as u8;
            rec.magic = magic as u16;

            rec.data = SmallVec::new();
            rec.data.try_reserve_exact(data.len()).map_err(|_| -1)?;
            for d in data {
                rec.data.push(d.dup());
            }

            Ok(rec)
        } else {
            ctx.JS_ThrowOutOfMemory();
            Err(-1)
        }
    }

    /// get data as slice
    #[inline(always)]
    pub fn data(&self) -> &[JSValue] {
        self.data.as_slice()
    }

    /// get data as slice mut
    #[inline(always)]
    pub fn data_mut(&mut self) -> &mut [JSValue] {
        self.data.as_mut_slice()
    }

    /// free data values, and drop Self
    pub unsafe fn free(rt: *mut JSRuntime, rec: *mut Self) {
        let a = Box::from_raw(rec);
        for d in a.data() {
            JS_FreeValueRT(rt, *d);
        }
    }
}

/// A callable object. Is an instance of JS_CLASS_C_FUNCTION_DATA class.
/// When called, will invoke native callback with attached data payload.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct CallableValue(JSValue);

impl JSValue {
    /// as C_FUNCTION_DATA class object
    pub unsafe fn as_callbable_unchecked(self) -> CallableValue {
        debug_assert_eq!(JS_VALUE_GET_TAG(self), JS_TAG_OBJECT);
        debug_assert_eq!(self.object().class_id(), JS_CLASS_C_FUNCTION_DATA);
        std::mem::transmute(self)
    }
}

impl JSContext {
    /// Create a C_FUNCTION_DATA class object, which behaves like a native function, with attached data.
    /// Note: `data` values are not consumed.
    pub fn create_native_callable_object(
        &mut self,
        callback: JSNativeCallback,
        length: u8,
        magic: i32,
        data: &[JSValueConst],
    ) -> JSResult {
        let s = JSCFunctionDataRecord::new(self, callback, length, magic, data)?;

        let func_obj = unsafe {
            JS_NewObjectProtoClass(self, self.function_proto, JS_CLASS_C_FUNCTION_DATA)
                .check_exception()?
        };

        unsafe {
            JS_SetOpaque(func_obj, Box::into_raw(s).cast());
            js_function_set_properties(self, func_obj, JS_ATOM_empty_string, length as _);
        }

        Ok(func_obj)
    }
}

//
// Hooks
//
impl CallableValue {
    pub unsafe fn hook_call(
        ctx: &mut JSContext,
        func_obj: JSValueConst,
        this_val: JSValueConst,
        argc: i32,
        argv: *const JSValueConst,
        flags: i32,
    ) -> JSValue {
        let s = JS_GetOpaque(func_obj, JS_CLASS_C_FUNCTION_DATA).cast::<JSCFunctionDataRecord>();

        let cc = CallArgs::new(
            NonNull::new_unchecked(ctx as *mut JSContext),
            this_val,
            std::slice::from_raw_parts(argv, argc as usize),
            (*s).magic as _,
            Some((*s).data_mut()),
        );

        ((*s).func)(ctx, cc).unwrap_or(JSValue::EXCEPTION)
    }

    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        let s = JS_GetOpaque(val, JS_CLASS_C_FUNCTION_DATA).cast::<JSCFunctionDataRecord>();
        if !s.is_null() {
            JSCFunctionDataRecord::free(rt, s);
        }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
        let s = JS_GetOpaque(val, JS_CLASS_C_FUNCTION_DATA).cast::<JSCFunctionDataRecord>();
        if !s.is_null() {
            for d in (*s).data() {
                JS_MarkValue(rt, *d, mark_func);
            }
        }
    }
}
