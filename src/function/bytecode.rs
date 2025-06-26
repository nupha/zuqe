use std::{
    cell::{Ref, RefCell, RefMut},
    ops::{Deref, DerefMut},
    ptr::NonNull,
    rc::{Rc, Weak},
};

use smallvec::SmallVec;

use crate::{
    JS_CLASS_BYTECODE_FUNCTION,
    atom::{JS_ATOM_NULL, JS_FreeAtomRT, JSAtom, consts::JS_ATOM_prototype},
    bytecode::{opcode_info_final, opcode_info_intermediate, run_bytecodes},
    context::{JSContext, JSContextWeakref},
    error::JSResult,
    eval::JS_EVAL_TYPE_INDIRECT,
    function::{CallContext, JS_CALL_FLAG_COPY_ARGV, JS_GetFunctionRealm, func_kind_to_class_id},
    gc::{GCMarkFunc, GCObjectHeader},
    global,
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    utils::{get_leb128, get_sleb128, read_from_ptr},
    value::{
        JSValue,
        object::{GCObject, JS_SetPrototypeInternal},
    },
};

use super::{JSFunctionKindEnum, clousure::JSClosureVar, var::JSVarDef};

#[derive(Debug)]
pub struct BytecodeDebugInfo {
    pub filename: JSAtom,
    pub line_num: i32,
    pub source: Option<String>,
    pub pc2line_buf: Option<Vec<u8>>,
}

impl Default for BytecodeDebugInfo {
    fn default() -> Self {
        Self {
            filename: JS_ATOM_NULL,
            line_num: 0,
            source: None,
            pc2line_buf: None,
        }
    }
}

/* for the encoding of the pc2line table */
pub const PC2LINE_BASE: i32 = -1;
pub const PC2LINE_RANGE: i32 = 5;
pub const PC2LINE_OP_FIRST: i32 = 1;
pub const PC2LINE_DIFF_PC_MAX: i32 = (255 - PC2LINE_OP_FIRST) / PC2LINE_RANGE;

#[repr(C)]
#[derive(Debug)]
pub(crate) struct JSFunctionBytecodeInner {
    pub header: GCObjectHeader,

    pub func_name: JSAtom,
    pub realm: JSContextWeakref, // function realm

    pub js_mode: u8,
    pub has_prototype: bool, // true if a prototype field is necessary
    pub has_simple_parameter_list: bool,
    pub is_derived_class_constructor: bool,
    pub need_home_object: bool, // true if home_object needs to be initialized
    pub func_kind: JSFunctionKindEnum, // 2 bits
    pub new_target_allowed: bool,
    pub super_call_allowed: bool,
    pub super_allowed: bool,
    pub arguments_allowed: bool,
    pub backtrace_barrier: bool, // stop backtrace on this function
    pub read_only_bytecode: bool,
    pub is_direct_or_indirect_eval: bool, // used by JS_GetScriptOrModuleName()

    pub stack_size: u16,

    pub cpool_count: u16,
    pub arg_count: u16,
    pub var_count: u16,
    /// for length function property
    pub defined_arg_count: u16,
    pub closure_var_count: u16,

    // TODO: use Option<JSFunctionBytecodeDebug>>
    pub debug: BytecodeDebugInfo,
    pub has_debug: bool,

    /// binary content
    pub binary: Box<[u8]>,
    /// start index of cpool in binary >=0 for valid index
    pub cpool_start: i32,
    /// start index of vardefs in binary >=0 for valid index
    pub vardefs_start: i32,
    /// start index of closure vars >=0 for valid index
    pub closure_var_start: i32,
    /// index in binary where bytecode starts
    pub bytecode_start: u32,
}

