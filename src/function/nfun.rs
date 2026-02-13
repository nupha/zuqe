use std::{cell::RefCell, ptr::NonNull, rc::Rc};

use gc_lite::{GcRef, GcTracable, GcTraceOp};

use crate::{
    JS_CLASS_C_FUNCTION, JS_CLASS_C_FUNCTION_DATA,
    atom::Atom,
    builtins::iterator::js_create_iterator_result,
    context::{Context, ContextImpl, ContextPtr},
    defer,
    error::JSResult,
    object::{Object, ObjectImpl, ObjectPayload},
    object_cast,
    runtime::js_check_stack_overflow,
    stack_frame::{ExecMode, StackFrame},
    value::JSValue,
};

use super::{CallContext, IteratorState, JS_CALL_FLAG_CONSTRUCTOR, js_function_set_properties};

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

    FF(JSCFunctionFF),
    FFF(JSCFunctionFFF),

    #[deprecated]
    CFunc(JSCFunction),
    #[deprecated]
    CFuncMagic(JSCFunctionMagic),
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

            (Self::FF(a), Self::FF(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::FFF(a), Self::FFF(b)) => std::ptr::fn_addr_eq(*a, *b),

            (Self::CFunc(a), Self::CFunc(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::CFuncMagic(a), Self::CFuncMagic(b)) => std::ptr::fn_addr_eq(*a, *b),

            _ => false,
        }
    }
}

/// object payload for native function
#[derive(Debug)]
pub(crate) struct NFuncPayload {
    pub(crate) callback: Callback,
    pub(crate) length: u8,
    pub(crate) magic: i16,
    pub(crate) realm: ContextPtr,

    /// binding values attached to native func
    pub(crate) data: Option<Rc<RefCell<Box<[JSValue]>>>>,
}

unsafe impl GcTracable for NFuncPayload {
    fn trace(&self, tr: GcTraceOp) {
        self.realm.trace(tr);
        if let Some(data) = &self.data {
            data.borrow().iter().for_each(|x| x.trace(tr));
        }
    }
}

impl NFuncPayload {
    /// get nth binding data
    pub fn data(&self, nth: usize) -> Option<JSValue> {
        self.data
            .as_ref()
            .and_then(|x| x.borrow().get(nth).copied())
    }
}

/// Native function object
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct NativeFunction(GcRef<ObjectImpl>);

object_cast!(
    NativeFunction,
    JS_CLASS_C_FUNCTION | JS_CLASS_C_FUNCTION_DATA
);

