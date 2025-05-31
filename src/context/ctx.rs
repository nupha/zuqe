use std::{
    ops::DerefMut,
    os::raw::c_char,
    ptr::{self, NonNull},
};

use crate::{
    array::JSObjectArray,
    atom::{
        consts::{JS_ATOM_empty_string, JS_ATOM_length, JS_ATOM_message, JS_ATOM_name},
        JS_NewAtomString,
    },
    error::{js_error_proto_funcs, JS_NATIVE_ERROR_COUNT, NATIVE_ERROR_NAMES},
    eval::EvalFunc,
    function::{js_function_proto, JSCFunctionEnum, JSVarBinding, JS_SetPropertyFunctionList},
    gc::{GCObject, GCObjectNodeData, GCObjectType, JSGCObjectHeader, JSGCPhaseEnum, RefCounted},
    mem::js_malloc_rt_typed,
    module::{js_free_modules, JSFreeModuleEnum},
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    utils::{init_list_head, list_head, Node},
    value::{
        object::{
            get_proto_obj, JSObjectIsa, JSShape, JS_PROP_CONFIGURABLE, JS_PROP_INITIAL_HASH_SIZE,
            JS_PROP_LENGTH, JS_PROP_WRITABLE,
        },
        JSValue, JS_FreeValue,
    },
    JS_CLASS_ARRAY, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_ERROR, JS_CLASS_OBJECT,
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

impl RefCounted for JSContext {
    fn ref_count(&self) -> i32 {
        self.header.ref_count
    }

    fn inc_ref(&mut self) -> i32 {
        self.header.ref_count += 1;
        self.header.ref_count
    }

    fn dec_ref(&mut self) -> i32 {
        self.header.ref_count -= 1;
        self.header.ref_count
    }
}

impl GCObject for JSContext {
    fn gc_type(&self) -> crate::gc::GCObjectType {
        GCObjectType::Context(NonNull::from(self))
    }

    fn gc_mark(&self) -> u8 {
        self.header.gc_mark()
    }

    fn set_gc_mark(&mut self, mark: u8) {
        self.header.set_gc_mark(mark);
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14p} {:4} [context]", self, self.ref_count());
    }

    fn finalize(&mut self, rt: &mut JSRuntime) {
        println!("[O.o] ************* Context finalize");

        #[cfg(feature = "dump_atoms")]
        JS_DumpAtoms(rt);

        #[cfg(feature = "dump_shapes")]
        crate::shape::JS_DumpShapes((*ctx).rt);

        #[cfg(feature = "dump_objects")]
        self.rt_mut().gc_dump_objects(1);

        #[cfg(feature = "DUMP_MEM")]
        {
            let mut stats: JSMemoryUsage;
            JS_ComputeMemoryUsage(rt, &mut stats);
            JS_DumpMemoryUsage(stdout, &stats, rt);
        }

        unsafe {
            js_free_modules(self, JSFreeModuleEnum::JS_FREE_MODULE_ALL);
        }

        JS_FreeValue(self, self.global_obj);
        JS_FreeValue(self, self.global_var_obj);
        JS_FreeValue(self, self.throw_type_error);
        JS_FreeValue(self, self.eval_func);
        JS_FreeValue(self, self.array_proto_values);

        for i in 0..JS_NATIVE_ERROR_COUNT {
            JS_FreeValue(self, self.native_error_proto[i as usize]);
        }

        while let Some(c) = self.class_proto.pop() {
            JS_FreeValue(self, c);
        }

        JS_FreeValue(self, self.iterator_proto);
        JS_FreeValue(self, self.async_iterator_proto);
        JS_FreeValue(self, self.promise_ctor);
        JS_FreeValue(self, self.array_ctor);
        JS_FreeValue(self, self.regexp_ctor);
        JS_FreeValue(self, self.function_ctor);
        JS_FreeValue(self, self.function_proto);

        if !self.array_shape.is_null() {
            let sh = self.array_shape;
            JSShape::free(self.rt_mut(), sh.into());
        }
    }

    fn gc_list_node(&self) -> NonNull<Node<GCObjectNodeData>> {
        debug_assert!(!self.header.list_node.is_null());
        unsafe { NonNull::new_unchecked(self.header.list_node) }
    }
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
        JS_FreeValue(self, self.current_exception);
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

    /// Add minimum amount of objects to be able to compile code and display error messages.
    /// No JSAtom should be allocated by this function.
    fn add_core_objects(&mut self) {
        let object_proto = self
            .create_instance_from_proto(JSValue::NULL, JSObjectIsa::OBJECT)
            .unwrap_or(JSValue::EXCEPTION);

        // Object
        self.class_proto[JS_CLASS_OBJECT as usize] = object_proto;

        // Function
        {
            self.function_proto = self
                .create_native_function_with_proto(
                    js_function_proto,
                    "",
                    0,
                    JSCFunctionEnum::NativeFunc,
                    0,
                    object_proto,
                )
                .unwrap();
            self.class_proto[JS_CLASS_BYTECODE_FUNCTION as usize] = self.function_proto.dup();
        }

        // Error
        unsafe {
            let err_proto = self.create_base_object().unwrap();
            self.class_proto[JS_CLASS_ERROR as usize] = err_proto;

            JS_SetPropertyFunctionList(
                self,
                err_proto,
                js_error_proto_funcs.as_ptr(),
                js_error_proto_funcs.len(),
            );

            // standard Errors
            for i in 0..JS_NATIVE_ERROR_COUNT {
                let proto = self
                    .create_instance_from_proto(err_proto, JSObjectIsa::OBJECT)
                    .unwrap();

                let name = JS_NewAtomString(self, NATIVE_ERROR_NAMES[i]);
                proto
                    .define_property_value(
                        self,
                        JS_ATOM_name,
                        name,
                        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                    )
                    .unwrap();

                let msg = JS_ATOM_empty_string.to_value(self, true);
                proto
                    .define_property_value(
                        self,
                        JS_ATOM_message,
                        msg,
                        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                    )
                    .unwrap();

                self.native_error_proto[i] = proto;
            }
        }

        // Array
        {
            // the array prototype is an array
            let array_proto = self
                .create_instance_from_proto(
                    object_proto,
                    JSObjectIsa::ARRAY(JSObjectArray::default()),
                )
                .unwrap();
            self.class_proto[JS_CLASS_ARRAY as usize] = array_proto;

            self.array_shape = JSShape::with_capacity(
                self,
                unsafe { get_proto_obj(array_proto).as_mut() },
                JS_PROP_INITIAL_HASH_SIZE,
                1,
            )
            .unwrap()
            .as_ptr();

            // add length property as index 0
            unsafe {
                (*self.array_shape)
                    .add_shape_property(
                        self,
                        None,
                        JS_ATOM_length,
                        JS_PROP_WRITABLE | JS_PROP_LENGTH,
                    )
                    .unwrap();
            }
        }

        /* XXX: could test it on first context creation to ensure that no
        new atoms are created in JS_AddIntrinsicBasicObjects(). It is
        necessary to avoid useless renumbering of atoms after
        JS_EvalBinary() if it is done just after JS_AddIntrinsicBasicObjects(). */
        // assert_eq!((*(*ctx).rt).atom_count as u32, JS_ATOM_INIT_COUNT);
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
    if (*ctx).dec_ref() == 0 {
        let rt = (*ctx).rt_mut();

        if rt.gc_phase != JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES {
            let node = (*ctx).gc_list_node();
            Node::detach_from_list(node);
            rt.gc_zero_ref_count_list.push_raw(node);

            if rt.gc_phase == JSGCPhaseEnum::JS_GC_PHASE_NONE {
                rt.gc_free_zeroed();
            }
        }
    }
}

impl JSRuntime {
    /// Create JSContext register to gc list, then returns the context handle.
    unsafe fn create_context_raw(&mut self) -> Result<JSContextHandle, i32> {
        let p = js_malloc_rt_typed::<JSContext>(self, 1, false).ok_or(-1)?;
        p.write(JSContext {
            header: JSGCObjectHeader::new(1),
            rt: NonNull::new_unchecked(self),
            link: list_head::default(),
            binary_object_count: 0,
            binary_object_size: 0,

            array_shape: std::ptr::null_mut(),

            function_ctor: JSValue::NULL,
            array_ctor: JSValue::NULL,
            regexp_ctor: JSValue::NULL,
            promise_ctor: JSValue::NULL,

            class_proto: {
                let mut lst = Vec::<JSValue>::new();
                lst.try_reserve_exact(self.class_array.capacity())
                    .map_err(|_| -1)?;
                lst.extend(std::iter::repeat_n(JSValue::NULL, self.class_array.len()));
                lst
            },
            function_proto: JSValue::UNDEFINED,
            native_error_proto: [JSValue::UNDEFINED; JS_NATIVE_ERROR_COUNT],
            iterator_proto: JSValue::UNDEFINED,
            async_iterator_proto: JSValue::UNDEFINED,
            array_proto_values: JSValue::UNDEFINED,
            throw_type_error: JSValue::UNDEFINED,
            eval_func: JSValue::UNDEFINED,

            current_exception: JSValue::NULL,
            global_obj: JSValue::UNDEFINED,
            global_var_obj: JSValue::UNDEFINED,

            random_state: 0,
            interrupt_counter: 0,
            loaded_modules: list_head::default(),
            compile_regexp: None,
            eval_handle: None,

            stack_trace: JSValue::UNDEFINED,
            stack_limit: 0,
            stack_pointer: 0,
            stack_size: 0,
            stack_frame: std::ptr::null_mut(),

            user_opaque: std::ptr::null_mut(),
        });
        let mut ctx = Box::from_raw(p.as_ptr());

        init_list_head(&mut ctx.loaded_modules);
        init_list_head(&mut ctx.header.link);

        ctx.add_core_objects();

        let ctx_ptr: JSContextHandle = ctx.handle();

        self.context_list.push(ctx_ptr);
        self.gc_add_object(ctx);

        Ok(ctx_ptr)
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
