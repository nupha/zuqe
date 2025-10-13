use std::{marker::PhantomData, ops::DerefMut, ptr::NonNull};

use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JS_FreeAtomRT, JSAtom, consts::*},
    bytecode::{OpCode, OpCodeTemp, OpcodeFormat, opcode_info_intermediate, opcodes},
    context::{JS_MODE_STRIP, JSContext},
    eval::JS_EVAL_TYPE_GLOBAL,
    function::{
        DEFINE_GLOBAL_LEX_VAR, JSClosureVar, JSFunctionKindEnum, JSGlobalVar, JSVarDef, JSVarScope,
        free_bytecode_atoms,
    },
    mem::js_malloc_typed,
    module::ModuleDef,
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
    parse::JSParseFunctionEnum,
    utils::{push_vec_u8, push_vec_u16, push_vec_u32, read_from_ptr},
    value::JSValue,
};

use super::Label;

#[cfg(debug_assertions)]
#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct BytecodeDebugSetting {
    #[cfg(feature = "dump_bytecode")]
    /// 0: disabled, 1: dump instruction, 2: dump instruction and stack
    pub dump_run: u8,
}

#[derive(Debug)]
pub struct JSFunctionDef {
    pub ctx: JSContext,

    /// parent function def, null for root fd.
    pub parent: *mut JSFunctionDef,
    /// scope level of parent fd
    pub parent_scope_level: i32,
    /// 在父级常量池中的索引，-1表示无
    /// TODO: use Option<u16>
    pub parent_cpool_idx: i32,

    /// list of children function defs
    pub child_list: Vec<NonNull<JSFunctionDef>>,

    // TODO: use Option<EvalType>
    /// 是否是eval代码
    pub is_eval: bool,
    /// 仅当is_eval=true时有效
    pub eval_type: u32,

    /// 变量是否未在本地定义
    pub is_global_var: bool,
    /// 是否是函数表达式
    pub is_func_expr: bool,
    /// 是否拥有home object
    pub has_home_object: bool,
    /// 是否需要prototype字段
    pub has_prototype: bool,
    /// 是否有简单参数列表
    pub has_simple_parameter_list: bool,
    /// 是否需要创建参数作用域
    pub has_parameter_expressions: bool,
    /// 严格模式指令
    pub has_use_strict: bool,
    /// 是否包含eval调用
    pub has_eval_call: bool,
    /// 是否可访问arguments绑定
    pub has_arguments_binding: bool,
    /// can access `this` binding?
    pub has_this_binding: bool,
    /// 是否允许new.target
    pub new_target_allowed: bool,
    /// 是否允许super()
    pub super_call_allowed: bool,
    /// 是否允许super.或super[]
    pub super_allowed: bool,
    /// 是否允许arguments标识符
    pub arguments_allowed: bool,
    /// 是否是派生类构造函数
    pub is_derived_class_constructor: bool,
    /// 是否在函数体内
    pub in_function_body: bool,
    /// 回溯屏障
    pub backtrace_barrier: bool,

    /// 函数类型枚举
    pub func_kind: JSFunctionKindEnum,
    pub func_type: JSParseFunctionEnum,
    pub func_name: JSAtom,

    pub js_mode: u8,

    /// constants pool (string, function, number）.
    pub cpool: Vec<JSValue>,
    /// vars defination list
    pub vars: Vec<JSVarDef>,
    /// arguments defination list
    pub args: Vec<JSVarDef>,
    /// global vars defination list.
    pub global_vars: Vec<JSGlobalVar>,

    /// for length function property
    pub defined_arg_count: u16,
    // 当前函数的变量索引
    pub func_var_idx: Option<u16>,

    /// closure vars list
    pub closure_vars: Option<Vec<JSClosureVar>>,

    //
    // pseudo vars
    //
    /// `this`
    pub this_var_idx: Option<u16>,
    /// `new.target`
    pub new_target_var_idx: Option<u16>,
    /// `this.active_func`
    pub this_active_func_var_idx: Option<u16>,
    /// `home`
    pub home_object_var_idx: Option<u16>,
    /// `arguments` variable. could be Arguments or MappedArguments
    pub arguments_var_idx: Option<u16>,
    /// `arguments` in arg section
    pub arguments_arg_idx: Option<u16>,
    /// `<ret>`
    pub eval_ret_idx: Option<u16>,
    /// `<arg_var>`
    pub arg_var_object_idx: Option<u16>,
    /// `<var>`
    pub var_object_idx: Option<u16>,

