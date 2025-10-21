use std::{
    cell::RefCell,
    ffi::c_void,
    ops::DerefMut,
    pin::Pin,
    ptr::NonNull,
    rc::{Rc, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    error::STD_ERROR_COUNT,
    eval::EvalFn,
    module::{JSFreeModuleEnum, ModuleDef, js_free_modules},
    object::{JSObject, JSShape},
    regexp::RegExpData,
    runtime::JSRuntime,
    stack_frame::StackFrame,
    value::JSValue,
};

/// Execution context
///
/// Represents a JavaScript execution environment with its own global object,
/// stack, and execution state. Each context is associated with a runtime.
pub struct JSContextInner {
    /// pointer to runtime
    pub rt: NonNull<JSRuntime>,

    pub flags: u8,

    /// Count of binary objects in this context
    pub binary_object_count: u16,

    /// Total size of binary objects in this context
    pub binary_object_size: i32,

    /// Initial shape for Array objects
    pub array_shape: Option<JSShape>,

    /// Class protos
    pub class_proto: Vec<JSValue>,
    /// same as class_proto[JS_CLASS_BYTECODE_FUNCTION]
    pub function_proto: JSValue,

    pub function_ctor: JSValue,
    pub array_ctor: JSValue,
    pub regexp_ctor: JSValue,
    pub promise_ctor: JSValue,
    pub native_error_proto: [JSValue; STD_ERROR_COUNT],
    pub iterator_proto: JSValue,
    pub async_iterator_proto: JSValue,
    pub array_proto_values: JSValue,
    pub throw_type_error: JSValue,

    /// the function of eval().
    pub eval_func: JSValue,

    /// 传统的全局对象（类似ES3的全局环境），存储普通的全局变量（如var声明的变量）
    /// 默认WRITABLE，权限更宽松
    pub global_obj: JSValue, // TODO: use JSObject instead

    /// let/const或特殊标记(DEFINE_GLOBAL_LEX_VAR)定义的变量
    /// 强调CONFIGURABLE和约束写权限, 禁止复杂属性行为（如代理式操作）
    pub global_var_obj: JSValue, // TODO: use JSObject instead

    /// Random number generator state
    pub random_state: u64,

    /// Interrupt counter for async operations
    pub interrupt_counter: i32,

    /// List of loaded modules
    pub loaded_modules: Vec<ModuleDef>,

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
    pub stack_frame: *mut Pin<StackFrame>,
}

impl Drop for JSContextInner {
    fn drop(&mut self) {
        #[cfg(debug_assertions)]
        println!("JSContextInner::drop({self:p})");

        self.finalize();
    }
}

impl JSContextInner {
    pub fn new(rt: &mut JSRuntime) -> Result<Self, i32> {
        let class_proto = {
            let mut lst = Vec::<JSValue>::new();
            lst.try_reserve_exact(rt.class_defs.capacity())
                .map_err(|_| -1)?;
            lst.extend(std::iter::repeat_n(JSValue::NULL, rt.class_defs.len()));
            lst
        };

        let c = Self {
            rt: NonNull::from(rt),
            flags: 0,

            binary_object_count: 0,
            binary_object_size: 0,

            array_shape: None,

            function_ctor: JSValue::NULL,
            array_ctor: JSValue::NULL,
            regexp_ctor: JSValue::NULL,
            promise_ctor: JSValue::NULL,

            class_proto,

            function_proto: JSValue::UNDEFINED,
            native_error_proto: [const { JSValue::UNDEFINED }; STD_ERROR_COUNT],
            iterator_proto: JSValue::UNDEFINED,
            async_iterator_proto: JSValue::UNDEFINED,
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

            user_opaque: std::ptr::null_mut(),
        };

        Ok(c)
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub const fn rt_ptr(&self) -> NonNull<JSRuntime> {
        self.rt
    }

    #[inline(always)]
    pub const fn rt(&self) -> &JSRuntime {
        unsafe { self.rt.as_ref() }
    }

    #[inline(always)]
    pub const fn rt_mut(&mut self) -> &mut JSRuntime {
        unsafe { self.rt.as_mut() }
    }

    /// initialize random state
    pub fn init_random_seed(&mut self) {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;

        // seed must not be zero.
        self.random_state = if seed != 0 { seed } else { 1 };
    }

    fn gc_dump(&self, _rt: &JSRuntime) {
        println!("{:14p} [context]", self);
    }

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
        self.function_proto = JSValue::UNDEFINED;
        self.native_error_proto = [const { JSValue::UNDEFINED }; STD_ERROR_COUNT];
        self.iterator_proto = JSValue::UNDEFINED;
        self.async_iterator_proto = JSValue::UNDEFINED;
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
        self.rt_mut().gc_dump_objects(1);

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
pub struct JSContext(pub Rc<RefCell<JSContextInner>>);

impl PartialEq for JSContext {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl std::fmt::Debug for JSContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<JSContext {:p}>", self.id() as *const c_void)
    }
}

impl JSContext {
    #[inline]
    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    pub fn with_mut<R>(&self, f: impl FnOnce(&mut JSContextInner) -> R) -> R {
        f(self.0.borrow_mut().deref_mut())
    }

    /// Returns the runtime prt
    #[inline]
    pub fn rt_ptr(&self) -> NonNull<JSRuntime> {
        self.0.borrow().rt_ptr()
    }

    /// Returns the runtime reference
    #[inline(always)]
    pub fn rt(&self) -> &JSRuntime {
        unsafe { self.rt_ptr().as_ref() }
    }

    /// Returns the runtime mut reference
    #[allow(clippy::mut_from_ref)]
    #[inline(always)]
    pub fn rt_mut(&self) -> &mut JSRuntime {
        unsafe { self.rt_ptr().as_mut() }
    }

    /// get global object
    #[inline]
    pub fn global_obj(&self) -> JSObject {
        unsafe { self.0.borrow().global_obj.as_object_unchecked().clone() }
    }

    /// get global var object
    #[inline]
    pub fn global_var_obj(&self) -> JSObject {
        unsafe { self.0.borrow().global_var_obj.as_object_unchecked().clone() }
    }

    /// Pushes a new stack frame onto the execution stack
    ///
    /// # Safety
    /// The caller must ensure the frame pointer is valid
    pub fn push_stack_frame(&mut self, frame: *mut Pin<StackFrame>) {
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
    pub fn pop_stack_frame(&mut self) -> *mut Pin<StackFrame> {
        let mut ctx = self.0.borrow_mut();

        if ctx.stack_frame.is_null() {
            std::ptr::null_mut()
        } else {
            let frame = ctx.stack_frame;
            ctx.stack_frame = unsafe { (*frame.cast::<StackFrame>()).prev_frame };
            frame
        }
    }

    pub fn downgrade(&self) -> JSContextWeakref {
        JSContextWeakref(Rc::downgrade(&self.0))
    }

    pub fn gc_has_mark(&self) -> bool {
        self.0.borrow().flags & 1 != 0
    }

    pub fn gc_set_mark(&self, mark: bool) {
        let mut b = self.0.borrow_mut();
        b.flags = (b.flags & 0b1111_1110) | (mark as u8)
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct JSContextWeakref(Weak<RefCell<JSContextInner>>);

impl JSContextWeakref {
    pub fn new() -> Self {
        Self(Weak::new())
    }

    pub fn upgrade(&self) -> Option<JSContext> {
        self.0.upgrade().map(JSContext)
    }
}

/* only valid inside C functions */
pub(crate) fn JS_GetActiveFunction(ctx: &JSContext) -> &JSValue {
    unsafe { &(*ctx.rt().cur_stack_frame.cast::<StackFrame>()).cur_func }
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

impl JSRuntime {
    /// Creates a new JavaScript execution context base, and call customize.
    pub fn create_context_customize(
        &mut self,
        setup: impl FnOnce(&JSContext) -> Result<(), i32>,
    ) -> Result<JSContext, i32> {
        let inner = JSContextInner::new(self)?;
        let ctx = JSContext(Rc::new(RefCell::new(inner)));

        setup(&ctx)?;
        self.context_list.push(ctx.clone());
        Ok(ctx)
    }

    /// Creates a new JavaScript execution context core.
    pub fn create_context_core(&mut self) -> Result<JSContext, i32> {
        let ctx = self.create_context_customize(|c| {
            c.init_core_objects();
            Ok(())
        })?;
        Ok(ctx)
    }

    /// Creates a new JavaScript execution context
    pub fn create_context(&mut self) -> Result<JSContext, i32> {
        let ctx = self.create_context_core()?;

        ctx.intrinsic_base_objects();
        ctx.intrinsic_proxy();
        ctx.intrinsic_async();
        ctx.intrinsic_typed_arrays();
        ctx.intrinsic_regexp();
        ctx.intrinsic_mapset();
        ctx.intrinsic_date();

        ctx.intrinsic_math(); // Math: lazy init object
        ctx.intrinsic_json(); // JSON: lazy init object

        Ok(ctx)
    }
}