impl NativeFunction {
    pub fn class_call(
        ctx: Context,
        func: JSValue,
        mut this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let mut rt = ctx.rt_ptr();

        let func_obj = Self::try_from(func).unwrap();

        #[cfg(debug_assertions)]
        func_obj
            .gc_node()
            .debug_assert_node_tree_valid(rt.gc_heap());

        let nfunc = func_obj.payload();
        let realm = nfunc.realm;
        let magic = nfunc.magic;
        let data = nfunc.data.clone();

        // better to always check stack overflow
        if js_check_stack_overflow(&rt, std::mem::size_of::<JSValue>() * nfunc.length as usize) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        // new stack frame
        let mut stack_frame = StackFrame::new(func_obj.into(), ExecMode::empty());

        // set new stack frame to be the 'current-stack-frame',
        // setup to perform pop stack frame on function exit.
        rt.push_stack_frame(NonNull::from_ref(&stack_frame));
        defer!({
            ctx.rt_ptr().pop_stack_frame();
        });

        if args.len() < nfunc.length as usize {
            // copy arguments to local buffer
            stack_frame
                .local_buf
                .try_reserve_exact(nfunc.length as usize)
                .map_err(|_| {
                    realm.throw_out_of_memory();
                    -1
                })?;

            // copy args up to `length` count
            stack_frame.local_buf.extend(
                args.iter()
                    .copied()
                    .chain(std::iter::repeat(JSValue::UNDEFINED))
                    .take(nfunc.length as usize),
            );
            debug_assert_eq!(stack_frame.local_buf.len(), nfunc.length as usize);

            stack_frame.arg_count = nfunc.length as _;
            stack_frame.arg_real_count = args.len() as _;
            stack_frame.arg_base_ptr = stack_frame.local_buf.as_mut_ptr();
        } else {
            stack_frame.arg_count = args.len() as _;
            stack_frame.arg_real_count = args.len() as _;
            stack_frame.arg_base_ptr = args.as_ptr() as *mut JSValue;
        }

        let ret_val: JSResult = match nfunc.callback {
            Callback::Func(f) => {
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, data);
                f(cc)
            }
            Callback::Getter(f) => {
                debug_assert_eq!(stack_frame.arg_count, 0);
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, data);
                f(cc)
            }
            Callback::Setter(f) => {
                debug_assert_eq!(stack_frame.arg_count, 1);
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, data);
                f(cc)
            }
            Callback::IteratorNext(f) => {
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, data);
                let mut iter_state = IteratorState::NotDone;
                let mut r = f(cc, &mut iter_state);
                if r.is_ok() && iter_state != IteratorState::NotParsed {
                    r = js_create_iterator_result(
                        &realm,
                        r.unwrap(),
                        iter_state == IteratorState::Done,
                    );
                }
                r
            }
            Callback::Constructor(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    realm.throw_type_error("must be called with new");
                    return Err(-1);
                }
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, data);
                f(cc)
            }
            Callback::ConstructorOrFunc(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    this_obj = JSValue::UNDEFINED;
                }
                let cc =
                    CallContext::new(realm, this_obj, stack_frame.arg_values(), magic as _, data);
                f(cc)
            }

            Callback::FF(f) => {
                debug_assert!(data.is_none(), "binding data not allowed");
                stack_frame.arg_values()[0]
                    .to_f64(&realm)
                    .map(|n| JSValue::from(unsafe { f(n) }))
            }
            Callback::FFF(f) => {
                debug_assert!(data.is_none(), "binding data not allowed");
                let args = stack_frame.arg_values();
                let d1 = args[0].to_f64(&realm)?;
                let d2 = args[1].to_f64(&realm)?;

                Ok(JSValue::from(unsafe { f(d1, d2) }))
            }

            //
            // LEGACIES
            //
            Callback::CFunc(f) => unsafe {
                debug_assert!(data.is_none(), "binding data not allowed");
                f(&realm, this_obj, stack_frame.arg_values()).err()
            },
            Callback::CFuncMagic(f) => unsafe {
                debug_assert!(data.is_none(), "binding data not allowed");
                f(&realm, this_obj, stack_frame.arg_values(), magic as i32).err()
            },
        };

        // Binding data could be updated inside CallContext,
        // promote binding data to at least func's gc scope.
        if let Some(data) = &nfunc.data {
            let func_scope = func_obj.gc_node().scope_id();
            for v in data.borrow().iter() {
                if let Some(n) = v.gc_node_ptr() {
                    ctx.rt_ptr().gc_heap_mut().set_xref(func_scope, n);
                }
            }
        }

        // promote return value gc scope.
        if let Ok(Some(n)) = ret_val.as_ref().map(JSValue::gc_node_ptr)
            && unsafe { n.as_ref().scope_id() == stack_frame.gc_scope_id }
        {
            // `ret_val` was allocated in `stack_frame` gc scope, the `stack_frame` will be dropped at exit,
            // promote `ret_val` to upper scope
            let xref = stack_frame
                .parent_mut()
                .map_or(rt.gc_root_scope, |up| up.init_gc_scope());

            rt.gc_heap_mut().set_xref(xref, n);
        }

        ret_val
    }

    pub fn class_call_with_data(
        ctx: Context,
        func_obj: JSValue,
        this_val: JSValue,
        args: &[JSValue],
        _: i32,
    ) -> JSResult {
        match unsafe { func_obj.as_object_unchecked().payload() } {
            ObjectPayload::C_FUNCTION_DATA(s) => {
                if let Callback::Func(cb) = s.callback {
                    let ret = cb(CallContext::new(
                        ctx.into(),
                        this_val,
                        args,
                        s.magic as _,
                        s.data.clone(),
                    ));

                    // Binding data could be updated inside CallContext,
                    // promote binding data to at least nfunc's gc scope.
                    if let Some(data) = &s.data {
                        let scope = func_obj.gc_node().unwrap().scope_id();
                        for v in data.borrow().iter() {
                            if let Some(n) = v.gc_node_ptr() {
                                ctx.rt_ptr().gc_heap_mut().set_xref(scope, n);
                            }
                        }
                    }

                    ret
                } else {
                    unreachable!()
                }
            }
            _ => {
                unreachable!()
            }
        }
    }

    /// Create native function object.
    /// at least `length` arguments can be readable in arguments.
    pub(crate) fn create(
        ctx: &ContextImpl,
        callback: Callback,
        name: &str,
        length: u8,
        magic: i16,
        proto: Option<Object>,
        binding_data: Option<Box<[JSValue]>>,
    ) -> Result<Self, i32> {
        let has_binding_data = binding_data.is_some();

        let p = Box::new(NFuncPayload {
            callback,
            length,
            magic,
            realm: unsafe { ContextPtr::from_ref_unchecked(ctx) },
            data: binding_data.map(|d| Rc::new(RefCell::new(d))),
        });
        let mut func_obj = ctx
            .create_object_with_proto(
                proto,
                if has_binding_data {
                    ObjectPayload::C_FUNCTION_DATA(p)
                } else {
                    ObjectPayload::C_FUNCTION(p)
                },
            )
            .map_err(|_| -1)?;

        if matches!(
            callback,
            Callback::Constructor(_) | Callback::ConstructorOrFunc(_)
        ) {
            func_obj.set_flag_constructor(true);
        }

        // define `length` and `name` properties on function.
        let name_atom = Atom::new(ctx.rt_ptr(), name)?;
        js_function_set_properties(ctx, func_obj, name_atom, length as i32);

        Ok(Self(func_obj.gc_ref()))
    }

    #[inline(always)]
    pub(crate) fn payload(&self) -> &NFuncPayload {
        self.0.payload().native_func().unwrap()
    }
}

