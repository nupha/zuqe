use crate::{
    atom::consts::JS_ATOM__star_, context::JSContext, function::JS_Call,
    runtime::js_check_stack_overflow, value::JSValue,
};

use super::{
    JSExportTypeEnum, JSModuleStatus, ModuleDef, ModuleDefInner, ModuleResolveExport,
    js_create_module_var,
};

impl ModuleDefInner {
    /// Prepare module `self` to be executed by resolving all the imported variables.
    pub(super) fn inner_module_linking(
        &self,
        ctx: &JSContext,
        md: ModuleDef,
        stack_top: &mut Option<ModuleDef>,
        index: i32,
    ) -> Result<i32, i32> {
        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        #[cfg(DUMP_MODULE_RESOLVE)]
        {
            let buf1: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];
            println!(
                "js_inner_module_linking '{}':\n",
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                    JS_AtomGetStr(ctx, buf1.as_mut_ptr(), buf1.len(), (*m).module_name),
                    buf1.len()
                ))
            );
        }

        if matches!(
            self.status.get(),
            JSModuleStatus::JS_MODULE_STATUS_LINKING
                | JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ) {
            return Ok(index);
        }

        debug_assert!(self.status.get() == JSModuleStatus::JS_MODULE_STATUS_UNLINKED);

        self.status.set(JSModuleStatus::JS_MODULE_STATUS_LINKING);
        self.dfs_index.set(index);
        self.dfs_ancestor_index.set(index);

        // push `self` on stack
        self.stack_prev.replace(stack_top.take());
        stack_top.replace(md);

        let mut index = index + 1;

        for dep in self.dependencies.borrow().iter() {
            let dep_m = dep.module.clone().unwrap();
            index = dep_m.inner_module_linking(ctx, stack_top, index)?;
            debug_assert!(index > 0);
            // if index < 0 {
            //     return Err(-1);
            // }

            debug_assert!(matches!(
                dep_m.status(),
                JSModuleStatus::JS_MODULE_STATUS_LINKING
                    | JSModuleStatus::JS_MODULE_STATUS_LINKED
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));

            if dep_m.status() == JSModuleStatus::JS_MODULE_STATUS_LINKING {
                self.dfs_ancestor_index.set(std::cmp::min(
                    self.dfs_ancestor_index.get(),
                    dep_m.0.dfs_ancestor_index.get(),
                ));
            }
        }

        #[cfg(DUMP_MODULE_RESOLVE)]
        {
            let buf1: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];
            println!(
                "instantiating module '{}':\n",
                std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                    JS_AtomGetStr(ctx, buf1.as_mut_ptr(), buf1.len(), (*m).module_name),
                    buf1.len()
                ))
            );
        }

        // check the indirect exports
        for exp_entry in self.export_entries.borrow().iter() {
            if let JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) = exp_entry.export_type
                && exp_entry.local_name != JS_ATOM__star_
            {
                let m1 = self.dependencies.borrow()[req_module_idx as usize]
                    .module
                    .clone()
                    .unwrap();
                let ret = m1.resolve_export(ctx, exp_entry.local_name, None)?;
                if !matches!(ret, ModuleResolveExport::Found { .. }) {
                    ret.throw_error_inner(ctx, self, exp_entry.export_name);
                    return Err(-1);
                }
            }
        }

        #[cfg(DUMP_MODULE_RESOLVE)]
        {
            println!("exported bindings:\n");
            i = 0;
            while i < (*m).export_entries_count {
                let me: *mut JSExportEntry = (*m).export_entries.offset(i as isize);
                print!(" name=");
                print_atom(ctx, (*me).export_name);
                print!(" local=");
                print_atom(ctx, (*me).local_name);
                println!(" type={} idx={}", (*me).export_type, (*me).u.local.var_idx);
                i += 1;
            }
        }

        let bc_val = unsafe { (*self.bytecode.as_ptr()).clone() };
        if let JSValue::Object(func_obj) = bc_val {
            // js js module, and function has been created
            debug_assert!(self.func_created.get());

            // enclose `isa` borrow in scoped block.
            {
                let mut isa = func_obj.0.payload.borrow_mut();
                let var_refs = isa.func_mut().unwrap().var_refs.as_mut_slice();

                for imp_entry in self.import_entries.borrow().iter() {
                    let import_name = imp_entry.import_name;
                    let import_var_idx = imp_entry.var_idx;

                    let dep_m = self.dependencies.borrow()[imp_entry.req_module_idx as usize]
                        .module
                        .clone()
                        .unwrap();

                    #[cfg(DUMP_MODULE_RESOLVE)]
                    {
                        print!("import var_idx={} name=", import_var_idx);
                        print_atom(ctx, import_name);
                        print!(": ");
                    }

                    if import_name == JS_ATOM__star_ {
                        // name space import
                        let val = dep_m.ns(ctx)?;
                        var_refs[import_var_idx as usize].set_value(val.into());

                        #[cfg(DUMP_MODULE_RESOLVE)]
                        {
                            println!("namespace");
                        }
                    } else {
                        match dep_m.resolve_export(ctx, import_name, None)? {
                            ModuleResolveExport::Found { module, entry } => {
                                match entry.export_type {
                                    JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                                        if entry.local_name == JS_ATOM__star_ {
                                            // name space import from
                                            let m2 = module.0.dependencies.borrow()
                                                [req_module_idx as usize]
                                                .module
                                                .clone()
                                                .unwrap();

                                            let val = m2.ns(ctx)?;
                                            let vref = js_create_module_var(ctx, true)?;
                                            vref.set_value(val.into());
                                            var_refs[import_var_idx as usize] = vref;

                                            #[cfg(DUMP_MODULE_RESOLVE)]
                                            {
                                                println!("namespace from");
                                            }
                                        }
                                    }
                                    JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } => {
                                        debug_assert_ne!(entry.local_name, JS_ATOM__star_);
                                        debug_assert_ne!(var_idx, u16::MAX);

                                        var_refs[import_var_idx as usize] = if let Some(v) = var_ref
                                        {
                                            v
                                        } else {
                                            unsafe {
                                                (*module.0.bytecode.as_ptr())
                                                    .object_unchecked()
                                                    .0
                                                    .payload
                                                    .borrow()
                                                    .func()
                                                    .unwrap()
                                                    .var_refs
                                                    [var_idx as usize]
                                                    .clone()
                                            }
                                        };

                                        #[cfg(DUMP_MODULE_RESOLVE)]
                                        {
                                            println!("local export (var_ref={:?})", var_ref);
                                        }
                                    }
                                }
                            }
                            ret => {
                                ret.throw_error(ctx, &dep_m, import_name);
                                return Err(-1);
                            }
                        }
                    }
                }

                // keep the exported variables in the module export entries (they are used when the eval function is deleted and cannot be initialized before in case imports are exported)
                {
                    for me in self.export_entries.borrow_mut().iter_mut() {
                        if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } =
                            &mut me.export_type
                        {
                            debug_assert_ne!(*var_idx, u16::MAX);
                            var_ref.replace(var_refs[*var_idx as usize].clone());
                        }
                    }

                    debug_assert!(self.func_created.get());
                    unsafe { (*self.bytecode.as_ptr()).clone() }
                };
            }

            // initialize the global variables
            JS_Call(ctx, &JSValue::Object(func_obj), &JSValue::Bool(true), &[])
                .error()?;
        }

        let (dfs_ancestor_index, dfs_index, self_id) = {
            (
                self.dfs_ancestor_index.get(),
                self.dfs_index.get(),
                self.id(),
            )
        };
        debug_assert!(dfs_ancestor_index <= dfs_index);

        if dfs_index == dfs_ancestor_index {
            loop {
                // pop from top module from stack
                let m1 = stack_top.take().unwrap();

                *stack_top = m1.0.stack_prev.take();

                m1.0.status.set(JSModuleStatus::JS_MODULE_STATUS_LINKED);
                if m1.id() == self_id {
                    break;
                }
            }
        }

        #[cfg(DUMP_MODULE_RESOLVE)]
        {
            println!("js_inner_module_linking done");
        }

        Ok(index)
    }
}

