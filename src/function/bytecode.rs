use std::ptr::NonNull;

use smallvec::SmallVec;

use crate::{
    atom::{consts::JS_ATOM_prototype, JSAtom, JS_FreeAtomRT, JS_ATOM_NULL},
    bytecode::{opcode_info_final, opcode_info_intermediate},
    context::{JSContext, JS_FreeContext},
    eval::{JS_EvalObject, JS_EVAL_TYPE_INDIRECT},
    function::{func_kind_to_class_id, JS_GetFunctionRealm},
    gc::{remove_gc_object, JSGCObjectHeader, JSGCPhaseEnum},
    mem::js_free_rt,
    runtime::JSRuntime,
    types::list_add_tail,
    utils::{get_leb128, get_sleb128, get_u32},
    value::{object::JS_SetPrototypeInternal, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT},
};

use super::{clousure::JSClosureVar, var::JSVarDef, JSFunctionKindEnum};

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
