// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cell::Cell,
    collections::VecDeque,
    ffi::c_void,
    time::{SystemTime, UNIX_EPOCH},
};

use gc_lite::{GcHeap, GcNode, GcRef, GcScope, GcScopeStackId, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, JS_CLASS_ASYNC_FUNCTION,
    JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_ASYNC_ITERABLE_ITERATOR,
    JS_CLASS_DATAVIEW, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION,
    JS_CLASS_PROMISE, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8C_ARRAY, Job,
    atom::Atom,
    builtins::global::js_global_props,
    class::ClassID,
    context::{ScopedContext, TopLevelContext},
    error::STD_ERROR_COUNT,
    gcref_wrapper,
    module::{JSFreeModuleEnum, ModuleDef, js_free_modules},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object, Shape},
    runtime::{RuntimeImpl, RuntimePtr},
    value::JSValue,
};

/// Class prototype and constructor object
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ProtoCtorPair {
    pub(crate) proto: Option<Object>,
    pub(crate) ctor: Option<Object>,
}

/// An execution context, holds environment with global object, calling stack, and execution state.
pub struct ContextImpl {
    rt: RuntimePtr,

    /// Class prototypes+constructors slot.
    pub(crate) class_slots: Vec<ProtoCtorPair>,
    /// Standard error prototypes slot.
    pub(crate) error_protos: [JSValue; STD_ERROR_COUNT],

    pub(crate) iterator_proto: Option<Object>,
    pub(crate) async_iterator_proto: Option<Object>,

    /// Initial shape for Array objects
    pub(crate) array_shape: Option<Shape>,

    pub(crate) throw_type_error: JSValue,
    pub(crate) array_proto_values: JSValue,

    /// the eval() function
    pub(crate) eval_func: JSValue,

    /// 传统的全局对象（类似ES3的全局环境），存储普通的全局变量（如var声明的变量）
    /// 默认WRITABLE，权限更宽松
    global_obj: Option<Object>,
    /// let/const或特殊标记(DEFINE_GLOBAL_LEX_VAR)定义的变量
    /// 强调CONFIGURABLE和约束写权限, 禁止复杂属性行为（如代理式操作）
    global_var_obj: Option<Object>,

    /// Random number generator state
    pub(crate) random_state: u64,

    /// Interrupt counter for async operations
    pub(crate) interrupt_counter: Cell<i32>,

    /// List of loaded modules
    pub(crate) loaded_modules: Vec<ModuleDef>,

    pub(crate) allow_eval: bool,

    /// context level queued jobs
    pub(crate) job_list: VecDeque<Job>,
    /// user defined tick handlers
    pub(crate) tick_handlers: Vec<fn(&mut ScopedContext) -> bool>,

    /// The top-level GC scope for this context.
    /// Its stack_id identifies which scope stack this context uses.
    /// Dropped in ContextImpl::drop before releasing the scope stack.
    pub(crate) gc_top_scope: Option<gc_lite::GcScope<'static>>,
}

impl GcTrace for ContextImpl {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.class_slots.iter().for_each(|x| {
            if let Some(o) = x.proto.as_ref() {
                o.trace(tr);
            }
            if let Some(o) = x.ctor.as_ref() {
                o.trace(tr);
            }
        });
        self.error_protos.iter().for_each(|x| x.trace(tr));

        if let Some(x) = self.array_shape.as_ref() {
            x.trace(tr)
        }
        if let Some(x) = self.iterator_proto {
            x.trace(tr)
        }
        if let Some(x) = self.async_iterator_proto {
            x.trace(tr)
        }
        self.array_proto_values.trace(tr);

        self.throw_type_error.trace(tr);
        self.eval_func.trace(tr);
        self.loaded_modules.iter().for_each(|x| x.trace(tr));

        if let Some(o) = self.global_obj.as_ref() {
            o.trace(tr)
        };
        if let Some(o) = self.global_var_obj.as_ref() {
            o.trace(tr)
        };

        // pending jobs
        self.job_list.iter().for_each(|x| x.trace(tr));
    }
}

impl Drop for ContextImpl {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        unsafe {
            let r = GcRef::<ContextImpl>::from_ref_unchecked(self);
            log::trace!("[ContextImpl::drop] {:?}", r.node_info());
        }

