use std::{
    ops::DerefMut,
    os::raw::c_char,
    ptr::{self, NonNull},
};

use crate::{
    error::JS_NATIVE_ERROR_COUNT,
    eval::EvalFunc,
    function::JSVarBinding,
    gc::{add_gc_object, remove_gc_object, JSGCObjectHeader, JSGCObjectTypeEnum},
    mem::js_malloc_rt_typed,
    module::{js_free_modules, JSFreeModuleEnum},
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    types::{init_list_head, list_head},
    value::{object::JSShape, JSValue, JS_FreeValue},
};

use super::JSContextHandle;

/// Represents a lexical environment
#[repr(C)]
pub struct JSLexEnv {
    /// Environment header (must be first field)
    header: JSGCObjectHeader,

    /// Outer environment
    outer: *mut JSLexEnv,

    /// Variable bindings
    vars: *mut JSVarBinding,
}

/// Represents debug information for a function
#[repr(C)]
pub struct JSDebugInfo {
    /// Debug info header (must be first field)
    header: JSGCObjectHeader,

    /// Source file name
    filename: *const c_char,

    /// Line number table
    line_num_table: *const u8,

    /// Source code
    source: *const c_char,

    /// Source length
    source_len: usize,
}

/// Execution context
///
/// Represents a JavaScript execution environment with its own global object,
/// stack, and execution state. Each context is associated with a runtime.
#[repr(C)]
pub struct JSContext {
    /// Garbage collection header (must be first field)
    pub header: JSGCObjectHeader,

    /// Pointer to parent runtime instance
    pub rt: NonNull<JSRuntime>,

    /// Linked list node for runtime context list
    pub link: list_head,

    /// Count of binary objects in this context
    pub binary_object_count: u16,

    /// Total size of binary objects in this context
    pub binary_object_size: i32,

    /// Initial shape for Array objects
    pub array_shape: *mut JSShape,

    /// Class protos
    pub class_proto: Vec<JSValue>,

    pub function_proto: JSValue,
    pub function_ctor: JSValue,
    pub array_ctor: JSValue,
    pub regexp_ctor: JSValue,
    pub promise_ctor: JSValue,
    pub native_error_proto: [JSValue; JS_NATIVE_ERROR_COUNT],
    pub iterator_proto: JSValue,
    pub async_iterator_proto: JSValue,
    pub array_proto_values: JSValue,
    pub throw_type_error: JSValue,

    /// function to eval()
    pub eval_func: JSValue,

    /// 传统的全局对象（类似ES3的全局环境），存储普通的全局变量（如var声明的变量）
    /// 默认WRITABLE，权限更宽松
    pub global_obj: JSValue,

    /// let/const或特殊标记(DEFINE_GLOBAL_LEX_VAR)定义的变量
    /// 强调CONFIGURABLE和约束写权限, 禁止复杂属性行为（如代理式操作）
    pub global_var_obj: JSValue,

    /// Random number generator state
    pub random_state: u64,

    /// Interrupt counter for async operations
    pub interrupt_counter: i32,

    /// List of loaded modules
    pub loaded_modules: list_head,

    /// Regexp compilation callback
    pub compile_regexp:
        Option<unsafe fn(ctx: *mut JSContext, pattern: JSValue, flags: JSValue) -> JSValue>,

    /// eval callback
    pub eval_handle: Option<EvalFunc>,

    /// User data pointer
    pub user_opaque: *mut std::ffi::c_void,

    /// Current exception value
    pub current_exception: JSValue,

    /// Current stack trace
    pub stack_trace: JSValue,

    /// Stack information
    pub stack_limit: usize,
    pub stack_pointer: usize,
    pub stack_size: usize,
    pub stack_frame: *mut JSStackFrame,
}

impl<'a> From<&'a JSContext> for &'a JSRuntime {
    #[inline(always)]
    fn from(value: &'a JSContext) -> Self {
        value.rt()
    }
}

