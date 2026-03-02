// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cell::Cell,
    collections::VecDeque,
    ffi::c_void,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    time::{SystemTime, UNIX_EPOCH},
};

use gc_lite::{GcError, GcHeap, GcLocal, GcNode, GcRef, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_BUFFER, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR,
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_DATAVIEW, JS_CLASS_ERROR, JS_CLASS_FLOAT64_ARRAY,
    JS_CLASS_OBJECT, JS_CLASS_PROMISE, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8C_ARRAY,
    atom::Atom,
    builtins::{
        array::ArrayPayload, error::js_error_proto_funcs, global::js_global_props, promise::Job,
    },
    class::ClassID,
    error::{STD_ERROR_COUNT, STD_ERROR_NAMES},
    eval::EvalFn,
    function::{Callback, NativeFunction, js_function_proto},
    gcref_wrapper,
    module::{JSFreeModuleEnum, ModuleDef, js_free_modules},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_LENGTH, JS_PROP_WRITABLE, Object, ObjectPayload, Shape,
    },
    runtime::{RuntimeImpl, RuntimePtr},
    stack_frame::ExecMode,
    value::JSValue,
};

/// An execution context, holds environment with global object, calling stack, and execution state.
pub struct ContextImpl {
    rt: RuntimePtr,

    /// Initial shape for Array objects
    pub(crate) array_shape: Option<Shape>,

    /// Class prototypes slot.
    /// element could be object or null, default to null.
    /// TODO: use Vec<Option<Object>> instead
    pub(crate) class_protos: Vec<JSValue>,
    /// Standard error prototypes slot.
    pub(crate) error_protos: [JSValue; STD_ERROR_COUNT],

    pub(crate) function_ctor: JSValue,
    pub(crate) array_ctor: JSValue,
    #[cfg(feature = "builtin_regexp")]
    pub(crate) regexp_ctor: JSValue,
    pub(crate) promise_ctor: JSValue,
    pub(crate) iterator_proto: Option<Object>,
    pub(crate) async_iterator_proto: Option<Object>,

    pub(crate) throw_type_error: JSValue,
    pub(crate) array_proto_values: JSValue,

    /// the eval() function
    pub(crate) eval_func: JSValue,

    /// 传统的全局对象（类似ES3的全局环境），存储普通的全局变量（如var声明的变量）
    /// 默认WRITABLE，权限更宽松
    pub(crate) global_obj: JSValue, // TODO: use Object instead

    /// let/const或特殊标记(DEFINE_GLOBAL_LEX_VAR)定义的变量
    /// 强调CONFIGURABLE和约束写权限, 禁止复杂属性行为（如代理式操作）
    pub(crate) global_var_obj: JSValue, // TODO: use Object instead

    /// Random number generator state
    pub(crate) random_state: u64,

    /// Interrupt counter for async operations
    pub(crate) interrupt_counter: Cell<i32>,

    /// List of loaded modules
    pub(crate) loaded_modules: Vec<ModuleDef>,

    /// eval callback
    pub(crate) eval_handle: Option<EvalFn>,

    /// context level queued jobs
    pub(crate) job_list: VecDeque<Job>,
    /// user defined tick handlers
    pub(crate) tick_handlers: Vec<fn(Context) -> bool>,

    /// User data pointer
    pub(crate) user_opaque: *mut std::ffi::c_void,
}

impl GcTrace for ContextImpl {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.class_protos.iter().for_each(|x| x.trace(tr));
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
        self.function_ctor.trace(tr);
        self.array_ctor.trace(tr);
        #[cfg(feature = "builtin_regexp")]
        self.regexp_ctor.trace(tr);
        self.promise_ctor.trace(tr);

        self.throw_type_error.trace(tr);
        self.eval_func.trace(tr);
        self.global_obj.trace(tr);
        self.global_var_obj.trace(tr);
        self.error_protos.iter().for_each(|x| x.trace(tr));
        self.loaded_modules.iter().for_each(|x| x.trace(tr));

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

