// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcNode, GcRef, GcTrace, GcTraceCtx};

use crate::{
    atom::{Atom, is_const_atom, is_small_int_atom},
    bytecode::{OpcodeFormat, opcode_info_final},
    context::{ContextImpl, ContextPtr, ScopedContext},
    error::JSResult,
    eval::EvalType,
    function::{FunctionDef, VarRef},
    gcref_wrapper,
    stack_frame::{ExecMode, StackFrame},
    utils::{get_leb128, get_sleb128, read_from_ptr},
    value::JSValue,
};

use super::{FunctionKind, closure::ClosureVar, var::JSVarDef};

#[derive(Debug)]
pub(crate) struct BytecodeDebugInfo {
    pub filename: Atom,
    pub line_num: i32,
    pub source: Option<String>,
    pub pc2line_buf: Option<Vec<u8>>,

    #[cfg(debug_assertions)]
    pub(crate) debug_setting: super::BytecodeDebugSetting,
}

impl GcTrace for BytecodeDebugInfo {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.filename.trace(tr);
    }
}

/* for the encoding of the pc2line table */
pub const PC2LINE_BASE: i32 = -1;
pub const PC2LINE_RANGE: i32 = 5;
pub const PC2LINE_OP_FIRST: i32 = 1;
pub const PC2LINE_DIFF_PC_MAX: i32 = (255 - PC2LINE_OP_FIRST) / PC2LINE_RANGE;

#[derive(Debug, Clone)]
pub(crate) struct BytecodeAttrs {
    pub(crate) func_kind: FunctionKind,
    /// true if a prototype field is necessary
    pub(crate) has_prototype: bool,
    /// 是否有简单参数列表
    pub(crate) has_simple_parameter_list: bool,
    /// 是否是派生类构造函数
    pub(crate) is_derived_class_constructor: bool,
    /// 是否允许new.target
    pub(crate) new_target_allowed: bool,
    /// 是否允许super()
    pub(crate) super_call_allowed: bool,
    /// 是否允许super.或super[]
    pub(crate) super_allowed: bool,
    /// 是否允许arguments标识符
    pub(crate) arguments_allowed: bool,
    /// stop backtrace on this function
    pub(crate) backtrace_barrier: bool,
    /// for length function property
    pub(crate) defined_arg_count: u16,
    //// true if home_object needs to be initialized
    pub(crate) need_home_object: bool,
    /// execution mode
    pub(crate) exec_mode: ExecMode,
}

impl Default for BytecodeAttrs {
    fn default() -> Self {
        Self {
            func_kind: FunctionKind::Normal,
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
            exec_mode: ExecMode::empty(),
        }
    }
}

pub struct BytecodeImpl {
    realm: ContextPtr,
    pub(crate) func_name: Atom,
    pub(crate) attrs: BytecodeAttrs,

    /// used by JS_GetScriptOrModuleName()
    pub(crate) is_direct_or_indirect_eval: bool,

    pub(crate) stack_size: u16,

    /// Precomputed `var_count + stack_size` for zero-copy arg calls.
    pub(crate) local_buf_size: u16,

    pub(crate) arg_count: u16,
    pub(crate) var_count: u16,
    pub(crate) closure_var_count: u16,

    /// binary image.
    /// SAFETY: may contains const, vararge, bytecode etc
    /// which may contain value and atom. must manually free them when dropped
    pub(crate) binary: Box<[u8]>,
    /// consts section (start_index, length)
    consts_section: Option<(u16, u16)>,
    /// start index of vardefs in binary >=0 for valid index
    vardefs_start: i32,
    /// start index of closure vars >=0 for valid index
    closure_var_start: i32,
    /// index where opcodes starts
    opcodes_start: u32,

    /// debug info
    pub(crate) debug: Option<BytecodeDebugInfo>,
}

impl GcTrace for BytecodeImpl {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.func_name.trace(tr);
        self.consts().iter().for_each(|x| x.trace(tr));
        self.var_defs().iter().for_each(|x| x.trace(tr));
        self.closure_vars().iter().for_each(|x| x.trace(tr));

