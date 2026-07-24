// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    function::BCWrapper,
    module::{ExportType, JSModuleStatus, ModuleDef, ResolveExport, js_create_module_var},
    runtime::js_check_stack_overflow,
    utils::unlikely,
};

impl ModuleDef {
    fn resolve_export_internal(
        &self,
        ctx: &mut ScopedContext,
        export_name: Atom,
        mut expect: Option<(ModuleDef, Atom)>, // (ModuleDef, local_name_atom)
        resolving: &mut Vec<(ModuleDef, Atom)>, // (ModuleDef, export_atom), to detect circular
    ) -> Result<ResolveExport, i32> {
        if unlikely(
            resolving
                .iter()
                .any(|(m, a)| m.eq(self) && *a == export_name),
        ) {
            // per ES spec, resolveSet duplicate returns null (NotFound),
            // not ~circular~. The star export loop skips null and tries
            // the next star export.
            return Ok(ResolveExport::NotFound);
        }

        // add resolving
        if resolving.len() == resolving.capacity() {
            resolving.try_reserve(4).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
        resolving.push((*self, export_name));

        if let Some(me) = self.find_export(export_name) {
            match me.export_type {
                ExportType::JS_EXPORT_TYPE_LOCAL { .. } => Ok(ResolveExport::Found {
                    module: *self,
                    export_type: me.export_type,
                    export_name: me.export_name,
                    local_name: me.local_name,
                }),
                ExportType::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                    if me.local_name == Atom::CONST__star_ {
                        // export ns from — per ES spec ResolveExport step 6.iii.1,
                        // return the *imported* module, not self.
                        let dep = self.dependencies[req_module_idx as usize].module.unwrap();
                        Ok(ResolveExport::Found {
                            module: dep,
                            export_type: me.export_type,
                            export_name: me.export_name,
                            local_name: me.local_name,
                        })
                    } else {
                        let dep = self.dependencies[req_module_idx as usize].module.unwrap();
                        dep.resolve_export_internal(ctx, me.local_name, expect, resolving)
                    }
                }
            }
        } else {
            let mut ret = ResolveExport::NotFound;

            if export_name != Atom::CONST_default {
                // not found in direct or indirect exports: try star exports
                for idx in self.star_export_entries.iter() {
                    let dep = self.dependencies[*idx as usize].module.unwrap();
                    match dep.resolve_export_internal(ctx, export_name, expect, resolving)? {
                        ResolveExport::Found {
                            module,
                            local_name,
                            export_name,
                            export_type,
                        } => {
                            if expect.is_some_and(|(test_module_id, test_local_name)| {
                                test_module_id != module || test_local_name != local_name
                            }) {
                                return Ok(ResolveExport::Ambiguous);
                            } else {
                                expect.replace((module, local_name));
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
    /// optionally restricted by (expect_ModuleDef, expect_local_name)==(ModuleDef, local_name)
    pub(super) fn resolve_export(
        &self,
        ctx: &mut ScopedContext,
        export_name: Atom,
        expect: Option<(ModuleDef, Atom)>, // (ModuleDef, local_name_atom)
    ) -> Result<ResolveExport, i32> {
        let mut lst = Vec::with_capacity(8);
        self.resolve_export_internal(ctx, export_name, expect, &mut lst)
    }

    /// Prepare module `self` to be executed by resolving all the imported variables.
    /// Resolving imports, exports.
    ///
    /// Iterative version: uses an explicit worklist instead of recursion
    /// to avoid C stack overflow on deep module dependency trees.
    fn inner_module_linking(
        &mut self,
        ctx: &mut ScopedContext,
        stack_top: &mut Option<ModuleDef>,
        index: i32,
    ) -> Result<i32, i32> {
        /// Per-module state during iterative linking.
        struct LinkState {
            module: ModuleDef,
            /// Index into `module.dependencies` to track which dependency is next.
            /// Also indicates phase: `dep_index == 0 && !entered` means entry phase,
            /// otherwise processing dependencies.
            dep_index: usize,
            /// Whether the entry phase (overflow check, status init, stack push) is done.
            entered: bool,
        }

        let mut worklist: Vec<LinkState> = Vec::new();
        worklist.push(LinkState {
            module: *self,
            dep_index: 0,
            entered: false,
        });

        let mut result = Ok(());
        let mut index = index;

        while !worklist.is_empty() && result.is_ok() {
            let top = worklist.len() - 1;

            // === ENTER phase ===
            if !worklist[top].entered {
                worklist[top].entered = true;

                if js_check_stack_overflow(ctx.rt(), 0) {
                    ctx.throw_stack_overflow();
                    result = Err(-1);
                    break;
                }

                // Status check (same as recursive early return)
                if matches!(
                    worklist[top].module.status,
                    JSModuleStatus::JS_MODULE_STATUS_LINKING
                        | JSModuleStatus::JS_MODULE_STATUS_LINKED
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                ) {
                    // Already processed — skip entry, pop immediately
                    worklist.pop();
                    continue;
                }

                debug_assert_eq!(
                    worklist[top].module.status,
                    JSModuleStatus::JS_MODULE_STATUS_UNLINKED
                );

                worklist[top].module.status = JSModuleStatus::JS_MODULE_STATUS_LINKING;
                worklist[top].module.dfs_index = index;
                worklist[top].module.dfs_ancestor_index = index;

                // push onto stack_top (same as recursive `self.stack_prev = stack_top.take(); stack_top.replace(*self);`)
                worklist[top].module.stack_prev = stack_top.take();
                stack_top.replace(worklist[top].module);

                index += 1;
            }

            // === Check previous dependency's ancestor status ===
            // This mirrors the post-recursive-call check in the recursive version:
            //   if m1.status == LINKING { self.dfs_ancestor_index = min(..., m1.dfs_ancestor_index); }
            if worklist[top].dep_index > 0 {
                let prev_dep = worklist[top].module.dependencies[worklist[top].dep_index - 1]
                    .module
                    .unwrap();
                if prev_dep.status == JSModuleStatus::JS_MODULE_STATUS_LINKING {
                    worklist[top].module.dfs_ancestor_index = std::cmp::min(
                        worklist[top].module.dfs_ancestor_index,
                        prev_dep.dfs_ancestor_index,
                    );
                }
            }

            // === All dependencies done → post-processing ===
            if worklist[top].dep_index >= worklist[top].module.dependencies.len() {
                let cur_module = &mut worklist[top].module;

                // check the indirect exports
                for i in 0..cur_module.export_entries.len() {
                    if let ExportType::JS_EXPORT_TYPE_INDIRECT(req_module_idx) =
                        cur_module.export_entries[i].export_type
                        && cur_module.export_entries[i].local_name != Atom::CONST__star_
                    {
                        let m1 = cur_module.dependencies[req_module_idx as usize]
                            .module
                            .unwrap();
                        let ret =
                            m1.resolve_export(ctx, cur_module.export_entries[i].local_name, None)?;
                        if !matches!(ret, ResolveExport::Found { .. }) {
                            ret.throw_syntax_error_export(
                                ctx,
                                cur_module,
                                &cur_module.export_entries[i].export_name,
                            );
                            result = Err(-1);
                            break;
                        }
                    }
                }

                if result.is_err() {
                    break;
                }

                // bytecode module init (same logic as recursive version)
                if cur_module.init_func.is_none() {
                    // is bytecode module
                    debug_assert!(cur_module.func_created, "module function should be created");
                    let mut md_func = BCWrapper::try_from(cur_module.bytecode).unwrap();

                    for i in 0..cur_module.import_entries.len() {
                        let import_var_idx = cur_module.import_entries[i].var_idx;

                        let mut reqm = cur_module.dependencies
                            [cur_module.import_entries[i].req_module_idx as usize]
                            .module
                            .unwrap();

                        if cur_module.import_entries[i].import_name == Atom::CONST__star_ {
                            // name space import
                            let val = reqm.ns(ctx)?;
                            md_func.payload_mut().store_closure_value(
                                import_var_idx as _,
                                ctx,
                                val.into(),
                            );

                            // let gvc = md_func.payload().global_var_count as usize;
                            // md_func.payload_mut().closure_var_refs_mut()
                            //     [gvc + import_var_idx as usize]
                            //     .set_value(&mut ctx.rt_ptr(), val.into());
                        } else {
                            match reqm.resolve_export(
                                ctx,
                                cur_module.import_entries[i].import_name,
                                None,
                            )? {
                                ResolveExport::Found {
                                    module: dep,
                                    export_type,
                                    local_name,
                                    ..
                                } => {
                                    match export_type {
                                        ExportType::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                                            if local_name == Atom::CONST__star_ {
                                                // namespace import from — after the resolve fix,
                                                // dep is already the target module (not self),
                                                // so ns() can be called directly.
                                                let val = {
                                                    let mut md = dep;
                                                    md.ns(ctx)?
                                                };
                                                let vref = {
                                                    let mut vr = js_create_module_var(ctx, true)?;
                                                    vr.set_value(&mut ctx.rt_ptr(), val.into());
                                                    vr
                                                };
                                                md_func
                                                    .payload_mut()
                                                    .set_closure(import_var_idx as usize, vref);
                                            }
                                        }
                                        ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } => {
                                            debug_assert_ne!(local_name, Atom::CONST__star_);
                                            let vr = var_ref.unwrap_or_else(|| {
                                                debug_assert!(
                                                    dep.func_created,
                                                    "module function must have been created"
                                                );
                                                dep.bytecode
                                                    .as_object_BCWrapper()
                                                    .unwrap()
                                                    .payload()
                                                    .get_closure(var_idx as usize)
                                            });
                                            md_func
                                                .payload_mut()
                                                .set_closure(import_var_idx as usize, vr);
                                        }
                                    }
                                }
                                r => {
                                    r.throw_syntax_error_export(
                                        ctx,
                                        &reqm,
                                        &cur_module.import_entries[i].import_name,
                                    );
                                    result = Err(-1);
                                    break;
                                }
                            }
                        }
                    }

                    if result.is_err() {
                        break;
                    }

                    // keep the exported variables in the module export entries (they are used when the eval function is deleted and cannot be initialized before in case imports are exported)
                    {
                        for i in 0..cur_module.export_entries.len() {
                            if let ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } =
                                &mut cur_module.export_entries[i].export_type
                            {
                                debug_assert_ne!(*var_idx, u16::MAX);
                                let vr = md_func.payload().get_closure(*var_idx as usize);
                                var_ref.replace(vr);
                            }
                        }

                        debug_assert!(cur_module.func_created);
                    }

                    // initialize the global variables
                    // Note: following function call with this_obj=TRUE actually executes bytecode like this:
                    //     push_this ;; true
                    //     if_false 7
                    //     return_undef ;; exit immediately here
                    //     ...
                    // it seems this call has no actual effect according to the bytecode.
                    // should this call be removed? and the emitted bytecode (see above) be optimized?

                    md_func.call(ctx, true.into(), &[])?;
                }

                if result.is_err() {
                    break;
                }

                // Tarjan SCC: if this module is the root of a strongly connected component,
                // pop all modules in the cycle from stack_top.
                if cur_module.dfs_index == cur_module.dfs_ancestor_index {
                    loop {
                        let mut m1 = stack_top.take().unwrap();
                        *stack_top = m1.stack_prev.take();

                        m1.status = JSModuleStatus::JS_MODULE_STATUS_LINKED;
                        if m1.eq(cur_module) {
                            break;
                        }
                    }
                }

                // Pop from worklist without touching stack_top.
                // For non-SCC-root modules, stack_top cleanup is handled by the SCC root's pop.
                // For SCC-root modules, the pop above already restored stack_top.
                worklist.pop();
                continue;
            }

            // === PROCESS next dependency ===
            {
                let dep_idx = worklist[top].dep_index;
                let child = worklist[top].module.dependencies[dep_idx].module.unwrap();

                worklist[top].dep_index += 1;
                worklist.push(LinkState {
                    module: child,
                    dep_index: 0,
                    entered: false,
                });
            }
        }

        result.map(|_| index)
    }

    /// Prepare a module to be executed by resolving all the imported variables.
    /// 准备模块执行前的链接工作，即解析和绑定所有导入的变量。
    /// - 模块依赖链接：解析模块之间的依赖关系，将导入和导出绑定起来
    /// - 变量引用解析：建立导入变量与导出变量之间的引用关系
    /// - 循环依赖处理：使用深度优先搜索(DFS)算法处理模块间的循环依赖
    /// - 状态管理：管理模块的链接状态转换
    pub(super) fn link(&mut self, ctx: &mut ScopedContext) -> Result<(), i32> {
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
            while let Some(mut m1) = stack_top.take() {
                debug_assert_eq!(m1.status(), JSModuleStatus::JS_MODULE_STATUS_LINKING);
                m1.status = JSModuleStatus::JS_MODULE_STATUS_UNLINKED;
                stack_top = m1.stack_prev.take();
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