    pub need_home_object: bool,

    /// scopes stack
    pub scopes: Vec<JSVarScope>,
    /// scope level of self function def
    pub scope_level: i32,
    /// the starting index to retrive chain of variable defs since this scope.
    pub scope_first: i32,

    /// scope for function body, should be 1 after setup.
    pub body_scope: i32,

    /// 字节码缓冲区
    pub byte_code: Vec<u8>,
    /// 最后操作码位置. -1 if no last opcode
    pub last_opcode_pos: i32,
    /// 最后操作码行号
    pub last_opcode_line_num: i32,
    /// 是否使用短操作码
    pub use_short_opcodes: bool,

    pub label_slots: Vec<Label>,

    /// raw pointer to top break entry chain.
    /// this pointer does not take ownership of block env.
    pub top_break: *mut BlockEnv,

    /// jump slots capacity
    #[cfg(feature = "short_opcode")]
    pub jump_slots_cap: u32,
    #[cfg(feature = "short_opcode")]
    pub jump_slots: Vec<JumpSlot>,

    /// line number capacity
    pub line_number_slots_cap: usize,
    /// line number slots
    pub line_number_slots: Vec<LineNumberSlot>,

    /// last line number
    pub line_number_last: i32,
    /// the PC associated to last line number
    pub line_number_last_pc: u32,

    /// file name
    pub filename: JSAtom,
    /// source code (utf-8 encoded)
    pub source: Option<String>,
    /// current line number
    pub line_num: i32,
    /// pc到行号的映射表
    pub pc2line: Option<Vec<u8>>,

    /// 模块定义（解析模块时非空）
    pub module: Option<ModuleDef>,

    /// 是否包含await（用于模块eval）
    pub has_await: bool,

    #[cfg(debug_assertions)]
    pub(crate) debug: BytecodeDebugSetting,
}

impl Drop for JSFunctionDef {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl JSFunctionDef {
    /// alloc a function def, and initialize it.
    pub(crate) fn new0(
        ctx: &JSContext,
        is_eval: bool,
        is_func_expr: bool,
        filename: &str,
        line_num: i32,
    ) -> Result<Box<Self>, i32> {
        let o = JSFunctionDef {
            ctx: ctx.clone(),
            parent: std::ptr::null_mut(),
            parent_cpool_idx: -1,
            parent_scope_level: 0,
            child_list: Vec::with_capacity(16),
            is_eval,
            eval_type: 0,
            is_global_var: false,
            is_func_expr,
            has_home_object: false,
            has_prototype: false,
            has_simple_parameter_list: false,
            has_parameter_expressions: false,
            has_use_strict: false,
            has_eval_call: false,
            has_arguments_binding: false,
            has_this_binding: false,
            new_target_allowed: false,
            super_call_allowed: false,
            super_allowed: false,
            arguments_allowed: false,
            is_derived_class_constructor: false,
            in_function_body: false,
            backtrace_barrier: false,
            func_kind: JSFunctionKindEnum::JS_FUNC_NORMAL,
            func_type: JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
            func_name: JS_ATOM_NULL,
            js_mode: 0,

            cpool: Vec::new(),
            vars: Vec::new(),
            args: Vec::new(),
            global_vars: Vec::new(),

            defined_arg_count: 0,
            func_var_idx: None,
            this_var_idx: None,
            new_target_var_idx: None,
            this_active_func_var_idx: None,
            home_object_var_idx: None,
            arguments_var_idx: None,
            arguments_arg_idx: None,
            eval_ret_idx: None,
            arg_var_object_idx: None,
            var_object_idx: None,
            need_home_object: false,
            scope_level: 0,
            scope_first: -1,
            scopes: {
                // scope stack: initialized with one default scope 0
                let mut arr = Vec::<JSVarScope>::new();
                arr.try_reserve_exact(4).map_err(|_| -1)?;
                arr.push(JSVarScope {
                    first: -1,
                    parent: -1,
                });
                arr
            },
            body_scope: -1,

            byte_code: Vec::with_capacity(128),
            last_opcode_pos: -1,
            last_opcode_line_num: line_num,
            use_short_opcodes: false,

            label_slots: Vec::new(),

            top_break: std::ptr::null_mut(),
            closure_vars: None,

            #[cfg(feature = "short_opcode")]
            jump_slots: Vec::new(),
            #[cfg(feature = "short_opcode")]
            jump_slots_cap: 0,

            line_number_slots: Vec::new(),
            line_number_slots_cap: 0,
            line_number_last: 0,
            line_number_last_pc: 0,

            filename: JSAtom::new(ctx.rt_mut(), filename)?,
            line_num,
            pc2line: None,
            source: None,

            module: None,
            has_await: false,

            #[cfg(debug_assertions)]
            debug: BytecodeDebugSetting::default(),
        };

        let fd = unsafe {
            let p = js_malloc_typed::<JSFunctionDef>(ctx, 1, true).ok_or(-1)?;
            p.write(o);
            Box::from_raw(p.as_ptr())
        };

        Ok(fd)
    }

