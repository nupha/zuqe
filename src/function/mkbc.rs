use std::rc::Rc;

use crate::{
    atom::{JS_ATOM_NULL, consts::*},
    context::{JS_MODE_STRICT, JS_MODE_STRIP, JSContext},
    function::{
        ARG_SCOPE_END, ARG_SCOPE_INDEX, Bytecode, BytecodeInner, JSClosureVar, JSVarDef,
        JSVarKindEnum, is_var_in_arg_scope,
    },
    module::add_module_variables,
    object::GCObjectWeakref,
    value::JSValue,
};

use super::JSFunctionDef;

impl JSFunctionDef {
    /// Compile function def to bytecode, returns the Bytecode.
    ///
    /// All the child functions are also created. It must be done this way to resolve all the variables.
    pub fn compile(mut self: Box<Self>, ctx: &JSContext) -> Result<Bytecode, i32> {
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
            self.add_eval_variables(ctx);
        }

        // add the module global variables in the closure
        if self.module.is_some() {
            if unsafe { add_module_variables(ctx, &mut self) } != 0 {
                return Err(-1);
            }
        }

        // compile child functions.
        // IMPORTANT: must consume all children fds, even compile a child function has failure.
        let mut has_err = false;
        for fd1 in self.child_list.drain(..) {
            // restore children fd ownership: allow auto drop after use.
            let sub_fd = unsafe { Box::from_raw(fd1.as_ptr()) };

            if !has_err {
                let cpool_idx: i32 = sub_fd.parent_cpool_idx;
                debug_assert!((0..self.cpool.len() as i32).contains(&cpool_idx));

                if let Ok(sub_bc) = sub_fd.compile(ctx) {
                    // strore child function to parent's constant pool slot
                    self.cpool[cpool_idx as usize] = JSValue::Bytecode(sub_bc);
                } else {
                    has_err = true;
                }
            }
        }
        debug_assert!(
            self.child_list.is_empty(),
            "must consume all children function defs"
        );
        if has_err {
            return Err(-1);
        }

        #[cfg(feature = "dump_bytecode")]
        self.dump_bytecode(ctx, 1);

        // pass 2
        self.resolve_variables(ctx)?;

        #[cfg(feature = "dump_bytecode")]
        self.dump_bytecode(ctx, 2);

        // pass 3
        self.resolve_labels(ctx)?;

        let stack_size = self.compute_stack_size(ctx)?;

        //
        // make binary
        //

        // cpool start
        let cpool_count = self.cpool.len();
        let (mut bin_size, cpool_start) = if cpool_count != 0 {
            (cpool_count * std::mem::size_of::<JSValue>(), 0)
        } else {
            (0, -1)
        };

        // vardefs (arg+var) start
        let mut vardefs_start = -1;
        let args_count = self.args.len();
        let vars_count = self.vars.len();
        if self.attrs.js_mode & JS_MODE_STRIP as u8 == 0 || self.has_eval_call {
            let n: usize = args_count + vars_count;
            if n != 0 {
                const ly: std::alloc::Layout = std::alloc::Layout::new::<JSVarDef>();
                let mut i = bin_size;
                let m = i % ly.align();
                if m != 0 {
                    let gap = ly.align() - m;
                    i += gap;
                    bin_size += gap;
                }
                vardefs_start = i as i32;
                bin_size += n * ly.size();
            }
        }

        // closure vars
        let mut closure_var_start = -1;
        let closure_var_count = self.closure_vars.as_ref().map_or(0, |x| x.len());
        if closure_var_count != 0 {
            const ly: std::alloc::Layout = std::alloc::Layout::new::<JSClosureVar>();
            let mut i = bin_size;
            let m = i % ly.align();
            if m != 0 {
                let gap = ly.align() - m;
                i += gap;
                bin_size += gap;
            }
            bin_size += closure_var_count as usize * ly.size();
            closure_var_start = i as i32;
        }

        // opcodes section
        let opcodes_start = bin_size as u32;
        bin_size += self.byte_code.len();

        debug_assert!(bin_size > 0);

        // alloc binary buffer
        let mut bin = Vec::<u8>::new();
        bin.try_reserve_exact(bin_size).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        unsafe {
            // initialize buf with zero: don't change order.
            bin.set_len(bin_size);
            bin.fill(0);
        }

        // 1. cpool section
        if cpool_start >= 0 {
            // migrate cpool values to function bytecode binary.
            unsafe {
                let mut dst = bin.as_mut_ptr().add(cpool_start as usize).cast::<JSValue>();
                for v in self.cpool.drain(..) {
                    *dst = v;
                    dst = dst.add(1);
                }
            }
        }

