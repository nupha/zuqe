use std::{ops::DerefMut, ptr::NonNull};

use crate::{
    atom::consts::JS_ATOM_empty_string,
    context::JSContext,
    error::JSResult,
    function::{js_function_set_properties, CallArgs, JSNativeCallback},
    gc::GCMarkFunc,
    mem::js_malloc_typed,
    runtime::JSRuntime,
    value::{object::JSObjectIsa, JSValue, JSValueConst, JS_FreeValueRT, JS_TAG_OBJECT},
    JS_CLASS_C_FUNCTION_DATA,
};

pub struct JSCFunctionDataRecord {
    pub func: JSNativeCallback,
    pub length: u8,
    pub magic: u16,
    pub data: Vec<JSValue>,
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
            unsafe {
                s.write(JSCFunctionDataRecord {
                    func: callback,
                    length,
                    magic: magic as _,
                    data: {
                        let mut lst = Vec::new();
                        if lst.try_reserve_exact(data.len()).is_err() {
                            ctx.JS_ThrowOutOfMemory();
                            return Err(-1);
                        }
                        for d in data {
                            lst.push(d.dup());
                        }
                        lst
                    },
                });
            }
            Ok(unsafe { Box::from_raw(s.as_ptr()) })
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
        debug_assert_eq!(self.tag(), JS_TAG_OBJECT);
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
        let mut ctx = self.handle();

        let func_obj = self
            .create_instance_from_proto(self.function_proto, {
                let s = JSCFunctionDataRecord::new(ctx.deref_mut(), callback, length, magic, data)?;
                JSObjectIsa::C_FUNCTION_DATA(Box::into_raw(s))
            })
            .map_err(|isa| {
                if let JSObjectIsa::C_FUNCTION_DATA(p) = isa {
                    drop(unsafe { Box::from_raw(p) });
                }
                -1
            })?;

        unsafe {
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
        if let Some(JSObjectIsa::C_FUNCTION_DATA(s)) = func_obj.object().map(|p| p.isa_mut()) {
            let s = *s;

            let cc = CallArgs::new(
                ctx.handle(),
                this_val,
                std::slice::from_raw_parts(argv, argc as usize),
                (*s).magic as _,
                Some((*s).data_mut()),
            );

            ((*s).func)(ctx, cc).unwrap_or(JSValue::EXCEPTION)
        } else {
            unreachable!()
        }
    }

    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        if let JSObjectIsa::C_FUNCTION_DATA(p) = val.object_unchecked().isa_mut() {
            JSCFunctionDataRecord::free(rt, NonNull::new_unchecked(*p));
        }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
        if let JSObjectIsa::C_FUNCTION_DATA(p) = val.object_unchecked().isa() {
            for d in (*p).as_ref().unwrap().data() {
                (*rt).gc_mark_value(*d, mark_func);
            }
        }
    }
}
