use std::ptr::NonNull;

use smallvec::SmallVec;

use crate::{
    atom::{consts::JS_ATOM_prototype, JSAtom, JS_FreeAtomRT, JS_ATOM_NULL},
    bytecode::{opcode_info_final, opcode_info_intermediate},
    context::{JSContext, JS_DupContext, JS_FreeContext, JS_MODE_STRIP},
    eval::{JS_EvalObject, JS_EVAL_TYPE_DIRECT, JS_EVAL_TYPE_INDIRECT},
    function::{
        add_eval_variables, func_kind_to_class_id, js_free_function_def, resolve_variables,
        JSFunctionDef, JS_GetFunctionRealm, ARG_SCOPE_END, ARG_SCOPE_INDEX,
    },
    gc::{add_gc_object, remove_gc_object, JSGCObjectHeader, JSGCObjectTypeEnum, JSGCPhaseEnum},
    list_entry,
    mem::{js_free, js_free_rt, js_malloc_typed},
    module::add_module_variables,
    runtime::JSRuntime,
    types::{list_add_tail, list_foreach_safe_with_ret},
    utils::{get_leb128, get_sleb128, get_u32},
    value::{
        object::JS_SetPrototypeInternal, JSTag, JSValue, JSValueConst, JS_FreeValue,
        JS_FreeValueRT, JS_IsException, JS_TAG_FUNCTION_BYTECODE, JS_VALUE_GET_TAG,
    },
    JS_MKPTR,
};

use super::{clousure::JSClosureVar, label::resolve_labels, var::JSVarDef, JSFunctionKindEnum};

pub struct JSFunctionBytecodeDebug {
    pub filename: JSAtom,
    pub line_num: u32,
    pub source: *mut u8,
    pub source_len: u32,
    pub pc2line_buf: Option<Vec<u8>>,
}

#[repr(C)]
pub struct JSFunctionBytecode {
    pub header: JSGCObjectHeader, // must come first

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
    pub defined_arg_count: u16, // for length function property
    pub closure_var_count: u16,

    // TODO: use Option<JSFunctionBytecode>>
    pub debug: JSFunctionBytecodeDebug,
    pub has_debug: bool,

    /// binary content
    pub binary: Box<[u8]>,
    pub cpool_offset: i32,       // >=0 for valid index
    pub vardefs_offset: i32,     // >=0 for valid index
    pub closure_var_offset: i32, // >=0 for valid index
    pub bytecode_offset: u32,
}

/* for the encoding of the pc2line table */
pub const PC2LINE_BASE: i32 = -1;
pub const PC2LINE_RANGE: i32 = 5;
pub const PC2LINE_OP_FIRST: i32 = 1;
pub const PC2LINE_DIFF_PC_MAX: i32 = (255 - PC2LINE_OP_FIRST) / PC2LINE_RANGE;

impl JSFunctionBytecode {
    /// get cpool values start pointer
    pub unsafe fn cpools_ptr(&self) -> Option<NonNull<JSValue>> {
        if self.cpool_offset >= 0 {
            debug_assert!(self.cpool_count > 0);
            NonNull::new(
                self.binary
                    .as_ptr()
                    .add(self.cpool_offset as usize)
                    .cast::<JSValue>() as _,
            )
        } else {
            debug_assert_eq!(self.cpool_count, 0);
            None
        }
    }

    /// get vardefs (args+vars) starting pointer
    pub unsafe fn vardefs_ptr(&mut self) -> Option<NonNull<JSVarDef>> {
        if self.vardefs_offset >= 0 {
            debug_assert!(self.arg_count > 0 || self.var_count > 0);
            NonNull::new(
                self.binary
                    .as_mut_ptr()
                    .add(self.vardefs_offset as usize)
                    .cast::<JSVarDef>(),
            )
        } else {
            None
        }
    }

    /// get closure var starting pointer
    pub unsafe fn closure_var_ptr(&mut self) -> Option<NonNull<JSClosureVar>> {
        if self.closure_var_offset >= 0 {
            debug_assert!(self.closure_var_count > 0);
            NonNull::new(
                self.binary
                    .as_mut_ptr()
                    .add(self.closure_var_offset as usize)
                    .cast(),
            )
        } else {
            None
        }
    }

