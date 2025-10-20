use crate::{
    atom::{
        Atom, AtomRef,
        consts::{JS_ATOM__star_, JS_ATOM_default},
    },
    branch_hints::unlikely,
    context::JSContext,
    function::BCFunction,
    module::{ExportType, JSModuleStatus, ModuleDef, ResolveExport, js_create_module_var},
    runtime::js_check_stack_overflow,
    value::JSValue,
};

impl ModuleDef {
    fn resolve_export_internal(
        &self,
        ctx: &JSContext,
        export_name: &Atom,
        mut expect: Option<(usize, AtomRef)>, // (module_id, local_name_atom)
        resolving: &mut Vec<(usize, AtomRef)>, // (module_id, export_atom), to detect circular
    ) -> Result<ResolveExport, i32> {
        if unlikely(
            resolving
                .iter()
                .any(|x| x.0 == self.id() && x.1 == export_name.as_ref()),
        ) {
            return Ok(ResolveExport::Circular);
        }

        // add resolving
        if resolving.len() == resolving.capacity() {
            resolving.try_reserve(4).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
        resolving.push((self.id(), export_name.as_ref()));

        if let Some(me) = self.find_export(export_name) {
            match me.export_type {
                ExportType::JS_EXPORT_TYPE_LOCAL { .. } => Ok(ResolveExport::Found {
                    module: self.clone(),
                    export_type: me.export_type,
                    export_name: me.export_name,
                    local_name: me.local_name,
                }),
                ExportType::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                    if me.local_name.as_ref() == JS_ATOM__star_ {
                        // export ns from
                        Ok(ResolveExport::Found {
                            module: self.clone(),
                            export_type: me.export_type,
                            export_name: me.export_name,
                            local_name: me.local_name,
                        })
                    } else {
                        let dep = self.0.dependencies.borrow()[req_module_idx as usize]
                            .module
                            .clone()
                            .unwrap();
                        dep.resolve_export_internal(ctx, &me.local_name, expect, resolving)
                    }
                }
            }
        } else {
            let mut ret = ResolveExport::NotFound;

            if export_name.as_ref() != JS_ATOM_default {
                // not found in direct or indirect exports: try star exports
                for idx in self.0.star_export_entries.borrow().iter() {
                    let dep = self.0.dependencies.borrow()[*idx as usize]
                        .module
                        .clone()
                        .unwrap();

                    match dep.resolve_export_internal(ctx, export_name, expect, resolving)? {
                        ResolveExport::Found {
                            module,
                            local_name,
                            export_name,
                            export_type,
                        } => {
                            if expect.map_or(false, |(test_module_id, test_local_name)| {
                                test_module_id != module.id()
                                    || test_local_name != local_name.as_ref()
                            }) {
                                return Ok(ResolveExport::Ambiguous);
                            } else {
                                expect.replace((module.id(), local_name.as_ref()));
                                ret = ResolveExport::Found {
                                    module,
                                    local_name,
                                    export_name,
                                    export_type,
                                };
                            }
                        }
                        r @ (ResolveExport::Ambiguous | ResolveExport::Circular) => {
                            return Ok(r);
                        }
                        ResolveExport::NotFound => {
                            continue;
                        }
                    }
                }
            }

            Ok(ret)
        }
    }

    /// resolve `export_name` in module `self`,
    /// optionally restricted by (expect_module_id, expect_local_name)==(module_id, local_name)
    pub(super) fn resolve_export(
        &self,
        ctx: &JSContext,
        export_name: &Atom,
        expect: Option<(usize, AtomRef)>, // (module_id, local_name_atom)
    ) -> Result<ResolveExport, i32> {
        let mut lst = Vec::<(usize, AtomRef)>::with_capacity(8);
        self.resolve_export_internal(ctx, export_name, expect, &mut lst)
    }

