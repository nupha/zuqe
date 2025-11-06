use std::{
    cell::RefCell,
    collections::VecDeque,
    ffi::c_void,
    ops::DerefMut,
    pin::Pin,
    rc::{Rc, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_ERROR, JS_CLASS_OBJECT,
    array::ArrayObjPayload,
    atom::{
        Atom,
        consts::{JS_ATOM_empty_string, JS_ATOM_length, JS_ATOM_message, JS_ATOM_name},
    },
    class::ClassID,
    error::{STD_ERROR_COUNT, STD_ERROR_NAMES, js_error_proto_funcs},
    eval::EvalFn,
    function::{NativeFunctionType, js_function_proto},
    module::{JSFreeModuleEnum, ModuleDef, js_free_modules},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_LENGTH, JS_PROP_WRITABLE, JSObject, JSShape, ObjectPayload,
    },
    promise::Job,
    regexp::RegExpData,
    runtime::{JSRuntime, RuntimePtr},
    stack_frame::StackFrame,
    value::JSValue,
};

/// Execution context
///
/// Represents a JavaScript execution environment with its own global object,
/// stack, and execution state. Each context is associated with a runtime.
pub struct JSContextInner<'a> {
    /// back ref to runtime
    pub rt: RuntimePtr<'a>,

    pub flags: u8,

    /// Count of binary objects in this context
    pub binary_object_count: u16,

    /// Total size of binary objects in this context
    pub binary_object_size: i32,

    /// Initial shape for Array objects
    pub array_shape: Option<JSShape<'a>>,

    /// Class prototypes slot.
    /// element could be object or null, default to null.
    /// TODO: use Vec<Option<JSObject>> instead
    pub class_proto: Vec<JSValue<'a>>,

    pub(crate) iterator_proto: Option<JSObject<'a>>,
    pub(crate) async_iterator_proto: Option<JSObject<'a>>,
    pub(crate) native_error_proto: [JSValue<'a>; STD_ERROR_COUNT],

    pub function_ctor: JSValue<'a>,
    pub array_ctor: JSValue<'a>,
    pub regexp_ctor: JSValue<'a>,
    pub promise_ctor: JSValue<'a>,
    pub throw_type_error: JSValue<'a>,
    pub array_proto_values: JSValue<'a>,

    /// the function of eval().
    pub eval_func: JSValue<'a>,

    /// 传统的全局对象（类似ES3的全局环境），存储普通的全局变量（如var声明的变量）
    /// 默认WRITABLE，权限更宽松
    pub global_obj: JSValue<'a>, // TODO: use JSObject instead

    /// let/const或特殊标记(DEFINE_GLOBAL_LEX_VAR)定义的变量
    /// 强调CONFIGURABLE和约束写权限, 禁止复杂属性行为（如代理式操作）
    pub global_var_obj: JSValue<'a>, // TODO: use JSObject instead

    /// Random number generator state
    pub random_state: u64,

    /// Interrupt counter for async operations
    pub interrupt_counter: i32,

    /// List of loaded modules
    pub loaded_modules: Vec<ModuleDef<'a>>,

    /// Regexp compilation callback
    #[deprecated(note = "unused")]
    pub compile_regexp: Option<
        fn(ctx: &JSContext, pattern: JSValue, flags: Option<&str>) -> Result<RegExpData, i32>,
    >,

    /// eval callback
    pub eval_handle: Option<EvalFn>,

    /// User data pointer
    pub user_opaque: *mut std::ffi::c_void,

    /// Stack information
    pub stack_limit: usize,
    pub stack_pointer: usize,
    pub stack_size: usize,
    pub stack_frame: *mut Pin<StackFrame<'a>>,

    /// queued context jobs
    pub(crate) job_list: VecDeque<Job<'a>>,
}

impl Drop for JSContextInner<'_> {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        println!("JSContextInner::drop({self:p})");

        self.finalize();
    }
}