        // Drop the top-level scope first (pops it from the scope stack),
        // then release the scope stack.
        // All child scopes should have been destroyed by RAII (GcScope lives
        // on the stack, shorter than ContextImpl which lives in the GC heap).
        if let Some(scope) = self.gc_top_scope.take() {
            let stack_id = scope.stack_id();
            drop(scope);
            self.rt_mut().gc_heap_mut().release_scope_stack(stack_id);
        }

        self.finalize();
    }
}

impl ContextImpl {
    pub fn new(rt: &mut RuntimeImpl) -> Result<Self, i32> {
        let class_proto = {
            let n = rt.class_registry.len();
            let mut lst = Vec::<ProtoCtorPair>::new();
            lst.try_reserve(n).map_err(|_| -1)?;
            lst.extend(std::iter::repeat_n(ProtoCtorPair::default(), n));
            lst
        };

        let c = Self {
            rt: (&*rt).into(),

            array_shape: None,

            class_slots: class_proto,
            error_protos: [const { JSValue::UNDEFINED }; STD_ERROR_COUNT],

            iterator_proto: None,
            async_iterator_proto: None,
            array_proto_values: JSValue::UNDEFINED,
            throw_type_error: JSValue::UNDEFINED,
            eval_func: JSValue::UNDEFINED,

            global_obj: None,
            global_var_obj: None,

            random_state: 0,
            interrupt_counter: Cell::new(0),
            loaded_modules: Vec::new(),

            allow_eval: true,

            job_list: VecDeque::new(),
            tick_handlers: Vec::new(),

            gc_top_scope: None,
        };

        Ok(c)
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    /// Get the scope stack ID for this context.
    #[inline(always)]
    pub fn scope_stack_id(&self) -> GcScopeStackId {
        self.gc_top_scope
            .as_ref()
            .expect("top_level_scope must be set")
            .stack_id()
    }

    /// Set class prototype with GC write barrier.
    /// `ContextImpl` is a GC node, and `proto` is a GC reference (Object).
    /// Without the barrier, if `self` is BLACK and `proto` is WHITE,
    /// the proto could be prematurely collected.
    pub(crate) fn set_class_proto(&mut self, class_id: ClassID, proto: Object) {
        let self_node = self.gc_head_ptr();
        self.rt.gc_heap_mut().bind(self_node, proto.gc_node_ptr());

        self.class_slots[class_id as usize].proto = Some(proto);
    }

    /// get runtime ptr
    #[inline(always)]
    pub(crate) const fn rt_ptr(&self) -> RuntimePtr {
        self.rt
    }

    /// get runtime ref
    #[inline(always)]
    pub fn rt(&self) -> &RuntimeImpl {
        &self.rt
    }

    #[inline(always)]
    #[deprecated(note = "An alias of ::rt()")]
    pub fn rt_ref(&self) -> &RuntimeImpl {
        &self.rt
    }

    /// get runtime mut
    #[inline(always)]
    pub fn rt_mut(&mut self) -> &mut RuntimeImpl {
        &mut self.rt
    }

    #[inline(always)]
    pub fn gc_heap(&self) -> &GcHeap {
        self.rt.gc_heap()
    }

    #[inline(always)]
    pub fn gc_heap_mut(&mut self) -> &mut GcHeap {
        self.rt.gc_heap_mut()
    }

    /// get global object
    #[inline(always)]
    pub const fn global_obj(&self) -> Object {
        #[cfg(debug_assertions)]
        {
            self.global_obj.unwrap()
        }
        #[cfg(not(debug_assertions))]
        unsafe {
            self.global_obj.unwrap_unchecked()
        }
    }

    /// get global var object
    #[inline(always)]
    pub const fn global_var_obj(&self) -> Object {
        #[cfg(debug_assertions)]
        {
            self.global_var_obj.unwrap()
        }
        #[cfg(not(debug_assertions))]
        unsafe {
            self.global_var_obj.unwrap_unchecked()
        }
    }

    pub fn class_proto(&self, class_id: ClassID) -> Option<Object> {
        self.class_slots
            .get(class_id as usize)
            .and_then(|x| x.proto)
    }

    pub fn class_constructor(&self, class_id: ClassID) -> Option<Object> {
        self.class_slots.get(class_id as usize).and_then(|x| x.ctor)
    }

    #[cfg(feature = "builtin_regexp")]
    #[inline]
    pub fn regexp_ctor(&self) -> Object {
        debug_assert!(self.class_slots.len() > crate::JS_CLASS_REGEXP as usize);
        self.class_constructor(crate::JS_CLASS_REGEXP).unwrap()
    }

    #[inline]
    pub(crate) fn promise_ctor(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_PROMISE as usize);
        self.class_constructor(JS_CLASS_PROMISE).unwrap()
    }