impl Drop for JSFunctionBytecodeInner {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl JSFunctionBytecodeInner {
    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    /// get nth arg or var vardef. for example:
    /// get 2nd var def: (2, false)
    /// get 3rd arg def: (3, true)
    pub fn get_vardef(&self, nth: u16, is_arg: bool) -> Option<&JSVarDef> {
        if self.vardefs_start >= 0
            && nth
                < if is_arg {
                    self.arg_count
                } else {
                    self.var_count
                }
        {
            unsafe {
                self.binary
                    .as_ptr()
                    .add(self.vardefs_start as usize)
                    .cast::<JSVarDef>()
                    .add(nth as usize + if is_arg { 0 } else { self.arg_count as usize })
                    .as_ref()
            }
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn get_arg(&self, nth: u16) -> Option<&JSVarDef> {
        self.get_vardef(nth, true)
    }

    #[inline(always)]
    pub fn get_var(&self, nth: u16) -> Option<&JSVarDef> {
        self.get_vardef(nth, false)
    }

    /// get nth closure var def
    pub fn get_closure_var(&self, nth: u16) -> Option<&JSClosureVar> {
        if self.closure_var_start >= 0 && nth < self.closure_var_count {
            unsafe {
                self.binary
                    .as_ptr()
                    .add(self.closure_var_start as usize)
                    .cast::<JSClosureVar>()
                    .add(nth as usize)
                    .as_ref()
            }
        } else {
            None
        }
    }

    /// get nth closure var def
    pub fn get_closure_var_mut(&mut self, nth: u16) -> Option<&mut JSClosureVar> {
        if self.closure_var_start >= 0 && nth < self.closure_var_count {
            unsafe {
                self.binary
                    .as_mut_ptr()
                    .add(self.closure_var_start as usize)
                    .cast::<JSClosureVar>()
                    .add(nth as usize)
                    .as_mut()
            }
        } else {
            None
        }
    }

    /// get args slice
    pub fn args_slice(&self) -> &[JSVarDef] {
        if self.arg_count > 0 {
            debug_assert!(self.vardefs_start >= 0);
            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.vardefs_start as usize)
                        .cast::<JSVarDef>(),
                    self.arg_count as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// get vars slice
    pub fn vars_slice(&self) -> &[JSVarDef] {
        if self.var_count > 0 {
            debug_assert!(self.vardefs_start >= 0);
            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.vardefs_start as usize)
                        .cast::<JSVarDef>()
                        .add(self.arg_count as usize),
                    self.var_count as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// get args slice
    pub fn args_slice_mut(&mut self) -> Option<&mut [JSVarDef]> {
        if self.arg_count > 0 {
            unsafe {
                self.vardefs_ptr()
                    .map(|p| std::slice::from_raw_parts_mut(p.as_ptr(), self.arg_count as usize))
            }
        } else {
            None
        }
    }

    /// get vars slice
    pub fn vars_slice_mut(&mut self) -> Option<&mut [JSVarDef]> {
        if self.var_count > 0 {
            unsafe {
                self.vardefs_ptr().map(|p| {
                    std::slice::from_raw_parts_mut(
                        p.as_ptr().add(self.arg_count as usize),
                        self.var_count as usize,
                    )
                })
            }
        } else {
            None
        }
    }

    /// get args+vars slice. combination of args and vars slice.
    pub fn vardefs_slice(&self) -> &[JSVarDef] {
        if self.vardefs_start >= 0 {
            debug_assert!(self.arg_count > 0 || self.var_count > 0);
            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.vardefs_start as usize)
                        .cast::<JSVarDef>(),
                    (self.arg_count + self.var_count) as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// get args+vars slice. combination of args and vars slice.
    pub fn vardefs_slice_mut(&mut self) -> Option<&mut [JSVarDef]> {
        unsafe {
            self.vardefs_ptr().map(|p| {
                std::slice::from_raw_parts_mut(
                    p.as_ptr(),
                    (self.arg_count + self.var_count) as usize,
                )
            })
        }
    }

    /// get vardefs (args+vars) starting pointer
    unsafe fn vardefs_ptr(&mut self) -> Option<NonNull<JSVarDef>> {
        if self.vardefs_start >= 0 {
            debug_assert!(self.arg_count > 0 || self.var_count > 0);

            NonNull::new(unsafe {
                self.binary
                    .as_mut_ptr()
                    .add(self.vardefs_start as usize)
                    .cast::<JSVarDef>()
            })
        } else {
            None
        }
    }

    pub fn closure_vars_slice(&self) -> &[JSClosureVar] {
        if self.closure_var_count > 0 {
            debug_assert!(self.closure_var_start >= 0);
            debug_assert!(self.closure_var_count < u16::MAX);

            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.closure_var_start as usize)
                        .cast::<JSClosureVar>(),
                    self.closure_var_count as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// get closure-vars slice
    pub fn closure_vars_slice_mut(&mut self) -> Option<&mut [JSClosureVar]> {
        unsafe {
            self.closure_var_ptr().map(|p| {
                std::slice::from_raw_parts_mut(p.as_ptr(), self.closure_var_count as usize)
            })
        }
    }

    /// get closure var starting pointer
    unsafe fn closure_var_ptr(&mut self) -> Option<NonNull<JSClosureVar>> {
        if self.closure_var_count > 0 {
            debug_assert!(self.closure_var_start >= 0);
            NonNull::new(
                self.binary
                    .as_mut_ptr()
                    .add(self.closure_var_start as usize)
                    .cast(),
            )
        } else {
            debug_assert_eq!(self.closure_var_start, -1);
            None
        }
    }

    /// get bytecode starting pointer
    pub unsafe fn bytecode_ptr(&mut self) -> NonNull<u8> {
        NonNull::new(unsafe { self.binary.as_mut_ptr().add(self.bytecode_start as usize) }).unwrap()
    }

    /// get bytecode opcodes slice
    #[inline(always)]
    pub fn bytecode_slice(&self) -> &[u8] {
        &self.binary[self.bytecode_start as usize..]
    }

    /// get const pool values slice
    pub fn cpools_slice(&self) -> &[JSValue] {
        if self.cpool_start >= 0 {
            debug_assert!(self.cpool_count > 0);
            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.cpool_start as usize)
                        .cast::<JSValue>(),
                    self.cpool_count as usize,
                )
            }
        } else {
            debug_assert_eq!(self.cpool_count, 0);
            &[]
        }
    }

    pub unsafe fn find_line_num(&self, pc_value: u32) -> Option<i32> {
        if !self.has_debug || self.debug.pc2line_buf.is_none() {
            // function was stripped
            return None;
        }

        let mut p = self.debug.pc2line_buf.as_ref().unwrap().as_ptr();
        let p_end = p.offset(self.debug.pc2line_buf.as_ref().map(|x| x.len()).unwrap() as isize);

        let mut pc = 0;
        let mut line_num = self.debug.line_num;

        while p < p_end {
            let mut op = *p as i32;
            p = p.offset(1);

            let new_line_num: i32;

            if op == 0 {
                let mut val = 0;
                // FIXME: hard coded len 5
                let ret = if let Ok(n) = get_leb128(&mut val, std::slice::from_raw_parts(p, 5)) {
                    n
                } else {
                    return Some(self.debug.line_num);
                };
                pc += val as i32;
                p = p.offset(ret as isize);

                let mut v = 0_i32;
                // FIXME: hard coded len 5
                let ret = if let Ok(n) = get_sleb128(&mut v, std::slice::from_raw_parts(p, 5)) {
                    n
                } else {
                    return Some(self.debug.line_num);
                };
                p = p.offset(ret as isize);
                new_line_num = line_num + v;
            } else {
                op -= PC2LINE_OP_FIRST;
                pc += op / PC2LINE_RANGE;
                new_line_num = line_num + (op as i32 % PC2LINE_RANGE) + PC2LINE_BASE;
            }

            if pc_value < pc as u32 {
                return Some(line_num);
            }
            line_num = new_line_num;
        }

        Some(line_num)
    }

    fn finalize(&mut self) {
        let rt = self.header.rt_mut() as *mut JSRuntime;

        free_bytecode_atoms(rt, &self.binary[self.bytecode_start as usize..], true);

        // free vardef var_name atom
        for vd in self.vardefs_slice() {
            JS_FreeAtomRT(rt, vd.var_name);
        }

        // free closure vars atom
        for cv in self.closure_vars_slice() {
            JS_FreeAtomRT(rt, cv.var_name);
        }

        // free func name atom
        JS_FreeAtomRT(rt, self.func_name);
        self.func_name = JS_ATOM_NULL;

        if self.has_debug {
            JS_FreeAtomRT(rt, self.debug.filename);
            self.debug.filename = JS_ATOM_NULL;
        }
    }

    fn gc_mark_children(&mut self, rt: &mut JSRuntime, mark_func: GCMarkFunc) {
        // The template objects can be part of a cycle
        self.cpools_slice().iter().for_each(|v| {
            rt.gc_mark_value(v, mark_func);
        });

        if let Some(r) = self.realm.upgrade() {
            // unsafe {
            //     mark_func(rt, r);
            // }
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct JSFunctionBytecode(pub(crate) Rc<RefCell<JSFunctionBytecodeInner>>);

impl From<JSFunctionBytecode> for GCObject {
    fn from(value: JSFunctionBytecode) -> Self {
        Self::FunctionBytecode(value)
    }
}

impl JSFunctionBytecode {
    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    pub fn rt(&self) -> &JSRuntime {
        unsafe { self.0.borrow().header.rt_ptr().as_ref() }
    }

    pub fn with<R>(&self, f: impl FnOnce(&JSFunctionBytecodeInner) -> R) -> R {
        f(self.0.borrow().deref())
    }

    pub fn with_mut<R>(&self, f: impl FnOnce(&mut JSFunctionBytecodeInner) -> R) -> R {
        f(self.0.borrow_mut().deref_mut())
    }

    pub fn downgrade(&self) -> JSFunctionBytecodeWeakref {
        JSFunctionBytecodeWeakref(Rc::downgrade(&self.0))
    }

    /// get const pool values slice
    pub fn cpools_slice(&self) -> Ref<'_, [JSValue]> {
        Ref::map(self.0.borrow(), |b| b.cpools_slice())
    }

    /// get a value by index in cpool
    #[inline(always)]
    pub fn get_cpool(&self, idx: usize) -> Option<JSValue> {
        self.0.borrow().cpools_slice().get(idx).cloned()
    }

    /// get args slice
    #[inline(always)]
    pub fn args_slice(&self) -> Ref<'_, [JSVarDef]> {
        Ref::map(self.0.borrow(), |b| b.args_slice())
    }

    /// get vars slice
    #[inline(always)]
    pub fn vars_slice(&self) -> Ref<'_, [JSVarDef]> {
        Ref::map(self.0.borrow(), |b| b.vars_slice())
    }

    #[inline(always)]
    pub fn closure_vars_slice(&self) -> Ref<'_, [JSClosureVar]> {
        Ref::map(self.0.borrow(), |b| b.closure_vars_slice())
    }

    /// get args slice
    #[inline]
    pub fn args_slice_mut(&self) -> Option<RefMut<'_, [JSVarDef]>> {
        let mut b = self.0.borrow_mut();
        if b.args_slice_mut().is_some() {
            Some(RefMut::map(b, |b| b.args_slice_mut().unwrap()))
        } else {
            None
        }
    }

    /// get vars slice
    #[inline]
    pub fn vars_slice_mut(&self) -> Option<RefMut<'_, [JSVarDef]>> {
        let mut b = self.0.borrow_mut();
        if b.vars_slice_mut().is_some() {
            Some(RefMut::map(b, |b| b.vars_slice_mut().unwrap()))
        } else {
            None
        }
    }

