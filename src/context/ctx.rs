use std::{
    cell::Cell,
    collections::VecDeque,
    ffi::c_void,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    time::{SystemTime, UNIX_EPOCH},
};

use gc_lite::{GcRef, GcTracable, GcTraceOp, GcWeak};

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_ERROR, JS_CLASS_OBJECT,
    atom::Atom,
    builtins::{array::ArrayPayload, error::js_error_proto_funcs, promise::Job},
    class::ClassID,
    error::{STD_ERROR_COUNT, STD_ERROR_NAMES},
    eval::EvalFn,
    function::{Callback, js_function_proto},
    gcref_wrapper,
    module::{JSFreeModuleEnum, ModuleDef, js_free_modules},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_LENGTH, JS_PROP_WRITABLE, Object, ObjectPayload, ShapeHandle,
    },
    runtime::{RuntimeImpl, RuntimePtr},
    stack_frame::ExecMode,
    value::JSValue,
};

/// An execution context, holds environment with global object, calling stack, and execution state.
pub struct ContextImpl {
    rt: RuntimePtr,

    /// Initial shape for Array objects
    pub(crate) array_shape: Option<ShapeHandle>,

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

unsafe impl GcTracable for ContextImpl {
    fn trace(&self, tr: GcTraceOp) {
        self.class_protos.iter().for_each(|x| x.trace(tr));
        self.array_shape.trace(tr);
        self.iterator_proto.trace(tr);
        self.async_iterator_proto.trace(tr);
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
        self.job_list.iter().for_each(|x| x.gc_trace(tr));
    }
}

impl Drop for ContextImpl {
    fn drop(&mut self) {
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

    /// get global object
    #[inline]
    pub fn global_obj(&self) -> Object {
        unsafe { *self.global_obj.as_object_unchecked() }
    }

    /// get global var object
    #[inline]
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

impl ContextPtr {
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
            let func = self.create_native_function_with_proto(
                Callback::Func(js_function_proto),
                "",
                0,
                0,
                object.into(),
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
                    JSValue::String(name),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

                err.define_property_value(
                    self,
                    Atom::CONST_message,
                    JSValue::String(msg),
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
            let mut array_shape = ShapeHandle::new(self, Some(array_proto), 1).unwrap();
            // define `length` property at #0
            array_shape
                .add_shape_properties(&[(Atom::CONST_length, JS_PROP_WRITABLE | JS_PROP_LENGTH)]);

            // register shape
            self.rt_ptr().register_shape(&array_shape).unwrap();
            self.array_shape.replace(array_shape);
        }

        Ok(())
    }

    #[inline(always)]
    pub fn downgrade(&self) -> GcWeak<ContextImpl> {
        self.0.downgrade(self.rt_ptr().gc_heap_mut())
    }
}

impl RuntimeImpl {
    /// Creates context with setup closure.
    pub(super) fn create_context_raw<'a>(
        &mut self,
        setup: impl FnOnce(ContextPtr) -> Result<(), i32>,
    ) -> Result<Context<'a>, i32> {
        let inner = ContextImpl::new(self)?;
        let gc_ref = self.gc_alloc(inner).map_err(|_| -1)?;
        self.gc_heap_mut().set_root(gc_ref, true);

        let mut ctx = ContextPtr(gc_ref);
        ctx.init_core_objects()?;
        setup(ctx)?;

        // register context
        self.context_list.push(ctx);

        Ok(Context {
            gc_ref,
            _mark: PhantomData,
        })
    }
}
