use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    rc::Rc,
};

use crate::{
    atom::consts::JS_ATOM_empty_string,
    context::JSContext,
    error::JSResult,
    function::{CallContext, NativeCallback, js_function_set_properties},
    object::{JSObject, ObjectPayload},
    runtime::JSRuntime,
    value::JSValue,
};

#[derive(Debug)]
pub struct NativeFuncData {
    pub func: NativeCallback,
    pub length: u8,
    pub magic: u16,
    pub data: Rc<RefCell<Box<[JSValue]>>>,
}

impl NativeFuncData {
    /// `data` values are not consumed.
    fn new(callback: NativeCallback, length: u8, magic: i32, data: Box<[JSValue]>) -> Box<Self> {
        Box::new(NativeFuncData {
            func: callback,
            length,
            magic: magic as _,
            data: Rc::new(RefCell::new(data)),
        })
    }

    pub fn len(&self) -> usize {
        self.data.borrow().len()
    }

    pub fn get(&self, nth: usize) -> Option<JSValue> {
        self.data.borrow().get(nth).cloned()
    }
}

/// A callable object. Is an instance of JS_CLASS_C_FUNCTION_DATA class.
/// When called, will invoke native callback with attached data payload.
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct NativeFunctionWithData(JSObject);

impl JSContext {
    /// Create a C_FUNCTION_DATA class object, which behaves like a native function, with attached data.
    /// Note: `data` values are not consumed.
    pub fn create_native_function_with_data(
        &self,
        callback: NativeCallback,
        length: u8,
        magic: i32,
        data: Box<[JSValue]>,
    ) -> Result<JSObject, i32> {
        let func_obj = self
            .create_object_from_proto(
                self.0.borrow().function_proto.clone(),
                ObjectPayload::C_FUNCTION_DATA(NativeFuncData::new(callback, length, magic, data)),
            )
            .map_err(|_| -1)?;

        js_function_set_properties(self, &func_obj, JS_ATOM_empty_string, length as _);
        Ok(func_obj)
    }
}

//
// Hooks
//
impl NativeFunctionWithData {
    pub fn class_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_val: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let isa = unsafe { func_obj.as_object_unchecked().0.payload.borrow() };

        if let ObjectPayload::C_FUNCTION_DATA(s) = isa.deref() {
            let f = s.func.clone();
            let cc = CallContext::new(
                ctx,
                this_val.clone(),
                args,
                s.magic as _,
                Some(s.data.clone()),
            );
            f(ctx, cc)
        } else {
            unreachable!()
        }
    }

    pub fn class_finalizer(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {}

    pub fn class_mark(val: JSValue) {
        unsafe {
            if let ObjectPayload::C_FUNCTION_DATA(p) =
                val.as_object_unchecked().0.payload.borrow().deref()
            {
                let n = p.len();
                for i in 0..n {
                    if let Some(v) = p.get(i) {
                        v.gc_mark()
                    }
                }
            }
        }
    }
}