    /// get args+vars slice. combination of args and vars slice.
    #[inline]
    pub fn vardefs_slice_mut(&self) -> Option<RefMut<'_, [JSVarDef]>> {
        let mut b = self.0.borrow_mut();
        if b.vardefs_slice_mut().is_some() {
            Some(RefMut::map(b, |b| b.vardefs_slice_mut().unwrap()))
        } else {
            None
        }
    }

    /// get closure-vars slice
    #[inline]
    pub fn closure_vars_slice_mut(&mut self) -> Option<RefMut<'_, [JSClosureVar]>> {
        let mut b = self.0.borrow_mut();
        if b.closure_vars_slice_mut().is_some() {
            Some(RefMut::map(b, |b| b.closure_vars_slice_mut().unwrap()))
        } else {
            None
        }
    }

    /// get bytecode opcodes slice
    #[inline]
    pub fn bytecode_slice(&self) -> Ref<'_, [u8]> {
        Ref::map(self.0.borrow(), |b| b.bytecode_slice())
    }

    /// get nth arg or var vardef. for example:
    /// get 2nd var def: (2, false)
    /// get 3rd arg def: (3, true)
    pub fn with_vardef<R>(
        &self,
        nth: u16,
        is_arg: bool,
        f: impl FnOnce(&JSVarDef) -> R,
    ) -> Option<R> {
        self.0.borrow().get_vardef(nth, is_arg).map(f)
    }

    #[inline(always)]
    pub fn with_arg<R>(&self, nth: u16, f: impl FnOnce(&JSVarDef) -> R) -> Option<R> {
        self.0.borrow().get_arg(nth).map(f)
    }

    #[inline(always)]
    pub fn with_var<R>(&self, nth: u16, f: impl FnOnce(&JSVarDef) -> R) -> Option<R> {
        self.0.borrow().get_vardef(nth, false).map(f)
    }

    /// get nth closure var def
    pub fn with_closure_var<R>(&self, nth: u16, f: impl FnOnce(&JSClosureVar) -> R) -> Option<R> {
        self.0.borrow().get_closure_var(nth).map(f)
    }

    /// get nth closure var def
    pub fn with_closure_var_mut<R>(
        &mut self,
        nth: u16,
        f: impl FnOnce(&mut JSClosureVar) -> R,
    ) -> Option<R> {
        self.0.borrow_mut().get_closure_var_mut(nth).map(f)
    }

    pub fn find_line_num(&self, pc_value: u32) -> Option<i32> {
        unsafe { self.0.borrow().find_line_num(pc_value) }
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14x} [function bytecode]", self.id());
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct JSFunctionBytecodeWeakref(Weak<RefCell<JSFunctionBytecodeInner>>);