        // 2. vardef section
        if vardefs_start >= 0 {
            if args_count != 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.args.as_ptr(),
                        bin.as_mut_ptr()
                            .add(vardefs_start as usize)
                            .cast::<JSVarDef>(),
                        args_count,
                    );
                }
            }
            if vars_count != 0 {
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        self.vars.as_ptr(),
                        bin.as_mut_ptr()
                            .add(
                                vardefs_start as usize
                                    + args_count * std::mem::size_of::<JSVarDef>(),
                            )
                            .cast::<JSVarDef>(),
                        vars_count,
                    );
                }
            }

            // IMPORTANT:
            // vardefs have been copied to bin, we must clean args, vars list here,
            // so that when calling JSFunctionDef::finalize() in ::drop(),
            // JSVarDef::name atom won't free twice.
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
        if closure_var_start >= 0 {
            debug_assert!(closure_var_count < u16::MAX as _);

            let cvars = self.closure_vars.take().unwrap();
            unsafe {
                let mut ptr = bin
                    .as_mut_ptr()
                    .add(closure_var_start as usize)
                    .cast::<JSClosureVar>();

                for cv in cvars {
                    ptr.write(cv);
                    ptr = ptr.add(1);
                }
            }
        }

        // 4. opcodes section
        debug_assert_eq!(opcodes_start as usize + self.byte_code.len(), bin.len());
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.byte_code.as_ptr(),
                bin.as_mut_ptr().add(opcodes_start as usize),
                self.byte_code.len(),
            );
        }

        // SAFEGUARD: clear bytecode, so that JSFunctionDef::finalize() will not free atoms in it.
        self.byte_code.clear();
        debug_assert_eq!(bin.len(), bin_size);

        // create bytecode
        let mut inner = BytecodeInner::new(
            ctx,
            &self,
            bin.into(),
            cpool_start,
            cpool_count as _,
            vardefs_start,
            args_count as _,
            vars_count as _,
            closure_var_start,
            closure_var_count as _,
            opcodes_start,
            stack_size,
        );

        // SAFEGUARD: atom self.func_name was transferred to BytecodeInner::func_name field,
        // reset self.func_name to prevent free this atom in JSFunctionDef::finalize()
        self.func_name = JS_ATOM_NULL;

        if self.attrs.js_mode & JS_MODE_STRIP as u8 == 0 {
            inner.has_debug = true;
            inner.debug.line_num = self.line_num;
            inner.debug.filename = self.filename;

            // move source and pc2line to func object.
            inner.debug.source = self.source.take();
            if let Some(mut lst) = self.pc2line.take() {
                lst.shrink_to_fit();
                inner.debug.pc2line_buf.replace(lst);
            }

            // SAFEGUARD: reset to prevent free atom in JSFunctionDef::finalize()
            self.filename = JS_ATOM_NULL;
        }

        let bc: Bytecode = Rc::new(inner).into();

        #[cfg(feature = "dump_bytecode")]
        if self.attrs.js_mode & JS_MODE_STRIP as u8 == 0 {
            bc.dump(ctx);
        }

        ctx.rt_mut()
            .gc_add_object(bc.id(), GCObjectWeakref::Bytecode(bc.downgrade()));

        Ok(bc)
    }

    fn add_eval_variables(&mut self, ctx: &JSContext) {
        // in non strict mode, variables are created in the caller's environment object
        if !self.is_eval && self.attrs.js_mode & JS_MODE_STRICT as u8 == 0 {
            self.var_object_idx = Some(self.add_var(ctx, JS_ATOM__var_).unwrap());
            if self.has_parameter_expressions {
                // an additional variable object is needed for the argument scope
                self.arg_var_object_idx = Some(self.add_var(ctx, JS_ATOM__arg_var_).unwrap());
            }
        }

        // eval can potentially use 'arguments' so we must define it
        let mut has_this_binding = self.has_this_binding;
        if has_this_binding {
            if self.this_var_idx.is_none() {
                self.this_var_idx = Some(self.add_var_this(ctx).unwrap());
            }
            if self.new_target_var_idx.is_none() {
                self.new_target_var_idx =
                    Some(self.add_var(ctx, JS_ATOM_new_target).expect("TODO"));
            }
            if self.attrs.is_derived_class_constructor && self.this_active_func_var_idx.is_none() {
                self.this_active_func_var_idx =
                    Some(self.add_var(ctx, JS_ATOM_this_active_func).expect("TODO"));
            }
            if self.has_home_object && (*self).home_object_var_idx.is_none() {
                self.home_object_var_idx =
                    Some(self.add_var(ctx, JS_ATOM__home_object_).expect("TODO"));
            }
        }

        let mut has_arguments_binding = self.has_arguments_binding;
        if has_arguments_binding {
            self.add_arguments(ctx, false).unwrap();

            // also add `arguments` binding in the argument scope.
            // to raise an error if a direct eval in the argument scope tries to redefine it.
            if self.has_parameter_expressions && self.attrs.js_mode & JS_MODE_STRICT as u8 == 0 {
                self.add_arguments(ctx, true).unwrap();
            }
        }

        if self.is_func_expr && self.func_name != JS_ATOM_NULL {
            self.add_func_var(ctx, self.func_name).unwrap();
        }

        // eval can use all the variables of the enclosing functions, so
        // they must be all put in the closure. The closure variables are
        // ordered by scope. It works only because no closure are created
        // before.
        debug_assert!(self.is_eval || self.closure_vars.as_ref().map_or(0, |x| x.len()) == 0);

        // XXX: inefficient, but eval performance is less critical
        // TODO: use parent_iter() iterator
        let mut cur_fd = self as *mut JSFunctionDef;
        let mut scope_level = unsafe { (&*cur_fd).parent_scope_level };

        while let Some(fd) = unsafe { (&*cur_fd).parent.as_mut() } {
            // add 'this' if it was not previously added
            if !has_this_binding && fd.has_this_binding {
                if fd.this_var_idx.is_none() {
                    let i = fd.add_var_this(ctx).unwrap();
                    fd.this_var_idx.replace(i);
                }

                if fd.new_target_var_idx.is_none() {
                    let i = fd.add_var(ctx, JS_ATOM_new_target).expect("TODO");
                    fd.new_target_var_idx.replace(i);
                }

                if fd.attrs.is_derived_class_constructor && fd.this_active_func_var_idx.is_none() {
                    let i = fd.add_var(ctx, JS_ATOM_this_active_func).expect("TODO");
                    fd.this_active_func_var_idx.replace(i);
                }

                if fd.has_home_object && fd.home_object_var_idx.is_none() {
                    let i = fd.add_var(ctx, JS_ATOM__home_object_).expect("TODO");
                    fd.home_object_var_idx.replace(i);
                }

                has_this_binding = true;
            }

            // add 'arguments' if it was not previously added
            if !has_arguments_binding && fd.has_arguments_binding {
                fd.add_arguments(ctx, false).unwrap();
                has_arguments_binding = true;
            }

            // add function name
            if fd.is_func_expr && fd.func_name != JS_ATOM_NULL {
                fd.add_func_var(ctx, fd.func_name).unwrap();
            }

            // add lexical variables
            debug_assert!((0..fd.scopes.len() as i32).contains(&scope_level));
            let mut scope_idx = fd.scopes[scope_level as usize].first;
            while scope_idx >= 0 {
                let fd_ptr = fd as *const JSFunctionDef;

                let vd = fd.vars.get_mut(scope_idx as usize).unwrap();
                vd.set_is_captured(true);
                self.get_closure_var2(
                    ctx,
                    fd_ptr,
                    true,
                    false,
                    scope_idx as u16,
                    vd.var_name,
                    vd.is_const(),
                    vd.is_lexical(),
                    vd.var_kind(),
                )
                .expect("TODO");

                scope_idx = vd.scope_next;
            }

            let is_arg_scope = scope_idx == ARG_SCOPE_END;
            if !is_arg_scope {
                // add unscoped variables
                // XXX: propagate is_const and var_kind too ?
                for (i, vd) in fd.args.iter().enumerate() {
                    if vd.var_name != JS_ATOM_NULL {
                        self.get_closure_var2(
                            ctx,
                            fd as *const JSFunctionDef,
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

                for (i, vd) in fd.vars.iter().enumerate() {
                    // do not close top level last result
                    if vd.scope_level == 0 && !matches!(vd.var_name, JS_ATOM__ret_ | JS_ATOM_NULL) {
                        self.get_closure_var2(
                            ctx,
                            fd as *const JSFunctionDef,
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
                for (i, vd) in fd.vars.iter().enumerate() {
                    // do not close top level last result
                    if vd.scope_level == 0 && is_var_in_arg_scope(vd) {
                        self.get_closure_var2(
                            ctx,
                            fd as *const JSFunctionDef,
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
            }

            if fd.is_eval {
                // add direct eval variables (we are necessarily at the top level)
                if let Some(cvars) = fd.closure_vars.as_ref() {
                    for (i, cv) in cvars.iter().enumerate() {
                        self.get_closure_var2(
                            ctx,
                            fd as *const JSFunctionDef,
                            false,
                            cv.is_arg(),
                            i as _,
                            cv.var_name,
                            cv.is_const(),
                            cv.is_lexical(),
                            cv.var_kind(),
                        )
                        .expect("TODO");
                    }
                }
            }

            scope_level = fd.parent_scope_level;
            cur_fd = fd as *mut JSFunctionDef;
        }
    }
}
