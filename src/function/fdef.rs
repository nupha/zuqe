use std::ops::DerefMut;

use crate::{
    atom::{consts::*, js_atom_concat_str, JSAtom, JS_DupAtom, JS_FreeAtom, JS_ATOM_NULL},
    bytecode::{opcode_info_intermediate, opcodes, OpCode, OpCodeTemp},
    context::{JSContext, JS_MODE_STRICT, JS_MODE_STRIP},
    error::{JS_ThrowSyntaxErrorAtom, JS_THROW_VAR_RO},
    eval::{JS_EVAL_TYPE_DIRECT, JS_EVAL_TYPE_GLOBAL, JS_EVAL_TYPE_INDIRECT},
    function::{
        free_bytecode_atoms, is_var_in_arg_scope, resolve_labels, resolve_variables, JSClosureVar,
        JSFunctionBytecode, JSFunctionKindEnum, JSGlobalVar, JSVarDef, JSVarKindEnum, JSVarScope,
        ARG_SCOPE_END, ARG_SCOPE_INDEX, DEFINE_GLOBAL_LEX_VAR,
    },
    gc::{add_gc_object, JSGCObjectTypeEnum},
    list_entry,
    mem::{js_free, js_malloc_typed},
    module::{add_module_variables, JSModuleDef},
    parse::JSParseFunctionEnum,
    types::{
        init_list_head, list_add_tail, list_del, list_foreach_safe, list_foreach_safe_with_ret,
        list_head,
    },
    utils::{get_u32, push_vec_u16, push_vec_u32, push_vec_u8},
    value::{
        object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
        JSTag, JSValue, JS_FreeValue, JS_TAG_FUNCTION_BYTECODE, JS_VALUE_GET_TAG,
    },
    JS_MKPTR,
};

#[derive(Debug)]
#[repr(C)]
pub struct JSFunctionDef {
    pub ctx: *mut JSContext,

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

    // 变量是否未在本地定义
    pub is_global_var: bool,
    // 是否是函数表达式
    pub is_func_expr: bool,
    // 是否拥有home object
    pub has_home_object: bool,
    // 是否需要prototype字段
    pub has_prototype: bool,
    // 是否有简单参数列表
    pub has_simple_parameter_list: bool,
    // 是否需要创建参数作用域
    pub has_parameter_expressions: bool,
    // 严格模式指令
    pub has_use_strict: bool,
    // 是否包含eval调用
    pub has_eval_call: bool,
    // 是否可访问arguments绑定
    pub has_arguments_binding: bool,
    // 是否可访问this绑定
    pub has_this_binding: bool,
    // 是否允许new.target
    pub new_target_allowed: bool,
    // 是否允许super()
    pub super_call_allowed: bool,
    // 是否允许super.或super[]
    pub super_allowed: bool,
    // 是否允许arguments标识符
    pub arguments_allowed: bool,
    // 是否是派生类构造函数
    pub is_derived_class_constructor: bool,
    // 是否在函数体内
    pub in_function_body: bool,
    // 回溯屏障
    pub backtrace_barrier: bool,

    // 函数类型枚举（原C使用位域）
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

    /// current scope id
    pub scope_level: i32,
    /// 第一个词法作用域变量索引
    pub scope_first: i32,
    /// list of all scopes
    pub scopes: Vec<JSVarScope>,

    // 函数体或eval的作用域
    pub body_scope: i32,

    // 字节码缓冲区
    pub byte_code: Vec<u8>,
    // 最后操作码位置
    pub last_opcode_pos: i32,
    // 最后操作码行号
    pub last_opcode_line_num: u32,
    // 是否使用短操作码
    pub use_short_opcodes: bool,

    /// 标签槽数组
    /// TODO: use Vec<LabelSlot>
    pub label_slots: *mut LabelSlot,
    // 标签槽分配大小
    pub label_size: i32,
    // 标签数量
    pub label_count: u16,

    // break/continue label stack
    pub top_break: *mut BlockEnv,

    // 跳转槽数组. TODO: use Vec<JumpSlot>
    pub jump_slots: *mut JumpSlot,
    // 跳转槽分配大小
    pub jump_size: u32,
    // 跳转计数
    pub jump_count: u32,

    /// 行号槽数组. TODO: use Vec<LineNumberSlot>
    pub line_number_slots: *mut LineNumberSlot,
    // 行号槽分配大小
    pub line_number_size: u32,
    // 行号计数
    pub line_number_count: u32,
    // 最后记录的行号
    pub line_number_last: u32,
    // 最后记录行号的PC
    pub line_number_last_pc: u32,

    pub filename: JSAtom,
    /// 当前行号
    pub line_num: u32,
    /// pc到行号的映射表
    pub pc2line: Option<Vec<u8>>,