impl<'a> From<&'a mut JSContext> for &'a mut JSRuntime {
    #[inline(always)]
    fn from(value: &'a mut JSContext) -> Self {
        value.rt_mut()
    }
}

impl From<&mut JSContext> for NonNull<JSRuntime> {
    #[inline(always)]
    fn from(value: &mut JSContext) -> Self {
        NonNull::from(value.rt_mut())
    }
}

#[cfg(debug_assertions)]
impl Drop for JSContext {
    fn drop(&mut self) {
        println!("JSContext::drop({self:p})");
    }
}

impl JSContext {
    /// Returns the runtime reference
    #[inline(always)]
    pub fn rt(&self) -> &JSRuntime {
        unsafe { self.rt.as_ref() }
    }

    /// Returns the runtime mut reference
    #[inline(always)]
    pub fn rt_mut(&mut self) -> &mut JSRuntime {
        unsafe { self.rt.as_mut() }
    }

    /// Increase context ref count by 1.
    #[inline(always)]
    pub fn dup(&mut self) -> &mut JSContext {
        self.header.ref_count += 1;
        self
    }

    /// Pushes a new stack frame onto the execution stack
    ///
    /// # Safety
    /// The caller must ensure the frame pointer is valid
    pub unsafe fn push_stack_frame(&mut self, frame: *mut JSStackFrame) {
        if self.stack_frame.is_null() {
            self.stack_frame = frame;
        } else {
            (*frame).prev_frame = self.stack_frame;
            self.stack_frame = frame;
        }
    }

    /// Pops the current stack frame from the execution stack
    ///
    /// # Safety
    /// The caller must ensure no references to the popped frame remain
    pub unsafe fn pop_stack_frame(&mut self) -> *mut JSStackFrame {
        if self.stack_frame.is_null() {
            return ptr::null_mut();
        }
        let frame = self.stack_frame;
        self.stack_frame = (*frame).prev_frame;
        frame
    }

    /// Returns whether an exception is pending
    #[inline(always)]
    pub fn has_exception(&self) -> bool {
        !self.current_exception.is_undefined()
    }

    /// Clears the current exception
    pub fn clear_exception(&mut self) {
        unsafe {
            JS_FreeValue(self, self.current_exception);
        }
        self.current_exception = JSValue::UNDEFINED;
    }

    /// Returns the current exception value with its ref count 'AS-IS'.
    #[inline(always)]
    pub fn exception(&self) -> JSValue {
        self.current_exception
    }

    /// Get global object value, duplicated.
    /// Note: the return value is duplicated, so the caller should free it.
    #[inline(always)]
    pub fn global_object_dup(&self) -> JSValue {
        self.global_obj.dup()
    }
}

/* only valid inside C functions */
#[inline(always)]
pub unsafe fn JS_GetActiveFunction(ctx: *const JSContext) -> JSValue {
    (*(*ctx).rt.as_ref().current_stack_frame).cur_func
}

pub const JS_MODE_STRICT: i32 = 1 << 0;
pub const JS_MODE_STRIP: i32 = 1 << 1;
pub const JS_MODE_MATH: i32 = 1 << 2;
pub const JS_MODE_ASYNC: i32 = 1 << 3; /* async function */