    /// create function def and attach to parent-child chain
    pub fn new(
        ctx: &JSContext,
        parent: *mut JSFunctionDef,
        is_eval: bool,
        is_func_expr: bool,
        filename: &str,
        line_num: i32,
    ) -> Result<Box<Self>, i32> {
        debug_assert!(line_num >= 0);

        let mut fd = Self::new0(ctx, is_eval, is_func_expr, filename, line_num)?;

        if !parent.is_null() {
            unsafe {
                fd.parent = parent;
                fd.parent_scope_level = (*parent).scope_level;
                fd.js_mode = (*parent).js_mode;

                if (*parent).child_list.len() == (*parent).child_list.capacity() {
                    (*parent).child_list.try_reserve(8).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                }
                (*parent).child_list.push(NonNull::from(fd.deref_mut()));
            }
        }

        Ok(fd)
    }

    /// update label ref count by delta. returns updated ref count.
    pub(crate) fn update_label_ref_count(&mut self, label: u32, delta: i32) -> i32 {
        debug_assert!(
            label < self.label_slots.len() as u32,
            "label={label}, count={}",
            self.label_slots.len()
        );

        let lb = self.label_slots.get_mut(label as usize).unwrap();
        lb.ref_count += delta;
        debug_assert!(lb.ref_count >= 0);

        lb.ref_count
    }

    /// the pc2line table gives a line number for each PC value
    pub fn add_pc2line_info(&mut self, pc: u32, line_num: i32) {
        if self.line_number_slots.len() < self.line_number_slots_cap as usize
            && pc >= self.line_number_last_pc
            && line_num != self.line_number_last
        {
            self.line_number_slots.push(LineNumberSlot { pc, line_num });
            self.line_number_last_pc = pc;
            self.line_number_last = line_num;
        }
    }

    #[cfg(feature = "dump_bytecode")]
    pub fn dump_bytecode(&self, ctx: &JSContext, pass: i32) {
        if self.js_mode & JS_MODE_STRIP as u8 == 0 {
            println!("pass {pass}");

            crate::bytecode::dump_opcodes(
                ctx,
                pass,
                self.byte_code.as_slice(),
                self.args.as_slice(),
                self.vars.as_slice(),
                self.closure_vars.as_deref().unwrap_or(&[]),
                self.cpool.as_slice(),
                self.source.as_deref(),
                self.line_num as _,
                self.label_slots.as_slice(),
                None,
            );

            println!();
        }
    }