impl ModuleDef {
    /// Prepare a module to be executed by resolving all the imported variables.
    pub fn inner_module_linking(
        &self,
        ctx: &JSContext,
        stack_top: &mut Option<ModuleDef>,
        index: i32,
    ) -> Result<i32, i32> {
        self.0
            .inner_module_linking(ctx, self.clone(), stack_top, index)
    }

    /// Prepare a module to be executed by resolving all the imported variables.
    pub fn link(&self, ctx: &JSContext) -> Result<(), i32> {
        #[cfg(DUMP_MODULE_RESOLVE)]
        {
            let buf1: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];
            println!(
                "js_link_module '{}':\n",
                JS_AtomGetStr(
                    ctx,
                    buf1.as_mut_ptr(),
                    std::mem::size_of::<[u8; ATOM_GET_STR_BUF_SIZE]>(),
                    (*m).module_name
                )
            );
        }

        debug_assert!(matches!(
            self.status(),
            JSModuleStatus::JS_MODULE_STATUS_UNLINKED
                | JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));

        let mut stack_top: Option<ModuleDef> = None;

        if self.inner_module_linking(ctx, &mut stack_top, 0).is_err() {
            while let Some(m1) = stack_top.take() {
                debug_assert_eq!(m1.status(), JSModuleStatus::JS_MODULE_STATUS_LINKING);
                m1.0.status.set(JSModuleStatus::JS_MODULE_STATUS_UNLINKED);
                stack_top = m1.0.stack_prev.take();
            }
            return Err(-1);
        }

        debug_assert!(stack_top.is_none());
        debug_assert!(matches!(
            self.status(),
            JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));

        Ok(())
    }
}
