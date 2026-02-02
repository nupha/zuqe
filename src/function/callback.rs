use std::ptr::NonNull;

use gc_lite::{GcPartitionId, GcTracable, GcTraceOp, GcWeak};

use crate::{
    atom::Atom,
    builtins::iterator::js_create_iterator_result,
    context::{Context, ContextImpl, ContextPtr},
    defer,
    error::JSResult,
    function::{CallContext, IteratorState, JS_CALL_FLAG_CONSTRUCTOR},
    object::{Object, ObjectImpl, ObjectPayload},
    runtime::js_check_stack_overflow,
    stack_frame::{ExecMode, StackFrame},
    value::JSValue,
};

use super::js_function_set_properties;

pub type CallbackFn = fn(cc: CallContext) -> JSResult;
pub type GetterFn = fn(cc: CallContext) -> JSResult;
pub type SetterFn = fn(cc: CallContext) -> JSResult;
pub type IteratorFn = fn(cc: CallContext, iter_state: &mut IteratorState) -> JSResult;

pub type JSCFunction = unsafe fn(ctx: &ContextImpl, this_val: JSValue, args: &[JSValue]) -> JSValue;
pub type JSCFunctionMagic = unsafe fn(
    ctx: &ContextImpl,
    this_val: JSValue,
    args: &[JSValue],
    magic: ::core::ffi::c_int,
) -> JSValue;

pub type JSCFunctionData = unsafe fn(
    ctx: &ContextImpl,
    this_val: JSValue,
    args: &[JSValue],
    magic: i32,
    func_data: &mut [JSValue],
) -> JSValue;

pub type JSCFunctionFF = unsafe fn(f64) -> f64;
pub type JSCFunctionFFF = unsafe fn(f64, f64) -> f64;

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum Callback {
    Func(CallbackFn),
    Getter(GetterFn),
    Setter(SetterFn),
    Constructor(CallbackFn),
    ConstructorOrFunc(CallbackFn),
    IteratorNext(IteratorFn),

    CFunc(JSCFunction),
    CFuncMagic(JSCFunctionMagic),

    FF(JSCFunctionFF),
    FFF(JSCFunctionFFF),
}

impl PartialEq for Callback {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Func(a), Self::Func(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::Getter(a), Self::Getter(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::Setter(a), Self::Setter(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::Constructor(a), Self::Constructor(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::ConstructorOrFunc(a), Self::ConstructorOrFunc(b)) => {
                std::ptr::fn_addr_eq(*a, *b)
            }
            (Self::IteratorNext(a), Self::IteratorNext(b)) => std::ptr::fn_addr_eq(*a, *b),

            (Self::CFunc(a), Self::CFunc(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::CFuncMagic(a), Self::CFuncMagic(b)) => std::ptr::fn_addr_eq(*a, *b),

            (Self::FF(a), Self::FF(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::FFF(a), Self::FFF(b)) => std::ptr::fn_addr_eq(*a, *b),
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NFuncPayload {
    pub handler: Callback,
    pub realm: GcWeak<ContextImpl>,
    pub length: u8,
    pub magic: i16,
}

unsafe impl GcTracable for NFuncPayload {
    #[inline(always)]
    fn trace(&self, _: GcTraceOp) {}
}

impl NFuncPayload {
    pub fn class_call(
        ctx: Context,
        func: JSValue,
        mut this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let mut rt = ctx.rt_ptr();
        let func_obj = Object::try_from(func).unwrap();

        let (callback, arg_length, magic, realm) =
            if let ObjectPayload::C_FUNCTION(c) = func_obj.payload() {
                (
                    c.handler,
                    c.length,
                    c.magic,
                    ContextPtr::from(c.realm.upgrade(rt.gc_heap()).unwrap()),
                )
            } else {
                unreachable!()
            };

        // better to always check stack overflow
        if js_check_stack_overflow(&rt, std::mem::size_of::<JSValue>() * arg_length as usize) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        // new stack frame
        let mut stack_frame = StackFrame::new(func_obj, ExecMode::empty());

        // set new stack frame to be the 'current-stack-frame',
        // setup to perform pop stack frame on function exit.
        rt.push_stack_frame(NonNull::from_ref(&stack_frame));
        defer!({
            ctx.rt_ptr().pop_stack_frame();
        });

        let realm_cx = &*realm;

        if args.len() < arg_length as usize {
            // copy arguments to local buffer
            stack_frame
                .local_buf
                .try_reserve_exact(arg_length as usize)
                .map_err(|_| {
                    realm_cx.throw_out_of_memory();
                    -1
                })?;

            // copy pass in args, cloned
            stack_frame.local_buf.extend(args.iter().copied());
            // padding with undefined
            stack_frame.local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                arg_length as usize - args.len(),
            ));
            debug_assert_eq!(stack_frame.local_buf.len(), arg_length as usize);

            stack_frame.arg_count = arg_length as _;
            stack_frame.arg_real_count = args.len() as _;
            stack_frame.arg_base_ptr = stack_frame.local_buf.as_mut_ptr();
        } else {
            stack_frame.arg_count = args.len() as _;
            stack_frame.arg_real_count = args.len() as _;
            stack_frame.arg_base_ptr = args.as_ptr() as *mut JSValue;
        }

        let ret_val: JSResult = match callback {
            Callback::Func(f) => {
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, None);
                f(cc)
            }

            Callback::Getter(f) => {
                debug_assert_eq!(stack_frame.arg_count, 0);
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, None);
                f(cc)
            }
            Callback::Setter(f) => {
                debug_assert_eq!(stack_frame.arg_count, 1);
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, None);
                f(cc)
            }
            Callback::IteratorNext(f) => {
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, None);
                let mut iter_state = IteratorState::NotDone;
                let mut r = f(cc, &mut iter_state);
                if r.is_ok() && iter_state != IteratorState::NotParsed {
                    r = js_create_iterator_result(
                        realm_cx,
                        r.unwrap(),
                        iter_state == IteratorState::Done,
                    );
                }
                r
            }
            Callback::Constructor(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    realm_cx.throw_type_error("must be called with new");
                    return Err(-1);
                }
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, None);
                f(cc)
            }
            Callback::ConstructorOrFunc(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    this_obj = JSValue::UNDEFINED;
                }
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, None);
                f(cc)
            }
            //
            // LEGACIES
            //
            Callback::CFunc(f) => unsafe { f(realm_cx, this_obj, stack_frame.arg_values()).err() },
            Callback::CFuncMagic(f) => unsafe {
                f(realm_cx, this_obj, stack_frame.arg_values(), magic as i32).err()
            },

            Callback::FF(f) => stack_frame.arg_values()[0]
                .to_f64(realm_cx)
                .map(|n| JSValue::from(unsafe { f(n) })),
            Callback::FFF(f) => {
                let args = stack_frame.arg_values();
                let d1 = args[0].to_f64(realm_cx)?;
                let d2 = args[1].to_f64(realm_cx)?;

                Ok(JSValue::from(unsafe { f(d1, d2) }))
            }
        };

        if let Ok(Some(n)) = ret_val.as_ref().map(JSValue::gc_node_ptr)
            && unsafe { n.as_ref().get_partition_id() } == stack_frame.gc_scope_id
        {
            // `ret_val` was allocated in `stack_frame` scope, the `stack_frame` will be dropped at exit,
            // must migrate `ret_val` to upper scope
            let root_scope = rt.gc_root_scope;
            rt.gc_heap_mut().set_xref(
                stack_frame
                    .parent_mut()
                    .map_or(root_scope, |up| up.init_gc_scope()),
                n,
            );
        }

        ret_val
    }
}