    fn finalize(&mut self) {
        // free child function defs
        for sub in self.child_list.drain(..) {
            drop(unsafe { Box::from_raw(sub.as_ptr()) });
        }

        let rt = self.ctx.rt_mut();

        free_bytecode_atoms(rt, &self.byte_code, self.use_short_opcodes);
        self.byte_code.clear();

        // #[cfg(feature = "short_opcode")]
        // self.jump_slots.clear();

        // self.label_slots.clear();
        // self.line_number_slots.clear();

        self.cpool.clear();

        //
        // if vardefs are not consumed, free atoms in it.
        //
        for mut vd in self.args.drain(..) {
            JS_FreeAtomRT(rt, vd.var_name);
            #[cfg(debug_assertions)]
            {
                vd.var_name = JS_ATOM_NULL;
            }
        }
        for mut vd in self.vars.drain(..) {
            JS_FreeAtomRT(rt, vd.var_name);
            #[cfg(debug_assertions)]
            {
                vd.var_name = JS_ATOM_NULL;
            }
        }

        for gv in self.global_vars.drain(..) {
            JS_FreeAtomRT(rt, gv.var_name);
        }

        if let Some(mut lst) = self.closure_vars.take() {
            for cv in lst.drain(..) {
                JS_FreeAtomRT(rt, cv.var_name);
            }
        }

        JS_FreeAtomRT(rt, self.func_name);
        JS_FreeAtomRT(rt, self.filename);

        // ensure `self` function def does not exist in parent's child_list
        if let Some(up) = unsafe { self.parent.as_mut() } {
            up.child_list.retain(|f| !std::ptr::eq(f.as_ptr(), self));
        }

        // check parent's child_list
        // #[cfg(debug_assertions)]
        // if let Some(up) = unsafe { self.parent.as_ref() } {
        //     debug_assert!(
        //         !up.child_list.iter().any(|f| std::ptr::eq(f.as_ptr(), self)),
        //         "the dropped function def must have been removed from its parent's child_list"
        //     );
        // }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LineNumberSlot {
    pub pc: u32,
    pub line_num: i32,
}

pub struct BlockEnv {
    pub prev: *mut BlockEnv,

    /// JS_ATOM_NULL for none.
    /// this field don't require ownership of atom,
    /// do not clone the atom for this field.
    pub label_name: JSAtom,

    /// -1 if none
    pub label_break: i32,
    /// -1 if none
    pub label_cont: i32,
    /// -1 if none
    pub label_finally: i32,

