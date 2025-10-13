use std::{
    cell::{Ref, RefCell, RefMut},
    ops::{Deref, DerefMut},
    os::raw::c_void,
    ptr::NonNull,
    rc::{Rc, Weak},
};

#[cfg(debug_assertions)]
use crate::function::BytecodeDebugSetting;
use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtomRT, JSAtom},
    bytecode::{OpcodeFormat, opcode_info_final, opcode_info_intermediate},
    context::{JSContext, JSContextWeakref},
    error::JSResult,
    eval::{JS_EVAL_TYPE_DIRECT, JS_EVAL_TYPE_INDIRECT},
    function::{JSFunctionDef, JSVarRef},
    gc::GCObjectHeader,
    object::GCObject,
    runtime::JSRuntime,
    stack_frame::StackFrame,
    utils::{get_leb128, get_sleb128, read_from_ptr},
    value::JSValue,
};

use super::{JSFunctionKindEnum, closure::JSClosureVar, var::JSVarDef};

#[derive(Debug)]
pub struct BytecodeDebugInfo {
    pub filename: JSAtom,
    pub line_num: i32,
    pub source: Option<String>,
    pub pc2line_buf: Option<Vec<u8>>,

    #[cfg(debug_assertions)]
    pub(crate) debug_setting: BytecodeDebugSetting,
}

impl Default for BytecodeDebugInfo {
    fn default() -> Self {
        Self {
            filename: JS_ATOM_NULL,
            line_num: 0,
            source: None,
            pc2line_buf: None,

            #[cfg(debug_assertions)]
            debug_setting: BytecodeDebugSetting::default(),
        }
    }
}

/* for the encoding of the pc2line table */
pub const PC2LINE_BASE: i32 = -1;
pub const PC2LINE_RANGE: i32 = 5;
pub const PC2LINE_OP_FIRST: i32 = 1;
pub const PC2LINE_DIFF_PC_MAX: i32 = (255 - PC2LINE_OP_FIRST) / PC2LINE_RANGE;

#[derive(Debug, Clone)]
pub(crate) struct BytecodeAttrs {
    pub func_kind: JSFunctionKindEnum,
    /// true if a prototype field is necessary
    pub has_prototype: bool,
    /// 是否有简单参数列表
    pub has_simple_parameter_list: bool,
    /// 是否是派生类构造函数
    pub is_derived_class_constructor: bool,
    /// 是否允许new.target
    pub new_target_allowed: bool,
    /// 是否允许super()
    pub super_call_allowed: bool,
    /// 是否允许super.或super[]
    pub super_allowed: bool,
    /// 是否允许arguments标识符
    pub arguments_allowed: bool,
    /// stop backtrace on this function
    pub backtrace_barrier: bool,
    /// for length function property
    pub defined_arg_count: u16,
    //// true if home_object needs to be initialized
    pub need_home_object: bool,
    /// execution mode
    pub js_mode: u8,
}

impl Default for BytecodeAttrs {
    fn default() -> Self {
        Self {
            func_kind: JSFunctionKindEnum::JS_FUNC_NORMAL,
            has_prototype: false,
            has_simple_parameter_list: false,
            is_derived_class_constructor: false,
            new_target_allowed: false,
            super_call_allowed: false,
            super_allowed: false,
            arguments_allowed: false,
            backtrace_barrier: false,
            defined_arg_count: 0,
            need_home_object: false,
            js_mode: 0,
        }
    }
}

#[derive(Debug)]
pub(crate) struct BytecodeInner {
    pub header: GCObjectHeader,

    pub func_name: JSAtom,
    /// function realm
    realm: JSContextWeakref,

    pub(crate) attrs: BytecodeAttrs,

    pub read_only_bytecode: bool,
    /// used by JS_GetScriptOrModuleName()
    pub is_direct_or_indirect_eval: bool,

    pub stack_size: u16,

    pub cpool_count: u16,
    pub arg_count: u16,
    pub var_count: u16,
    pub closure_var_count: u16,

    /// binary image
    pub binary: Box<[u8]>,
    /// start index of cpool in binary >=0 for valid index
    pub cpool_start: i32,
    /// start index of vardefs in binary >=0 for valid index
    pub vardefs_start: i32,
    /// start index of closure vars >=0 for valid index
    pub closure_var_start: i32,
    /// index where opcodes starts
    pub opcodes_start: u32,