        // trace atoms in opcodes
        let bc = self.opcodes();
        let bc_len = bc.len();
        let mut pos = 0;
        while pos < bc_len {
            let op = bc[pos];
            let oi = opcode_info_final(op);
            if matches!(
                oi.fmt,
                OpcodeFormat::ATOM
                    | OpcodeFormat::ATOM_U8
                    | OpcodeFormat::ATOM_U16
                    | OpcodeFormat::ATOM_LABEL_U8
                    | OpcodeFormat::ATOM_LABEL_U16
            ) {
                let id = unsafe { read_from_ptr::<u32>(bc.as_ptr().add(pos + 1)) };
                if !is_const_atom(id) && !is_small_int_atom(id) {
                    Atom::from_id(id).trace(tr);
                }
            }

            pos += oi.size as usize;
        }

        if let Some(x) = self.debug.as_ref() {
            x.trace(tr)
        }
    }
}

impl BytecodeImpl {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        ctx: &mut ContextImpl,
        fd: &FunctionDef,
        binary: Box<[u8]>,
        consts_section: Option<(u16, u16)>,
        vardefs_start: i32,
        arg_count: u16,
        var_count: u16,
        closure_var_start: i32,
        closure_var_count: u16,
        opcodes_start: u32,
        stack_size: u16,
    ) -> Self {
        Self {
            realm: ctx.gc_ref().into(),
            func_name: fd.func_name,

            attrs: BytecodeAttrs {
                need_home_object: fd.attrs.need_home_object || fd.home_object_var_idx.is_some(),
                ..fd.attrs
            },
            is_direct_or_indirect_eval: matches!(
                fd.eval_type,
                EvalType::Direct | EvalType::Indirect
            ),
            stack_size,
            local_buf_size: var_count.saturating_add(stack_size),

            binary,
            consts_section,
            vardefs_start,
            arg_count,
            var_count,
            closure_var_start,
            closure_var_count,
            opcodes_start,

            debug: Some(BytecodeDebugInfo {
                filename: Atom::NULL,
                line_num: 0,
                source: None,
                pc2line_buf: None,
                #[cfg(debug_assertions)]
                debug_setting: fd.debug,
            }),
        }
    }

    /// get bytecode realm context
    #[inline(always)]
    pub(crate) const fn realm(&self) -> ContextPtr {
        self.realm
    }

    #[inline(always)]
    pub const fn func_kind(&self) -> FunctionKind {
        self.attrs.func_kind
    }

    #[inline(always)]
    pub(crate) const fn mode(&self) -> ExecMode {
        self.attrs.exec_mode
    }

    #[inline(always)]
    pub const fn has_prototype(&self) -> bool {
        self.attrs.has_prototype
    }

    #[inline(always)]
    pub const fn need_home_object(&self) -> bool {
        self.attrs.need_home_object
    }

    /// get nth const value
    #[inline]
    pub(crate) fn load_const(&self, nth: usize) -> Option<JSValue> {
        if let Some((start, len)) = self.consts_section
            && nth < len as usize
        {
            debug_assert_eq!(start, 0);
            Some(unsafe {
                *self
                    .binary
                    .as_ptr()
                    .add(start as usize)
                    .cast::<JSValue>()
                    .add(nth)
            })
        } else {
            None
        }
    }

    /// get nth arg or local var. for example:
    /// get 2nd var def: (2, false)
    /// get 3rd arg def: (3, true)
    fn get_local_vardef(&self, nth: u16, is_arg: bool) -> Option<&JSVarDef> {
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

    /// get nth argument def
    #[inline(always)]
    pub(crate) fn get_arg(&self, nth: u16) -> Option<&JSVarDef> {
        self.get_local_vardef(nth, true)
    }

    /// get nth local var def
    #[inline(always)]
    pub(crate) fn get_var(&self, nth: u16) -> Option<&JSVarDef> {
        self.get_local_vardef(nth, false)
    }

    /// get nth closure var ref
    pub(crate) fn get_closure_var(&self, nth: u16) -> Option<&ClosureVar> {
        if self.closure_var_start >= 0 && nth < self.closure_var_count {
            unsafe {
                self.binary
                    .as_ptr()
                    .add(self.closure_var_start as usize)
                    .cast::<ClosureVar>()
                    .add(nth as usize)
                    .as_ref()
            }
        } else {
            None
        }
    }

    pub fn arguments(&self) -> &[JSVarDef] {
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

    pub fn local_vars(&self) -> &[JSVarDef] {
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

    /// arguments + local_vars.
    fn var_defs(&self) -> &[JSVarDef] {
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

    /// get closure vars slice
    pub(crate) fn closure_vars(&self) -> &[ClosureVar] {
        if self.closure_var_count > 0 {
            debug_assert!(self.closure_var_start >= 0);

            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.closure_var_start as usize)
                        .cast::<ClosureVar>(),
                    self.closure_var_count as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// get opcodes slice
    #[inline(always)]
    pub(crate) fn opcodes(&self) -> &[u8] {
        &self.binary[self.opcodes_start as usize..]
    }

    /// get const values
    pub(crate) fn consts(&self) -> &[JSValue] {
        if let Some((start, len)) = self.consts_section {
            debug_assert_eq!(start, 0);
            debug_assert!(len > 0);
            unsafe {
                std::slice::from_raw_parts(
                    self.binary.as_ptr().add(start as usize).cast::<JSValue>(),
                    len as usize,
                )
            }
        } else {
            &[]
        }
    }

    pub(crate) fn find_line_num(&self, pc_value: u32) -> Option<i32> {
        if self.debug.as_ref().is_none_or(|d| d.pc2line_buf.is_none()) {
            // function was stripped
            return None;
        }

        let debug = self.debug.as_ref().unwrap();
        let mut p = debug.pc2line_buf.as_ref().unwrap().as_ptr();
        let p_end = unsafe { p.add(debug.pc2line_buf.as_ref().map(|x| x.len()).unwrap()) };

        let mut pc = 0;
        let mut line_num = debug.line_num;

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
                        return Some(debug.line_num);
                    };
                    pc += val as i32;
                    p = p.add(ret);

                    let mut v = 0_i32;
                    // FIXME: hard coded len 5
                    let ret = if let Ok(n) = get_sleb128(&mut v, std::slice::from_raw_parts(p, 5)) {
                        n
                    } else {
                        return Some(debug.line_num);
                    };
                    p = p.add(ret);
                    new_line_num = line_num + v;
                } else {
                    op -= PC2LINE_OP_FIRST;
                    pc += op / PC2LINE_RANGE;
                    new_line_num = line_num + (op % PC2LINE_RANGE) + PC2LINE_BASE;
                }

                if pc_value < pc as u32 {
                    return Some(line_num);
                }
                line_num = new_line_num;
            }
        }

        Some(line_num)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq)]