        self.finalize();
    }
}

impl ContextImpl {
    pub fn new(rt: &mut RuntimeImpl) -> Result<Self, i32> {
        let class_proto = {
            let n = rt.class_registry.len();
            let mut lst = Vec::<JSValue>::new();
            lst.try_reserve(n).map_err(|_| -1)?;
            lst.extend(std::iter::repeat_n(JSValue::NULL, n));
            lst
        };

        let c = Self {
            rt: (&*rt).into(),

            array_shape: None,

            function_ctor: JSValue::NULL,
            array_ctor: JSValue::NULL,
            #[cfg(feature = "builtin_regexp")]
            regexp_ctor: JSValue::NULL,
            promise_ctor: JSValue::NULL,

            class_protos: class_proto,
            error_protos: [const { JSValue::UNDEFINED }; STD_ERROR_COUNT],

            iterator_proto: None,
            async_iterator_proto: None,
            array_proto_values: JSValue::UNDEFINED,
            throw_type_error: JSValue::UNDEFINED,
            eval_func: JSValue::UNDEFINED,

            global_obj: JSValue::UNDEFINED,
            global_var_obj: JSValue::UNDEFINED,

            random_state: 0,
            interrupt_counter: Cell::new(0),
            loaded_modules: Vec::new(),
            eval_handle: None,

            job_list: VecDeque::new(),
            tick_handlers: Vec::new(),

            user_opaque: std::ptr::null_mut(),
        };

        Ok(c)
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    /// get runtime ptr
    #[inline(always)]
    pub(crate) const fn rt_ptr(&self) -> RuntimePtr {
        self.rt
    }

    /// get runtime ref
    #[inline(always)]
    pub fn rt_ref(&self) -> &RuntimeImpl {
        &self.rt
    }

    /// get runtime ref
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
    pub fn global_obj(&self) -> Object {
        unsafe { *self.global_obj.as_object_unchecked() }
    }

    /// get global var object
    #[inline(always)]
    pub fn global_var_obj(&self) -> Object {
        unsafe { *self.global_var_obj.as_object_unchecked() }
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

    /// test if context is currently in strict mode
    pub(crate) fn is_strict_mode(&self) -> bool {
        self.rt_ptr()
            .current_stack_frame()
            .is_some_and(|sf| sf.mode.contains(ExecMode::STRICT))
    }

    /// Initialize minimum amount of objects to be able to compile code and display error messages.
    /// No Atom should be allocated by this function.
    fn init_core_objects(&mut self) -> Result<(), i32> {
        debug_assert!(
            [
                JS_CLASS_OBJECT,
                JS_CLASS_BYTECODE_FUNCTION,
                JS_CLASS_ARRAY,
                JS_CLASS_ERROR,
            ]
            .iter()
            .max()
            .copied()
            .unwrap()
                < self.rt_ptr().class_registry.len() as ClassID
        );

        // Object
        let mut object = self.create_base_object_with_proto(None)?;
        object.set_flag_immutable_prototype(true);
        self.class_protos[JS_CLASS_OBJECT as usize] = object.into();

        // Function
        {
            let func = NativeFunction::create(
                self,
                Callback::Func(js_function_proto),
                "",
                0,
                0,
                Some(object),
                None,
            )?;
            self.class_protos[JS_CLASS_BYTECODE_FUNCTION as usize] = func.into();
        }

        // Error
        {
            let mut error = self.create_base_object()?;
            error.install_fields(self, js_error_proto_funcs).unwrap();
            self.class_protos[JS_CLASS_ERROR as usize] = error.into();

            // standard error objects
            let msg = Atom::CONST_empty_string.force_string(self).unwrap();
            for (i, name) in STD_ERROR_NAMES.iter().enumerate() {
                let mut err = self.create_base_object_with_proto(Some(error))?;

                let name = self.create_atom_string(name).unwrap();
                err.define_property_value(
                    self,
                    Atom::CONST_name,
                    JSValue::from(name),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

                err.define_property_value(
                    self,
                    Atom::CONST_message,
                    JSValue::from(&msg),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

                self.error_protos[i] = err.into();
            }
        }

        // Array
        {
            // 1. make Array prototype object
            let array_proto = self
                .create_object_with_proto(
                    object.into(),
                    ObjectPayload::ARRAY(ArrayPayload::default()),
                )
                .map_err(|_| -1)?;

            self.class_protos[JS_CLASS_ARRAY as usize] = array_proto.into();

            // 2. make default shape for Array
            let mut array_shape = Shape::create(self, Some(array_proto), 1).unwrap();
            // define `length` property at #0
            array_shape.add_shape_properties(
                self,
                &[(Atom::CONST_length, JS_PROP_WRITABLE | JS_PROP_LENGTH)],
            );

            // register shape
            self.rt_ptr().register_shape(&array_shape).unwrap();
            self.array_shape.replace(array_shape);
        }

        Ok(())
    }

    fn finalize(&mut self) {
        self.job_list.clear();

        // free loaded modules
        js_free_modules(self, JSFreeModuleEnum::JS_FREE_MODULE_ALL);
        #[cfg(debug_assertions)]
        debug_assert!(self.loaded_modules.is_empty());

        self.array_shape.take();
        self.function_ctor = JSValue::UNDEFINED;
        self.array_ctor = JSValue::UNDEFINED;
        #[cfg(feature = "builtin_regexp")]
        {
            self.regexp_ctor = JSValue::UNDEFINED;
        }
        self.promise_ctor = JSValue::UNDEFINED;

        self.class_protos.clear();
        self.error_protos = [const { JSValue::UNDEFINED }; STD_ERROR_COUNT];

        self.iterator_proto.take();
        self.async_iterator_proto.take();
        self.array_proto_values = JSValue::UNDEFINED;
        self.throw_type_error = JSValue::UNDEFINED;
        self.global_obj = JSValue::UNDEFINED;
        self.global_var_obj = JSValue::UNDEFINED;
        self.eval_func = JSValue::UNDEFINED;
    }
}

/// Handle to context with lifetime
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct Context<'a> {
    gc_ref: GcRef<ContextImpl>,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Deref for Context<'a> {
    type Target = ContextImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.gc_ref.deref()
    }
}

impl<'a> DerefMut for Context<'a> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.gc_ref.deref_mut()
    }
}