#[inline(always)]
pub fn is_strict_mode(ctx: *mut JSContext) -> bool {
    unsafe {
        let sf = (*ctx).rt.as_ref().current_stack_frame;
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

#[deprecated(note = "use JSContext::dup() instead")]
pub fn JS_DupContext(ctx: *mut JSContext) -> *mut JSContext {
    unsafe { (*ctx).dup() }
}

pub unsafe fn JS_FreeContext(ctx: *mut JSContext) {
    (*ctx).header.ref_count -= 1;
    if (*ctx).header.ref_count > 0 {
        return;
    }

    let mut ctx = {
        let rt = (*ctx).rt_mut();

        let idx = rt
            .context_list
            .iter()
            .position(|c| c.as_ref() as *const JSContext as usize == ctx as usize)
            .unwrap();

        rt.context_list.swap_remove(idx)
    };

    // TODO: move following to JSContext::drop()

    #[cfg(feature = "dump_atoms")]
    JS_DumpAtoms(rt);

    #[cfg(feature = "dump_shapes")]
    crate::shape::JS_DumpShapes((*ctx).rt);

    #[cfg(feature = "dump_objects")]
    JS_DumpGCObjects(rt);

    #[cfg(feature = "DUMP_MEM")]
    {
        let mut stats: JSMemoryUsage;
        JS_ComputeMemoryUsage(rt, &mut stats);
        JS_DumpMemoryUsage(stdout, &stats, rt);
    }

    js_free_modules(ctx.as_mut(), JSFreeModuleEnum::JS_FREE_MODULE_ALL);

    JS_FreeValue(ctx.as_mut(), ctx.global_obj);
    JS_FreeValue(ctx.as_mut(), ctx.global_var_obj);
    JS_FreeValue(ctx.as_mut(), ctx.throw_type_error);
    JS_FreeValue(ctx.as_mut(), ctx.eval_func);
    JS_FreeValue(ctx.as_mut(), ctx.array_proto_values);

    for i in 0..JS_NATIVE_ERROR_COUNT {
        JS_FreeValue(ctx.as_mut(), ctx.native_error_proto[i as usize]);
    }

    while let Some(c) = ctx.class_proto.pop() {
        JS_FreeValue(ctx.as_mut(), c);
    }

    JS_FreeValue(ctx.as_mut(), ctx.iterator_proto);
    JS_FreeValue(ctx.as_mut(), ctx.async_iterator_proto);
    JS_FreeValue(ctx.as_mut(), ctx.promise_ctor);
    JS_FreeValue(ctx.as_mut(), ctx.array_ctor);
    JS_FreeValue(ctx.as_mut(), ctx.regexp_ctor);
    JS_FreeValue(ctx.as_mut(), ctx.function_ctor);
    JS_FreeValue(ctx.as_mut(), ctx.function_proto);

    if !ctx.array_shape.is_null() {
        let sh = ctx.array_shape;
        JSShape::free(ctx.rt_mut(), sh);
    }

    remove_gc_object(&mut ctx.header);

    // ctx: Box<JSContext> is dropped here
}

impl JSRuntime {
    /// Create JSContext, returns the new context pointer wrapper.
    unsafe fn create_context_raw(&mut self) -> Result<JSContextHandle, i32> {
        let p = js_malloc_rt_typed::<JSContext>(self, 1, true).ok_or(-1)?;
        let mut ctx = Box::from_raw(p.as_ptr());

        ctx.header.ref_count = 1;
        ctx.array_ctor = JSValue::NULL;
        ctx.regexp_ctor = JSValue::NULL;
        ctx.promise_ctor = JSValue::NULL;

        // initialize class proto list
        ctx.class_proto = Vec::<JSValue>::new();
        ctx.class_proto
            .try_reserve_exact(self.class_array.len())
            .map_err(|_| -1)?;
        ctx.class_proto
            .extend(std::iter::repeat_n(JSValue::NULL, self.class_array.len()));

        init_list_head(&mut ctx.loaded_modules);

        add_gc_object(
            self,
            std::ptr::addr_of_mut!(ctx.header),
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_CONTEXT,
        );

        ctx.rt = NonNull::new_unchecked(self as *mut JSRuntime);

        ctx.add_core_objects();

        let ctx_ref: JSContextHandle = ctx.deref_mut().handle();

        self.context_list.push(ctx);

        Ok(ctx_ref)
    }

    /// Creates a new JavaScript execution context
    pub fn create_context(&mut self) -> Result<JSContextHandle, i32> {
        let mut ctx = unsafe { self.create_context_raw()? };
        unsafe {
            ctx.intrinsic_base_objects();
            ctx.intrinsic_async();
            ctx.intrinsic_typed_arrays();
        }
        Ok(ctx)
    }
}
