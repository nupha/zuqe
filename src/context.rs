use std::{
    ffi::CStr,
    ops::{Deref, DerefMut},
    os::raw::c_char,
    ptr::{self, NonNull},
};

use fallible_collections::FallibleVec;

use crate::{
    context_intrinsic::{JS_AddIntrinsicBaseObjects, JS_AddIntrinsicBasicObjects},
    error::JS_NATIVE_ERROR_COUNT,
    eval::__JS_EvalInternal,
    function::JSVarBinding,
    gc::{add_gc_object, remove_gc_object, JSGCObjectHeader, JSGCObjectTypeEnum},
    mem::js_malloc_rt_typed,
    module::{js_free_modules, JSFreeModuleEnum},
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    types::{init_list_head, list_head},
    value::{
        object::{js_free_shape_null, JSShape},
        JSValue, JS_FreeValue, JS_IsUndefined, JS_UNDEFINED,
    },
};

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

/// JavaScript execution context
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
    pub eval_obj: JSValue,

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

    /// Internal eval implementation
    pub eval_internal: Option<
        unsafe fn(
            ctx: *mut JSContext,
            this_obj: JSValue,
            input: *const u8,
            input_len: usize,
            filename: &'static CStr,
            flags: u32,
            scope_idx: i32,
        ) -> JSValue,
    >,

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
    pub fn has_exception(&self) -> bool {
        !JS_IsUndefined(self.current_exception)
    }

    /// Clears the current exception
    pub fn clear_exception(&mut self) {
        self.current_exception = JS_UNDEFINED;
    }

    /// Returns the current exception
    pub fn exception(&self) -> JSValue {
        self.current_exception
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

pub unsafe fn JS_DupContext(ctx: *mut JSContext) -> *mut JSContext {
    (*ctx).header.ref_count += 1;
    ctx
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
    JS_FreeValue(ctx.as_mut(), ctx.eval_obj);

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

    js_free_shape_null(ctx.rt_mut(), ctx.array_shape);

    remove_gc_object(&mut ctx.header);

    // ctx: Box<JSContext> is dropped here
}

impl JSRuntime {
    unsafe fn create_context_raw(&mut self) -> Result<*mut JSContext, i32> {
        let ctx_ptr = js_malloc_rt_typed::<JSContext>(self, 1, true).ok_or(-1)?;
        let mut ctx = Box::from_raw(ctx_ptr.as_ptr());

        ctx.header.ref_count = 1;
        ctx.array_ctor = JSValue::NULL;
        ctx.regexp_ctor = JSValue::NULL;
        ctx.promise_ctor = JSValue::NULL;

        // initialize class proto list
        ctx.class_proto =
            Vec::<JSValue>::try_with_capacity(self.class_array.len()).map_err(|_| -1)?;
        ctx.class_proto
            .extend(std::iter::repeat_n(JSValue::NULL, self.class_array.len()));

        init_list_head(&mut ctx.loaded_modules);

        add_gc_object(
            self,
            std::ptr::addr_of_mut!(ctx.header),
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_CONTEXT,
        );

        ctx.rt = NonNull::new(self as *mut JSRuntime).unwrap();

        JS_AddIntrinsicBasicObjects(&mut ctx);

        // add eval() handler
        ctx.eval_internal.replace(__JS_EvalInternal);

        let ctx_ptr = ctx.as_ref() as *const JSContext as *mut JSContext;
        self.context_list.push(ctx);
        Ok(ctx_ptr)
    }

    /// Creates a new JavaScript execution context
    pub unsafe fn create_context(&mut self) -> Result<*mut JSContext, i32> {
        let ctx = self.create_context_raw()?;

        JS_AddIntrinsicBaseObjects(ctx);

        Ok(ctx)
    }
}

#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct JSContextRef(NonNull<JSContext>);

impl From<&mut JSContext> for JSContextRef {
    fn from(ctx: &mut JSContext) -> Self {
        unsafe { Self(NonNull::new_unchecked(ctx as *mut JSContext)) }
    }
}

impl From<*mut JSContext> for JSContextRef {
    fn from(ctx: *mut JSContext) -> Self {
        unsafe { Self(NonNull::new_unchecked(ctx)) }
    }
}

impl Deref for JSContextRef {
    type Target = JSContext;
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl DerefMut for JSContextRef {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}
