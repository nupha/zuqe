use std::ptr::NonNull;

use smallvec::SmallVec;

use crate::{
    atom::{consts::JS_ATOM_prototype, JSAtom, JS_FreeAtomRT, JS_ATOM_NULL},
    bytecode::{opcode_info_final, opcode_info_intermediate, run_bytecodes},
    context::{JSContext, JS_FreeContext},
    eval::JS_EVAL_TYPE_INDIRECT,
    function::{func_kind_to_class_id, JSVarRef, JS_GetFunctionRealm, JS_CALL_FLAG_COPY_ARGV},
    gc::{GCObject, GCObjectHeader, GCObjectNodeData, GCObjectType, RefCounted},
    runtime::JSRuntime,
    stack_frame::JSStackFrame,
    utils::{get_leb128, get_sleb128, read_from_ptr, Node},
    value::{
        object::JS_SetPrototypeInternal, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT,
        JS_TAG_FUNCTION_BYTECODE, JS_TAG_OBJECT,
    },
    JS_CLASS_BYTECODE_FUNCTION,
};

use super::{clousure::JSClosureVar, var::JSVarDef, JSFunctionKindEnum};

#[derive(Debug)]
pub struct JSFunctionBytecodeDebug {
    pub filename: JSAtom,
    pub line_num: i32,
    pub source: Option<String>,
    pub pc2line_buf: Option<Vec<u8>>,
}

impl Default for JSFunctionBytecodeDebug {
    fn default() -> Self {
        Self {
            filename: JS_ATOM_NULL,
            line_num: 0,
            source: None,
            pc2line_buf: None,
        }
    }
}

#[repr(C)]
pub struct JSFunctionBytecode {
    pub header: GCObjectHeader, // must come first

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

    pub func_name: JSAtom,
    pub realm: *mut JSContext, // function realm
    pub stack_size: u16,

    pub cpool_count: u16,
    pub arg_count: u16,
    pub var_count: u16,
    /// for length function property
    pub defined_arg_count: u16,
    pub closure_var_count: u16,

    // TODO: use Option<JSFunctionBytecodeDebug>>
    pub debug: JSFunctionBytecodeDebug,
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

impl RefCounted for JSFunctionBytecode {
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

impl GCObject for JSFunctionBytecode {
    fn gc_type(&self) -> GCObjectType {
        GCObjectType::FunctionBytecode(NonNull::from(self))
    }

    fn gc_mark(&self) -> u8 {
        self.header.gc_mark()
    }

    fn set_gc_mark(&mut self, mark: u8) {
        self.header.set_gc_mark(mark);
    }

    fn gc_mark_children(&mut self, rt: &mut JSRuntime, mark_func: crate::gc::GCMarkFunc) {
        // The template objects can be part of a cycle
        if let Some(cpool) = self.cpools_slice() {
            cpool.iter().for_each(|v| unsafe {
                rt.gc_mark_value(*v, mark_func);
            });
        }

        if !self.realm.is_null() {
            unsafe {
                mark_func(rt, self.realm);
            }
        }
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14p} {:4} [function bytecode]", self, self.ref_count());
    }

    fn finalize(&mut self, rt: &mut JSRuntime) {
        free_bytecode_atoms(rt, &self.binary[self.bytecode_start as usize..], true);

        // free const values
        if let Some(consts) = self.cpools_slice() {
            for v in consts {
                unsafe {
                    JS_FreeValueRT(rt, *v);
                }
            }
        }

        // free vardef var_name atom
        if let Some(vds) = unsafe { self.vardefs_slice_mut() } {
            for vd in vds {
                JS_FreeAtomRT(rt, vd.var_name);
            }
        }

        // free closure vars atom
        if let Some(cvs) = self.closure_vars_slice_mut() {
            for cv in cvs {
                JS_FreeAtomRT(rt, cv.var_name);
            }
        }

        if !self.realm.is_null() {
            unsafe {
                JS_FreeContext(self.realm);
            }
        }

        JS_FreeAtomRT(rt, self.func_name);
        self.func_name = JS_ATOM_NULL; // safe guard

        if self.has_debug {
            JS_FreeAtomRT(rt, self.debug.filename);
            self.debug.filename = JS_ATOM_NULL;

            // self.debug.pc2line_buf.take();
            // drop(self.debug.source.take());
        }
    }

