use std::{cell::RefCell, ops::Deref, rc::Rc};

use crate::{
    JS_CLASS_C_FUNCTION_DATA,
    atom::consts::JS_ATOM_empty_string,
    context::JSContext,
    error::JSResult,
    function::{CallContext, NativeCallback, js_function_set_properties},
    object::{JSObject, ObjectPayload},
    object_cast,
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
    fn new(callback: NativeCallback, length: u8, magic: i32, data: Box<[JSValue]>) -> Box<Self> {
        Box::new(NativeFuncData {
            func: callback,
            length,
            magic: magic as _,
            data: Rc::new(RefCell::new(data)),
        })
    }

    /// get data count
    pub fn len(&self) -> usize {
        self.data.borrow().len()
    }

    /// get nth data
    pub fn data(&self, nth: usize) -> Option<JSValue> {
        self.data.borrow().get(nth).cloned()
    }
}

/// A callable object. Is an instance of JS_CLASS_C_FUNCTION_DATA class.
/// When called, will invoke native callback with attached data payload.
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct NativeFunctionWithData(JSObject);

object_cast!(NativeFunctionWithData, JS_CLASS_C_FUNCTION_DATA);

impl NativeFunctionWithData {
    /// Create a C_FUNCTION_DATA class object, which behaves like a native function, with attached data.
    pub(crate) fn create(
        ctx: &JSContext,
        callback: NativeCallback,
        length: u8,
        magic: i32,
        data: Box<[JSValue]>,
    ) -> Result<Self, i32> {
        let func_obj = ctx
            .create_object_from_proto(
                ctx.0.borrow().function_proto.clone(),
                ObjectPayload::C_FUNCTION_DATA(NativeFuncData::new(callback, length, magic, data)),
            )
            .map_err(|_| -1)?;

        js_function_set_properties(ctx, &func_obj, JS_ATOM_empty_string, length as _);
        Ok(Self(func_obj))
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
        let (f, data, magic) = if let ObjectPayload::C_FUNCTION_DATA(s) =
            unsafe { func_obj.as_object_unchecked().payload() }
        {
            (s.func.clone(), s.data.clone(), s.magic)
        } else {
            unreachable!()
        };

        f(
            ctx,
            CallContext::new(ctx, this_val.clone(), args, magic as _, Some(data)),
        )
    }

    pub fn class_finalizer(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {}

    pub fn class_mark(val: JSValue) {
        unsafe {
            if let ObjectPayload::C_FUNCTION_DATA(p) = val.as_object_unchecked().payload() {
                let n = p.len();
                for i in 0..n {
                    if let Some(v) = p.data(i) {
                        v.gc_mark()
                    }
                }
            }
        }
    }
}
