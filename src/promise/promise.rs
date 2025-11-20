use std::{
    cell::{Cell, RefCell},
    ops::DerefMut,
    rc::Rc,
};

use gc_lite::{Collect, GcRef};

use crate::{
    JS_CLASS_PROMISE,
    atom::Atom,
    callable::NativeFunctionWithData,
    class::ClassID,
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{CallContext, JS_CallConstructor, NativeFunctionType, js_function_set_properties},
    js_core::error::js_aggregate_error_constructor,
    object::{JS_PROP_C_W_E, ObjectImpl, ObjectPayload, ObjectRef},
    object_cast,
    promise::{JSPromiseReactionData, js_promise_funcs, js_promise_proto_funcs},
    runtime::RuntimeImpl,
    value::JSValue,
};

use super::PromiseResolvePayload;

pub type JSHostPromiseRejectionTracker = fn(
    ctx: &ContextImpl,
    promise: Promise,
    reason: JSValue,
    is_handled: ::core::ffi::c_int,
    opaque: *mut ::core::ffi::c_void,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PromiseState {
    JS_PROMISE_PENDING,
    JS_PROMISE_FULFILLED,
    JS_PROMISE_REJECTED,
}

#[derive(Debug)]
pub struct PromisePayload {
    promise_state: Cell<PromiseState>,

    /// list of pending reactions:
    /// #0 for fulfill, #1 for reject.
    promise_reactions: RefCell<[Vec<Box<JSPromiseReactionData>>; 2]>,

    promise_result: RefCell<JSValue>,

    /// Note: only useful to debug
    is_handled: Cell<bool>,
}

unsafe impl Collect for PromisePayload {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.promise_reactions.borrow()[0].trace(tracer);
        self.promise_reactions.borrow()[1].trace(tracer);
        self.promise_result.borrow().trace(tracer);
    }
}

impl PromisePayload {
    #[inline(always)]
    pub const fn state(&self) -> PromiseState {
        self.promise_state.get()
    }

    #[inline(always)]
    pub fn set_state(&self, s: PromiseState) {
        self.promise_state.set(s);
    }

    #[inline(always)]
    pub fn result(&self) -> JSValue {
        self.promise_result.borrow().clone()
    }

    pub fn set_result(&self, val: JSValue) {
        *self.promise_result.borrow_mut() = val;
    }

    /// add pending reaction callback functions
    pub(super) fn add_reaction(
        &self,
        success: Box<JSPromiseReactionData>,
        fail: Box<JSPromiseReactionData>,
    ) {
        let mut b = self.promise_reactions.borrow_mut();
        b[0].push(success);
        b[1].push(fail);
    }

    /// take pending reactions out, reset reactions list to empty.
    pub(super) fn take_reactions(&self) -> [Vec<Box<JSPromiseReactionData>>; 2] {
        std::mem::take(self.promise_reactions.borrow_mut().deref_mut())
    }