    // TODO: use Option<JSFunctionBytecodeDebug>>
    pub debug: BytecodeDebugInfo,
    pub has_debug: bool,
}

impl Drop for BytecodeInner {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl BytecodeInner {
    pub(super) fn new(
        ctx: &JSContext,
        fd: &JSFunctionDef,
        binary: Box<[u8]>,
        cpool_start: i32,
        cpool_count: u16,
        vardefs_start: i32,
        arg_count: u16,
        var_count: u16,
        closure_var_start: i32,
        closure_var_count: u16,
        opcodes_start: u32,
        stack_size: u16,
    ) -> Self {
        let inner = Self {
            header: GCObjectHeader::new(ctx.rt_mut(), 1),
            func_name: fd.func_name,
            realm: ctx.downgrade(),

            attrs: BytecodeAttrs {
                need_home_object: fd.attrs.need_home_object || fd.home_object_var_idx.is_some(),
                ..fd.attrs
            },
            // js_mode: fd.js_mode,
            // has_prototype: fd.has_prototype,
            // has_simple_parameter_list: fd.has_simple_parameter_list,
            // is_derived_class_constructor: fd.is_derived_class_constructor,
            // func_kind: fd.func_kind,
            // new_target_allowed: fd.new_target_allowed,
            // super_call_allowed: fd.super_call_allowed,
            // super_allowed: fd.super_allowed,
            // arguments_allowed: fd.arguments_allowed,
            // backtrace_barrier: fd.backtrace_barrier,
            // defined_arg_count: fd.defined_arg_count,
            is_direct_or_indirect_eval: matches!(
                fd.eval_type,
                JS_EVAL_TYPE_DIRECT | JS_EVAL_TYPE_INDIRECT
            ),

            read_only_bytecode: false,
            stack_size,

            has_debug: false,
            debug: BytecodeDebugInfo {
                #[cfg(debug_assertions)]
                debug_setting: fd.debug,
                ..Default::default()
            },

            binary,
            cpool_start,
            cpool_count,
            vardefs_start,
            arg_count,
            var_count,
            closure_var_start,
            closure_var_count,
            opcodes_start,
        };

        inner
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub fn attrs(&self) -> &BytecodeAttrs {
        &self.attrs
    }

    #[inline(always)]
    pub fn func_kind(&self) -> JSFunctionKindEnum {
        self.attrs.func_kind
    }

    #[inline(always)]
    pub fn mode(&self) -> u8 {
        self.attrs.js_mode
    }

    #[inline(always)]
    pub fn has_prototype(&self) -> bool {
        self.attrs.has_prototype
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

    /// get opcodes slice
    #[inline(always)]
    pub fn opcodes(&self) -> &[u8] {
        &self.binary[self.opcodes_start as usize..]
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

    pub fn find_line_num(&self, pc_value: u32) -> Option<i32> {
        if !self.has_debug || self.debug.pc2line_buf.is_none() {
            // function was stripped
            return None;
        }

        let mut p = self.debug.pc2line_buf.as_ref().unwrap().as_ptr();
        let p_end =
            unsafe { p.offset(self.debug.pc2line_buf.as_ref().map(|x| x.len()).unwrap() as isize) };

        let mut pc = 0;
        let mut line_num = self.debug.line_num;

        while p < p_end {
            unsafe {
                let mut op = *p as i32;
                p = p.offset(1);

                let new_line_num: i32;

                if op == 0 {
                    let mut val = 0;
                    // FIXME: hard coded len 5
                    let ret = if let Ok(n) = get_leb128(&mut val, std::slice::from_raw_parts(p, 5))
                    {
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
        }

        Some(line_num)
    }

    fn finalize(&mut self) {
        let rt = self.header.rt_ptr();

        free_bytecode_atoms(
            rt.as_ptr(),
            &self.binary[self.opcodes_start as usize..],
            true,
        );

        // free vardef var_name atom
        for vd in self.vardefs_slice() {
            JS_FreeAtomRT(rt.as_ptr(), vd.var_name);
        }

        // free closure vars atom
        for cv in self.closure_vars_slice() {
            JS_FreeAtomRT(rt.as_ptr(), cv.var_name);
        }

        // free func name atom
        JS_FreeAtomRT(rt.as_ptr(), self.func_name);
        self.func_name = JS_ATOM_NULL;

        if self.has_debug {
            JS_FreeAtomRT(rt.as_ptr(), self.debug.filename);
            self.debug.filename = JS_ATOM_NULL;
        }
    }
}

#[repr(transparent)]
#[derive(Clone)]
pub struct Bytecode(pub(crate) Rc<RefCell<BytecodeInner>>);

impl std::fmt::Debug for Bytecode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Bytecode {:p}>", self.id() as *const c_void)
    }
}

impl From<Bytecode> for GCObject {
    fn from(value: Bytecode) -> Self {
        Self::FunctionBytecode(value)
    }
}

impl Bytecode {
    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    #[inline(always)]
    pub fn rt(&self) -> &JSRuntime {
        unsafe { self.0.borrow().header.rt_ptr().as_ref() }
    }

    #[inline(always)]
    pub fn func_kind(&self) -> JSFunctionKindEnum {
        self.0.borrow().func_kind()
    }

    #[inline(always)]
    pub fn mode(&self) -> u8 {
        self.0.borrow().mode()
    }

    #[inline(always)]
    pub fn has_prototype(&self) -> bool {
        self.0.borrow().attrs.has_prototype
    }

    /// get bytecode realm
    pub fn realm(&self) -> Option<JSContext> {
        self.0.borrow().realm.upgrade()
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&BytecodeInner) -> R) -> R {
        f(self.0.borrow().deref())
    }

    pub(crate) fn with_mut<R>(&self, f: impl FnOnce(&mut BytecodeInner) -> R) -> R {
        f(self.0.borrow_mut().deref_mut())
    }

    pub fn downgrade(&self) -> BytecodeWeakref {
        BytecodeWeakref(Rc::downgrade(&self.0))
    }

    #[inline(always)]
    pub fn need_home_object(&self) -> bool {
        self.0.borrow().attrs.need_home_object
    }

    /// get const pool values slice
    pub fn cpools_slice(&self) -> &[JSValue] {
        let r = self.0.borrow();
        if r.cpool_count > 0 {
            unsafe {
                std::slice::from_raw_parts(
                    r.binary
                        .as_ptr()
                        .add(r.cpool_start as usize)
                        .cast::<JSValue>(),
                    r.cpool_count as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// get a value by index in cpool
    #[inline(always)]
    pub fn load_const(&self, idx: usize) -> Option<JSValue> {
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

    /// get opcodes slice
    #[inline(always)]
    pub fn opcodes(&self) -> Ref<'_, [u8]> {
        Ref::map(self.0.borrow(), |b| b.opcodes())
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
        self.0.borrow().find_line_num(pc_value)
    }

    /// Evaluate `self` Bytecode.
    ///
    /// Internally, it will create a function with the bytecode, then eval the function.
    pub fn eval(
        &self,
        ctx: &JSContext,
        this_obj: &JSValue,
        var_refs: &[JSVarRef],
        parent_frame: Option<&mut StackFrame>,
    ) -> JSResult {
        self.make_closure(ctx, var_refs, parent_frame, None)?
            .call(ctx, &this_obj, &[])
    }

    pub fn gc_has_mark(&self) -> bool {
        self.0.borrow().header.gc_has_mark()
    }

    pub fn gc_set_mark(&self, b: bool) {
        self.0.borrow_mut().header.gc_set_mark(b)
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14x} [function bytecode]", self.id());
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct BytecodeWeakref(Weak<RefCell<BytecodeInner>>);

impl BytecodeWeakref {
    pub fn upgrade(&self) -> Option<Bytecode> {
        self.0.upgrade().map(Bytecode)
    }
}

/// free atom in bytecode
pub(super) fn free_bytecode_atoms(rt: *mut JSRuntime, bc: &[u8], is_short_opcodes: bool) {
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
            OpcodeFormat::ATOM
                | OpcodeFormat::ATOM_U8
                | OpcodeFormat::ATOM_U16
                | OpcodeFormat::ATOM_LABEL_U8
                | OpcodeFormat::ATOM_LABEL_U16
        ) {
            unsafe {
                let atom = JSAtom(read_from_ptr::<u32>(bc.as_ptr().add(pos as usize + 1)));
                JS_FreeAtomRT(rt, atom);
            }
        }

        pos += oi.size as usize;
    }
}