impl JSFunctionBytecodeWeakref {
    pub fn upgrade(&self) -> Option<JSFunctionBytecode> {
        self.0.upgrade().map(JSFunctionBytecode)
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct BytecodeFunctionValue(JSValue);

impl JSValue {
    pub unsafe fn as_bytecode_function_unchecked(self) -> BytecodeFunctionValue {
        // debug_assert!(matches!(self, Self::FunctionBytecode(_) | Self::Object(_)));
        debug_assert!(self.is_object());
        debug_assert_eq!(
            self.object_unchecked().class_id(),
            JS_CLASS_BYTECODE_FUNCTION
        );
        std::mem::transmute(self)
    }
}

impl BytecodeFunctionValue {
    /// call bytecode function
    pub fn call_internal(
        &self,
        caller_ctx: &JSContext,
        this_obj: JSValue,
        new_target: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSValue {
        let func_obj = &self.0;
        let p = unsafe { self.0.object_unchecked() };

        let (bc, home_obj) =
            p.0.isa
                .borrow()
                .func()
                .map(|f| (f.function_bytecode.clone().unwrap(), f.home_object.clone()))
                .unwrap();

        // how many args that should be copied to local buf?
        // `args.len()` is actual argument count
        // `b.arg_count` is the length when defining the function
        let arg_copy_num = if flags & JS_CALL_FLAG_COPY_ARGV != 0
            || args.len() < bc.0.borrow().arg_count as usize
        {
            std::cmp::max(args.len(), bc.0.borrow().arg_count as usize)
        } else {
            0
        };

        // how many bytes to allocate for local buf (args+vars+stack)?
        let (buf_values_count, js_mode) = {
            let b = bc.0.borrow();
            (
                arg_copy_num as usize + b.var_count as usize + b.stack_size as usize,
                b.js_mode as i32,
            )
        };

        let mut sframe = JSStackFrame {
            cur_func: func_obj.clone(),
            js_mode,
            ..Default::default()
        };
        let sf = &mut sframe;

        // make local buffer and running bytecodes with it.
        // local_buf layout:
        //
        // +------------------+
        // | argument values  | <- arg_buf (optional, if need to copy arg)
        // | variable values  | <- var_buf
        // | stack space      | <- stack_buf
        // +------------------+
        //
        if sf.local_buf.try_reserve_exact(buf_values_count).is_err() {
            caller_ctx.JS_ThrowOutOfMemory();
            return JSValue::EXCEPTION;
        }

        // initialize args section
        if arg_copy_num != 0 {
            debug_assert!(arg_copy_num >= args.len());

            // copy actual arg values - duplicated
            sf.local_buf.extend(args.iter().map(|x| x.clone()));
            // padding with undefined
            sf.local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                arg_copy_num - args.len(),
            ));

            sf.arg_count = arg_copy_num as _;
            sf.arg_base_ptr = sf.local_buf.as_mut_ptr()
        } else {
            sf.arg_count = args.len() as _;
            sf.arg_base_ptr = args.as_ptr().cast::<JSValue>() as *mut _;
        }

        // initialize (vars + stack) section
        {
            let b = bc.0.borrow();

            sf.local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                (b.var_count + b.stack_size) as usize,
            ));
            debug_assert_eq!(sf.local_buf.len(), buf_values_count);

            sf.var_base_ptr = unsafe { sf.local_buf.as_mut_ptr().add(arg_copy_num as usize) };
            sf.var_count = b.var_count;
        }

