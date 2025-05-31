use crate::{
    atom::{consts::*, js_atom_concat_str, JSAtom, JS_DupAtom, JS_FreeAtom, JS_ATOM_NULL},
    bytecode::{opcode_info_intermediate, opcodes, OpCode, OpCodeTemp},
    context::{JSContext, JSContextHandle, JS_MODE_STRIP},
    error::{JS_ThrowSyntaxErrorAtom, JS_THROW_VAR_RO},
    eval::JS_EVAL_TYPE_GLOBAL,
    function::{
        free_bytecode_atoms, JSClosureVar, JSFunctionKindEnum, JSGlobalVar, JSVarDef,
        JSVarKindEnum, JSVarScope, DEFINE_GLOBAL_LEX_VAR,
    },
    gc::GCObject,
    list_entry,
    mem::js_malloc_typed,
    module::JSModuleDef,
    parse::JSParseFunctionEnum,
    runtime::JSRuntime,
    types::{init_list_head, list_add_tail, list_del, list_foreach_safe, list_head},
    utils::{push_vec_u16, push_vec_u32, push_vec_u8, read_from_ptr},
    value::{
        object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
        JSValue, JS_FreeValue,
    },
};

use super::LabelSlot;

#[derive(Debug)]
#[repr(C)]
pub struct JSFunctionDef {
    pub ctx: JSContextHandle,

    /// parent function def
    /// TODO: use Option<Box<JSFunctionDef>>
    pub parent: *mut JSFunctionDef,

    /// 在父级常量池中的索引，-1表示无
    /// TODO: use Option<u16>
    pub parent_cpool_idx: i32,

    // 父级作用域层级
    pub parent_scope_level: i32,

    /// chain of children function defs
    /// TODO use Option<Box<JSFunctionDef>> for child
    pub child_list: list_head,

    pub link: list_head,

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
    /// `arguments` variable
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
    /// current scope level
    pub scope_level: i32,
    /// denotes the starting index to retrive chain of variable defs since this scope.
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

    pub label_slots: Vec<LabelSlot>,

    /// break/continue label stack
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

    // 模块定义（解析模块时非空）
    pub module: *mut JSModuleDef,
    // 是否包含await（用于模块eval）
    pub has_await: bool,
}

// #[cfg(debug_assertions)]
// impl Drop for JSFunctionDef {
//     fn drop(&mut self) {
//         println!("JSFunctionDef::drop({self:p})");
//     }
// }

impl JSFunctionDef {
    /// alloc a function def, and initialize it.
    pub(crate) fn new0(
        mut ctx: JSContextHandle,
        is_eval: bool,
        is_func_expr: bool,
        filename: &str,
        line_num: i32,
    ) -> Result<Box<Self>, i32> {
        let o = JSFunctionDef {
            ctx,
            parent: std::ptr::null_mut(),
            parent_cpool_idx: -1,
            parent_scope_level: 0,
            child_list: list_head::default(),
            link: list_head::default(),
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

            filename: ctx.create_atom(filename)?,
            line_num,
            pc2line: None,
            source: None,
            module: std::ptr::null_mut(),
            has_await: false,
        };

        let fd = unsafe {
            let mut p = js_malloc_typed::<JSFunctionDef>(ctx.as_ptr(), 1, true).ok_or(-1)?;
            p.write(o);

            init_list_head(&mut p.as_mut().child_list);
            Box::from_raw(p.as_ptr())
        };

        Ok(fd)
    }

    /// create function def and attach to parent-child chain
    pub fn new<X: Into<JSContextHandle>>(
        ctx: X,
        parent: *mut JSFunctionDef,
        is_eval: bool,
        is_func_expr: bool,
        filename: &str,
        line_num: i32,
    ) -> Result<Box<Self>, i32> {
        debug_assert!(line_num >= 0);

        Self::new0(ctx.into(), is_eval, is_func_expr, filename, line_num).map(|mut fd| {
            if !parent.is_null() {
                fd.set_parent(parent);
            }
            fd
        })
    }

    fn set_parent(&mut self, parent: *mut JSFunctionDef) {
        debug_assert!(!parent.is_null());
        debug_assert!(self.parent.is_null());

        self.parent = parent;
        self.js_mode = unsafe { (*parent).js_mode };
        self.parent_scope_level = unsafe { (*parent).scope_level as i32 };

        unsafe {
            list_add_tail(&mut self.link, &mut (*parent).child_list);
        }
    }

