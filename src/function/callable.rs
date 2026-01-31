use std::{cell::RefCell, rc::Rc};

use gc_lite::{GcRef, GcTracable, GcTraceOp};

use crate::{
    JS_CLASS_C_FUNCTION_DATA,
    atom::Atom,
    context::{Context, ContextImpl},
    error::JSResult,
    function::{CallContext, CallbackFn, js_function_set_properties},
    object::{Object, ObjectImpl, ObjectPayload},
    object_cast,
    value::JSValue,
};

#[derive(Debug)]
pub struct NativeFuncData {
    func: CallbackFn,
    length: u8,
    data: Rc<RefCell<Box<[JSValue]>>>,
    magic: u16,
}

unsafe impl GcTracable for NativeFuncData {
    fn trace(&self, tr: GcTraceOp) {
        self.data.borrow().iter().for_each(|x| x.trace(tr));
    }
}

impl NativeFuncData {
    fn new(callback: CallbackFn, length: u8, magic: i32, data: Box<[JSValue]>) -> Box<Self> {
        Box::new(NativeFuncData {
            func: callback,
            length,
            magic: magic as _,
            data: Rc::new(RefCell::new(data)),
        })
    }

    /// get data count
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.data.borrow().len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// get nth data
    pub fn data(&self, nth: usize) -> Option<JSValue> {
        self.data.borrow().get(nth).cloned()
    }
}

/// A callable object. Is an instance of JS_CLASS_C_FUNCTION_DATA class.
/// When called, will invoke native callback with attached data payload.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct NativeFunctionWithData(GcRef<ObjectImpl>);

object_cast!(NativeFunctionWithData, JS_CLASS_C_FUNCTION_DATA);

unsafe impl GcTracable for NativeFunctionWithData {
    fn trace(&self, mut tr: GcTraceOp) {
        tr.add(self.gc_ref());
    }
}

impl NativeFunctionWithData {
    /// Create a C_FUNCTION_DATA class object, which behaves like a native function, with attached data.
    pub(crate) fn create(
        ctx: &ContextImpl,
        callback: CallbackFn,
        length: u8,
        magic: i32,
        data: Box<[JSValue]>,
    ) -> Result<Self, i32> {
        let func_obj = ctx
            .create_object_with_proto(
                Some(ctx.function_proto()),
                ObjectPayload::C_FUNCTION_DATA(NativeFuncData::new(callback, length, magic, data)),
            )
            .map_err(|_| -1)?;

        js_function_set_properties(ctx, func_obj, Atom::CONST_empty_string, length as _);
        Ok(Self(func_obj.gc_ref()))
    }
}

//
// Hooks
//
impl NativeFunctionWithData {
    pub fn class_call(
        ctx: Context,
        func_obj: JSValue,
        this_val: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        if let ObjectPayload::C_FUNCTION_DATA(s) =
            unsafe { func_obj.as_object_unchecked().payload() }
        {
            (s.func)(CallContext::new(
                ctx.into(),
                this_val,
                args,
                s.magic as _,
                Some(s.data.clone()),
            ))
        } else {
            unreachable!()
        }
    }
}