        sf.prev_frame = caller_ctx.rt().current_stack_frame;
        caller_ctx.rt_mut().current_stack_frame = (sf as *mut JSStackFrame).cast();

        let retval = unsafe {
            let b = bc.0.borrow();
            run_bytecodes(
                &bc,
                home_obj.as_ref(),
                sf,
                caller_ctx,
                this_obj,
                new_target,
                &b.binary[b.bytecode_start as usize..],
                0,
                0,
                &p.0.isa.borrow().func().unwrap().var_refs,
                args,
                js_mode,
                false,
            )
        };

        // stack frame is about to drop
        sf.finalize();

        retval
    }
}

impl BytecodeFunctionValue {
    /// handle `Function` constructor. For example:
    ///
    /// ```js
    /// const f = new Function("a", "b", "return a+b")
    /// ```
    pub fn hook_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
        debug_assert!((0..4).contains(&cc.magic()));

        // XXX: add a specific eval mode so that Function("}), ({") is rejected

        let new_target = cc.this();
        let func_kind = unsafe { std::mem::transmute::<u8, JSFunctionKindEnum>(cc.magic() as u8) };

        //
        // make function source script:
        // ([async] function[*] anonymous(a,b){<body>})
        //
        let mut buf = SmallVec::<[u8; 256]>::new();
        buf.push(b'(');

