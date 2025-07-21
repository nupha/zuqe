use crate::{
    atom::consts::JS_ATOM__star_, context::JSContext, error::JS_ThrowStackOverflow,
    function::JS_Call, runtime::js_check_stack_overflow, value::JSValue,
};
use std::ops::{Deref, DerefMut};

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
            JS_ThrowStackOverflow(ctx);
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

        for req_entry in self.req_module_entries.borrow().iter() {
            let req_m = req_entry.module.clone().unwrap();
            index = req_m.inner_module_linking(ctx, stack_top, index)?;
            debug_assert!(index > 0);
            // if index < 0 {
            //     return Err(-1);
            // }

            debug_assert!(matches!(
                req_m.status(),
                JSModuleStatus::JS_MODULE_STATUS_LINKING
                    | JSModuleStatus::JS_MODULE_STATUS_LINKED
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));

            if req_m.status() == JSModuleStatus::JS_MODULE_STATUS_LINKING {
                self.dfs_ancestor_index.set(std::cmp::min(
                    self.dfs_ancestor_index.get(),
                    req_m.0.dfs_ancestor_index.get(),
                ));
            }
        }

        //
        // let entries_count = self.0.borrow().req_module_entries_count;
        // for i in 0..entries_count {
        //     // let rme: *mut JSReqModuleEntry = self.0.borrow().req_module_entries.add(i as usize);
        //     // let m1 = (*rme).module.as_ref().unwrap();
        //     let m1 = self
        //         .0
        //         .borrow()
        //         .req_module_entries
        //         .add(i as usize)
        //         .as_ref()
        //         .unwrap()
        //         .module
        //         .clone()
        //         .unwrap();
        //
        //     index = m1.inner_module_linking(ctx, pstack_top, index);
        //     if index < 0 {
        //         return -1;
        //     }
        //
        //     let inner1 = m1.0.borrow();
        //
        //     debug_assert!(matches!(
        //         inner1.status,
        //         JSModuleStatus::JS_MODULE_STATUS_LINKING
        //             | JSModuleStatus::JS_MODULE_STATUS_LINKED
        //             | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        //             | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        //     ));
        //
        //     if inner1.status == JSModuleStatus::JS_MODULE_STATUS_LINKING {
        //         let mut inner = self.0.borrow_mut();
        //         inner.dfs_ancestor_index =
        //             std::cmp::min(inner.dfs_ancestor_index, m1.0.borrow().dfs_ancestor_index);
        //     }
        // }

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
                let m1 = self.req_module_entries.borrow()[req_module_idx as usize]
                    .module
                    .clone()
                    .unwrap();
                let ret = m1.resolve_export(ctx, exp_entry.local_name, None);
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
                    let m1 = self.req_module_entries.borrow()[imp_entry.req_module_idx as usize]
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
                        let val = m1.ns(ctx)?;
                        var_refs[import_var_idx as usize].set_value(val.into());

                        #[cfg(DUMP_MODULE_RESOLVE)]
                        {
                            println!("namespace");
                        }
                    } else {
                        match m1.resolve_export(ctx, import_name, None) {
                            ModuleResolveExport::Found { module, entry } => {
                                match entry.export_type {
                                    JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                                        if entry.local_name == JS_ATOM__star_ {
                                            // name space import from
                                            let m2 = module.0.req_module_entries.borrow()
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
                                ret.throw_error(ctx, &m1, import_name);
                                return Err(-1);
                            }
                        }
                    }
                }

                ////
                // let entries_count = self.0.borrow().import_entries_count;
                // for i in 0..entries_count {
                //     let (m1, import_name, import_var_idx) = {
                //         let br = self.0.borrow();
                //         let entry = br.import_entries.add(i as usize);
                //         (
                //             (*br.req_module_entries.add((*entry).req_module_idx as usize))
                //                 .module
                //                 .clone()
                //                 .unwrap(),
                //             (*entry).import_name,
                //             (*entry).var_idx,
                //         )
                //     };
                //
                //     #[cfg(DUMP_MODULE_RESOLVE)]
                //     {
                //         print!("import var_idx={} name=", import_var_idx);
                //         print_atom(ctx, import_name);
                //         print!(": ");
                //     }
                //
                //     if import_name == JS_ATOM__star_ {
                //         // name space import
                //         if let Ok(val) = m1.ns(ctx) {
                //             var_refs[import_var_idx as usize].set_value(val);
                //
                //             #[cfg(DUMP_MODULE_RESOLVE)]
                //             {
                //                 println!("namespace");
                //             }
                //         } else {
                //             return -1;
                //         }
                //     } else {
                //         match m1.resolve_export(ctx, import_name, None) {
                //             ModuleResolveExport::Found { module, entry } => {
                //                 match entry.export_type {
                //                     JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                //                         if entry.local_name == JS_ATOM__star_ {
                //                             // name space import from
                //                             let m2 = (*module
                //                                 .0
                //                                 .borrow()
                //                                 .req_module_entries
                //                                 .add(req_module_idx as usize))
                //                             .module
                //                             .clone()
                //                             .unwrap();
                //
                //                             let val = if let Ok(o) = m2.ns(ctx) {
                //                                 o
                //                             } else {
                //                                 return -1;
                //                             };
                //
                //                             if let Ok(vref) = js_create_module_var(ctx, true) {
                //                                 vref.set_value(val);
                //                                 var_refs[import_var_idx as usize] = vref;
                //
                //                                 #[cfg(DUMP_MODULE_RESOLVE)]
                //                                 {
                //                                     println!("namespace from");
                //                                 }
                //                             } else {
                //                                 return -1;
                //                             }
                //                         }
                //                     }
                //                     JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } => {
                //                         debug_assert_ne!(entry.local_name, JS_ATOM__star_);
                //                         debug_assert_ne!(var_idx, u16::MAX);
                //
                //                         var_refs[import_var_idx as usize] = if let Some(v) = var_ref
                //                         {
                //                             v
                //                         } else {
                //                             module
                //                                 .0
                //                                 .borrow()
                //                                 .bytecode
                //                                 .object_unchecked()
                //                                 .0
                //                                 .payload
                //                                 .borrow()
                //                                 .func()
                //                                 .unwrap()
                //                                 .var_refs
                //                                 [var_idx as usize]
                //                                 .clone()
                //                         };
                //
                //                         #[cfg(DUMP_MODULE_RESOLVE)]
                //                         {
                //                             println!("local export (var_ref={:?})", var_ref);
                //                         }
                //                     }
                //                 }
                //             }
                //             ret => {
                //                 ret.throw_error(ctx, &m1, import_name);
                //                 return -1;
                //             }
                //         }
                //     }
                // }

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
            let ret_val = JS_Call(ctx, &JSValue::Object(func_obj), &JSValue::Bool(true), &[])
                .check_exception()?;
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
        let st0 = self.status();

        let idx = self
            .0
            .inner_module_linking(ctx, self.clone(), stack_top, index)?;

        // if st0 == JSModuleStatus::JS_MODULE_STATUS_UNLINKED
        //     && self.status() == JSModuleStatus::JS_MODULE_STATUS_LINKING
        // {
        //     // push `self` on stack
        //     self.0.borrow_mut().stack_prev = stack_top.take();
        //     stack_top.replace(self.clone());
        // }

        return Ok(idx);
    }
}