    /// update label ref count by delta. returns updated ref count.
    pub fn update_label_ref_count(&mut self, label: u32, delta: i32) -> i32 {
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
    pub unsafe fn add_pc2line_info(&mut self, pc: u32, line_num: i32) {
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
    pub fn dump_bytecode(&self, ctx: &mut JSContext, pass: i32) {
        if self.js_mode & JS_MODE_STRIP as u8 == 0 {
            println!("pass {pass}");

            unsafe {
                crate::bytecode::dump_byte_code(
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
                    std::ptr::null_mut(),
                );
            }

            println!();
        }
    }

    /// drop Box<Self> and all its subs.
    pub fn dispose(mut self: Box<Self>, ctx: &mut JSContext) {
        // free child function defs
        list_foreach_safe(&mut self.child_list, |el| unsafe {
            let fd1 = list_entry!(el, JSFunctionDef, link);
            Self::dispose(Box::from_raw(fd1), ctx);
        });

        free_bytecode_atoms(ctx.rt_mut(), &self.byte_code, self.use_short_opcodes);
        self.byte_code.clear();

        // #[cfg(feature = "short_opcode")]
        // self.jump_slots.clear();

        // self.label_slots.clear();
        // self.line_number_slots.clear();

        for v in self.cpool.drain(..) {
            JS_FreeValue(ctx, v);
        }

        for mut vd in self.args.drain(..) {
            JS_FreeAtom(ctx, vd.var_name);
            #[cfg(debug_assertions)]
            {
                vd.var_name = JS_ATOM_NULL;
            }
        }

        for mut vd in self.vars.drain(..) {
            JS_FreeAtom(ctx, vd.var_name);
            #[cfg(debug_assertions)]
            {
                vd.var_name = JS_ATOM_NULL;
            }
        }

        for gv in self.global_vars.drain(..) {
            JS_FreeAtom(ctx, gv.var_name);
        }

        if let Some(mut lst) = self.closure_vars.take() {
            for cv in lst.drain(..) {
                JS_FreeAtom(ctx, cv.var_name);
            }
        }

        JS_FreeAtom(ctx, self.func_name);
        JS_FreeAtom(ctx, self.filename);

        if !self.parent.is_null() {
            // remove from parent's list
            unsafe {
                list_del(&mut self.link);
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LineNumberSlot {
    pub pc: u32,
    pub line_num: i32,
}

pub struct BlockEnv {
    pub prev: *mut BlockEnv,
    /// JS_ATOM_NULL if none
    pub label_name: JSAtom,
    /// -1 if none
    pub label_break: i32,
    /// -1 if none
    pub label_cont: i32,
    /// number of stack elements to drop
    pub drop_count: i32,
    /// -1 if none
    pub label_finally: i32,
    pub scope_level: i32,
    pub has_iterator: bool,
}

#[cfg(feature = "short_opcode")]
#[derive(Debug, Clone, Copy)]
pub struct JumpSlot {
    pub op: i32,
    pub size: i32,
    pub pos: i32,
    pub label: i32,
}

#[deprecated(note = "use JSFunctionDef::new() instead")]
pub unsafe fn js_new_function_def(
    ctx: *mut JSContext,
    parent: *mut JSFunctionDef,
    is_eval: bool,
    is_func_expr: bool,
    filename: &str,
    line_num: i32,
) -> *mut JSFunctionDef {
    if let Ok(d) = JSFunctionDef::new(
        ctx.as_mut().unwrap(),
        parent,
        is_eval,
        is_func_expr,
        filename,
        line_num,
    ) {
        debug_assert_eq!(d.scope_first, -1);
        Box::into_raw(d)
    } else {
        std::ptr::null_mut()
    }
}

/// update label ref count by delta. returns new ref count.
#[deprecated(note = "use JSFunctionDef::update_label_ref_count() instead")]
pub unsafe fn update_label(fd: *mut JSFunctionDef, label: u32, delta: i32) -> i32 {
    (*fd).update_label_ref_count(label, delta)
}

pub unsafe fn push_break_entry(
    fd: *mut JSFunctionDef,
    be: *mut BlockEnv,
    label_name: JSAtom,
    label_break: i32,
    label_cont: i32,
    drop_count: i32,
) {
    (*be).prev = (*fd).top_break;
    (*fd).top_break = be;
    (*be).label_name = label_name;
    (*be).label_break = label_break;
    (*be).label_cont = label_cont;
    (*be).drop_count = drop_count;
    (*be).label_finally = -1;
    (*be).scope_level = (*fd).scope_level;
    (*be).has_iterator = false;
}

pub unsafe fn pop_break_entry(fd: *mut JSFunctionDef) {
    let be = (*fd).top_break;
    (*fd).top_break = (*be).prev;
}

#[deprecated(note = "use JSFunctionDef::add_func_var() instead")]
pub unsafe fn add_func_var(ctx: &mut JSContext, fd: *mut JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).add_func_var(ctx, name).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::add_var_this() instead")]
pub unsafe fn add_var_this(ctx: &mut JSContext, fd: *mut JSFunctionDef) -> i32 {
    (*fd).add_var_this(ctx).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::add_arguments(ctx, false) instead")]
pub unsafe fn add_arguments_var(ctx: &mut JSContext, fd: *mut JSFunctionDef) -> i32 {
    (*fd).add_arguments(ctx, false).map_or(-1, |x| x as i32)
}

// add an argument definition in the argument scope. Only needed when
// "eval()" may be called in the argument scope. Return 0 if OK.
#[deprecated(note = "use JSFunctionDef::add_arguments() instead")]
pub unsafe fn add_arguments_arg(ctx: &mut JSContext, fd: &mut JSFunctionDef) -> i32 {
    (*fd).add_arguments(ctx, true).map_or(-1, |x| x as i32)
}

/// find closure var by name, returning index of the closure var
#[deprecated(note = "use JSFunctionDef::find_closure_var() instead")]
pub unsafe fn find_closure_var(
    ctx: &mut JSContext,
    s: &mut JSFunctionDef,
    var_name: JSAtom,
) -> Option<u16> {
    (*s).find_closure_var(var_name)
}

// 'fd' must be a parent of 's'. Create in 's' a closure referencing a
// local variable (is_local = TRUE) or a closure (is_local = FALSE) in
// 'fd'
#[deprecated(note = "use JSFunctionDef::get_closure_var2() instead")]
pub unsafe fn get_closure_var2(
    ctx: &mut JSContext,
    s: &mut JSFunctionDef,
    fd: *const JSFunctionDef,
    is_local: bool,
    is_arg: bool,
    var_idx: u16,
    var_name: JSAtom,
    is_const: bool,
    is_lexical: bool,
    var_kind: JSVarKindEnum,
) -> Result<u16, i32> {
    s.get_closure_var2(
        ctx, fd, is_local, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
    )
}

#[deprecated(note = "use JSFunctionDef::get_closure_var2() instead")]
pub unsafe fn get_closure_var(
    ctx: &mut JSContext,
    s: &mut JSFunctionDef,
    fd: *const JSFunctionDef,
    is_arg: bool,
    var_idx: u16,
    var_name: JSAtom,
    is_const: bool,
    is_lexical: bool,
    var_kind: JSVarKindEnum,
) -> Result<u16, i32> {
    s.get_closure_var2(
        ctx, fd, true, is_arg, var_idx, var_name, is_const, is_lexical, var_kind,
    )
}

pub unsafe fn get_label_pos(s: *const JSFunctionDef, mut label: i32) -> i32 {
    let mut pos: i32 = 0;

    for _ in 0..20 {
        pos = (*s).label_slots[label as usize].pos;

        loop {
            match (*s).byte_code[pos as usize] {
                opcodes::op_line_num | opcodes::op_label => {
                    pos += 5;
                    continue;
                }
                opcodes::OP_goto => {
                    label = read_from_ptr::<i32>((*s).byte_code.as_ptr().add(pos as usize + 1));
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
    s: *mut JSFunctionDef,
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
                if (*s).update_label_ref_count(label, 0) > 0 {
                    break;
                }

                debug_assert!((*s).label_slots[label as usize].pending_relocs.is_empty());
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
                "label" | "label_u16" => unsafe {
                    let label = read_from_ptr::<u32>(bc_buf.add(current_pos + 1));
                    (*s).update_label_ref_count(label, -1);
                },
                f @ ("atom_label_u8" | "atom_label_u16" | "atom" | "atom_u8" | "atom_u16") => unsafe {
                    if matches!(f, "atom_label_u8" | "atom_label_u16") {
                        let label = read_from_ptr::<u32>(bc_buf.add(current_pos + 5));
                        (*s).update_label_ref_count(label, -1);
                    }

                    let atom = JSAtom(read_from_ptr::<u32>(bc_buf.add(current_pos + 1)));
                    JS_FreeAtom((*s).ctx.as_ptr(), atom);
                },
                _ => {}
            }
        }

        current_pos += len as usize;
    }

    current_pos
}

pub fn get_loc_or_ref(bc: &mut Vec<u8>, is_ref: bool, idx: i32) {
    // if the field is not initialized, the error is catched when accessing it
    push_vec_u8(
        bc,
        if is_ref {
            opcodes::OP_get_var_ref
        } else {
            opcodes::OP_get_loc
        },
    );
    push_vec_u16(bc, idx as u16);
}

/// var_name is not consumed.
// return 0 if OK or -1 if the private field could not be resolved
pub fn resolve_scope_private_field(
    ctx: &mut JSContext,
    s: *mut JSFunctionDef,
    var_name: JSAtom,
    scope_level: i32,
    op: u8,
    bc: &mut Vec<u8>,
) -> i32 {
    let mut is_ref = false;
    let mut var_kind = JSVarKindEnum::JS_VAR_NORMAL;

    let idx =
        resolve_scope_private_field1(ctx, &mut is_ref, &mut var_kind, s, var_name, scope_level);
    if idx < 0 {
        return -1;
    }
    debug_assert_ne!(var_kind, JSVarKindEnum::JS_VAR_NORMAL);

    match op as u8 {
        opcodes::op_scope_get_private_field | opcodes::op_scope_get_private_field2 => {
            match var_kind {
                JSVarKindEnum::JS_VAR_PRIVATE_FIELD => {
                    if op == opcodes::op_scope_get_private_field2 {
                        bc.push(opcodes::OP_dup);
                    }
                    get_loc_or_ref(bc, is_ref, idx);
                    bc.push(opcodes::OP_get_private_field);
                }
                JSVarKindEnum::JS_VAR_PRIVATE_METHOD => {
                    get_loc_or_ref(bc, is_ref, idx);
                    bc.push(opcodes::OP_check_brand);
                    if op != opcodes::op_scope_get_private_field2 {
                        bc.push(opcodes::OP_nip);
                    }
                }
                JSVarKindEnum::JS_VAR_PRIVATE_GETTER
                | JSVarKindEnum::JS_VAR_PRIVATE_GETTER_SETTER => {
                    if op == opcodes::op_scope_get_private_field2 {
                        bc.push(opcodes::OP_dup);
                    }
                    get_loc_or_ref(bc, is_ref, idx);

                    bc.push(opcodes::OP_check_brand);
                    bc.push(opcodes::OP_call_method);
                    bc.extend_from_slice(&[0, 0]); // 0_u16
                }
                JSVarKindEnum::JS_VAR_PRIVATE_SETTER => {
                    // XXX: add clearer error message
                    bc.push(opcodes::OP_throw_error);
                    bc.extend_from_slice(var_name.dup(ctx.rt()).0.to_ne_bytes().as_slice());
                    bc.push(JS_THROW_VAR_RO);
                }
                _ => {
                    unreachable!();
                }
            }
        }
        opcodes::op_scope_put_private_field => {
            match var_kind {
                JSVarKindEnum::JS_VAR_PRIVATE_FIELD => {
                    get_loc_or_ref(bc, is_ref, idx);
                    bc.push(opcodes::OP_put_private_field);
                }
                JSVarKindEnum::JS_VAR_PRIVATE_METHOD | JSVarKindEnum::JS_VAR_PRIVATE_GETTER => {
                    // XXX: add clearer error message
                    bc.push(opcodes::OP_throw_error);
                    bc.extend_from_slice(var_name.dup(ctx.rt()).0.to_ne_bytes().as_slice());
                    bc.push(JS_THROW_VAR_RO);
                }
                JSVarKindEnum::JS_VAR_PRIVATE_SETTER
                | JSVarKindEnum::JS_VAR_PRIVATE_GETTER_SETTER => {
                    let setter_name = get_private_setter_name(ctx, var_name);
                    if setter_name == JS_ATOM_NULL {
                        return -1;
                    }
                    let idx = resolve_scope_private_field1(
                        ctx,
                        &mut is_ref,
                        &mut var_kind,
                        s,
                        setter_name,
                        scope_level,
                    );
                    JS_FreeAtom(ctx, setter_name);
                    if idx < 0 {
                        return -1;
                    }
                    debug_assert_eq!(var_kind, JSVarKindEnum::JS_VAR_PRIVATE_SETTER);

                    get_loc_or_ref(bc, is_ref, idx);

                    bc.push(opcodes::OP_swap);
                    // obj func value
                    bc.push(opcodes::OP_rot3r);
                    // value obj func
                    bc.push(opcodes::OP_check_brand);
                    bc.push(opcodes::OP_rot3l);
                    // obj func value
                    bc.push(opcodes::OP_call_method);
                    bc.extend_from_slice(1_u16.to_ne_bytes().as_slice());
                    bc.push(opcodes::OP_drop);
                }
                _ => {
                    unreachable!(); // C2RS: Replaced abort() with unreachable!()
                }
            }
        }
        opcodes::op_scope_in_private_field => {
            get_loc_or_ref(bc, is_ref, idx);
            bc.push(opcodes::OP_private_in);
        }
        _ => {
            unreachable!();
        }
    }

    0
}

// search in all scopes
pub fn find_private_class_field_all(fd: *mut JSFunctionDef, name: JSAtom, scope_level: i32) -> i32 {
    debug_assert!(scope_level >= 0);

    let mut idx = unsafe { (*fd).scopes[scope_level as usize].first };
    while idx >= 0 {
        if unsafe { (*fd).vars.get(idx as usize).unwrap().var_name } == name {
            return idx;
        }
        idx = unsafe { (*fd).vars.get(idx as usize).unwrap().scope_next };
    }

    return -1;
}

/// var_name is not consumed
pub fn resolve_scope_private_field1(
    ctx: &mut JSContext,
    pis_ref: *mut bool,
    pvar_kind: *mut JSVarKindEnum,
    s: *mut JSFunctionDef,
    var_name: JSAtom,
    mut scope_level: i32,
) -> i32 {
    let mut fd = s;
    let mut is_ref = false;
    let mut idx: i32;
    let var_kind: JSVarKindEnum;

    'outer: loop {
        idx = find_private_class_field_all(fd, var_name, scope_level);
        if idx >= 0 {
            var_kind = unsafe { (*fd).vars.get(idx as usize).unwrap().var_kind() };
            if is_ref {
                idx = if let Ok(vi) = unsafe {
                    (*s).get_closure_var2(
                        ctx,
                        fd,
                        true,
                        false,
                        idx as u16,
                        var_name,
                        true,
                        true,
                        JSVarKindEnum::JS_VAR_NORMAL,
                    )
                } {
                    vi as i32
                } else {
                    return -1;
                }
            }
            break 'outer;
        }

        scope_level = unsafe { (*fd).parent_scope_level };
        if unsafe { (*fd).parent.is_null() } {
            if unsafe { (*fd).is_eval } {
                // closure of the eval function (top level)
                if let Some(cvars) = unsafe { (*fd).closure_vars.as_ref() } {
                    for (i, cv) in cvars.iter().enumerate() {
                        if cv.var_name == var_name {
                            var_kind = cv.var_kind();
                            is_ref = true;

                            if fd != s {
                                idx = if let Ok(vi) = unsafe {
                                    (*s).get_closure_var2(
                                        ctx,
                                        fd,
                                        false,
                                        (*cv).is_arg(),
                                        i as _,
                                        cv.var_name,
                                        cv.is_const(),
                                        cv.is_lexical(),
                                        cv.var_kind(),
                                    )
                                } {
                                    vi as i32
                                } else {
                                    return -1;
                                }
                            }

                            break 'outer; // goto done
                        }
                    }
                }
            }

            // XXX: no line number info
            JS_ThrowSyntaxErrorAtom(ctx, c"undefined private field '%s'", var_name);
            return -1;
        } else {
            fd = unsafe { (*fd).parent };
        }

        is_ref = true;
    }

    // done:
    unsafe {
        *pis_ref = is_ref;
        *pvar_kind = var_kind;
    }

    idx
}

/* build a private setter function name from the private getter name */
pub fn get_private_setter_name(ctx: *mut JSContext, name: JSAtom) -> JSAtom {
    unsafe { js_atom_concat_str(ctx, name, c"<set>".as_ptr().cast()) }
}

pub unsafe fn instantiate_hoisted_definitions(
    ctx: *mut JSContext,
    s: *mut JSFunctionDef,
    bc: &mut Vec<u8>,
) {
    // add the hoisted functions in arguments and local variables
    for (i, vd) in (*s).args.iter().enumerate() {
        if vd.func_pool_idx >= 0 {
            push_vec_u8(bc, OpCode::FCLOSURE as u8);
            push_vec_u32(bc, (*vd).func_pool_idx as u32);

            push_vec_u8(bc, OpCode::PUT_ARG as u8);
            push_vec_u16(bc, i as u16);
        }
    }

    for (i, vd) in (*s).vars.iter().enumerate() {
        if vd.scope_level == 0 && vd.func_pool_idx >= 0 {
            push_vec_u8(bc, OpCode::FCLOSURE as u8);
            push_vec_u32(bc, (*vd).func_pool_idx as u32);

            push_vec_u8(bc, OpCode::PUT_LOC as u8);
            push_vec_u16(bc, i as _);
        }
    }

    let mut label_next: Option<u32> = None;

    // the module global variables must be initialized before
    // evaluating the module so that the exported functions are
    // visible if there are cyclic module references
    if !(*s).module.is_null() {
        let l = (*s)
            .add_label()
            .expect("TODO: unhandled create label exception");
        label_next.replace(l);

        // if 'this' is true, initialize the global variables and return
        push_vec_u8(bc, OpCode::PUSH_THIS as u8);
        push_vec_u8(bc, OpCode::IF_FALSE as u8);
        push_vec_u32(bc, l);

        (*s).update_label_ref_count(l, 1);

        #[cfg(feature = "short_opcode")]
        {
            (*s).jump_slots_cap += 1;
        }
    }

    // add the global variables (only happens if JSFunctionDef::is_global_var is true)
    for hf in (*s).global_vars.iter() {
        let mut has_closure: i32 = 0;
        let mut force_init = hf.force_init();
        let mut idx = 0;

        // we are in an eval, so the closure contains all the enclosing variables.
        // If the outer function has a variable environment, create a property for the variable there
        if let Some(cvars) = (*s).closure_vars.as_ref() {
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
            if (*s).eval_type != JS_EVAL_TYPE_GLOBAL {
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
                push_vec_u32(bc, JS_DupAtom(ctx, hf.var_name).0); // dup to compensate later free
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
                    push_vec_u32(bc, JS_DupAtom(ctx, JS_ATOM_default).0);
                }
            } else {
                push_vec_u8(bc, OpCode::UNDEFINED as u8);
            }

            if has_closure == 2 {
                push_vec_u8(bc, OpCode::PUT_VAR_REF as u8);
                push_vec_u16(bc, idx as u16);
            } else if has_closure == 1 {
                push_vec_u8(bc, OpCode::DEFINE_FIELD as u8);
                push_vec_u32(bc, JS_DupAtom(ctx, (*hf).var_name).0);
                push_vec_u8(bc, OpCode::DROP as u8);
            } else {
                // XXX: Check if variable is writable and enumerable
                push_vec_u8(bc, OpCode::PUT_VAR as u8);
                push_vec_u32(bc, JS_DupAtom(ctx, (*hf).var_name).0);
            }
        }

        // done_global_var:
        JS_FreeAtom(ctx, hf.var_name);
    }

    if !(*s).module.is_null() {
        let l = label_next.unwrap();
        push_vec_u8(bc, OpCode::RETURN_UNDEF as u8);
        push_vec_u8(bc, OpCodeTemp::LABEL as u8);
        push_vec_u32(bc, l);
        (*s).label_slots[l as usize].pos2 = bc.len() as i32;
    }

    (*s).global_vars.clear();
}
