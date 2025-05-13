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
    value::{JSValue, JSValueConst, JS_FreeValueRT, JS_TAG_OBJECT, JS_VALUE_GET_TAG},
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
    pub unsafe fn free(rt: *mut JSRuntime, rec: NonNull<Self>) {
        let a = Box::from_raw(rec.as_ptr());
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
        debug_assert_eq!(self.object_unchecked().class_id(), JS_CLASS_C_FUNCTION_DATA);
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
        let func_obj = self
            .create_object_with_proto_and_classid(self.function_proto, JS_CLASS_C_FUNCTION_DATA)?;

        unsafe {
            func_obj
                .object_unchecked()
                .set_opaque(Box::into_raw(s).cast()); // <-- leak Box<JSCFunctionDataRecord> as payload

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
        let mut s = func_obj
            .object_unchecked()
            .opaque::<JSCFunctionDataRecord>(JS_CLASS_C_FUNCTION_DATA)
            .unwrap();

        let cc = CallArgs::new(
            ctx.handle(),
            this_val,
            std::slice::from_raw_parts(argv, argc as usize),
            s.as_ref().magic as _,
            Some(s.as_mut().data_mut()),
        );

        (s.as_ref().func)(ctx, cc).unwrap_or(JSValue::EXCEPTION)
    }

    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        if let Some(p) = val
            .object_unchecked()
            .opaque::<JSCFunctionDataRecord>(JS_CLASS_C_FUNCTION_DATA)
        {
            JSCFunctionDataRecord::free(rt, p);
        }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
        if let Some(p) = val
            .object_unchecked()
            .opaque::<JSCFunctionDataRecord>(JS_CLASS_C_FUNCTION_DATA)
        {
            for d in p.as_ref().data() {
                JS_MarkValue(rt, *d, mark_func);
            }
        }
    }
}