impl<'a> JSContextInner<'a> {
    pub fn new(rt: &mut JSRuntime<'a>) -> Result<Self, i32> {
        let class_proto = {
            let n = rt.class_registry.len();
            let mut lst = Vec::<JSValue>::new();
            lst.try_reserve(n).map_err(|_| -1)?;
            lst.extend(std::iter::repeat_n(JSValue::NULL, n));
            lst
        };

        let c = Self {
            rt: (&*rt).into(),
            flags: 0,

            binary_object_count: 0,
            binary_object_size: 0,

            array_shape: None,

            function_ctor: JSValue::NULL,
            array_ctor: JSValue::NULL,
            regexp_ctor: JSValue::NULL,
            promise_ctor: JSValue::NULL,

            class_proto,

            iterator_proto: None,
            async_iterator_proto: None,
            native_error_proto: [const { JSValue::UNDEFINED }; STD_ERROR_COUNT],
            array_proto_values: JSValue::UNDEFINED,
            throw_type_error: JSValue::UNDEFINED,
            eval_func: JSValue::UNDEFINED,

            global_obj: JSValue::UNDEFINED,
            global_var_obj: JSValue::UNDEFINED,

            random_state: 0,
            interrupt_counter: 0,
            loaded_modules: Vec::new(),
            compile_regexp: None,
            eval_handle: None,

            stack_limit: 0,
            stack_pointer: 0,
            stack_size: 0,
            stack_frame: std::ptr::null_mut(),

            job_list: VecDeque::new(),
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
    pub const fn rt(&self) -> RuntimePtr<'a> {
        self.rt
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

    // fn gc_dump(&self, _rt: &JSRuntime) {
    //     println!("{:14p} [context]", self);
    // }

    pub(crate) fn finalize(&mut self) {
        // free loaded modules
        js_free_modules(self, JSFreeModuleEnum::JS_FREE_MODULE_ALL);
        #[cfg(debug_assertions)]
        {
            // self.loaded_modules.retain(|w| w.upgrade().is_some());
            debug_assert!(self.loaded_modules.is_empty());
        }

        self.array_shape.take();
        self.function_ctor = JSValue::UNDEFINED;
        self.array_ctor = JSValue::UNDEFINED;
        self.regexp_ctor = JSValue::UNDEFINED;
        self.promise_ctor = JSValue::UNDEFINED;
        self.native_error_proto = [const { JSValue::UNDEFINED }; STD_ERROR_COUNT];
        self.iterator_proto.take();
        self.async_iterator_proto.take();
        self.array_proto_values = JSValue::UNDEFINED;
        self.throw_type_error = JSValue::UNDEFINED;
        self.global_obj = JSValue::UNDEFINED;
        self.global_var_obj = JSValue::UNDEFINED;
        self.eval_func = JSValue::UNDEFINED;

        self.class_proto.clear();

        #[cfg(feature = "dump_atoms")]
        JS_DumpAtoms(rt);

        #[cfg(feature = "dump_shapes")]
        crate::shape::JS_DumpShapes(ctx.rt);

        #[cfg(feature = "dump_objects")]
        self.rt().gc_dump_objects(1);

        println!("[O.o][TODO] do gc when context dropped");
        // let rt = self.rt_mut();
        // if rt.gc_phase != JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES {
        //     if rt.gc_phase == JSGCPhaseEnum::JS_GC_PHASE_NONE {
        //         rt.gc_free_zeroed();
        //     }
        // }

        #[cfg(feature = "DUMP_MEM")]
        {
            let mut stats: JSMemoryUsage;
            JS_ComputeMemoryUsage(rt, &mut stats);
            JS_DumpMemoryUsage(stdout, &stats, rt);
        }
    }
}

#[repr(transparent)]
#[derive(Clone)]
pub struct JSContext<'a>(pub Rc<RefCell<JSContextInner<'a>>>);

impl PartialEq for JSContext<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl std::fmt::Debug for JSContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<JSContext {:p}>", self.id() as *const c_void)
    }
}

impl<'a> JSContext<'a> {
    #[inline]
    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    pub fn with_mut<R>(&self, f: impl FnOnce(&mut JSContextInner) -> R) -> R {
        f(self.0.borrow_mut().deref_mut())
    }