    /// initialize random state
    pub(crate) fn init_random_seed(&mut self) {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        // seed must not be zero.
        self.random_state = if seed != 0 { seed } else { 1 };
    }

    fn finalize(&mut self) {
        self.job_list.clear();

        // free loaded modules
        js_free_modules(self, JSFreeModuleEnum::JS_FREE_MODULE_ALL);
        #[cfg(debug_assertions)]
        debug_assert!(self.loaded_modules.is_empty());

        self.class_slots.clear();

        self.error_protos = [const { JSValue::UNDEFINED }; STD_ERROR_COUNT];
        self.array_proto_values = JSValue::UNDEFINED;
        self.throw_type_error = JSValue::UNDEFINED;
        self.eval_func = JSValue::UNDEFINED;

        self.array_shape.take();
        self.iterator_proto.take();
        self.async_iterator_proto.take();
        self.global_obj.take();
        self.global_var_obj.take();
    }
}

impl ContextImpl {
    /// Standard ECMAScript context setup function.
    /// Can be passed to `RuntimeImpl::create_context`.
    pub const ECMA_CONTEXT_SETUP: fn(&mut ScopedContext) -> Result<(), i32> =
        |ctx: &mut ScopedContext| -> Result<(), i32> {
            // setup global namespace
            {
                let mut gobj = ctx.global_obj();

                // define name `globalThis` and `global`
                let val = JSValue::from(gobj);
                gobj.define_property_value(
                    ctx,
                    Atom::CONST_globalThis,
                    val,
                    JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
                )?;
                gobj.define_property_value(
                    ctx,
                    Atom::CONST_global,
                    val,
                    JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
                )?;

                // global funcs
                gobj.install_fields(ctx, js_global_props)?;

                // Reflect: lazy init
                #[cfg(feature = "builtin_reflect")]
                gobj.install_fields(ctx, crate::builtins::reflect::js_reflect_obj)?;

                // JSON: lazy init
                #[cfg(feature = "builtin_json")]
                gobj.install_fields(ctx, crate::builtins::json::js_json_obj)?;

                // Math: lazy init
                #[cfg(feature = "builtin_math")]
                {
                    ctx.init_random_seed();
                    gobj.install_fields(ctx, crate::builtins::math::js_math_obj)?;
                }
            }

            //
            // init intrinsic base classes
            //

            ctx.intrinsic_base_object(); // Object
            ctx.intrinsic_base_function(); // Function
            ctx.intrinsic_base_errors(); // Error
            ctx.intrinsic_base_array(); // Array
            ctx.intrinsic_base_number(); // Number
            ctx.intrinsic_base_boolean(); // Boolean
            ctx.intrinsic_base_string(); // String

            // Symbol
            #[cfg(feature = "builtin_symbol")]
            ctx.intrinsic_base_symbol();

            ctx.intrinsic_eval(); // `eval()` function

            //
            // init using classes
            //
            let mut use_classes = vec![
                // generator
                JS_CLASS_GENERATOR,
                JS_CLASS_GENERATOR_FUNCTION,
                // async
                JS_CLASS_PROMISE,
                JS_CLASS_ASYNC_FUNCTION,
                JS_CLASS_ASYNC_GENERATOR, // indicates the object returned by calling an async function* function
                JS_CLASS_ASYNC_GENERATOR_FUNCTION, // indicates the async function* function itself
                JS_CLASS_ASYNC_FROM_SYNC_ITERATOR,
                JS_CLASS_ASYNC_ITERABLE_ITERATOR,
                // array buffer & dataview
                JS_CLASS_ARRAY_BUFFER,
                JS_CLASS_SHARED_ARRAY_BUFFER,
                JS_CLASS_DATAVIEW,
            ];
            // TypedArray
            use_classes.extend(JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY);
            // Proxy
            #[cfg(feature = "builtin_proxy")]
            use_classes.push(crate::JS_CLASS_PROXY);
            // RegExp
            #[cfg(feature = "builtin_regexp")]
            use_classes.extend_from_slice(&[
                crate::JS_CLASS_REGEXP,
                crate::JS_CLASS_REGEXP_STRING_ITERATOR,
            ]);
            // Map/Set
            #[cfg(feature = "builtin_mapset")]
            use_classes.extend_from_slice(&[
                crate::JS_CLASS_MAP,
                crate::JS_CLASS_SET,
                crate::JS_CLASS_WEAKMAP,
                crate::JS_CLASS_WEAKSET,
                crate::JS_CLASS_MAP_ITERATOR,
                crate::JS_CLASS_SET_ITERATOR,
            ]);
            // Date
            #[cfg(feature = "builtin_date")]
            use_classes.push(crate::JS_CLASS_DATE);

            ctx.use_classes(&use_classes)?;

            Ok(())
        };
}

