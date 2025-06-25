use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use crate::{
    JS_CLASS_C_FUNCTION_DATA,
    atom::consts::JS_ATOM_empty_string,
    context::JSContext,
    error::JSResult,
    function::{CallContext, JSNativeCallback, js_function_set_properties},
    gc::GCMarkFunc,
    mem::js_malloc_typed,
    runtime::JSRuntime,
    value::{JSValue, JSValueConst, object::JSObjectIsa},
};

#[derive(Debug)]
pub struct JSCFunctionDataRecord {
    pub func: JSNativeCallback,
    pub length: u8,
    pub magic: u16,
    pub data: Vec<JSValue>,
}

impl JSCFunctionDataRecord {
    /// `data` values are not consumed.
    pub fn new(
        ctx: &JSContext,
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
                        lst.try_reserve_exact(data.len()).map_err(|_| {
                            ctx.JS_ThrowOutOfMemory();
                            -1
                        })?;
                        for d in data {
                            lst.push(d.clone());
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
}

/// A callable object. Is an instance of JS_CLASS_C_FUNCTION_DATA class.
/// When called, will invoke native callback with attached data payload.
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct CallableValue(JSValue);

impl JSValue {
    /// as C_FUNCTION_DATA class object
    pub unsafe fn as_callbable_unchecked(self) -> CallableValue {
        debug_assert!(self.is_object());
        debug_assert_eq!(self.object_unchecked().class_id(), JS_CLASS_C_FUNCTION_DATA);
        std::mem::transmute(self)
    }
}

impl JSContext {
    /// Create a C_FUNCTION_DATA class object, which behaves like a native function, with attached data.
    /// Note: `data` values are not consumed.
    pub fn create_native_callable_object(
        &self,
        callback: JSNativeCallback,
        length: u8,
        magic: i32,
        data: &[JSValue],
    ) -> JSResult {
        let isa = JSObjectIsa::C_FUNCTION_DATA(JSCFunctionDataRecord::new(
            self, callback, length, magic, data,
        )?);
        let func_obj = self
            .create_instance_from_proto(&self.0.borrow().function_proto, isa)
            .map_err(|_| -1)?;

        unsafe {
            js_function_set_properties(self, &func_obj, JS_ATOM_empty_string, length as _);
        }

        Ok(func_obj)
    }
}

//
// Hooks
//
impl CallableValue {
    pub fn hook_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_val: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let mut isa = unsafe { func_obj.object_unchecked().0.isa.borrow_mut() };
        if let JSObjectIsa::C_FUNCTION_DATA(s) = isa.deref_mut() {
            let f = s.func.clone();
            let cc = CallContext::new(
                ctx,
                this_val.clone(),
                args,
                s.magic as _,
                Some(s.data_mut()),
            );
            f(ctx, cc)
        } else {
            unreachable!()
        }
    }

    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {}

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: GCMarkFunc) {
        if let JSObjectIsa::C_FUNCTION_DATA(p) = val.object_unchecked().0.isa.borrow().deref() {
            for d in p.as_ref().data() {
                (*rt).gc_mark_value(d, mark_func);
            }
        }
    }
}
