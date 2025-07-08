use std::{cell::RefCell, ops::DerefMut, rc::Rc};

use crate::{
    atom::{JS_ATOM_NULL, consts::*},
    context::{JS_MODE_STRICT, JS_MODE_STRIP, JSContext},
    eval::{JS_EVAL_TYPE_DIRECT, JS_EVAL_TYPE_INDIRECT},
    function::{
        ARG_SCOPE_END, ARG_SCOPE_INDEX, Bytecode, BytecodeDebugInfo, BytecodeInner, JSClosureVar,
        JSVarDef, JSVarKindEnum, is_var_in_arg_scope, resolve_labels,
    },
    gc::GCObjectHeader,
    module::add_module_variables,
    object::GCObjectWeakref,
    value::JSValue,
};

use super::JSFunctionDef;

impl JSFunctionDef {
    /// Compile function def and returns result bytecode.
    /// All the child functions are also created. It must be done this way to resolve all the variables.
    /// Returns a bytecode value.
    ///
    /// Note: receiver must be `Box<Self>`, and is consumed.
    pub unsafe fn compile(mut self: Box<Self>, ctx: &JSContext) -> Result<Bytecode, i32> {
        //
        // recompute scope linkage
        //

        for scope in self.scopes.iter_mut() {
            scope.first = -1;
        }

        if self.has_parameter_expressions {
            // special end of variable list marker for the argument scope
            self.scopes[ARG_SCOPE_INDEX as usize].first = ARG_SCOPE_END;
        }

        for (var_idx, vd) in self.vars.iter_mut().enumerate() {
            debug_assert!((0..self.scopes.len() as i32).contains(&vd.scope_level));

            let scope = self.scopes.get_mut(vd.scope_level as usize).unwrap();
            vd.scope_next = scope.first;
            scope.first = var_idx as i32;
        }

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

        // if the function contains an eval call, the closure variables
        // are used to compile the eval and they must be ordered by scope,
        // so it is necessary to create the closure variables before any
        // other variable lookup is done.
        if self.has_eval_call {
            add_eval_variables(ctx, &mut self);
        }

        // add the module global variables in the closure
        if self.module.is_some() {
            if add_module_variables(ctx, &mut self) != 0 {
                return Err(-1);
            }
        }

        // first create all child functions
        for fd1 in self.child_list.drain(..) {
            let fd1 = Box::from_raw(fd1.as_ptr());

            let cpool_idx: i32 = fd1.parent_cpool_idx;
            debug_assert!((0..self.cpool.len() as i32).contains(&cpool_idx));

            let sub_bc = fd1.compile(ctx)?;

            // put child function to parent's constant pool
            self.cpool[cpool_idx as usize] = JSValue::Bytecode(sub_bc);
        }

        #[cfg(feature = "dump_bytecode")]
        self.dump_bytecode(ctx, 1);

        // pass 2
        self.resolve_variables(ctx)?;

        #[cfg(feature = "dump_bytecode")]
        self.dump_bytecode(ctx, 2);

        // pass 3
        resolve_labels(ctx, self.deref_mut())?;

        let stack_size = self.compute_stack_size(ctx)?;

        let mut inner = BytecodeInner {
            header: GCObjectHeader::new(ctx.rt_mut(), 1),
            func_name: self.func_name,
            realm: ctx.downgrade(),

            js_mode: self.js_mode,
            has_prototype: self.has_prototype,
            has_simple_parameter_list: self.has_simple_parameter_list,
            is_derived_class_constructor: self.is_derived_class_constructor,
            func_kind: self.func_kind,
            new_target_allowed: self.new_target_allowed,
            super_call_allowed: self.super_call_allowed,
            super_allowed: self.super_allowed,
            arguments_allowed: self.arguments_allowed,
            backtrace_barrier: self.backtrace_barrier,
            defined_arg_count: self.defined_arg_count,
            need_home_object: self.home_object_var_idx.is_some() || self.need_home_object,
            is_direct_or_indirect_eval: matches!(
                self.eval_type,
                JS_EVAL_TYPE_DIRECT | JS_EVAL_TYPE_INDIRECT
            ),

            read_only_bytecode: false,
            stack_size,

            has_debug: false,
            debug: BytecodeDebugInfo::default(),

            cpool_count: self.cpool.len() as _,
            arg_count: self.args.len() as _,
            var_count: self.vars.len() as _,
            closure_var_count: self.closure_vars.as_ref().map_or(0, |x| x.len()) as _,

            binary: Vec::new().into_boxed_slice(), // empty
            cpool_start: -1,
            vardefs_start: -1,
            closure_var_start: -1,
            opcodes_start: 0,
        };

        // cpool start
        let mut bin_size = if inner.cpool_count != 0 {
            inner.cpool_start = 0;
            inner.cpool_count as usize * std::mem::size_of::<JSValue>()
        } else {
            0
        };

        // vardefs (arg+var) start
        if self.js_mode & JS_MODE_STRIP as u8 == 0 || self.has_eval_call {
            let n: usize = self.args.len() + self.vars.len();
            if n != 0 {
                const ly: std::alloc::Layout = std::alloc::Layout::new::<JSVarDef>();
                let mut i = bin_size;
                let m = i % ly.align();
                if m != 0 {
                    let gap = ly.align() - m;
                    i += gap;
                    bin_size += gap;
                }
                inner.vardefs_start = i as i32;
                bin_size += n * ly.size();
            }
        }

        // closure vars start
        if inner.closure_var_count != 0 {
            const ly: std::alloc::Layout = std::alloc::Layout::new::<JSClosureVar>();
            let mut i = bin_size;
            let m = i % ly.align();
            if m != 0 {
                let gap = ly.align() - m;
                i += gap;
                bin_size += gap;
            }
            bin_size += inner.closure_var_count as usize * ly.size();
            inner.closure_var_start = i as i32;
        }

        // opcodes section
        inner.opcodes_start = bin_size as u32;
        bin_size += self.byte_code.len();

        debug_assert!(bin_size > 0);

        // alloc binary buffer
        let mut bin = Vec::<u8>::new();
        bin.try_reserve_exact(bin_size).map_err(|_| {
            ctx.JS_ThrowOutOfMemory();
            -1
        })?;

        // initialize buf with zero: don't change order.
        bin.set_len(bin_size);
        bin.fill(0);

        // 1. cpool section
        if inner.cpool_start >= 0 {
            // migrate cpool values to function bytecode binary.
            let mut dst = bin
                .as_mut_ptr()
                .add(inner.cpool_start as usize)
                .cast::<JSValue>();

            for v in self.cpool.drain(..) {
                *dst = v;
                dst = dst.add(1);
            }
        }

        // 2. vardef section
        if inner.vardefs_start >= 0 {
            if inner.arg_count != 0 {
                std::ptr::copy_nonoverlapping(
                    self.args.as_ptr(),
                    bin.as_mut_ptr()
                        .add(inner.vardefs_start as usize)
                        .cast::<JSVarDef>(),
                    inner.arg_count as usize,
                );
            }
            if inner.var_count != 0 {
                std::ptr::copy_nonoverlapping(
                    self.vars.as_ptr(),
                    bin.as_mut_ptr()
                        .add(
                            inner.vardefs_start as usize
                                + inner.arg_count as usize * std::mem::size_of::<JSVarDef>(),
                        )
                        .cast::<JSVarDef>(),
                    inner.var_count as usize,
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
        if inner.closure_var_start >= 0 {
            let cvars = self.closure_vars.take().unwrap();
            inner.closure_var_count = cvars.len() as _;
            debug_assert!(inner.closure_var_count < u16::MAX as _);

            let mut ptr = bin
                .as_mut_ptr()
                .add(inner.closure_var_start as usize)
                .cast::<JSClosureVar>();

            for cv in cvars {
                ptr.write(cv);
                ptr = ptr.add(1);
            }
        }

        // 4. opcodes section
        debug_assert_eq!(
            inner.opcodes_start as usize + self.byte_code.len(),
            bin.len()
        );
        std::ptr::copy_nonoverlapping(
            self.byte_code.as_ptr(),
            bin.as_mut_ptr().add(inner.opcodes_start as usize),
            self.byte_code.len(),
        );
        // SAFEGUARD: clear bytecode, so that js_free_function_def() will not free atoms in it.
        self.byte_code.clear();

        inner.binary = bin.into_boxed_slice();
        debug_assert_eq!(inner.binary.len(), bin_size);

        // SAFEGUARD: to prevent free atom in js_free_function_def()
        self.func_name = JS_ATOM_NULL;

        if self.js_mode & JS_MODE_STRIP as u8 == 0 {
            inner.has_debug = true;
            inner.debug.line_num = self.line_num;
            inner.debug.filename = self.filename;

            // SAFEGUARD: to prevent free in js_free_function_def()
            self.filename = JS_ATOM_NULL;

            // move source and pc2line to func object.
            inner.debug.source = self.source.take();
            if let Some(mut lst) = self.pc2line.take() {
                lst.shrink_to_fit();
                inner.debug.pc2line_buf.replace(lst);
            }
        }

        let bc = Bytecode(Rc::new(RefCell::new(inner)));

        #[cfg(feature = "dump_bytecode")]
        if self.js_mode & JS_MODE_STRIP as u8 == 0 {
            crate::bytecode::js_dump_function_bytecode(ctx, &bc);
        }

        ctx.rt_mut()
            .gc_add_object(bc.id(), GCObjectWeakref::FunctionBytecode(bc.downgrade()));

        Ok(bc)
    }
}

pub unsafe fn add_eval_variables(ctx: &JSContext, s: &mut JSFunctionDef) {
    // in non strict mode, variables are created in the caller's environment object
    if !s.is_eval && s.js_mode & JS_MODE_STRICT as u8 == 0 {
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
    let mut cur_fd = s as *mut JSFunctionDef;
    loop {
        let scope_level = (*cur_fd).parent_scope_level;

        if let Some(p) = (*cur_fd).parent.as_mut() {
            cur_fd = p;
        } else {
            break;
        }

        // add 'this' if it was not previously added
        if !has_this_binding && (*cur_fd).has_this_binding {
            if (*cur_fd).this_var_idx.is_none() {
                (*cur_fd)
                    .this_var_idx
                    .replace((*cur_fd).add_var_this(ctx).unwrap());
            }
            if (*cur_fd).new_target_var_idx.is_none() {
                (*cur_fd)
                    .new_target_var_idx
                    .replace((*cur_fd).add_var(ctx, JS_ATOM_new_target).expect("TODO"));
            }
            if (*cur_fd).is_derived_class_constructor
                && (*cur_fd).this_active_func_var_idx.is_none()
            {
                (*cur_fd).this_active_func_var_idx.replace(
                    (*cur_fd)
                        .add_var(ctx, JS_ATOM_this_active_func)
                        .expect("TODO"),
                );
            }
            if (*cur_fd).has_home_object && (*cur_fd).home_object_var_idx.is_none() {
                (*cur_fd)
                    .home_object_var_idx
                    .replace((*cur_fd).add_var(ctx, JS_ATOM__home_object_).expect("TODO"));
            }
            has_this_binding = true;
        }

        // add 'arguments' if it was not previously added
        if !has_arguments_binding && (*cur_fd).has_arguments_binding {
            (*cur_fd).add_arguments(ctx, false).unwrap();
            has_arguments_binding = true;
        }

        // add function name
        if (*cur_fd).is_func_expr && (*cur_fd).func_name != JS_ATOM_NULL {
            (*cur_fd).add_func_var(ctx, (*cur_fd).func_name).unwrap();
        }

        // add lexical variables
        debug_assert!((0..(*cur_fd).scopes.len() as i32).contains(&scope_level));
        let mut scope_idx = (*cur_fd).scopes[scope_level as usize].first;
        while scope_idx >= 0 {
            let vd = (*cur_fd).vars.get_mut(scope_idx as usize).unwrap();
            vd.set_is_captured(true);
            s.get_closure_var2(
                ctx,
                cur_fd,
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
            for (i, vd) in (*cur_fd).args.iter().enumerate() {
                if vd.var_name != JS_ATOM_NULL {
                    s.get_closure_var2(
                        ctx,
                        cur_fd,
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

            for (i, vd) in (*cur_fd).vars.iter().enumerate() {
                // do not close top level last result
                if vd.scope_level == 0 && !matches!(vd.var_name, JS_ATOM__ret_ | JS_ATOM_NULL) {
                    s.get_closure_var2(
                        ctx,
                        cur_fd,
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
            for (i, vd) in (*cur_fd).vars.iter().enumerate() {
                // do not close top level last result
                if vd.scope_level == 0 && is_var_in_arg_scope(vd) {
                    s.get_closure_var2(
                        ctx,
                        cur_fd,
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

        if (*cur_fd).is_eval {
            // add direct eval variables (we are necessarily at the top level)
            if let Some(cvars) = (*cur_fd).closure_vars.as_ref() {
                for (i, cv) in cvars.iter().enumerate() {
                    s.get_closure_var2(
                        ctx,
                        cur_fd,
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