impl<'a> From<&ContextImpl> for Context<'a> {
    #[inline]
    fn from(c: &ContextImpl) -> Self {
        Self {
            gc_ref: unsafe { gc_lite::GcRef::from_ref_unchecked(c) },
            _mark: PhantomData,
        }
    }
}

impl<'a> Context<'a> {
    pub const ECMA_CONTEXT_SETUP: fn(&mut ContextScope) -> Result<(), i32> =
        |ctx: &mut ContextScope| -> Result<(), i32> {
            // define name `globalThis` and `global`
            let mut gobj = ctx.global_obj();
            gobj.define_property_value(
                ctx,
                Atom::CONST_globalThis,
                gobj.into(),
                JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
            )?;
            gobj.define_property_value(
                ctx,
                Atom::CONST_global,
                gobj.into(),
                JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
            )?;

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

            ctx.intrinsic_base_generator(); // Generator

            ctx.intrinsic_eval(); // `eval()` function

            //
            // init user classes
            //
            let mut use_classes = vec![
                // async
                JS_CLASS_PROMISE,
                JS_CLASS_ASYNC_FUNCTION,
                JS_CLASS_ASYNC_GENERATOR,
                JS_CLASS_ASYNC_GENERATOR_FUNCTION,
                JS_CLASS_ASYNC_FROM_SYNC_ITERATOR,
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

            // setup global
            {
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

impl From<Context<'_>> for ContextPtr {
    #[inline(always)]
    fn from(h: Context<'_>) -> Self {
        Self(h.gc_ref)
    }
}

impl<'a> From<ContextPtr> for Context<'a> {
    #[inline(always)]
    fn from(p: ContextPtr) -> Self {
        Self {
            gc_ref: p.gc_ref(),
            _mark: PhantomData,
        }
    }
}

impl std::fmt::Debug for ContextPtr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<ContextPtr {:p}>", self.id() as *const c_void)
    }
}

impl RuntimeImpl {
    /// Create context with setup.
    pub fn create_context<'a>(
        &mut self,
        context_setup: impl FnOnce(&mut ContextScope) -> Result<(), i32>,
    ) -> Result<Context<'a>, i32> {
        let inner = ContextImpl::new(self)?;

