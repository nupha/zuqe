use std::ptr::NonNull;

use gc_lite::{GcPartitionId, GcRef, GcTracable, GcTraceCtx};

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
    pub(crate) data: Option<Box<[JSValue]>>,
}

unsafe impl GcTracable for NFuncPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.realm.trace(tr);
        if let Some(data) = &self.data {
            data.iter().for_each(|x| x.trace(tr));
        }
    }
}

impl NFuncPayload {
    /// get nth binding data
    pub fn data(&self, nth: usize) -> Option<JSValue> {
        self.data.as_ref().and_then(|x| x.get(nth).copied())
    }

    /// Call native callback
    fn call(
        &mut self,
        this_value: JSValue,
        args: &[JSValue],
        flags: i32,
        xref_scope: GcPartitionId,
    ) -> JSResult {
        let mut cc = CallContext::new(
            self.realm,
            this_value,
            args,
            self.magic as _,
            self.data.as_deref_mut(),
        );

        let ret_val: JSResult = match self.callback {
            Callback::Func(f) => f(cc),
            Callback::Getter(f) => {
                debug_assert_eq!(cc.length(), 0);
                f(cc)
            }
            Callback::Setter(f) => {
                debug_assert_eq!(cc.length(), 1);
                f(cc)
            }
            Callback::IteratorNext(f) => {
                let mut iter_state = IteratorState::NotDone;
                let r = f(cc, &mut iter_state);
                if let Ok(v) = r
                    && iter_state != IteratorState::NotParsed
                {
                    js_create_iterator_result(&self.realm, v, iter_state == IteratorState::Done)
                } else {
                    r
                }
            }
            Callback::Constructor(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    self.realm.throw_type_error("must be called with new");
                    return Err(-1);
                }
                f(cc)
            }
            Callback::ConstructorOrFunc(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    cc.set_this(JSValue::UNDEFINED); // set this_value to undefined
                }
                f(cc)
            }

            Callback::FF(f) => {
                //debug_assert!(data.is_none(), "binding data not allowed");
                cc.arg(0)
                    .to_f64(&self.realm)
                    .map(|n| JSValue::from(unsafe { f(n) }))
            }
            Callback::FFF(f) => {
                //debug_assert!(data.is_none(), "binding data not allowed");
                let d1 = cc.arg(0).to_f64(&self.realm)?;
                let d2 = cc.arg(1).to_f64(&self.realm)?;
                Ok(JSValue::from(unsafe { f(d1, d2) }))
            }
        };

        // Binding data could be updated inside callback,
        // promote scope of binding data
        if let Some(data) = &self.data
            && !xref_scope.is_null()
        {
            debug_assert!(!data.is_empty());
            for v in data {
                if let Some(n) = v.gc_node_ptr() {
                    self.realm.rt_ptr().gc_heap_mut().set_xref(xref_scope, n);
                }
            }
        }

        // promote gc scope of return value
        if let Ok(Some(n)) = ret_val.as_ref().map(JSValue::gc_node_ptr) {
            self.realm.rt_ptr().gc_heap_mut().set_xref(xref_scope, n);
        }

        ret_val
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
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let mut rt = ctx.rt_ptr();
        let mut nfunc = Self::try_from(func).unwrap();
        debug_assert_eq!(nfunc.class_id(), JS_CLASS_C_FUNCTION);

        #[cfg(debug_assertions)]
        nfunc.gc_node().debug_assert_node_valid(rt.gc_heap_mut());

        let (realm, args_length) = {
            let p = nfunc.payload();
            (p.realm, p.length as usize)
        };

        // better to always check stack overflow
        if js_check_stack_overflow(&rt, std::mem::size_of::<JSValue>() * args_length) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        // make a new stack frame to call native function.
        // push it as current_stack_from during running, and pop to restore previouse when calling exits.
        let mut stack_frame = StackFrame::new(nfunc.into(), ExecMode::empty());
        rt.push_stack_frame(NonNull::from_ref(&stack_frame));
        defer!({
            ctx.rt_ptr().pop_stack_frame();
        });

        if args.len() < args_length {
            // copy arguments to frame's local buffer
            stack_frame
                .local_buf
                .try_reserve_exact(args_length)
                .map_err(|_| {
                    realm.throw_out_of_memory();
                    -1
                })?;

            // copy args up to `length` count
            stack_frame.local_buf.extend(
                args.iter()
                    .copied()
                    .chain(std::iter::repeat(JSValue::UNDEFINED))
                    .take(args_length),
            );
            debug_assert_eq!(stack_frame.local_buf.len(), args_length);

            stack_frame.arg_count = args_length as _;
            stack_frame.arg_real_count = args.len() as _;
            stack_frame.arg_base_ptr = stack_frame.local_buf.as_mut_ptr();
        } else {
            stack_frame.arg_count = args.len() as _;
            stack_frame.arg_real_count = args.len() as _;
            stack_frame.arg_base_ptr = args.as_ptr() as *mut JSValue;
        }

        // Note: return value could be allocated in `stack_frame` gc scope,
        // must promote return value and binding_data (if any) before stack_frame drop.
        let xref_scope = stack_frame
            .parent_mut()
            .map_or(rt.gc_top_scope, |up| up.init_gc_scope());

        nfunc
            .payload_mut()
            .call(this_obj, stack_frame.arg_values(), flags, xref_scope)
    }

    // TODO: shall we combine this with `::class_call()`?
    pub fn class_call_with_data(
        _ctx: Context,
        func_obj: JSValue,
        this_val: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let mut nfunc = NativeFunction::try_from(func_obj).unwrap();
        let nfunc_scope = nfunc.gc_node().scope_id();

        debug_assert_eq!(nfunc.class_id(), JS_CLASS_C_FUNCTION_DATA);
        debug_assert!(matches!(nfunc.payload().callback, Callback::Func(_)));

        nfunc.payload_mut().call(this_val, args, 0, nfunc_scope)
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

        let mut func_obj = {
            let p = Box::new(NFuncPayload {
                callback,
                length,
                magic,
                realm: unsafe { ContextPtr::from_ref_unchecked(ctx) },
                data: binding_data,
            });

            ctx.create_object_with_proto(
                proto,
                if has_binding_data {
                    ObjectPayload::C_FUNCTION_DATA(p)
                } else {
                    ObjectPayload::C_FUNCTION(p)
                },
            )
            .map_err(|_| -1)?
        };

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

    #[inline(always)]
    pub(crate) fn payload_mut(&mut self) -> &mut NFuncPayload {
        self.0.payload_mut().native_func_mut().unwrap()
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
    ) -> Result<NativeFunction, i32> {
        NativeFunction::create(
            self,
            callback,
            name,
            length,
            magic,
            Some(self.function_proto()),
            binding_data,
        )
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
    ) -> Result<NativeFunction, i32> {
        Self::create_native_function_with_data(self, callback, name, length, magic, None)
    }
}