        if matches!(
            func_kind,
            JSFunctionKindEnum::JS_FUNC_ASYNC | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            buf.extend_from_slice(b"async ");
        }

        buf.extend_from_slice(b"function");
        if matches!(
            func_kind,
            JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            buf.push(b'*');
        }

        buf.extend_from_slice(b" anonymous(");

        // Note: the last argument is function body.
        if let Some((body, arglist)) = cc.arguments().split_last() {
            // args list
            for (i, a) in arglist.iter().enumerate() {
                if i != 0 {
                    buf.push(b',');
                }
                buf.extend_from_slice(a.coerce_to_utf8(ctx)?.as_bytes());
            }
            buf.extend_from_slice(b"){");

            // function body
            buf.extend_from_slice(body.coerce_to_utf8(ctx)?.as_bytes());
        }

        buf.extend_from_slice(b"})");

        // create script string
        let src = ctx
            .create_string_from_utf8(buf.into_vec())
            .check_exception()?;

        // eval string `src`
        let func_val = {
            // Note: to avoid ctx.0 borrow conflict, fetch global object in advance
            let global = JSValue::Object(ctx.global_obj());
            src.eval_object(ctx, global, JS_EVAL_TYPE_INDIRECT, -1)
                .check_exception()?
        };

        if !new_target.is_undefined() {
            // set the prototype
            let mut proto = new_target
                .get_property(ctx, JS_ATOM_prototype)
                .check_exception()?;

            if !proto.is_object() {
                let realm = JS_GetFunctionRealm(ctx, &new_target)?;
                proto = realm.0.borrow().class_proto
                    [func_kind_to_class_id[func_kind as usize] as usize]
                    .clone();
            }

            JS_SetPrototypeInternal(ctx, &func_val, proto, true)?;
        }

        Ok(func_val)
    }
}

#[deprecated(note = "unused")]
pub unsafe fn free_function_bytecode(
    rt: *mut JSRuntime,
    mut b: NonNull<JSFunctionBytecode>,
) -> Option<NonNull<JSFunctionBytecode>> {
    unreachable!("unused")
}

/// free atom in bytecode
pub fn free_bytecode_atoms(rt: *mut JSRuntime, bc: &[u8], is_short_opcodes: bool) {
    let bc_len = bc.len();
    let mut pos = 0;
    while pos < bc_len {
        let op = bc[pos];
        let oi = if is_short_opcodes {
            opcode_info_final(op as u8)
        } else {
            opcode_info_intermediate(op as u8)
        };

        if matches!(
            oi.fmt,
            "atom" | "atom_u8" | "atom_u16" | "atom_label_u8" | "atom_label_u16"
        ) {
            unsafe {
                let atom = JSAtom(read_from_ptr::<u32>(bc.as_ptr().add(pos as usize + 1)));
                JS_FreeAtomRT(rt, atom);
            }
        }

        pos += oi.size as usize;
    }
}