    fn gc_node_ptr(&self) -> NonNull<Node<GCObjectNodeData>> {
        debug_assert!(!self.header.list_node.is_null());
        unsafe { NonNull::new_unchecked(self.header.list_node) }
    }
}

/* for the encoding of the pc2line table */
pub const PC2LINE_BASE: i32 = -1;
pub const PC2LINE_RANGE: i32 = 5;
pub const PC2LINE_OP_FIRST: i32 = 1;
pub const PC2LINE_DIFF_PC_MAX: i32 = (255 - PC2LINE_OP_FIRST) / PC2LINE_RANGE;

impl JSFunctionBytecode {
    /// get cpool values start pointer
    pub unsafe fn cpools_ptr(&self) -> Option<NonNull<JSValue>> {
        if self.cpool_start >= 0 {
            debug_assert!(self.cpool_count > 0);
            NonNull::new(
                self.binary
                    .as_ptr()
                    .add(self.cpool_start as usize)
                    .cast::<JSValue>() as _,
            )
        } else {
            debug_assert_eq!(self.cpool_count, 0);
            None
        }
    }

    /// get vardefs (args+vars) starting pointer
    pub unsafe fn vardefs_ptr(&mut self) -> Option<NonNull<JSVarDef>> {
        if self.vardefs_start >= 0 {
            debug_assert!(self.arg_count > 0 || self.var_count > 0);
            NonNull::new(
                self.binary
                    .as_mut_ptr()
                    .add(self.vardefs_start as usize)
                    .cast::<JSVarDef>(),
            )
        } else {
            None
        }
    }

    /// get closure var starting pointer
    pub unsafe fn closure_var_ptr(&mut self) -> Option<NonNull<JSClosureVar>> {
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
        NonNull::new(self.binary.as_mut_ptr().add(self.bytecode_start as usize)).unwrap()
    }

    /// get const pool values slice
    pub fn cpools_slice(&self) -> Option<&[JSValue]> {
        unsafe {
            self.cpools_ptr()
                .map(|p| std::slice::from_raw_parts(p.as_ptr(), self.cpool_count as usize))
        }
    }