    /// get bytecode starting pointer
    pub unsafe fn bytecode_ptr(&mut self) -> NonNull<u8> {
        NonNull::new(self.binary.as_mut_ptr().add(self.bytecode_offset as usize)).unwrap()
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
                        .add(self.vardefs_offset as usize)
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
                        .add(self.vardefs_offset as usize)
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
            std::slice::from_raw_parts(
                self.binary
                    .as_ptr()
                    .add(self.closure_var_offset as usize)
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
        &self.binary[self.bytecode_offset as usize..]
    }

    pub fn get_cpool(&self, idx: usize) -> Option<JSValue> {
        if self.cpool_offset >= 0 && idx < self.cpool_count as usize {
            let p = unsafe {
                self.binary
                    .as_ptr()
                    .add(self.cpool_offset as usize)
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
        if self.vardefs_offset >= 0
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
                    .add(self.vardefs_offset as usize)
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
        if self.closure_var_offset >= 0 && nth < self.closure_var_count {
            unsafe {
                self.binary
                    .as_ptr()
                    .add(self.closure_var_offset as usize)
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
        if self.closure_var_offset >= 0 && nth < self.closure_var_count {
            unsafe {
                self.binary
                    .as_mut_ptr()
                    .add(self.closure_var_offset as usize)
                    .cast::<JSClosureVar>()
                    .add(nth as usize)
                    .as_mut()
            }
        } else {
            None
        }
    }

    pub unsafe fn find_line_num(&self, pc_value: u32) -> Option<u32> {
        if !self.has_debug || self.debug.pc2line_buf.is_none() {
            // function was stripped
            return None;
        }

        let mut p = self.debug.pc2line_buf.as_ref().unwrap().as_ptr();
        let p_end = p.offset(self.debug.pc2line_buf.as_ref().map(|x| x.len()).unwrap() as isize);

        let mut pc = 0;
        let mut line_num = self.debug.line_num as u32;

        while p < p_end {
            let mut op = *p as i32;
            p = p.offset(1);

            let new_line_num: u32;

            if op == 0 {
                let mut val: u32 = 0;
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
                new_line_num = line_num + v as u32;
            } else {
                op -= PC2LINE_OP_FIRST;
                pc += op / PC2LINE_RANGE;
                new_line_num =
                    (line_num as i32 + (op as i32 % PC2LINE_RANGE) + PC2LINE_BASE) as u32;
            }

            if pc_value < pc as u32 {
                return Some(line_num);
            }
            line_num = new_line_num;
        }

        Some(line_num)
    }
}

pub unsafe fn free_function_bytecode(rt: *mut JSRuntime, b: *mut JSFunctionBytecode) {
    free_bytecode_atoms(rt, &(*b).binary[(*b).bytecode_offset as usize..], true);

    // free const values
    if let Some(consts) = (*b).cpools_slice() {
        for v in consts {
            JS_FreeValueRT(rt, *v);
        }
    }

    // free vardef var_name atom
    if let Some(vds) = (*b).vardefs_slice_mut() {
        for vd in vds {
            JS_FreeAtomRT(rt, vd.var_name);
        }
    }

    // free closure vars atom
    if let Some(cvs) = (*b).closure_vars_slice_mut() {
        for cv in cvs {
            JS_FreeAtomRT(rt, cv.var_name);
        }
    }

    if !(*b).realm.is_null() {
        JS_FreeContext((*b).realm.as_mut().unwrap());
    }

    JS_FreeAtomRT(rt, (*b).func_name);
    (*b).func_name = JS_ATOM_NULL; // safe guard

    if (*b).has_debug {
        (*b).debug.pc2line_buf.take();
        JS_FreeAtomRT(rt, (*b).debug.filename);
        js_free_rt(rt, (*b).debug.source.cast());

        // safe guard
        (*b).debug.filename = JS_ATOM_NULL;
        (*b).debug.source = std::ptr::null_mut();
    }

    remove_gc_object(&mut (*b).header);

    if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES && (*b).header.ref_count != 0 {
        list_add_tail(&mut (*b).header.link, &mut (*rt).gc_zero_ref_count_list);
    } else {
        js_free_rt(rt, b.cast());
    }
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
                let atom = JSAtom(get_u32(bc.as_ptr().add(pos as usize + 1)));
                JS_FreeAtomRT(rt, atom);
            }
        }

        pos += oi.size as usize;
    }
}

/// create a function object from a function definition.
/// All the child functions are also created. It must be done this way to resolve all the variables.
/// The function definition is freed.
pub unsafe fn js_create_function(ctx: &mut JSContext, fd: *mut JSFunctionDef) -> JSValue {
    macro_rules! fail {
        () => {
            js_free_function_def(ctx, fd);
            return JSValue::EXCEPTION;
        };
    }

    //
    // recompute scope linkage
    //

    for scope in (*fd).scopes.iter_mut() {
        scope.first = -1;
    }

    if (*fd).has_parameter_expressions {
        // special end of variable list marker for the argument scope
        (*fd)
            .scopes
            .get_mut(ARG_SCOPE_INDEX as usize)
            .unwrap()
            .first = ARG_SCOPE_END;
    }

    for idx in 0..(*fd).var_count {
        let vd = (*fd).vars.offset(idx as isize);
        debug_assert!((*vd).scope_level >= 0 && (*vd).scope_level < (*fd).scopes.len() as i32);

        let scope = (*fd).scopes.get_mut((*vd).scope_level as usize).unwrap();
        (*vd).scope_next = scope.first;
        scope.first = idx as i32;

        // original code:
        // (*vd).scope_next = (*fd).scopes[(*vd).scope_level as usize].first;
        // (*fd).scopes[ (*vd).scope_level as usize].first = idx as i32;
    }

    for i in 2..(*fd).scopes.len() {
        let scope = (*fd).scopes.get_mut(i as usize).unwrap();
        if scope.first < 0 {
            debug_assert!(scope.parent >= 0);
            scope.first = (*fd).scopes[scope.parent as usize].first;
        }
    }

    for idx in 0..(*fd).var_count {
        let vd = (*fd).vars.offset(idx as isize);
        if (*vd).scope_next < 0 && (*vd).scope_level > 1 {
            let parent = (*fd).scopes[(*vd).scope_level as usize].parent;
            (*vd).scope_next = (*fd).scopes[parent as usize].first;
        }
    }

    // if the function contains an eval call, the closure variables
    // are used to compile the eval and they must be ordered by scope,
    // so it is necessary to create the closure variables before any
    // other variable lookup is done.
    if (*fd).has_eval_call {
        add_eval_variables(ctx, fd.as_mut().unwrap());
    }

    // add the module global variables in the closure
    if !(*fd).module.is_null() {
        if add_module_variables(ctx, fd) != 0 {
            fail!();
        }
    }

    // first create all the child functions
    let r = list_foreach_safe_with_ret(&mut (*fd).child_list, |el| {
        let fd1 = list_entry!(el, JSFunctionDef, link);
        let cpool_idx: i32 = (*fd1).parent_cpool_idx;
        debug_assert!(cpool_idx >= 0);

        let func_obj = js_create_function(ctx, fd1);
        if JS_IsException(func_obj) {
            return Some(-1); // fail exit
        }
        debug_assert_eq!(JS_VALUE_GET_TAG(func_obj), JS_TAG_FUNCTION_BYTECODE);

        // put child func to parent's constant pool
        *(*fd).cpool.add(cpool_idx as usize) = func_obj;

        None
    });
    if r.is_some() {
        fail!();
    }

    #[cfg(feature = "dump_bytecode")]
    if (*fd).js_mode & JS_MODE_STRIP as u8 == 0 {
        println!("pass 1");
        crate::bytecode::dump_byte_code(
            ctx,
            1,
            (*fd).byte_code.as_slice(),
            if (*fd).arg_count > 0 {
                std::slice::from_raw_parts((*fd).args, (*fd).arg_count as _)
            } else {
                &[]
            },
            if (*fd).var_count > 0 {
                std::slice::from_raw_parts((*fd).vars, (*fd).var_count as _)
            } else {
                &[]
            },
            (*fd).closure_vars.as_deref().unwrap_or(&[]),
            if (*fd).cpool_count > 0 {
                std::slice::from_raw_parts((*fd).cpool, (*fd).cpool_count as _)
            } else {
                &[]
            },
            (*fd).source.cast(),
            (*fd).line_num as _,
            (*fd).label_slots,
            std::ptr::null_mut(),
        );
        println!();
    }

    // pass 2
    if resolve_variables(ctx, fd).is_err() {
        fail!();
    }

    #[cfg(feature = "dump_bytecode")]
    if (*fd).js_mode & JS_MODE_STRIP as u8 == 0 {
        println!("pass 2");
        crate::bytecode::dump_byte_code(
            ctx,
            2,
            (*fd).byte_code.as_slice(),
            if (*fd).arg_count > 0 {
                std::slice::from_raw_parts((*fd).args, (*fd).arg_count as _)
            } else {
                &[]
            },
            if (*fd).var_count > 0 {
                std::slice::from_raw_parts((*fd).vars, (*fd).var_count as _)
            } else {
                &[]
            },
            (*fd).closure_vars.as_deref().unwrap_or(&[]),
            if (*fd).cpool_count > 0 {
                std::slice::from_raw_parts((*fd).cpool, (*fd).cpool_count as _)
            } else {
                &[]
            },
            (*fd).source.cast(),
            (*fd).line_num as _,
            (*fd).label_slots,
            std::ptr::null_mut(),
        );
        println!();
    }

    // pass 3
    if resolve_labels(ctx, fd).is_err() {
        fail!();
    }

    let stack_size = if let Ok(ss) = (*fd).compute_stack_size(ctx) {
        ss
    } else {
        fail!();
    };

    // new function
    let mut b = if let Some(p) = js_malloc_typed::<JSFunctionBytecode>(ctx, 1, true) {
        Box::from_raw(p.as_ptr())
    } else {
        fail!();
    };

    b.func_name = (*fd).func_name;
    (*fd).func_name = JS_ATOM_NULL; // avoid to free func_name in js_free_function_def()

    b.header.ref_count = 1;
    b.stack_size = stack_size;
    b.has_prototype = (*fd).has_prototype;
    b.has_simple_parameter_list = (*fd).has_simple_parameter_list;
    b.js_mode = (*fd).js_mode;
    b.is_derived_class_constructor = (*fd).is_derived_class_constructor;
    b.func_kind = (*fd).func_kind;
    b.need_home_object = (*fd).home_object_var_idx.is_some() || (*fd).need_home_object;
    b.new_target_allowed = (*fd).new_target_allowed;
    b.super_call_allowed = (*fd).super_call_allowed;
    b.super_allowed = (*fd).super_allowed;
    b.arguments_allowed = (*fd).arguments_allowed;
    b.backtrace_barrier = (*fd).backtrace_barrier;
    b.is_direct_or_indirect_eval =
        matches!((*fd).eval_type, JS_EVAL_TYPE_DIRECT | JS_EVAL_TYPE_INDIRECT);
    b.realm = JS_DupContext(ctx);

    b.cpool_count = (*fd).cpool_count as _;
    b.var_count = (*fd).var_count;
    b.arg_count = (*fd).arg_count;
    b.defined_arg_count = (*fd).defined_arg_count;

    // initialize offsets to invalid
    b.cpool_offset = -1;
    b.vardefs_offset = -1;
    b.closure_var_offset = -1;

    let mut bin_size = if b.cpool_count != 0 {
        b.cpool_offset = 0;
        b.cpool_count as usize * std::mem::size_of::<JSValue>()
    } else {
        0
    };

    if (*fd).js_mode & JS_MODE_STRIP as u8 == 0 || (*fd).has_eval_call {
        let n = (*fd).arg_count + (*fd).var_count;
        if n != 0 {
            const ly: std::alloc::Layout = std::alloc::Layout::new::<JSVarDef>();
            let mut i = bin_size;
            let m = i % ly.align();
            if m != 0 {
                i += ly.align() - m;
                bin_size += ly.align() - m
            }
            b.vardefs_offset = i as i32;
            bin_size += n as usize * ly.size();
        }
    }

    let num_closures = (*fd).closure_vars.as_ref().map_or(0, |x| x.len());
    if num_closures != 0 {
        const ly: std::alloc::Layout = std::alloc::Layout::new::<JSClosureVar>();
        let mut i = bin_size;
        let m = i % ly.align();
        if m != 0 {
            i += ly.align() - m;
            bin_size += ly.align() - m
        }
        bin_size += num_closures * ly.size();
        b.closure_var_offset = i as i32;
    }

    b.bytecode_offset = bin_size as u32;
    bin_size += (*fd).byte_code.len();

    debug_assert!(bin_size > 0);

    // alloc binary content
    let mut bin = Vec::<u8>::new();
    if bin.try_reserve_exact(bin_size).is_err() {
        ctx.JS_ThrowOutOfMemory();
        fail!();
    }
    bin.set_len(bin_size);

    // 1. cpool section
    if b.cpool_offset >= 0 {
        std::ptr::copy_nonoverlapping(
            (*fd).cpool,
            bin.as_mut_ptr()
                .add(b.cpool_offset as usize)
                .cast::<JSValue>(),
            b.cpool_count as usize,
        );

        // SAFEGUARD: we have copy cpool values to function bytecode above.
        // we do cpool pointer and count clean-ups here in advance,
        // so that js_free_function_def() will not free them again.
        js_free(ctx, (*fd).cpool.cast());
        (*fd).cpool = std::ptr::null_mut();
        (*fd).cpool_count = 0;
    }

    // 2. vardef section
    if b.vardefs_offset >= 0 {
        if b.arg_count != 0 {
            std::ptr::copy_nonoverlapping(
                (*fd).args.cast::<u8>(),
                bin.as_mut_ptr().add(b.vardefs_offset as usize),
                b.arg_count as usize * std::mem::size_of::<JSVarDef>(),
            );
        }
        if b.var_count != 0 {
            std::ptr::copy_nonoverlapping(
                (*fd).vars.cast::<u8>(),
                bin.as_mut_ptr().add(
                    b.vardefs_offset as usize
                        + b.arg_count as usize * std::mem::size_of::<JSVarDef>(),
                ),
                b.var_count as usize * std::mem::size_of::<JSVarDef>(),
            );
        }

        // SAFEGUARD: we have copy arg and var definations to function bytecode above.
        // we do vardefs and *_count clean-ups here in advance,
        // so that when calling js_free_function_def() later, it will not free vardefs.
        js_free(ctx, (*fd).args.cast());
        js_free(ctx, (*fd).vars.cast());
        (*fd).args = std::ptr::null_mut();
        (*fd).vars = std::ptr::null_mut();
        (*fd).arg_count = 0;
        (*fd).var_count = 0;
    }

    // 3. closure vars section
    if b.closure_var_offset >= 0 {
        let cvars = (*fd).closure_vars.take().unwrap();
        b.closure_var_count = cvars.len() as _;

        let mut ptr = bin
            .as_mut_ptr()
            .add(b.closure_var_offset as usize)
            .cast::<JSClosureVar>();

        for cv in cvars {
            ptr.write(cv);
            ptr = ptr.add(1);
        }
    }

    // 4. bytecode section
    std::ptr::copy_nonoverlapping(
        (*fd).byte_code.as_ptr(),
        bin.as_mut_ptr().add(b.bytecode_offset as usize),
        (*fd).byte_code.len(),
    );
    // SAFEGUARD: clear bytecode in advance, so that js_free_function_def() will not free atoms in it.
    (*fd).byte_code.clear();

    debug_assert_eq!(bin.len(), bin_size);
    b.binary = bin.into_boxed_slice();

    if (*fd).js_mode & JS_MODE_STRIP as u8 == 0 {
        (*b).has_debug = true;
        (*b).debug.line_num = (*fd).line_num;
        (*b).debug.filename = (*fd).filename;
        // SAFEGUARD: to prevent free in js_free_function_def()
        (*fd).filename = JS_ATOM_NULL;

        (*b).debug.source_len = (*fd).source_len;
        (*b).debug.source = (*fd).source;
        // SAFEGUARD: to prevent free in js_free_function_def()
        (*fd).source = std::ptr::null_mut();

        // move pc2line to func object.
        if let Some(mut lst) = (*fd).pc2line.take() {
            lst.shrink_to_fit();
            (*b).debug.pc2line_buf.replace(lst);
        }
    }

    add_gc_object(
        ctx.rt_mut(),
        &mut b.header,
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE,
    );

    #[cfg(feature = "dump_bytecode")]
    if (*fd).js_mode & JS_MODE_STRIP as u8 == 0 {
        crate::bytecode::js_dump_function_bytecode(ctx, b.as_mut());
    }

    js_free_function_def(ctx, fd);

    JS_MKPTR!(JSTag::FunctionBytecode, Box::into_raw(b).cast())
}

#[deprecated(note = "use JSFunctionBytecode::find_line_num() instead")]
pub unsafe fn find_line_num(
    ctx: *const JSContext,
    b: *mut JSFunctionBytecode,
    pc_value: u32,
) -> i32 {
    (*b).find_line_num(pc_value).map_or(-1, |x| x as i32)
}

// XXX: add a specific eval mode so that Function("}), ({") is rejected
pub unsafe fn js_function_constructor(
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
        if let Ok(sv) = v.to_utf8_string(ctx.as_mut().unwrap()) {
            buf.extend_from_slice(sv.as_bytes());
        } else {
            return JSValue::EXCEPTION;
        }
    }

    buf.extend_from_slice(b") {\n");
    if n >= 0 {
        if let Ok(sv) = (*argv.offset(n as _)).to_utf8_string(ctx.as_mut().unwrap()) {
            buf.extend_from_slice(sv.as_bytes());
        } else {
            return JSValue::EXCEPTION;
        }
    }
    buf.extend_from_slice(b"\n})");

    // create script string
    let src = (*ctx).create_string_from_utf8(buf.to_vec());
    if src.is_exception() {
        return JSValue::EXCEPTION;
    }

    // eval script
    let obj = JS_EvalObject(ctx, (*ctx).global_obj, src, JS_EVAL_TYPE_INDIRECT, -1);
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
            proto = (*realm).class_proto[func_kind_to_class_id[func_kind as usize] as usize].dup();
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