    /// 原始源码（UTF-8编码）
    /// TODO: use String
    pub source: *mut u8,
    // 源码长度
    pub source_len: u32,

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
    fn new0(
        ctx: &mut JSContext,
        is_eval: bool,
        is_func_expr: bool,
        filename: &str,
        line_num: u32,
    ) -> Result<Box<Self>, i32> {
        let o = JSFunctionDef {
            ctx: ctx,
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
            label_slots: std::ptr::null_mut(),
            label_size: 0,
            label_count: 0,
            top_break: std::ptr::null_mut(),
            closure_vars: None,
            jump_slots: std::ptr::null_mut(),
            jump_size: 0,
            jump_count: 0,
            line_number_slots: std::ptr::null_mut(),
            line_number_size: 0,
            line_number_count: 0,
            line_number_last: 0,
            line_number_last_pc: 0,
            filename: ctx.create_atom(filename)?,
            line_num,
            pc2line: None,
            source: std::ptr::null_mut(),
            source_len: 0,
            module: std::ptr::null_mut(),
            has_await: false,
        };

        let mut fd = unsafe {
            let mut p = js_malloc_typed::<JSFunctionDef>(ctx, 1, true).ok_or(-1)?;
            p.as_ptr().write(o);

            init_list_head(&mut p.as_mut().child_list);

            Box::from_raw(p.as_ptr())

            // let ptr = js_malloc_typed::<JSFunctionDef>(ctx, 1, true).ok_or(-1)?;
            // let p = ptr.as_ptr();

            // (*p).closure_vars = None;
            // (*p).ctx = ctx;
            // (*p).parent = std::ptr::null_mut();
            // (*p).parent_cpool_idx = -1;
            // (*p).is_eval = is_eval;
            // (*p).is_func_expr = is_func_expr;
            // (*p).byte_code = Vec::with_capacity(128);
            // (*p).last_opcode_pos = -1;
            // (*p).func_name = JS_ATOM_NULL;
            // (*p).var_object_idx = None;
            // (*p).arg_var_object_idx = None;
            // (*p).arguments_var_idx = None;
            // (*p).arguments_arg_idx = None;
            // (*p).func_var_idx = None;
            // (*p).eval_ret_idx = None;
            // (*p).this_var_idx = None;
            // (*p).new_target_var_idx = None;
            // (*p).home_object_var_idx = None;
            // (*p).this_active_func_var_idx = None;

            // /* XXX: should distinguish arg, var and var object and body scopes */
            // (*p).scopes = {
            //     let mut arr = Vec::<JSVarScope>::new();
            //     arr.try_reserve_exact(4).map_err(|_| -1)?;
            //     arr.push(JSVarScope {
            //         first: -1,
            //         parent: -1,
            //     });
            //     arr
            // };
            // (*p).scope_level = 0;
            // (*p).scope_first = -1;
            // (*p).body_scope = -1;

            // (*p).filename = ctx.create_atom(filename)?;
            // (*p).line_num = line_num;

            // (*p).pc2line = None;
            // // (*fd).pc2line_last_line_num = line_num;
            // // (*fd).pc2line_last_pc = 0;

            // (*p).last_opcode_line_num = line_num;

            // (*p).cpool = std::ptr::null_mut();
            // (*p).cpool_count = 0;
            // (*p).cpool_size = 0;
            // (*p).arg_count = 0;
            // (*p).defined_arg_count = 0;
            // (*p).global_var_count = 0;
            // (*p).global_var_size = 0;
            // (*p).global_vars = std::ptr::null_mut();

            // (*p).label_slots = std::ptr::null_mut();
            // (*p).label_size = 0;
            // (*p).label_count = 0;
            // (*p).top_break = std::ptr::null_mut();

            // (*p).jump_slots = std::ptr::null_mut();
            // (*p).jump_size = 0;
            // (*p).jump_count = 0;

            // (*p).line_number_slots = std::ptr::null_mut();
            // (*p).line_number_size = 0;
            // (*p).line_number_count = 0;

            // Box::from_raw(ptr.as_ptr())
        };

        unsafe {
            init_list_head(&mut fd.child_list);
        }

        Ok(fd)
    }