    /// get args slice
    pub fn args_slice(&self) -> Option<&[JSVarDef]> {
        if self.arg_count > 0 {
            unsafe {
                Some(std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.vardefs_start as usize)
                        .cast::<JSVarDef>(),
                    self.arg_count as usize,
                ))
            }
        } else {
            None
        }
    }

    /// get vars slice
    pub fn vars_slice(&self) -> Option<&[JSVarDef]> {
        if self.var_count > 0 {
            unsafe {
                Some(std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.vardefs_start as usize)
                        .cast::<JSVarDef>()
                        .add(self.arg_count as usize),
                    self.var_count as usize,
                ))
            }
        } else {
            None
        }
    }

    pub unsafe fn closure_vars_slice(&self) -> &[JSClosureVar] {
        if self.closure_var_count > 0 {
            debug_assert!(self.closure_var_start >= 0);
            debug_assert!(self.closure_var_count < u16::MAX);

            std::slice::from_raw_parts(
                self.binary
                    .as_ptr()
                    .add(self.closure_var_start as usize)
                    .cast::<JSClosureVar>(),
                self.closure_var_count as usize,
            )
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
    pub fn vars_slice_mut<'a>(&'a mut self) -> Option<&'a mut [JSVarDef]> {
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
    pub unsafe fn vardefs_slice_mut<'a>(&'a mut self) -> Option<&'a mut [JSVarDef]> {
        self.vardefs_ptr().map(|p| {
            std::slice::from_raw_parts_mut(p.as_ptr(), (self.arg_count + self.var_count) as usize)
        })
    }

    /// get closure-vars slice
    pub fn closure_vars_slice_mut<'a>(&'a mut self) -> Option<&'a mut [JSClosureVar]> {
        unsafe {
            self.closure_var_ptr().map(|p| {
                std::slice::from_raw_parts_mut(p.as_ptr(), self.closure_var_count as usize)
            })
        }
    }

    /// get bytecode opcodes slice
    #[inline(always)]
    pub fn bytecode_slice(&self) -> &[u8] {
        &self.binary[self.bytecode_start as usize..]
    }

    pub fn get_cpool(&self, idx: usize) -> Option<JSValue> {
        if self.cpool_start >= 0 && idx < self.cpool_count as usize {
            let p = unsafe {
                self.binary
                    .as_ptr()
                    .add(self.cpool_start as usize)
                    .cast::<JSValue>()
            };

            Some(unsafe { *p.add(idx) })
        } else {
            None
        }
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
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct BytecodeFunctionValue(JSValue);

impl JSValue {
    pub unsafe fn as_bytecode_function_unchecked(self) -> BytecodeFunctionValue {
        debug_assert!(matches!(
            self.tag(),
            JS_TAG_FUNCTION_BYTECODE | JS_TAG_OBJECT
        ));
        debug_assert_eq!(
            self.object_unchecked().class_id(),
            JS_CLASS_BYTECODE_FUNCTION
        );
        std::mem::transmute(self)
    }
}

impl BytecodeFunctionValue {
    /// call bytecode function
    pub unsafe fn call_internal(
        self,
        caller_ctx: &mut JSContext,
        this_obj: JSValue,
        new_target: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSValue {
        let func_obj = self.0;
        let p = self.0.object_unchecked();
        let func = p.isa().func().unwrap();
        let bf = func.function_bytecode.as_ref().unwrap();

        // how many args that should be copied to local buf?
        // `args.len()` is actual argument count
        // `b.arg_count` is the length when defining the function
        let arg_copy_num =
            if flags & JS_CALL_FLAG_COPY_ARGV != 0 || args.len() < bf.arg_count as usize {
                std::cmp::max(args.len(), bf.arg_count as usize)
            } else {
                0
            };

        // how many bytes to allocate for local buf (args+vars+stack)?
        let buf_values_count =
            arg_copy_num as usize + bf.var_count as usize + bf.stack_size as usize;

        let mut sf_s = JSStackFrame {
            prev_frame: std::ptr::null_mut(),
            cur_func: func_obj,
            local_buf: Vec::<JSValue>::new(),
            arg_base_ptr: std::ptr::null_mut(),
            arg_count: 0,
            var_base_ptr: std::ptr::null_mut(),
            var_count: 0,
            cur_pc_offset: 0,
            cur_sp_offset: 0,
            var_ref_list: Vec::new(),
            js_mode: bf.js_mode as _,
        };
        let sf = &mut sf_s;

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

        if arg_copy_num != 0 {
            debug_assert!(arg_copy_num >= args.len());

            // copy actual arg values - duplicated
            sf.local_buf.extend(args.iter().map(|x| x.dup()));
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

        // initialize vars + stack section
        sf.local_buf.extend(std::iter::repeat_n(
            JSValue::UNDEFINED,
            (bf.var_count + bf.stack_size) as usize,
        ));
        debug_assert_eq!(sf.local_buf.len(), buf_values_count);

        sf.var_base_ptr = sf.local_buf.as_mut_ptr().add(arg_copy_num as usize);
        sf.var_count = bf.var_count;

        sf.prev_frame = caller_ctx.rt().current_stack_frame;
        caller_ctx.rt_mut().current_stack_frame = sf;

        let retval = run_bytecodes(
            bf,
            func.home_object,
            sf,
            caller_ctx,
            this_obj,
            new_target,
            &bf.binary[bf.bytecode_start as usize..],
            0,
            0,
            func.var_refs.as_slice(),
            args,
            bf.js_mode as _,
            false,
        );

        // stack frame is about to drop, check var_ref_list
        for vr in sf.var_ref_list.drain(..) {
            JSVarRef::free(caller_ctx.rt_mut(), vr.cast());
        }

        retval
    }
}

impl BytecodeFunctionValue {
    // XXX: add a specific eval mode so that Function("}), ({") is rejected
    pub unsafe fn hook_constructor(
        ctx: *mut JSContext,
        new_target: JSValueConst,
        argc: i32,
        argv: *mut JSValueConst,
        magic: i32,
    ) -> JSValue {
        debug_assert!((0..4).contains(&magic));
        let func_kind = std::mem::transmute::<u8, JSFunctionKindEnum>(magic as u8);

        let mut buf = SmallVec::<[u8; 64]>::new();
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

        let n = argc - 1;
        for i in 0..n {
            if i != 0 {
                buf.push(b',');
            }

            let v = *argv.offset(i as _);
            if let Ok(sv) = v.coerce_to_utf8(ctx.into()) {
                buf.extend_from_slice(sv.as_bytes());
            } else {
                return JSValue::EXCEPTION;
            }
        }

        buf.extend_from_slice(b") {\n");
        if n >= 0 {
            if let Ok(sv) = (*argv.offset(n as _)).coerce_to_utf8(ctx.into()) {
                buf.extend_from_slice(sv.as_bytes());
            } else {
                return JSValue::EXCEPTION;
            }
        }
        buf.extend_from_slice(b"\n})");

        // create script string
        let src = (*ctx).create_string_from_utf8(buf.into_vec());
        if src.is_exception() {
            return JSValue::EXCEPTION;
        }

        // eval string `src`
        let obj = src.eval_object(
            ctx.as_mut().unwrap(),
            (*ctx).global_obj,
            JS_EVAL_TYPE_INDIRECT,
            -1,
        );
        JS_FreeValue(ctx, src);

        if obj.is_exception() {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        if !new_target.is_undefined() {
            // set the prototype
            let mut proto = new_target.get_property(ctx.as_mut().unwrap(), JS_ATOM_prototype);
            if proto.is_exception() {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
            if !proto.is_object() {
                let realm = JS_GetFunctionRealm(ctx, new_target);
                if realm.is_null() {
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
                proto =
                    (*realm).class_proto[func_kind_to_class_id[func_kind as usize] as usize].dup();
            }

            let ret = JS_SetPrototypeInternal(ctx, obj, proto, true);
            JS_FreeValue(ctx, proto);
            if ret.is_err() {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }

        obj
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