    /// Prepare module `self` to be executed by resolving all the imported variables.
    /// Resolving imports, exports.
    fn inner_module_linking(
        &self,
        ctx: &JSContext,
        stack_top: &mut Option<ModuleDef>,
        index: i32,
    ) -> Result<i32, i32> {
        if unlikely(js_check_stack_overflow(ctx.rt(), 0)) {
            ctx.throw_stack_overflow();
            return Err(-1);
        } else if matches!(
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
        self.set_dfs_index(index);
        self.set_dfs_ancestor_index(index);

        // push `self` on stack
        self.stack_prev.replace(stack_top.take());
        stack_top.replace(self.clone());

        let mut index = index + 1;

        for req in self.dependencies.borrow().iter() {
            let m1 = req.module.clone().unwrap();
            index = m1.inner_module_linking(ctx, stack_top, index)?;

            debug_assert!(matches!(
                m1.status(),
                JSModuleStatus::JS_MODULE_STATUS_LINKING
                    | JSModuleStatus::JS_MODULE_STATUS_LINKED
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));

            if m1.status() == JSModuleStatus::JS_MODULE_STATUS_LINKING {
                self.set_dfs_ancestor_index(std::cmp::min(
                    self.dfs_ancestor_index(),
                    m1.dfs_ancestor_index(),
                ));
            }
        }

        // check the indirect exports
        for exp in self.export_entries.borrow().iter() {
            if let ExportType::JS_EXPORT_TYPE_INDIRECT(req_module_idx) = exp.export_type
                && exp.local_name.as_ref() != JS_ATOM__star_
            {
                let m1 = self.dependencies.borrow()[req_module_idx as usize]
                    .module
                    .clone()
                    .unwrap();

                let ret = m1.resolve_export(ctx, &exp.local_name, None)?;
                if !matches!(ret, ResolveExport::Found { .. }) {
                    ret.throw_syntax_error_export(ctx, self, &exp.export_name);
                    return Err(-1);
                }
            }
        }

        if self.init_func.is_none() {
            // is bytecode module
            debug_assert!(self.func_created.get(), "module function should be created");
            let md_func =
                BCFunction::try_from(unsafe { (*self.bytecode.as_ptr()).clone() }).unwrap();

            for imp in self.import_entries.borrow().iter() {
                let import_var_idx = imp.var_idx;

                let reqm = self.dependencies.borrow()[imp.req_module_idx as usize]
                    .module
                    .clone()
                    .unwrap();

                if imp.import_name.as_ref() == JS_ATOM__star_ {
                    // name space import
                    let val = reqm.ns(ctx)?;
                    md_func.payload().var_refs.borrow_mut()[import_var_idx as usize]
                        .set_value(val.into());
                } else {
                    match reqm.resolve_export(ctx, &imp.import_name, None)? {
                        ResolveExport::Found {
                            module: dep,
                            export_type,
                            local_name,
                            export_name,
                        } => {
                            match export_type {
                                ExportType::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                                    if local_name.as_ref() == JS_ATOM__star_ {
                                        // name space import from
                                        let m2 = dep.0.dependencies.borrow()
                                            [req_module_idx as usize]
                                            .module
                                            .clone()
                                            .unwrap();

                                        let val = m2.ns(ctx)?;
                                        let vref = js_create_module_var(ctx, true)?;
                                        vref.set_value(val.into());
                                        md_func.payload().var_refs.borrow_mut()
                                            [import_var_idx as usize] = vref;
                                    }
                                }
                                ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } => {
                                    debug_assert_ne!(local_name.as_ref(), JS_ATOM__star_);

                                    let vr = var_ref.unwrap_or_else(|| {
                                        debug_assert!(
                                            dep.0.func_created.get(),
                                            "module function must have been created"
                                        );
                                        unsafe {
                                            (*dep.0.bytecode.as_ptr())
                                                .as_object_BCFunction()
                                                .unwrap()
                                                .payload()
                                                .var_refs
                                                .borrow()
                                                [var_idx as usize]
                                                .clone()
                                        }
                                    });
                                    md_func.payload().var_refs.borrow_mut()
                                        [import_var_idx as usize] = vr;
                                }
                            }
                        }
                        r => {
                            r.throw_syntax_error_export(ctx, &reqm.0, &imp.import_name);
                            return Err(-1);
                        }
                    }
                }
            }

            // keep the exported variables in the module export entries (they are used when the eval function is deleted and cannot be initialized before in case imports are exported)
            {
                for exp in self.export_entries.borrow_mut().iter_mut() {
                    if let ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } =
                        &mut exp.export_type
                    {
                        debug_assert_ne!(*var_idx, u16::MAX);
                        let vr = md_func.payload().var_refs.borrow()[*var_idx as usize].clone();
                        var_ref.replace(vr);
                    }
                }

                debug_assert!(self.func_created.get());
            }

            // initialize the global variables
            // Note: following function call with this_obj=TRUE actually executes bytecode like this:
            //     push_this ;; true
            //     if_false 7
            //     return_undef ;; exit immediately here
            //     ...
            // it seems this call has no actual effect according to the bytecode.
            // should this call be removed? and the emitted bytecode (see above) be optimized?

            md_func.call(ctx, &JSValue::Bool(true), &[])?;
        }

        debug_assert!(self.dfs_ancestor_index() <= self.dfs_index());

        if self.dfs_index() == self.dfs_ancestor_index() {
            loop {
                // pop from top module from stack
                let m1 = stack_top.take().unwrap();
                *stack_top = m1.0.stack_prev.take();

                m1.set_status(JSModuleStatus::JS_MODULE_STATUS_LINKED);
                if m1.id() == self.id() {
                    break;
                }
            }
        }

        Ok(index)
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