    pub(super) const fn is_handled(&self) -> bool {
        self.is_handled.get()
    }
    pub(super) fn set_handled(&self, v: bool) {
        self.is_handled.set(v);
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct Promise(GcRef<ObjectImpl>);

object_cast!(Promise, JS_CLASS_PROMISE);

impl Promise {
    /// create new Promise object with given executor function.
    /// `new_target` - the 'new' operator target.
    /// `executor` must be a callable object.
    fn from_executor(ctx: &ContextImpl, executor: ObjectRef, ctor: JSValue) -> Result<Self, i32> {
        executor.ensure_function(ctx)?;

        // create js object of promise class
        let isa = Box::new(PromisePayload {
            promise_state: Cell::new(PromiseState::JS_PROMISE_PENDING),
            promise_reactions: RefCell::new([Vec::new(), Vec::new()]),
            promise_result: RefCell::new(JSValue::UNDEFINED),
            is_handled: Cell::new(false),
        });
        let promise = Self(
            ctx.create_object_from_ctor(ctor, ObjectPayload::PROMISE(isa))
                .map_err(|_| -1)?
                .0,
        );

        let rr = promise.create_resolving_functions(ctx)?;
        if executor.call(ctx, JSValue::UNDEFINED, &rr).is_err() {
            let exc = ctx.rt().take_exception();
            rr[1].call(ctx, JSValue::UNDEFINED, &[exc])?;
        }

        Ok(promise)
    }

    /// create new Promise and associated resolving function pairs.
    /// returns Object (could be Promise or derived class instance) and \[resolve, reject\] functions.
    /// `ctor` is the constructor function value to instantiate Promise object:
    ///  - when provided, will call this function to create customized Promise object,
    ///  - when it is `undefined`, creates default Promise object.
    pub(super) fn create_from_ctor(
        ctx: &ContextImpl,
        ctor: JSValue,
    ) -> Result<(ObjectRef, [JSValue; 2]), i32> {
        let executor = NativeFunctionWithData::create(
            ctx,
            Promise::executor_handler,
            2,
            0,
            // resolving functions, initially undefined.
            // these two fields will be replace by real Function object
            // in callback handler, which takes two arguments: resolve, reject.
            [JSValue::UNDEFINED, JSValue::UNDEFINED].into(),
        )?;

        let promise: ObjectRef = if ctor.is_undefined() {
            Self::from_executor(ctx, executor.clone().into(), JSValue::UNDEFINED)?.into()
        } else {
            JS_CallConstructor(ctx, ctor, std::slice::from_ref(&executor.into()))
                .err()?
                .try_into()?
        };

        // collect resolving functions
        let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];
        if let Some(s) = executor.payload().native_func_with_data() {
            for i in 0..2 {
                let item = s.data(i).unwrap();
                item.ensure_function(ctx)?;
                resolving_funcs[i] = item;
            }

            Ok((promise, resolving_funcs))
        } else {
            unreachable!()
        }
    }

    /// create default Promise and associated resolving functions.
    /// returns a Promise object and \[resolve, reject\] functions pair.
    #[inline]
    pub fn create(ctx: &ContextImpl) -> Result<(Self, [JSValue; 2]), i32> {
        Self::create_from_ctor(ctx, JSValue::UNDEFINED).map(|(p, rr)| {
            debug_assert_eq!(p.class_id(), JS_CLASS_PROMISE);
            (Self(p.0), rr)
        })
    }

    /// create \[resolve, reject\] function pairs for `self` promise object.
    pub(super) fn create_resolving_functions(
        &self,
        ctx: &ContextImpl,
    ) -> Result<[JSValue; 2], i32> {
        // [resolve, reject]
        let mut resolving_funcs = [JSValue::UNDEFINED, JSValue::UNDEFINED];
        let resolved = Rc::new(Cell::new(false));
        let func_proto = ctx.function_proto();

        for i in 0..2 {
            let s = Box::new(PromiseResolvePayload {
                promise: self.clone(),
                resolved: resolved.clone(), // shared between two functions
            });
            let func = ctx
                .create_object_with_proto(
                    Some(func_proto),
                    if i == 0 {
                        ObjectPayload::PROMISE_RESOLVE_FUNCTION(s)
                    } else {
                        ObjectPayload::PROMISE_REJECT_FUNCTION(s)
                    },
                )
                .map_err(|_| -1)?;
            js_function_set_properties(ctx, func, Atom::CONST_empty_string, 1);
            resolving_funcs[i as usize] = func.into();
        }

        Ok(resolving_funcs)
    }

    /// get Promise payload
    #[inline(always)]
    pub fn payload(&self) -> &PromisePayload {
        if let ObjectPayload::PROMISE(p) = unsafe { self.0.as_ref().payload() } {
            p.as_ref()
        } else {
            #[cfg(debug_assertions)]
            unreachable!();
            #[cfg(not(debug_assertions))]
            unsafe {
                std::hint::unreachable_unchecked();
            }
        }
    }

    /// get Promise state.
    #[inline(always)]
    pub fn state(&self) -> PromiseState {
        self.payload().state()
    }

    /// get Promise result.
    /// returned value is duplicated, caller must free it.
    #[inline(always)]
    pub fn result(&self) -> JSValue {
        self.payload().result()
    }
}

//
// Hooks for Promise class
//
impl Promise {
    /// callback to execute a promise:
    /// setting resolve and reject functions as attaching data of 'executor'.
    /// arg#0: resolve function, arg#1: reject function
    fn executor_handler(ctx: ContextRef, mut cc: CallContext) -> JSResult {
        for i in 0..2 {
            if !cc.data(i).is_undefined() {
                ctx.throw_type_error("resolving function already set");
                return Err(-1);
            } else {
                let _ = cc.replace_data(i, cc.arg(i).clone());
            }
        }
        Ok(JSValue::UNDEFINED)
    }

    /// the native callback for Promise constructor function
    pub fn class_constructor(mut ctx: ContextRef, cc: CallContext) -> JSResult {
        let executor = *cc.arg(0).ensure_function(&ctx)?.as_object().unwrap();
        Promise::from_executor(&mut ctx, executor, *cc.this()).map(|p| p.into())
    }

    pub fn class_initialize(mut ctx: ContextRef, _: ClassID) -> Result<Option<ObjectRef>, i32> {
        let mut proto = ctx.create_base_object()?;
        proto.install_field_defs(&ctx, &js_promise_proto_funcs)?;

        let mut ctor: JSValue = ctx
            .create_native_function(
                NativeFunctionType::ConstructorOrFunc(Self::class_constructor),
                "Promise",
                1,
                0,
            )?
            .into();
        ctor.install_field_defs(&ctx, &js_promise_funcs)?;
        ctx.bind_ctor_proto(ctor.clone(), proto.into())?;

        ctx.global_obj()
            .define_property_value_str(&ctx, "Promise", ctor, JS_PROP_C_W_E)?;
        ctx.promise_ctor = ctor;

        Ok(Some(proto))
    }

