use crate::{
    atom::{
        JS_FreeAtom, JSAtom,
        consts::{JS_ATOM__star_, JS_ATOM_default},
    },
    branch_hints::unlikely,
    context::JSContext,
    module::{
        JSExportEntry, JSExportTypeEnum, JSModuleStatus, JSResolveEntry, ModuleDef, ModuleDefInner,
        js_create_module_var,
    },
    runtime::js_check_stack_overflow,
    value::JSValue,
};

pub enum ModuleResolveExport {
    Found {
        module: ModuleDef,
        entry: JSExportEntry,
    },
    NotFound,
    Ambiguous,
    Circular,
}

impl ModuleResolveExport {
    pub(super) fn throw_error_inner(
        &self,
        ctx: &JSContext,
        m: &ModuleDefInner,
        export_name: JSAtom,
    ) {
        match self {
            Self::Found { .. } => {}
            Self::NotFound => {
                ctx.throw_syntax_error(&format!(
                    "Could not find export '{}' in module '{}'",
                    export_name.to_utf8(ctx.rt()),
                    m.module_name.to_utf8(ctx.rt()),
                ));
            }
            Self::Circular => {
                ctx.throw_syntax_error(&format!(
                    "circular reference when looking for export '{}' in module '{}'",
                    export_name.to_utf8(ctx.rt()),
                    m.module_name.to_utf8(ctx.rt()),
                ));
            }
            Self::Ambiguous => {
                ctx.throw_syntax_error(&format!(
                    "export '{}' in module '{}' is ambiguous",
                    export_name.to_utf8(ctx.rt()),
                    m.module_name.to_utf8(ctx.rt()),
                ));
            }
        }
    }

    /// put exception to current_exception according to status
    pub fn throw_error(&self, ctx: &JSContext, m: &ModuleDef, export_name: JSAtom) {
        self.throw_error_inner(ctx, &m.0, export_name);
    }
}

impl ModuleDef {
    unsafe fn resolve_export_inner(
        &self,
        ctx: &JSContext,
        export_name: JSAtom,
        expect: Option<(&ModuleDef, JSAtom)>, // (module, local_name)
        resolved: &mut Vec<JSResolveEntry>,
    ) -> Result<ModuleResolveExport, i32> {
        if resolved
            .iter()
            .find(|re| re.module.id() == self.id() && re.name == export_name)
            .is_some()
        {
            return Ok(ModuleResolveExport::Circular);
        }

        // add resolve
        if resolved.len() == resolved.capacity() {
            resolved.try_reserve(4).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
        resolved.push(JSResolveEntry {
            module: self.clone(),
            name: export_name.dup(&ctx.rt()),
        });

        if let Some(me) = self.find_export(export_name) {
            match me.export_type {
                JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { .. } => Ok(ModuleResolveExport::Found {
                    module: self.clone(),
                    entry: me.clone(),
                }),
                JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                    if me.local_name == JS_ATOM__star_ {
                        // export ns from
                        Ok(ModuleResolveExport::Found {
                            module: self.clone(),
                            entry: me.clone(),
                        })
                    } else {
                        let m1 = self.0.dependencies.borrow()[req_module_idx as usize]
                            .module
                            .clone()
                            .unwrap();

                        m1.resolve_export_inner(ctx, me.local_name, expect, resolved)
                    }
                }
            }
        } else {
            if export_name != JS_ATOM_default {
                // not found in direct or indirect exports: try star exports
                for idx in self.0.star_export_entries.borrow().iter() {
                    let m1 = self.0.dependencies.borrow()[*idx as usize]
                        .module
                        .clone()
                        .unwrap();

                    match m1.resolve_export_inner(ctx, export_name, expect, resolved)? {
                        ModuleResolveExport::Found { module, entry } => {
                            return Ok(
                                if let Some((test_module, test_local_name)) = expect
                                    && (test_local_name != entry.local_name
                                        || test_module.id() != module.id())
                                {
                                    ModuleResolveExport::Ambiguous
                                } else {
                                    ModuleResolveExport::Found { module, entry }
                                },
                            );
                        }
                        ModuleResolveExport::NotFound | ModuleResolveExport::Circular => {
                            return Ok(ModuleResolveExport::NotFound);
                        }
                        ModuleResolveExport::Ambiguous => {
                            return Ok(ModuleResolveExport::Ambiguous);
                        }
                    }
                }
            }

            Ok(ModuleResolveExport::NotFound)
        }
    }

    pub fn resolve_export(
        &self,
        ctx: &JSContext,
        export_name: JSAtom,
        expect: Option<(&ModuleDef, JSAtom)>, // expected (module, local_name)
    ) -> Result<ModuleResolveExport, i32> {
        let mut lst = Vec::<JSResolveEntry>::with_capacity(8);
        let ret = unsafe { self.resolve_export_inner(ctx, export_name, expect, &mut lst) };
        for ent in lst.drain(..) {
            JS_FreeAtom(ctx, ent.name);
        }
        ret
    }

    /// Prepare a module to be executed by resolving all the imported variables.
    #[inline(always)]
    fn inner_module_linking(
        &self,
        ctx: &JSContext,
        stack_top: &mut Option<ModuleDef>,
        index: i32,
    ) -> Result<i32, i32> {
        self.0
            .inner_module_linking(ctx, self.clone(), stack_top, index)
    }

    /// Prepare a module to be executed by resolving all the imported variables.
    /// 准备模块执行前的链接工作，即解析和绑定所有导入的变量。
    /// - 模块依赖链接：解析模块之间的依赖关系，将导入和导出绑定起来
    /// - 变量引用解析：建立导入变量与导出变量之间的引用关系
    /// - 循环依赖处理：使用深度优先搜索(DFS)算法处理模块间的循环依赖
    /// - 状态管理：管理模块的链接状态转换
    pub(super) fn link(&self, ctx: &JSContext) -> Result<(), i32> {
        debug_assert!(matches!(
            self.status(),
            JSModuleStatus::JS_MODULE_STATUS_UNLINKED
                | JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));

        let mut stack_top: Option<ModuleDef> = None;
        let r = self.inner_module_linking(ctx, &mut stack_top, 0);

        if unlikely(r.is_err()) {
            while let Some(m1) = stack_top.take() {
                debug_assert_eq!(m1.status(), JSModuleStatus::JS_MODULE_STATUS_LINKING);
                m1.0.status.set(JSModuleStatus::JS_MODULE_STATUS_UNLINKED);
                stack_top = m1.0.stack_prev.take();
            }
            Err(-1)
        } else {
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
}

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

                    if import_name == JS_ATOM__star_ {
                        // name space import
                        let val = dep_m.ns(ctx)?;
                        var_refs[import_var_idx as usize].set_value(val.into());
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
                                                    .as_object_unchecked()
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
            JSValue::Object(func_obj).call(ctx, &JSValue::Bool(true), &[])?;
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