    /// create function def and attach to parent-child chain
    pub fn new(
        ctx: &mut JSContext,
        parent: *mut JSFunctionDef,
        is_eval: bool,
        is_func_expr: bool,
        filename: &str,
        line_num: u32,
    ) -> Result<Box<Self>, i32> {
        Self::new0(ctx, is_eval, is_func_expr, filename, line_num).map(|mut fd| {
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
    pub unsafe fn update_label_ref_count(&mut self, label: u32, delta: i32) -> i32 {
        debug_assert!(
            label < self.label_count as u32,
            "label={label}, count={}",
            self.label_count
        );

        let ls = self.label_slots.offset(label as isize);
        (*ls).ref_count += delta;
        debug_assert!((*ls).ref_count >= 0);
        (*ls).ref_count
    }

    /// the pc2line table gives a line number for each PC value
    pub unsafe fn add_pc2line_info(&mut self, pc: u32, line_num: u32) {
        if !self.line_number_slots.is_null()
            && self.line_number_count < self.line_number_size
            && pc >= self.line_number_last_pc
            && line_num != self.line_number_last
        {
            (*self
                .line_number_slots
                .offset(self.line_number_count as isize))
            .pc = pc as u32;

            (*self
                .line_number_slots
                .offset(self.line_number_count as isize))
            .line_num = line_num;

            self.line_number_count += 1;
            self.line_number_last_pc = pc;
            self.line_number_last = line_num;
        }
    }

    /// Create a bytecode function from function definition.
    /// All the child functions are also created. It must be done this way to resolve all the variables.
    /// The function definition is consumed.
    pub unsafe fn make_function(mut self: Box<Self>, ctx: &mut JSContext) -> JSValue {
        macro_rules! fail {
            () => {
                self.free(ctx);
                return JSValue::EXCEPTION;
            };
        }

        //
        // recompute scope linkage
        //

        for scope in self.scopes.iter_mut() {
            scope.first = -1;
        }

        if self.has_parameter_expressions {
            // special end of variable list marker for the argument scope
            self.scopes.get_mut(ARG_SCOPE_INDEX as usize).unwrap().first = ARG_SCOPE_END;
        }

        for (idx, vd) in self.vars.iter_mut().enumerate() {
            debug_assert!(vd.scope_level >= 0 && vd.scope_level < self.scopes.len() as i32);
            let scope = self.scopes.get_mut(vd.scope_level as usize).unwrap();
            vd.scope_next = scope.first;
            scope.first = idx as i32;
        }
        // for idx in 0..self.var_count {
        //     let vd = self.vars.offset(idx as isize);
        //     debug_assert!((*vd).scope_level >= 0 && (*vd).scope_level < self.scopes.len() as i32);

        //     let scope = self.scopes.get_mut((*vd).scope_level as usize).unwrap();
        //     (*vd).scope_next = scope.first;
        //     scope.first = idx as i32;
        // }

        for i in 2..self.scopes.len() {
            if self.scopes[i].first < 0 {
                let parent = self.scopes[i].parent;
                debug_assert!(parent >= 0);
                self.scopes[i].first = self.scopes[parent as usize].first;
            }
        }

        for vd in self.vars.iter_mut() {
            if vd.scope_next < 0 && vd.scope_level > 1 {
                let parent = self.scopes[vd.scope_level as usize].parent;
                vd.scope_next = self.scopes[parent as usize].first;
            }
        }
        // for idx in 0..self.var_count {
        //     let vd = self.vars.offset(idx as isize);
        //     if (*vd).scope_next < 0 && (*vd).scope_level > 1 {
        //         let parent = self.scopes[(*vd).scope_level as usize].parent;
        //         (*vd).scope_next = self.scopes[parent as usize].first;
        //     }
        // }

        // if the function contains an eval call, the closure variables
        // are used to compile the eval and they must be ordered by scope,
        // so it is necessary to create the closure variables before any
        // other variable lookup is done.
        if self.has_eval_call {
            add_eval_variables(ctx, self.deref_mut());
        }

        // add the module global variables in the closure
        if !self.module.is_null() {
            if add_module_variables(ctx, self.deref_mut()) != 0 {
                fail!();
            }
        }

        // first create all the child functions
        let r = list_foreach_safe_with_ret(&mut self.child_list, |el| {
            let fd1 = list_entry!(el, JSFunctionDef, link);

            let cpool_idx: i32 = (*fd1).parent_cpool_idx;
            debug_assert!((0..self.cpool.len() as i32).contains(&cpool_idx));

            let func_obj = Box::from_raw(fd1).make_function(ctx);
            if func_obj.is_exception() {
                return Some(-1); // fail exit
            }
            debug_assert_eq!(JS_VALUE_GET_TAG(func_obj), JS_TAG_FUNCTION_BYTECODE);

            // put child function to parent's constant pool
            self.cpool[cpool_idx as usize] = func_obj;

            None
        });
        if r.is_some() {
            fail!();
        }
        #[cfg(feature = "dump_bytecode")]
        self.dump_bytecode(ctx, 1);

        // pass 2
        if resolve_variables(ctx, self.deref_mut()).is_err() {
            fail!();
        }
        #[cfg(feature = "dump_bytecode")]
        self.dump_bytecode(ctx, 2);

        // pass 3
        if resolve_labels(ctx, self.deref_mut()).is_err() {
            fail!();
        }

        let stack_size = if let Ok(ss) = self.compute_stack_size(ctx) {
            ss
        } else {
            fail!();
        };

        // alloc bytecode function
        let mut bf = if let Some(p) = js_malloc_typed::<JSFunctionBytecode>(ctx, 1, true) {
            Box::from_raw(p.as_ptr())
        } else {
            fail!();
        };

        bf.func_name = self.func_name;
        self.func_name = JS_ATOM_NULL; // avoid to free func_name in js_free_function_def()

        bf.header.ref_count = 1;
        bf.stack_size = stack_size;
        bf.has_prototype = self.has_prototype;
        bf.has_simple_parameter_list = self.has_simple_parameter_list;
        bf.js_mode = self.js_mode;
        bf.is_derived_class_constructor = self.is_derived_class_constructor;
        bf.func_kind = self.func_kind;
        bf.need_home_object = self.home_object_var_idx.is_some() || self.need_home_object;
        bf.new_target_allowed = self.new_target_allowed;
        bf.super_call_allowed = self.super_call_allowed;
        bf.super_allowed = self.super_allowed;
        bf.arguments_allowed = self.arguments_allowed;
        bf.backtrace_barrier = self.backtrace_barrier;
        bf.is_direct_or_indirect_eval =
            matches!(self.eval_type, JS_EVAL_TYPE_DIRECT | JS_EVAL_TYPE_INDIRECT);
        bf.realm = ctx.dup();

        bf.cpool_count = self.cpool.len() as _;
        bf.var_count = self.vars.len() as _;
        bf.arg_count = self.args.len() as _;
        bf.defined_arg_count = self.defined_arg_count;

        // initialize offsets to invalid
        bf.cpool_start = -1;
        bf.vardefs_start = -1;
        bf.closure_var_start = -1;

        let mut bin_size = if bf.cpool_count != 0 {
            bf.cpool_start = 0;
            bf.cpool_count as usize * std::mem::size_of::<JSValue>()
        } else {
            0
        };

        if self.js_mode & JS_MODE_STRIP as u8 == 0 || self.has_eval_call {
            let n: usize = self.args.len() + self.vars.len();
            if n != 0 {
                const ly: std::alloc::Layout = std::alloc::Layout::new::<JSVarDef>();
                let mut i = bin_size;
                let m = i % ly.align();
                if m != 0 {
                    i += ly.align() - m;
                    bin_size += ly.align() - m
                }
                bf.vardefs_start = i as i32;
                bin_size += n * ly.size();
            }
        }

        let num_closures = self.closure_vars.as_ref().map_or(0, |x| x.len());
        if num_closures != 0 {
            const ly: std::alloc::Layout = std::alloc::Layout::new::<JSClosureVar>();
            let mut i = bin_size;
            let m = i % ly.align();
            if m != 0 {
                i += ly.align() - m;
                bin_size += ly.align() - m
            }
            bin_size += num_closures * ly.size();
            bf.closure_var_start = i as i32;
        }

        bf.bytecode_start = bin_size as u32;
        bin_size += self.byte_code.len();

        debug_assert!(bin_size > 0);

        // alloc binary content
        let mut bin = Vec::<u8>::new();
        if bin.try_reserve_exact(bin_size).is_err() {
            ctx.JS_ThrowOutOfMemory();
            fail!();
        }
        bin.set_len(bin_size);

        // 1. cpool section
        if bf.cpool_start >= 0 {
            std::ptr::copy_nonoverlapping(
                self.cpool.as_ptr(),
                bin.as_mut_ptr()
                    .add(bf.cpool_start as usize)
                    .cast::<JSValue>(),
                bf.cpool_count as usize,
            );

            // SAFEGUARD: we have copy cpool values to function bytecode above.
            // we do cpool pointer and count clean-ups here in advance,
            // so that js_free_function_def() will not free them again.
            self.cpool.clear();
        }

        // 2. vardef section
        if bf.vardefs_start >= 0 {
            if bf.arg_count != 0 {
                std::ptr::copy_nonoverlapping(
                    self.args.as_ptr().cast::<u8>(),
                    bin.as_mut_ptr().add(bf.vardefs_start as usize),
                    bf.arg_count as usize * std::mem::size_of::<JSVarDef>(),
                );
            }
            if bf.var_count != 0 {
                std::ptr::copy_nonoverlapping(
                    self.vars.as_ptr().cast::<u8>(),
                    bin.as_mut_ptr().add(
                        bf.vardefs_start as usize
                            + bf.arg_count as usize * std::mem::size_of::<JSVarDef>(),
                    ),
                    bf.var_count as usize * std::mem::size_of::<JSVarDef>(),
                );
            }

            // SAFEGUARD: we have copy arg and var definations to function bytecode above.
            // we do vardefs and *_count clean-ups here in advance,
            // so that when calling js_free_function_def() later, it will not free vardefs.
            #[cfg(not(debug_assertions))]
            {
                self.args.clear();
                self.vars.clear();
            }
            #[cfg(debug_assertions)]
            {
                for mut vd in self.args.drain(..) {
                    vd.var_name = JS_ATOM_NULL;
                }
                for mut vd in self.vars.drain(..) {
                    vd.var_name = JS_ATOM_NULL;
                }
            }
        }

        // 3. closure vars section
        if bf.closure_var_start >= 0 {
            let cvars = self.closure_vars.take().unwrap();
            bf.closure_var_count = cvars.len() as _;
            debug_assert!(bf.closure_var_count < u16::MAX as _);

            let mut ptr = bin
                .as_mut_ptr()
                .add(bf.closure_var_start as usize)
                .cast::<JSClosureVar>();

            for cv in cvars {
                ptr.write(cv);
                ptr = ptr.add(1);
            }
        }

        // 4. bytecode section
        std::ptr::copy_nonoverlapping(
            self.byte_code.as_ptr(),
            bin.as_mut_ptr().add(bf.bytecode_start as usize),
            self.byte_code.len(),
        );
        // SAFEGUARD: clear bytecode in advance, so that js_free_function_def() will not free atoms in it.
        self.byte_code.clear();

        debug_assert_eq!(bin.len(), bin_size);
        bf.binary = bin.into_boxed_slice();

        if self.js_mode & JS_MODE_STRIP as u8 == 0 {
            bf.has_debug = true;
            bf.debug.line_num = self.line_num;
            bf.debug.filename = self.filename;
            // SAFEGUARD: to prevent free in js_free_function_def()
            self.filename = JS_ATOM_NULL;

            bf.debug.source_len = self.source_len;
            bf.debug.source = self.source;
            // SAFEGUARD: to prevent free in js_free_function_def()
            self.source = std::ptr::null_mut();

            // move pc2line to func object.
            if let Some(mut lst) = self.pc2line.take() {
                lst.shrink_to_fit();
                bf.debug.pc2line_buf.replace(lst);
            }
        }

        add_gc_object(
            ctx.rt_mut(),
            &mut bf.header,
            JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_FUNCTION_BYTECODE,
        );

        #[cfg(feature = "dump_bytecode")]
        if self.js_mode & JS_MODE_STRIP as u8 == 0 {
            crate::bytecode::js_dump_function_bytecode(ctx, bf.as_mut());
        }

        // drop function defination
        self.free(ctx);

        // return bytecode function value
        JS_MKPTR!(JSTag::FunctionBytecode, Box::into_raw(bf).cast())
    }

    #[cfg(feature = "dump_bytecode")]
    fn dump_bytecode(&self, ctx: &mut JSContext, pass: i32) {
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
                    self.source.cast(),
                    self.line_num as _,
                    self.label_slots,
                    std::ptr::null_mut(),
                );
            }

            println!();
        }
    }