/// A light-weight context pointer, efficent to copy, like a raw pointer.
/// It can be dereferenced to ref or mut ref automatically.
/// Safety: you are responsible to take more care to use it safe, keep in mind pointer could be dangling.
///
/// This is suggested to use internally only,
/// for public api, use `Context` instead, which is same but has lifetime restriction.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub(crate) struct ContextPtr(GcRef<ContextImpl>);

gcref_wrapper!(ContextPtr, ContextImpl);

impl PartialEq for ContextPtr {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl std::fmt::Debug for ContextPtr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<ContextPtr {:p}>", self.id() as *const c_void)
    }
}

impl RuntimeImpl {
    /// Create a context with a top-level GC scope.
    ///
    /// # Safety
    ///
    /// The returned `TopLevelContext` references a GC scope stored in
    /// `RuntimeImpl`. The caller must ensure the `RuntimeImpl` outlives the
    /// `TopLevelContext`.
    pub fn create_context<'a>(
        &mut self,
        setup: impl FnOnce(&mut ScopedContext) -> Result<(), i32>,
    ) -> Result<TopLevelContext<'a>, i32> {
        let inner = ContextImpl::new(self)?;

        let ctx_ptr = {
            let par = self.gc_part_id();
            let heap = self.gc_heap_mut();

            let stack_id = heap.acquire_scope_stack(par);
            let gc_top_scope = heap.new_scope(stack_id);

            let mut ctx = ContextPtr::from(gc_top_scope.alloc_local(inner).map_err(|_| -1)?);
            ctx.gc_top_scope = Some(unsafe {
                // Store the top-level scope in ContextImpl so it lives as long
                // as the context. It will be dropped in ContextImpl::drop.
                // SAFETY: GcScope lives in ContextImpl which is a GC node on the
                // heap, while GcScope borrows GcHeap which outlives all GC nodes.
                // The lifetime transmute is safe because GcScope only uses the
                // lifetime for borrow checking; internally it uses NonNull.
                std::mem::transmute::<GcScope<'_>, GcScope<'static>>(gc_top_scope)
            });

            ctx
        };

        // Register to context list.
        self.context_list.push(ctx_ptr);

        let mut top = TopLevelContext {
            ctx_ptr,
            _marker: std::marker::PhantomData,
        };

        // Initialize in a sub-scope. Allocations are protected by LOCAL in the
        // sub-scope during setup. When the sub-scope drops, LOCAL is removed,
        // but the objects remain reachable through ContextImpl fields
        // (global_obj, global_var_obj, etc.) which are roots via context_list.
        top.with_new_scope(|mut cx| {
            // DON'T CHANGE FOLLOWING ORDER

            // base object must be initialized before creating global
            let r1 = cx.init_core_objects();
            if r1.is_err() {
                return cx.return_value(r1);
            }
            // make global namespace
            let r2 = cx.create_base_object();
            if let Err(e) = r2 {
                return cx.return_value(Err(e));
            }
            cx.global_obj = Some(r2.unwrap());
            // make global var object
            let r3 = cx.create_base_object_with_proto(None);
            if let Err(e) = r3 {
                return cx.return_value(Err(e));
            }
            cx.global_var_obj = Some(r3.unwrap());
            // user initialization
            let result = setup(&mut cx);
            cx.return_value(result)
        })?;

        Ok(top)
    }
}