// #[repr(transparent)]
// #[derive(Debug, Clone)]
// pub struct NativeFunction(JSValue);

impl ObjectImpl {
    /// Checks if Object is native function of specific type.
    /// Matches both the callback pointer and magic.
    pub fn match_native_callback(&self, callback: Callback, magic: i32) -> bool {
        if let ObjectPayload::C_FUNCTION(cfunc) = self.payload() {
            cfunc.magic == magic as i16 && callback.eq(&cfunc.handler)
        } else {
            false
        }
    }
}

impl JSValue {
    /// Checks if a JSValue is native function of specific type.
    /// Matches both the callback pointer and magic.
    pub fn match_native_callback(&self, callback: Callback, magic: i32) -> bool {
        if let Self::Object(o) = self {
            o.match_native_callback(callback, magic)
        } else {
            false
        }
    }
}

impl ContextImpl {
    /// Create new function object with native callback and shape of specified prototype.
    /// at least `length` arguments will be readable in arguments.
    pub fn create_native_function_with_proto(
        &self,
        call: Callback,
        name: &str,
        length: u8,
        magic: i16,
        proto: Option<Object>,
    ) -> Result<Object, i32> {
        let mut func_obj = self
            .create_object_with_proto(
                proto,
                ObjectPayload::C_FUNCTION(NFuncPayload {
                    handler: call,
                    length,
                    magic,
                    realm: unsafe { ContextPtr::from_ref_unchecked(self).downgrade() },
                }),
            )
            .map_err(|_| -1)?;

        func_obj.set_flag_constructor(matches!(
            call,
            Callback::Constructor(_) | Callback::ConstructorOrFunc(_)
        ));

        // define properties length and name on function.
        let name_atom = Atom::new(self.rt_ptr(), name)?;
        js_function_set_properties(self, func_obj, name_atom, length as i32);

        Ok(func_obj)
    }

    /// Create new function object with native callback. using function proto as proto object.
    /// at least `length` arguments will be readable in arguments.
    #[inline(always)]
    pub fn create_native_function(
        &self,
        handler: Callback,
        name: &str,
        length: u8,
        magic: i16,
    ) -> Result<Object, i32> {
        self.create_native_function_with_proto(
            handler,
            name,
            length,
            magic,
            Some(self.function_proto()),
        )
    }
}