    /// to free a JSFunctionDef and all its children
    pub unsafe fn free(mut self: Box<Self>, ctx: &mut JSContext) {
        // free child function defs
        list_foreach_safe(&mut self.child_list, |el| {
            let fd1 = list_entry!(el, JSFunctionDef, link);
            Self::free(Box::from_raw(fd1), ctx);
        });

        free_bytecode_atoms(
            ctx.rt_mut(),
            std::slice::from_raw_parts(self.byte_code.as_ptr(), self.byte_code.len() as usize),
            self.use_short_opcodes,
        );
        self.byte_code.clear();

        if !self.jump_slots.is_null() {
            js_free(ctx, self.jump_slots.cast());
            self.jump_slots = std::ptr::null_mut();
        }
        if !self.label_slots.is_null() {
            js_free(ctx, self.label_slots.cast());
            self.label_slots = std::ptr::null_mut();
        }
        if !self.line_number_slots.is_null() {
            js_free(ctx, self.line_number_slots.cast());
            self.line_number_slots = std::ptr::null_mut();
        }

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

        if !self.source.is_null() {
            js_free(ctx, self.source.cast());
            self.source = std::ptr::null_mut();
        }

        if !self.parent.is_null() {
            // remove from parent's list
            list_del(&mut self.link);
        }
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct LineNumberSlot {
    pub pc: u32,
    pub line_num: u32,
}

#[repr(C)]
pub struct BlockEnv {
    pub prev: *mut BlockEnv,
    // JS_ATOM_NULL if none
    pub label_name: JSAtom,
    // -1 if none
    pub label_break: i32,
    // -1 if none
    pub label_cont: i32,
    // number of stack elements to drop
    pub drop_count: i32,
    // -1 if none
    pub label_finally: i32,
    pub scope_level: i32,
    pub has_iterator: bool,
}

#[derive(Debug)]
pub struct LabelSlot {
    pub ref_count: i32,

    /// phase 1 address, -1 means not resolved yet
    pub pos: i32,
    /// phase 2 address, -1 means not resolved yet
    pub pos2: i32,
    /// phase 3 address, -1 means not resolved yet
    pub addr: i32,

    /// relocations
    /// TODO: use Vec<RelocEntry> or LinkedList<RelocEntry>
    pub first_reloc: *mut RelocEntry,
}

#[derive(Debug)]
pub struct RelocEntry {
    pub next: *mut Self,
    pub addr: usize, /* address to patch */
    pub size: i32,   /* address size: 1, 2 or 4 bytes */
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
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
    line_num: u32,
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

pub unsafe fn pop_break_entry(fd: *mut JSFunctionDef) {
    let be = (*fd).top_break;
    (*fd).top_break = (*be).prev;
}

pub unsafe fn add_eval_variables(ctx: &mut JSContext, s: &mut JSFunctionDef) {
    // in non strict mode, variables are created in the caller's environment object
    if !s.is_eval && (s.js_mode & JS_MODE_STRICT as u8) == 0 {
        s.var_object_idx = Some(s.add_var(ctx, JS_ATOM__var_).unwrap());
        if s.has_parameter_expressions {
            // an additional variable object is needed for the argument scope
            s.arg_var_object_idx = Some(s.add_var(ctx, JS_ATOM__arg_var_).unwrap());
        }
    }

    // eval can potentially use 'arguments' so we must define it
    let mut has_this_binding = s.has_this_binding;
    if has_this_binding {
        if s.this_var_idx.is_none() {
            s.this_var_idx = Some(s.add_var_this(ctx).unwrap());
        }
        if s.new_target_var_idx.is_none() {
            s.new_target_var_idx = Some(s.add_var(ctx, JS_ATOM_new_target).expect("TODO"));
        }
        if s.is_derived_class_constructor && s.this_active_func_var_idx.is_none() {
            s.this_active_func_var_idx =
                Some(s.add_var(ctx, JS_ATOM_this_active_func).expect("TODO"));
        }
        if s.has_home_object && (*s).home_object_var_idx.is_none() {
            s.home_object_var_idx = Some(s.add_var(ctx, JS_ATOM__home_object_).expect("TODO"));
        }
    }

    let mut has_arguments_binding = s.has_arguments_binding;
    if has_arguments_binding {
        s.add_arguments(ctx, false).unwrap();

        // also add `arguments` binding in the argument scope.
        // to raise an error if a direct eval in the argument scope tries to redefine it.
        if (*s).has_parameter_expressions && s.js_mode & JS_MODE_STRICT as u8 == 0 {
            s.add_arguments(ctx, true).unwrap();
        }
    }

    if s.is_func_expr && s.func_name != JS_ATOM_NULL {
        s.add_func_var(ctx, s.func_name).unwrap();
    }

    // eval can use all the variables of the enclosing functions, so
    // they must be all put in the closure. The closure variables are
    // ordered by scope. It works only because no closure are created
    // before.
    debug_assert!(s.is_eval || s.closure_vars.as_ref().map_or(0, |x| x.len()) == 0);

    // XXX: inefficient, but eval performance is less critical
    let mut fd = s as *mut JSFunctionDef;
    loop {
        let scope_level = (*fd).parent_scope_level;

        if let Some(p) = (*fd).parent.as_mut() {
            fd = p;
        } else {
            break;
        }

        // add 'this' if it was not previously added
        if !has_this_binding && (*fd).has_this_binding {
            if (*fd).this_var_idx.is_none() {
                (*fd).this_var_idx.replace((*fd).add_var_this(ctx).unwrap());
            }
            if (*fd).new_target_var_idx.is_none() {
                (*fd)
                    .new_target_var_idx
                    .replace((*fd).add_var(ctx, JS_ATOM_new_target).expect("TODO"));
            }
            if (*fd).is_derived_class_constructor && (*fd).this_active_func_var_idx.is_none() {
                (*fd)
                    .this_active_func_var_idx
                    .replace((*fd).add_var(ctx, JS_ATOM_this_active_func).expect("TODO"));
            }
            if (*fd).has_home_object && (*fd).home_object_var_idx.is_none() {
                (*fd)
                    .home_object_var_idx
                    .replace((*fd).add_var(ctx, JS_ATOM__home_object_).expect("TODO"));
            }
            has_this_binding = true;
        }

        // add 'arguments' if it was not previously added
        if !has_arguments_binding && (*fd).has_arguments_binding {
            (*fd).add_arguments(ctx, false).unwrap();
            has_arguments_binding = true;
        }

        // add function name
        if (*fd).is_func_expr && (*fd).func_name != JS_ATOM_NULL {
            (*fd).add_func_var(ctx, (*fd).func_name).unwrap();
        }

        // add lexical variables
        debug_assert!((0..(*fd).scopes.len() as i32).contains(&scope_level));
        let mut scope_idx = (*fd).scopes[scope_level as usize].first;
        while scope_idx >= 0 {
            let vd = (*fd).vars.get_mut(scope_idx as usize).unwrap();
            vd.set_is_captured(true);
            s.get_closure_var2(
                ctx,
                fd,
                true,
                false,
                scope_idx as u16,
                vd.var_name,
                vd.is_const(),
                vd.is_lexical(),
                vd.var_kind(),
            )
            .expect("TODO");
            scope_idx = (*vd).scope_next;
        }

        let is_arg_scope = scope_idx == ARG_SCOPE_END;
        if !is_arg_scope {
            // add unscoped variables
            // XXX: propagate is_const and var_kind too ?
            for (i, vd) in (*fd).args.iter().enumerate() {
                if vd.var_name != JS_ATOM_NULL {
                    s.get_closure_var2(
                        ctx,
                        fd,
                        true,
                        true,
                        i as _,
                        vd.var_name,
                        false,
                        vd.is_lexical(),
                        JSVarKindEnum::JS_VAR_NORMAL,
                    )
                    .unwrap();
                }
            }

            for (i, vd) in (*fd).vars.iter().enumerate() {
                // do not close top level last result
                if vd.scope_level == 0 && !matches!(vd.var_name, JS_ATOM__ret_ | JS_ATOM_NULL) {
                    s.get_closure_var2(
                        ctx,
                        fd,
                        true,
                        false,
                        i as _,
                        vd.var_name,
                        false,
                        vd.is_lexical(),
                        JSVarKindEnum::JS_VAR_NORMAL,
                    )
                    .expect("TODO");
                }
            }
        } else {
            for (i, vd) in (*fd).vars.iter().enumerate() {
                // do not close top level last result
                if vd.scope_level == 0 && is_var_in_arg_scope(vd) {
                    s.get_closure_var2(
                        ctx,
                        fd,
                        true,
                        false,
                        i as _,
                        (*vd).var_name,
                        false,
                        (*vd).is_lexical(),
                        JSVarKindEnum::JS_VAR_NORMAL,
                    )
                    .expect("TODO");
                }
            }
        }

        if (*fd).is_eval {
            // add direct eval variables (we are necessarily at the top level)
            if let Some(cvars) = (*fd).closure_vars.as_ref() {
                for (i, cv) in cvars.iter().enumerate() {
                    s.get_closure_var2(
                        ctx,
                        fd,
                        false,
                        (*cv).is_arg(),
                        i as _,
                        (*cv).var_name,
                        (*cv).is_const(),
                        (*cv).is_lexical(),
                        (*cv).var_kind(),
                    )
                    .expect("TODO");
                }
            }
        }
    }
}

#[deprecated(note = "use JSFunctionDef::add_func_var() instead")]
pub unsafe fn add_func_var(ctx: &mut JSContext, fd: *mut JSFunctionDef, name: JSAtom) -> i32 {
    (*fd).add_func_var(ctx, name).map_or(-1, |x| x as i32)

    // let idx = if (*fd).func_var_idx < 0 {
    //     (*fd).add_var(ctx, name).unwrap_or(-1)
    // } else {
    //     (*fd).func_var_idx
    // };

    // if idx >= 0 {
    //     (*fd).func_var_idx = idx;
    //     (*(*fd).vars.add(idx as usize)).set_var_kind(JSVarKindEnum::JS_VAR_FUNCTION_NAME as _);
    //     if (*fd).js_mode & JS_MODE_STRICT as u8 != 0 {
    //         (*(*fd).vars.add(idx as usize)).set_is_const(true);
    //     }
    // }

    // idx
}

#[deprecated(note = "use JSFunctionDef::add_var_this() instead")]
pub unsafe fn add_var_this(ctx: &mut JSContext, fd: *mut JSFunctionDef) -> i32 {
    (*fd).add_var_this(ctx).map_or(-1, |x| x as i32)
}

#[deprecated(note = "use JSFunctionDef::add_arguments() instead")]
pub unsafe fn add_arguments_var(ctx: &mut JSContext, fd: *mut JSFunctionDef) -> i32 {
    (*fd).add_arguments(ctx, false).map_or(-1, |x| x as i32)
}

// add an argument definition in the argument scope. Only needed when
// "eval()" may be called in the argument scope. Return 0 if OK.
#[deprecated(note = "use JSFunctionDef::add_arguments() instead")]
pub unsafe fn add_arguments_arg(ctx: &mut JSContext, fd: &mut JSFunctionDef) -> i32 {
    (*fd).add_arguments(ctx, true).map_or(-1, |x| x as i32)
}

/// find a variable declaration in a given scope
pub unsafe fn find_var_in_scope(
    ctx: &JSContext,
    fd: &mut JSFunctionDef,
    name: JSAtom,
    scope_level: i32,
) -> Option<u16> {
    debug_assert!(scope_level >= 0);

    let mut scope_idx = fd.scopes[scope_level as usize].first;
    while scope_idx >= 0 {
        let vd = fd.vars.get(scope_idx as usize).unwrap();
        if vd.scope_level != scope_level {
            break;
        } else if vd.var_name == name {
            return Some(scope_idx as u16);
        }
        scope_idx = (*vd).scope_next;
    }

    None
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

pub unsafe fn get_label_pos(s: *mut JSFunctionDef, mut label: i32) -> i32 {
    let mut pos: i32 = 0;

    for _ in 0..20 {
        pos = (*(*s).label_slots.add(label as usize)).pos;

        loop {
            match (*s).byte_code[pos as usize] {
                opcodes::op_line_num | opcodes::op_label => {
                    pos += 5;
                    continue;
                }
                opcodes::OP_goto => {
                    label = get_u32((*s).byte_code.as_ptr().add(pos as usize + 1)) as i32;
                }
                _ => {
                    return pos;
                }
            }

            break; // loop
        }
    }

    pos
}

pub unsafe fn skip_dead_code(
    s: *mut JSFunctionDef,
    bc_buf: *const u8,
    bc_len: usize,
    pos: usize,
    linep: *mut i32,
) -> usize {
    let mut current_pos = pos;

    while current_pos < bc_len {
        let op = *bc_buf.add(current_pos as usize);
        let oi = opcode_info_intermediate(op);
        let len = oi.size;

        if op == opcodes::op_line_num {
            *linep = get_u32(bc_buf.add(current_pos as usize + 1)) as i32;
        } else if op == opcodes::op_label {
            let label = get_u32(bc_buf.add(current_pos as usize + 1));
            if (*s).update_label_ref_count(label, 0) > 0 {
                break;
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

            debug_assert!((*(*s).label_slots.add(label as usize))
                .first_reloc
                .is_null());
        } else {
            // XXX: output a warning for unreachable code?
            match oi.fmt {
                "label" | "label_u16" => {
                    let label = get_u32(bc_buf.add(current_pos as usize + 1));
                    (*s).update_label_ref_count(label, -1);
                }
                f @ ("atom_label_u8" | "atom_label_u16" | "atom" | "atom_u8" | "atom_u16") => {
                    if matches!(f, "atom_label_u8" | "atom_label_u16") {
                        let label = get_u32(bc_buf.add(current_pos as usize + 5));
                        (*s).update_label_ref_count(label, -1);
                    }

                    let atom = JSAtom(get_u32(bc_buf.add(current_pos as usize + 1)));
                    JS_FreeAtom((*s).ctx, atom);
                }
                _ => {}
            }
        }

        current_pos += len as usize;
    }

    current_pos
}

pub unsafe fn get_loc_or_ref(bc: &mut Vec<u8>, is_ref: bool, idx: i32) {
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

// return 0 if OK or -1 if the private field could not be resolved
pub unsafe fn resolve_scope_private_field(
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
                    bc.extend_from_slice(JS_DupAtom(ctx, var_name).0.to_ne_bytes().as_slice());
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
                    bc.extend_from_slice(JS_DupAtom(ctx, var_name).0.to_ne_bytes().as_slice());
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
pub unsafe fn find_private_class_field_all(
    ctx: &JSContext,
    fd: *mut JSFunctionDef,
    name: JSAtom,
    scope_level: i32,
) -> i32 {
    debug_assert!(scope_level >= 0);

    let mut idx = (*fd).scopes[scope_level as usize].first;
    while idx >= 0 {
        if (*fd).vars.get(idx as usize).unwrap().var_name == name {
            return idx;
        }
        idx = (*fd).vars.get(idx as usize).unwrap().scope_next;
    }

    return -1;
}

pub unsafe fn resolve_scope_private_field1(
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
        idx = find_private_class_field_all(ctx, fd, var_name, scope_level);
        if idx >= 0 {
            var_kind = (*fd).vars.get(idx as usize).unwrap().var_kind();
            if is_ref {
                idx = if let Ok(vi) = (*s).get_closure_var2(
                    ctx,
                    fd,
                    true,
                    false,
                    idx as u16,
                    var_name,
                    true,
                    true,
                    JSVarKindEnum::JS_VAR_NORMAL,
                ) {
                    vi as i32
                } else {
                    return -1;
                }
            }
            break 'outer;
        }

        scope_level = (*fd).parent_scope_level;
        if (*fd).parent.is_null() {
            if (*fd).is_eval {
                // closure of the eval function (top level)
                if let Some(cvars) = (*fd).closure_vars.as_ref() {
                    for (i, cv) in cvars.iter().enumerate() {
                        if cv.var_name == var_name {
                            var_kind = cv.var_kind();
                            is_ref = true;

                            if fd != s {
                                idx = if let Ok(vi) = (*s).get_closure_var2(
                                    ctx,
                                    fd,
                                    false,
                                    (*cv).is_arg(),
                                    i as _,
                                    cv.var_name,
                                    cv.is_const(),
                                    cv.is_lexical(),
                                    cv.var_kind(),
                                ) {
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
            fd = (*fd).parent;
        }

        is_ref = true;
    }

    // done:
    *pis_ref = is_ref;
    *pvar_kind = var_kind;
    idx
}

/* build a private setter function name from the private getter name */
pub unsafe fn get_private_setter_name(ctx: *mut JSContext, name: JSAtom) -> JSAtom {
    js_atom_concat_str(ctx, name, c"<set>".as_ptr().cast())
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
        (*s).jump_size += 1;
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
        (*(*s).label_slots.offset(l as isize)).pos2 = bc.len() as i32;
    }

    (*s).global_vars.clear();
}