    /// number of stack elements to drop
    pub drop_count: i32,
    pub scope_level: i32,
    pub has_iterator: bool,
}

impl Default for BlockEnv {
    fn default() -> Self {
        Self {
            prev: std::ptr::null_mut(),
            label_name: JS_ATOM_NULL,

            label_break: -1,
            label_cont: -1,
            label_finally: -1,

            drop_count: 0,
            scope_level: 0,
            has_iterator: false,
        }
    }
}

#[cfg(feature = "short_opcode")]
#[derive(Debug, Clone, Copy)]
pub struct JumpSlot {
    pub op: i32,
    pub size: i32,
    pub pos: i32,
    pub label: i32,
}

pub fn get_label_pos(s: &JSFunctionDef, mut label: i32) -> i32 {
    let mut pos: i32 = 0;

    for _ in 0..20 {
        pos = s.label_slots[label as usize].pos;
        loop {
            match s.byte_code[pos as usize] {
                opcodes::op_line_num | opcodes::op_label => {
                    pos += 5;
                    continue;
                }
                opcodes::OP_goto => {
                    label =
                        unsafe { read_from_ptr::<i32>(s.byte_code.as_ptr().add(pos as usize + 1)) };
                }
                _ => {
                    return pos;
                }
            }
            break;
        }
    }

    pos
}

pub fn skip_dead_code(
    s: &mut JSFunctionDef,
    bc_buf: *const u8,
    bc_len: usize,
    pos: usize,
    linep: *mut i32,
) -> usize {
    let mut current_pos = pos;

    while current_pos < bc_len {
        let op = unsafe { *bc_buf.add(current_pos as usize) };
        let oi = opcode_info_intermediate(op);
        let len = oi.size;

        if op == opcodes::op_line_num {
            unsafe {
                *linep = read_from_ptr::<i32>(bc_buf.add(current_pos as usize + 1));
            }
        } else if op == opcodes::op_label {
            unsafe {
                let label = read_from_ptr::<u32>(bc_buf.add(current_pos as usize + 1));
                if s.update_label_ref_count(label, 0) > 0 {
                    break;
                }

                debug_assert!(s.label_slots[label as usize].pending_relocs.is_empty());
            }

            // if false {
            //     if !(*s)
            //         .label_slots
            //         .offset(label as isize)
            //         .first_reloc
            //         .is_null()
            //     {
            //         println!(
            //             "line {}: unreferenced label {}:{} has relocations",
            //             *linep,
            //             label,
            //             (*(*s).label_slots.offset(label as isize)).pos2
            //         );
            //     }
            // }
        } else {
            // XXX: output a warning for unreachable code?
            match oi.fmt {
                OpcodeFormat::LABEL | OpcodeFormat::LABEL_U16 => unsafe {
                    let label = read_from_ptr::<u32>(bc_buf.add(current_pos + 1));
                    s.update_label_ref_count(label, -1);
                },
                f @ (OpcodeFormat::ATOM_LABEL_U8
                | OpcodeFormat::ATOM_LABEL_U16
                | OpcodeFormat::ATOM
                | OpcodeFormat::ATOM_U8
                | OpcodeFormat::ATOM_U16) => unsafe {
                    if matches!(
                        f,
                        OpcodeFormat::ATOM_LABEL_U8 | OpcodeFormat::ATOM_LABEL_U16
                    ) {
                        let label = read_from_ptr::<u32>(bc_buf.add(current_pos + 5));
                        s.update_label_ref_count(label, -1);
                    }

                    let atom = JSAtom(read_from_ptr::<u32>(bc_buf.add(current_pos + 1)));
                    JS_FreeAtom(&s.ctx, atom);
                },
                _ => {}
            }
        }

        current_pos += len as usize;
    }

    current_pos
}

pub fn get_loc_or_ref(bc: &mut Vec<u8>, is_ref: bool, idx: u16) {
    // if the field is not initialized, the error is catched when accessing it
    push_vec_u8(
        bc,
        if is_ref {
            opcodes::OP_get_var_ref
        } else {
            opcodes::OP_get_loc
        },
    );
    push_vec_u16(bc, idx);
}

pub unsafe fn instantiate_hoisted_definitions(
    ctx: &JSContext,
    s: &mut JSFunctionDef,
    bc: &mut Vec<u8>,
) {
    // add the hoisted functions in arguments and local variables
    for (i, vd) in s.args.iter().enumerate() {
        if vd.func_pool_idx >= 0 {
            push_vec_u8(bc, OpCode::FCLOSURE as u8);
            push_vec_u32(bc, vd.func_pool_idx as u32);

            push_vec_u8(bc, OpCode::PUT_ARG as u8);
            push_vec_u16(bc, i as u16);
        }
    }

    for (i, vd) in s.vars.iter().enumerate() {
        if vd.scope_level == 0 && vd.func_pool_idx >= 0 {
            push_vec_u8(bc, OpCode::FCLOSURE as u8);
            push_vec_u32(bc, vd.func_pool_idx as u32);

            push_vec_u8(bc, OpCode::PUT_LOC as u8);
            push_vec_u16(bc, i as _);
        }
    }

    let mut label_next: Option<u32> = None;

    // the module global variables must be initialized before
    // evaluating the module so that the exported functions are
    // visible if there are cyclic module references
    if s.module.is_some() {
        let l = s
            .add_label()
            .expect("TODO: unhandled create label exception");
        label_next.replace(l);

        // if 'this' is true, initialize the global variables and return
        push_vec_u8(bc, OpCode::PUSH_THIS as u8);
        push_vec_u8(bc, OpCode::IF_FALSE as u8);
        push_vec_u32(bc, l);

        s.update_label_ref_count(l, 1);

        #[cfg(feature = "short_opcode")]
        {
            s.jump_slots_cap += 1;
        }
    }

    // add the global variables (only happens if JSFunctionDef::is_global_var is true)
    for hf in s.global_vars.iter() {
        let mut has_closure: i32 = 0;
        let mut force_init = hf.force_init();
        let mut idx = 0;

        // we are in an eval, so the closure contains all the enclosing variables.
        // If the outer function has a variable environment, create a property for the variable there
        if let Some(cvars) = s.closure_vars.as_ref() {
            for (j, cv) in cvars.iter().enumerate() {
                idx = j;
                match cv.var_name {
                    n if n == hf.var_name => {
                        has_closure = 2;
                        force_init = false;
                        break;
                    }
                    JS_ATOM__var_ | JS_ATOM__arg_var_ => {
                        push_vec_u8(bc, OpCode::GET_VAR_REF as u8);
                        push_vec_u16(bc, j as u16);
                        has_closure = 1;
                        force_init = true;
                        break;
                    }
                    _ => {}
                }
            }
        }

        if has_closure == 0 {
            let mut flags: u32 = 0;
            if s.eval_type != JS_EVAL_TYPE_GLOBAL {
                flags |= JS_PROP_CONFIGURABLE;
            }

            if hf.cpool_idx >= 0 && !hf.is_lexical() {
                // global function definitions need a specific handling
                push_vec_u8(bc, OpCode::FCLOSURE as u8);
                push_vec_u32(bc, hf.cpool_idx as u32);

                push_vec_u8(bc, OpCode::DEFINE_FUNC as u8);
                push_vec_u32(bc, hf.var_name.0);
                push_vec_u8(bc, flags as u8);

                continue; // next global var
            } else {
                if hf.is_lexical() {
                    flags |= DEFINE_GLOBAL_LEX_VAR as u32;
                    if !hf.is_const() {
                        flags |= JS_PROP_WRITABLE;
                    }
                }
                push_vec_u8(bc, OpCode::DEFINE_VAR as u8);
                push_vec_u32(bc, hf.var_name.dup(ctx.rt()).0); // dup to compensate later free
                push_vec_u8(bc, flags as u8);
            }
        }

        if hf.cpool_idx >= 0 || force_init {
            if hf.cpool_idx >= 0 {
                push_vec_u8(bc, OpCode::FCLOSURE as u8);
                push_vec_u32(bc, (*hf).cpool_idx as u32);
                if hf.var_name == JS_ATOM__default_ {
                    // set default export function name
                    push_vec_u8(bc, OpCode::SET_NAME as u8);
                    push_vec_u32(bc, JS_ATOM_default.dup(ctx.rt()).0);
                }
            } else {
                push_vec_u8(bc, OpCode::UNDEFINED as u8);
            }

            if has_closure == 2 {
                push_vec_u8(bc, OpCode::PUT_VAR_REF as u8);
                push_vec_u16(bc, idx as u16);
            } else if has_closure == 1 {
                push_vec_u8(bc, OpCode::DEFINE_FIELD as u8);
                push_vec_u32(bc, hf.var_name.dup(ctx.rt()).0);
                push_vec_u8(bc, OpCode::DROP as u8);
            } else {
                // XXX: Check if variable is writable and enumerable
                push_vec_u8(bc, OpCode::PUT_VAR as u8);
                push_vec_u32(bc, hf.var_name.dup(ctx.rt()).0);
            }
        }

        // done_global_var:
        JS_FreeAtom(ctx, hf.var_name);
    }

    if s.module.is_some() {
        let l = label_next.unwrap();
        push_vec_u8(bc, OpCode::RETURN_UNDEF as u8);
        push_vec_u8(bc, OpCodeTemp::LABEL as u8);
        push_vec_u32(bc, l);
        s.label_slots[l as usize].pos2 = bc.len() as i32;
    }

    s.global_vars.clear();
}

pub struct FunctionDefParentIter<'a> {
    cur_fd: *const JSFunctionDef,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Iterator for FunctionDefParentIter<'a> {
    type Item = &'a JSFunctionDef;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.cur_fd.is_null() {
            let fd = self.cur_fd;
            if let Some(up) = unsafe { self.cur_fd.as_ref().map(|x| x.parent) } {
                self.cur_fd = up;
            }
            Some(unsafe { fd.as_ref().unwrap() })
        } else {
            None
        }
    }
}

impl JSFunctionDef {
    /// iterate over parent function def chain.
    pub fn parent_iter<'a>(&self, include_self: bool) -> FunctionDefParentIter<'a> {
        if include_self {
            FunctionDefParentIter {
                cur_fd: self as *const Self,
                _mark: PhantomData,
            }
        } else {
            let p = self.parent;
            if !p.is_null() {
                debug_assert_eq!(self.parent_scope_level, unsafe { (*p).scope_level });
                FunctionDefParentIter {
                    cur_fd: p,
                    _mark: PhantomData,
                }
            } else {
                FunctionDefParentIter {
                    cur_fd: std::ptr::null(),
                    _mark: PhantomData,
                }
            }
        }
    }
}