pub struct Bytecode(GcRef<BytecodeImpl>);

gcref_wrapper!(Bytecode, BytecodeImpl);

impl From<Bytecode> for JSValue {
    #[inline(always)]
    fn from(value: Bytecode) -> Self {
        Self::Bytecode(value)
    }
}

impl std::fmt::Debug for Bytecode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Bytecode {:?}>", self.0)
    }
}

impl Bytecode {
    /// Create Function from `self` Bytecode with captured variables, then calls the function.
    /// Always executes via arena (trampoline) path — `BytecodeFunction::call` no longer
    /// has a legacy fallback. Closure creation still routes based on `ctx.is_arena_mode()`:
    /// arena mode uses `make_closure_for_arena` (resolves var refs from arena frames);
    /// legacy mode uses `make_closure` (resolves from the provided `parent_stack_frame`).
    pub(crate) fn run_as_function(
        &self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        captured_vars: &[VarRef],
        parent_stack_frame: Option<&mut StackFrame>,
    ) -> JSResult {
        if ctx.is_arena_mode() {
            let mut rt = ctx.rt_ptr();
            let parent_frame = rt.active_trampoline_frame;
            self.make_closure_for_arena(
                ctx,
                None,
                captured_vars,
                rt.stack_frame_arena_mut(),
                parent_frame,
            )?
            .call(ctx, this_obj, &[])
        } else {
            let f = self.make_closure(ctx, None, captured_vars, parent_stack_frame)?;
            f.call(ctx, this_obj, &[])
        }
    }
}