    pub const fn class_finalizer(_: &mut RuntimeImpl, _isa: &ObjectPayload, _: usize) {
        // if let ObjectPayload::PROMISE(mut s) = isa {
        //     s.promise_reactions[0].clear();
        //     s.promise_reactions[1].clear();
        // }
    }

    pub fn class_mark(val: JSValue) {
        if let Some(promise) = val.as_object_Promise() {
            let s = promise.payload();

            let re = s.promise_reactions.borrow();
            for i in 0..2 {
                for rd in re[i].iter() {
                    rd.resolving_funcs[0].gc_mark();
                    rd.resolving_funcs[1].gc_mark();
                    rd.handler.gc_mark();
                }
            }

            s.result().gc_mark();
        }
    }

    /// see ECMAScript spec `Promise.resolve()` and `Promise.reject()` for details.
    fn from_resolve_or_reject_value(
        ctx: &ContextImpl,
        value: &JSValue,
        is_reject: bool,
    ) -> Result<Self, i32> {
        let (promise, resolving_funcs) = Self::create(ctx)?;
        resolving_funcs[is_reject as usize].call(
            ctx,
            JSValue::UNDEFINED,
            std::slice::from_ref(value),
        )?;
        Ok(promise)
    }

    /// Create an Promise with resolving a value.
    /// see ECMAScript spec `Promise.resolve()` for details.
    #[inline(always)]
    pub fn from_resolve_value(ctx: &mut ContextImpl, value: &JSValue) -> Result<Self, i32> {
        Self::from_resolve_or_reject_value(ctx, value, false)
    }

    /// Create an Promise with rejecting a value.
    /// see ECMAScript spec `Promise.reject()` for details.
    #[inline(always)]
    pub fn from_reject_value(ctx: &mut ContextImpl, value: &JSValue) -> Result<Self, i32> {
        Self::from_resolve_or_reject_value(ctx, value, true)
    }
}

pub const PROMISE_MAGIC_all: i32 = 0;
pub const PROMISE_MAGIC_allSettled: i32 = 1;
pub const PROMISE_MAGIC_any: i32 = 2;

pub fn remainingElementsCount_add(
    ctx: &ContextImpl,
    mut resolve_element_env: JSValue,
    addend: i32,
) -> Result<i32, i32> {
    let mut remainingElementsCount = resolve_element_env.get_property_u32(ctx, 0)?.to_i32(ctx)?;

    remainingElementsCount += addend;
    resolve_element_env.set_property_u32(ctx, 0, JSValue::Int(remainingElementsCount))?;

    Ok(if remainingElementsCount == 0 { 1 } else { 0 })
}

pub fn js_promise_all_resolve_element(mut ctx: ContextRef, mut cc: CallContext) -> JSResult {
    let resolve_type = cc.magic() & 3;
    let is_reject = cc.magic() & 4 != 0;

    let alreadyCalled = cc.data(0).to_bool()?;
    let mut values = cc.data(2);
    let resolve = cc.data(3);
    let resolve_element_env = cc.data(4);
    let index = cc.data(1).to_i32(&ctx)?;

    if alreadyCalled {
        return Ok(JSValue::UNDEFINED);
    }

    let _ = cc.replace_data(0, JSValue::Bool(true));

    let obj: JSValue = if resolve_type == PROMISE_MAGIC_allSettled {
        let mut ob = ctx.create_base_object()?;

        ob.define_property_value(
            &ctx,
            Atom::CONST_status,
            ctx.create_string_from_str(if is_reject { "rejected" } else { "fulfilled" })?
                .into(),
            JS_PROP_C_W_E,
        )?;
        ob.define_property_value(
            &ctx,
            if is_reject {
                Atom::CONST_reason
            } else {
                Atom::CONST_value
            },
            cc.arg(0).clone(),
            JS_PROP_C_W_E,
        )?;

        ob.into()
    } else {
        cc.arg(0).clone()
    };

    values.define_property_value_u32(&ctx, index as u32, obj, JS_PROP_C_W_E)?;

    match remainingElementsCount_add(&ctx, resolve_element_env, -1)? {
        1 => {
            let arg = if resolve_type == PROMISE_MAGIC_any {
                js_aggregate_error_constructor(&ctx, values).err()?
            } else {
                values
            };

            resolve.call(&mut ctx, JSValue::UNDEFINED, std::slice::from_ref(&arg))?;
        }
        0 => {}
        _ => {
            unreachable!()
        }
    }

    Ok(JSValue::UNDEFINED)
}