impl ObjectImpl {
    /// Checks if Object is native function of specific type.
    /// Matches both the callback pointer and magic.
    pub fn match_native_callback(&self, callback: Callback, magic: i32) -> bool {
        if let Some(nf) = self.payload().native_func() {
            nf.magic == magic as i16 && callback.eq(&nf.callback)
        } else {
            false
        }
    }
}

impl JSValue {
    /// Checks if a JSValue is native function of specific type.
    /// Matches both the callback pointer and magic.
    pub fn match_native_callback(&self, callback: Callback, magic: i32) -> bool {
        self.as_object()
            .is_some_and(|o| o.match_native_callback(callback, magic))
    }
}

impl ContextImpl {
    /// Create native function object with callback and binding data, using function proto as proto object.
    /// at least `length` arguments will be readable in arguments.
    #[inline(always)]
    pub fn create_native_function_with_data(
        &self,
        callback: Callback,
        name: &str,
        length: u8,
        magic: i16,
        binding_data: Option<Box<[JSValue]>>,
    ) -> Result<Object, i32> {
        NativeFunction::create(
            self,
            callback,
            name,
            length,
            magic,
            Some(self.function_proto()),
            binding_data,
        )
        .map(|x| x.into())
    }

    /// Create native function object with callback, using function proto as proto object.
    /// at least `length` arguments will be readable in arguments.
    #[inline(always)]
    pub fn create_native_function(
        &self,
        callback: Callback,
        name: &str,
        length: u8,
        magic: i16,
    ) -> Result<Object, i32> {
        NativeFunction::create(
            self,
            callback,
            name,
            length,
            magic,
            Some(self.function_proto()),
            None,
        )
        .map(|x| x.into())
    }
}