    /// Returns the runtime reference
    #[inline(always)]
    pub fn rt(&self) -> RuntimePtr<'a> {
        self.0.borrow().rt()
    }

    /// get global object
    #[inline]
    pub fn global_obj(&self) -> JSObject<'a> {
        unsafe { self.0.borrow().global_obj.as_object_unchecked().clone() }
    }

    /// get global var object
    #[inline]
    pub fn global_var_obj(&self) -> JSObject<'a> {
        unsafe { self.0.borrow().global_var_obj.as_object_unchecked().clone() }
    }

    /// Pushes a new stack frame onto the execution stack
    ///
    /// # Safety
    /// The caller must ensure the frame pointer is valid
    pub fn push_stack_frame(&mut self, frame: *mut Pin<StackFrame<'a>>) {
        let mut ctx = self.0.borrow_mut();

        if ctx.stack_frame.is_null() {
            ctx.stack_frame = frame;
        } else {
            unsafe {
                (*frame.cast::<StackFrame>()).prev_frame = ctx.stack_frame;
            }
            ctx.stack_frame = frame;
        }
    }

    /// Pops the current stack frame from the execution stack
    ///
    /// # Safety
    /// The caller must ensure no references to the popped frame remain
    pub fn pop_stack_frame(&mut self) -> *mut Pin<StackFrame<'a>> {
        let mut ctx = self.0.borrow_mut();

        if ctx.stack_frame.is_null() {
            std::ptr::null_mut()
        } else {
            let frame = ctx.stack_frame;
            ctx.stack_frame = unsafe { (*frame.cast::<StackFrame>()).prev_frame };
            frame
        }
    }

    pub fn downgrade(&self) -> JSContextWeakref<'a> {
        JSContextWeakref(Rc::downgrade(&self.0))
    }

    pub fn gc_has_mark(&self) -> bool {
        self.0.borrow().flags & 1 != 0
    }

    pub fn gc_set_mark(&self, mark: bool) {
        let mut b = self.0.borrow_mut();
        b.flags = (b.flags & 0b1111_1110) | (mark as u8)
    }

    /// Initialize minimum amount of objects to be able to compile code and display error messages.
    /// No Atom should be allocated by this function.
    fn init_core_objects(&self) -> Result<(), i32> {
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
                < self.rt().class_registry.len() as ClassID
        );

        // Object
        let object = self.create_base_object_with_proto(None)?;
        self.0.borrow_mut().class_proto[JS_CLASS_OBJECT as usize] = object.clone().into();

        // Function
        {
            let func = self.create_native_function_with_proto(
                NativeFunctionType::Func(js_function_proto),
                "",
                0,
                0,
                object.clone().into(),
            )?;
            self.0.borrow_mut().class_proto[JS_CLASS_BYTECODE_FUNCTION as usize] = func.into();
        }

        // Error
        {
            let error = self.create_base_object()?;
            error
                .install_field_defs(self, &js_error_proto_funcs)
                .unwrap();
            self.0.borrow_mut().class_proto[JS_CLASS_ERROR as usize] = error.clone().into();

            // standard error objects
            let msg = JS_ATOM_empty_string.force_string(self).unwrap();
            for (i, name) in STD_ERROR_NAMES.iter().enumerate() {
                let err = self.create_base_object_with_proto(Some(error.clone()))?;

                let name = self.create_atom_string(name).unwrap();
                err.define_property_value(
                    self,
                    &JS_ATOM_name,
                    JSValue::String(name),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

                err.define_property_value(
                    self,
                    &JS_ATOM_message,
                    JSValue::String(msg.clone()),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

                self.0.borrow_mut().native_error_proto[i] = err.into();
            }
        }

        // Array
        {
            // 1. make Array prototype object
            let array_proto = self
                .create_object_with_proto(
                    object.clone().into(),
                    ObjectPayload::ARRAY(ArrayObjPayload::default()),
                )
                .map_err(|_| -1)?;

            self.0.borrow_mut().class_proto[JS_CLASS_ARRAY as usize] = array_proto.clone().into();

            // 2. make default shape for Array
            let array_shape = JSShape::new(self, Some(array_proto.clone()), 1).unwrap();

            // define length property, index=0
            array_shape
                .add_shape_property(
                    self,
                    Atom::new_const(JS_ATOM_length),
                    JS_PROP_WRITABLE | JS_PROP_LENGTH,
                    None,
                )
                .unwrap();

            self.0.borrow_mut().array_shape.replace(array_shape);
        }

        Ok(())
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct JSContextWeakref<'a>(Weak<RefCell<JSContextInner<'a>>>);

impl<'a> Default for JSContextWeakref<'a> {
    fn default() -> Self {
        Self(Weak::new())
    }
}

impl<'a> JSContextWeakref<'a> {
    pub fn upgrade(&self) -> Option<JSContext<'a>> {
        self.0.upgrade().map(JSContext)
    }
}

/* only valid inside C functions */
pub(crate) fn JS_GetActiveFunction<'a>(ctx: &JSContext<'a>) -> JSValue<'a> {
    unsafe { (&(*ctx.rt().cur_stack_frame.cast::<StackFrame<'a>>()).cur_func).clone() }
}

pub const JS_MODE_STRICT: i32 = 1 << 0;
pub const JS_MODE_STRIP: i32 = 1 << 1;
pub const JS_MODE_MATH: i32 = 1 << 2;
pub const JS_MODE_ASYNC: i32 = 1 << 3; /* async function */

/// test if content is currently in strict mode
pub fn is_strict_mode(ctx: &JSContext) -> bool {
    unsafe {
        let sf = ctx.rt().cur_stack_frame.cast::<StackFrame>();
        !sf.is_null() && (*sf).js_mode & JS_MODE_STRICT != 0
    }
}

/// Represents atomic operations
pub enum AtomicOp {
    Add,
    And,
    CompareExchange,
    Exchange,
    Load,
    Or,
    Store,
    Sub,
    Xor,
}

impl<'a> JSRuntime<'a> {
    /// Creates context with setup closure.
    pub(super) fn create_context_raw(
        &mut self,
        setup: impl FnOnce(&JSContext<'a>) -> Result<(), i32>,
    ) -> Result<JSContext<'a>, i32> {
        let inner = JSContextInner::<'a>::new(self)?;
        let ctx = JSContext(Rc::new(RefCell::new(inner)));

        ctx.init_core_objects()?;
        setup(&ctx)?;

        // register context to runtime
        self.context_list.push(ctx.clone());

        Ok(ctx)
    }
}