        let pid = self.gc_part_id();
        let ctx = self
            .gc_heap_mut()
            .with_new_scope(pid, |gc| -> Result<Context<'a>, i32> {
                let gc_ref = gc.alloc(inner).map_err(|_| -1)?;

                // setup context.
                // Note: in case of exception, ContextImpl node will be purged in mark-sweep.
                {
                    let mut ctx = ContextPtr(gc_ref);
                    // make global namespace
                    ctx.global_obj = ctx.create_base_object()?.into();
                    // make global var object
                    ctx.global_var_obj = ctx.create_base_object_with_proto(None)?.into();

                    // don't change order
                    let mut sc = ctx.create_scope();
                    sc.init_core_objects()?;
                    context_setup(&mut sc)?;

                    // ctx.init_core_objects()?;
                    // context_setup(&mut ctx)?;
                }

                Ok(Context {
                    gc_ref,
                    _mark: PhantomData,
                })
            })
            .map_err(|_| -1)?;

        self.context_list.push(ctx.into());
        Ok(ctx)
    }
}

pub struct ContextScope<'a> {
    pub(crate) ctx: GcRef<ContextImpl>,

    #[cfg(debug_assertions)]
    level: u32,

    _mark: PhantomData<&'a ()>,
}

impl<'a> Drop for ContextScope<'a> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        debug_assert_eq!(self.level, self.ctx.gc_heap().scope_level() as u32);

        self.ctx.gc_heap_mut().pop_gc_scope();
    }
}

impl<'a> Deref for ContextScope<'a> {
    type Target = ContextImpl;

    fn deref(&self) -> &Self::Target {
        self.ctx.deref()
    }
}

impl<'a> DerefMut for ContextScope<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ctx.deref_mut()
    }
}

impl<'a> ContextScope<'a> {
    /// Allocate a node, and put it to scope cache to protect it.
    pub fn alloc<T: GcNode>(&mut self, payload: T) -> Result<GcRef<T>, (GcError, T)> {
        self.gc_heap_mut()
            .with_current_scope(|gc| gc.alloc(payload))
            .unwrap()
    }

    pub fn alloc_root<T: GcNode>(&mut self, payload: T) -> Result<GcRef<T>, (GcError, T)> {
        self.gc_heap_mut()
            .with_current_scope(|gc| gc.alloc_root(payload))
            .unwrap()
    }

    #[deprecated]
    pub fn alloc_local<T: GcNode>(&mut self, payload: T) -> Result<GcLocal<T>, (GcError, T)> {
        self.gc_heap_mut()
            .with_current_scope(|gc| gc.alloc_local(payload))
            .unwrap()
    }
}

impl ContextImpl {
    /// create a context scope
    pub fn create_scope(&mut self) -> ContextScope<'_> {
        let pid = self.rt_ref().gc_part_id();
        self.gc_heap_mut().push_gc_scope(pid);

        ContextScope {
            ctx: self.gc_ref(),

            #[cfg(debug_assertions)]
            level: self.gc_heap().scope_level() as u32,

            _mark: PhantomData,
        }
    }
}
