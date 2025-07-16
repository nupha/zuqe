use crate::module::ModuleDefInner;
use crate::{
    atom::{JSAtom, consts::JS_ATOM__star_},
    context::JSContext,
    error::{JS_ThrowReferenceError, JS_ThrowStackOverflow, JS_ThrowSyntaxError},
    function::JS_Call,
    mem::js_resize_array,
    module::{
        JSExportTypeEnum, ModuleDef, ModuleResolveExport, js_create_module_var,
        js_default_module_normalize_name,
    },
    runtime::js_check_stack_overflow,
    value::JSValue,
};

pub struct JSReqModuleEntry {
    pub module_name: JSAtom,
    pub module: Option<ModuleDef>, /* used using resolution */
}

#[repr(C)]
pub struct JSResolveEntry {
    pub module: ModuleDef,
    pub name: JSAtom,
}

#[repr(C)]
pub struct JSResolveState {
    pub array: *mut JSResolveEntry,
    pub size: i32,
    pub count: i32,
}

impl JSResolveState {
    pub(super) unsafe fn find(&self, m: &ModuleDef, name: JSAtom) -> Option<i32> {
        for i in 0..self.count {
            let re = self.array.add(i as usize).as_ref().unwrap();
            if re.module.id() == m.id() && re.name == name {
                return Some(i);
            }
        }
        None
    }
}

impl JSContext {
    /// find the loaded module by module name
    pub fn find_loaded_module(&self, name: JSAtom) -> Option<ModuleDef> {
        self.0.borrow_mut().loaded_modules.iter().find_map(|w| {
            if let Some(m) = w.upgrade() {
                if m.0.borrow().module_name == name {
                    Some(m)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    /// Load all the required modules for module 'm'
    pub fn resolve_module(&self, m: &ModuleDef) -> Result<(), i32> {
        if m.0.borrow().resolved {
            return Ok(());
        }

        #[cfg(DUMP_MODULE_RESOLVE)]
        {
            let buf1: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];
            println!(
                "resolving module '{}':\n",
                JS_AtomGetStr(
                    ctx,
                    buf1.as_mut_ptr(),
                    std::mem::size_of::<[u8; ATOM_GET_STR_BUF_SIZE]>(),
                    (*m).module_name
                )
            );
        }

        m.0.borrow_mut().resolved = true;

        // resolve each requested module
        for i in 0..m.0.borrow().req_module_entries_count {
            let rme = unsafe { m.0.borrow().req_module_entries.add(i as usize) };

            let m1 =
                js_host_resolve_imported_module_atom(self, m.0.borrow().module_name, unsafe {
                    (*rme).module_name
                })?;

            unsafe {
                (*rme).module = Some(m1.clone());
            }

            // already done in js_host_resolve_imported_module() except if
            // the module was loaded with JS_EvalBinary()
            self.resolve_module(&m1)?;
        }

        Ok(())
    }
}

impl ModuleDef {
    // Prepare a module to be executed by resolving all the imported variables.
    pub unsafe fn inner_module_linking(
        &self,
        ctx: &JSContext,
        pstack_top: &mut Option<ModuleDef>,
        index: i32,
    ) -> i32 {
        if js_check_stack_overflow(ctx.rt(), 0) {
            JS_ThrowStackOverflow(ctx);
            return -1;
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

        {
            let mut inner = self.0.borrow_mut();

            if matches!(
                inner.status,
                JSModuleStatus::JS_MODULE_STATUS_LINKING
                    | JSModuleStatus::JS_MODULE_STATUS_LINKED
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ) {
                return index;
            }

            debug_assert!(inner.status == JSModuleStatus::JS_MODULE_STATUS_UNLINKED);

            inner.status = JSModuleStatus::JS_MODULE_STATUS_LINKING;
            inner.dfs_index = index;
            inner.dfs_ancestor_index = index;
            // push `self` on stack
            inner.stack_prev = pstack_top.take();
        }
        pstack_top.replace(self.clone());

        let mut index = index + 1;

        let entries_count = self.0.borrow().req_module_entries_count;
        for i in 0..entries_count {
            // let rme: *mut JSReqModuleEntry = self.0.borrow().req_module_entries.add(i as usize);
            // let m1 = (*rme).module.as_ref().unwrap();
            let m1 = self
                .0
                .borrow()
                .req_module_entries
                .add(i as usize)
                .as_ref()
                .unwrap()
                .module
                .clone()
                .unwrap();

            index = m1.inner_module_linking(ctx, pstack_top, index);
            if index < 0 {
                return -1;
            }

            let inner1 = m1.0.borrow();

            debug_assert!(matches!(
                inner1.status,
                JSModuleStatus::JS_MODULE_STATUS_LINKING
                    | JSModuleStatus::JS_MODULE_STATUS_LINKED
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));

            if inner1.status == JSModuleStatus::JS_MODULE_STATUS_LINKING {
                let mut inner = self.0.borrow_mut();
                inner.dfs_ancestor_index =
                    std::cmp::min(inner.dfs_ancestor_index, m1.0.borrow().dfs_ancestor_index);
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
        {
            let inner = self.0.borrow();

            for me in inner.export_entries.iter() {
                if let JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) = me.export_type {
                    if me.local_name != JS_ATOM__star_ {
                        let m1 = (*inner.req_module_entries.add(req_module_idx as usize))
                            .module
                            .clone()
                            .unwrap();

                        match m1.resolve_export(ctx, me.local_name, None) {
                            ModuleResolveExport::Found { .. } => {}
                            ret => {
                                ret.throw_error(ctx, self, me.export_name);
                                return -1;
                            }
                        }
                    }
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

        let bc_val = self.0.borrow().bytecode.clone();
        if let JSValue::Object(func_obj) = bc_val {
            // js js module, and function has been created
            debug_assert!(self.0.borrow().func_created);

            // enclose `isa` borrow in scoped block.
            {
                let mut isa = func_obj.0.payload.borrow_mut();
                let var_refs = isa.func_mut().unwrap().var_refs.as_mut_slice();

                let entries_count = self.0.borrow().import_entries_count;
                for i in 0..entries_count {
                    let (m1, import_name, import_var_idx) = {
                        let br = self.0.borrow();
                        let entry = br.import_entries.add(i as usize);
                        (
                            (*br.req_module_entries.add((*entry).req_module_idx as usize))
                                .module
                                .clone()
                                .unwrap(),
                            (*entry).import_name,
                            (*entry).var_idx,
                        )
                    };

                    #[cfg(DUMP_MODULE_RESOLVE)]
                    {
                        print!("import var_idx={} name=", import_var_idx);
                        print_atom(ctx, import_name);
                        print!(": ");
                    }

                    if import_name == JS_ATOM__star_ {
                        // name space import
                        if let Ok(val) = m1.ns(ctx) {
                            var_refs[import_var_idx as usize].set_value(val);

                            #[cfg(DUMP_MODULE_RESOLVE)]
                            {
                                println!("namespace");
                            }
                        } else {
                            return -1;
                        }
                    } else {
                        match m1.resolve_export(ctx, import_name, None) {
                            ModuleResolveExport::Found { module, entry } => {
                                match entry.export_type {
                                    JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                                        if entry.local_name == JS_ATOM__star_ {
                                            // name space import from
                                            let m2 = (*module
                                                .0
                                                .borrow()
                                                .req_module_entries
                                                .add(req_module_idx as usize))
                                            .module
                                            .clone()
                                            .unwrap();

                                            let val = if let Ok(o) = m2.ns(ctx) {
                                                o
                                            } else {
                                                return -1;
                                            };

                                            if let Ok(vref) = js_create_module_var(ctx, true) {
                                                vref.set_value(val);
                                                var_refs[import_var_idx as usize] = vref;

                                                #[cfg(DUMP_MODULE_RESOLVE)]
                                                {
                                                    println!("namespace from");
                                                }
                                            } else {
                                                return -1;
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
                                            module
                                                .0
                                                .borrow()
                                                .bytecode
                                                .object_unchecked()
                                                .0
                                                .payload
                                                .borrow()
                                                .func()
                                                .unwrap()
                                                .var_refs
                                                [var_idx as usize]
                                                .clone()
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
                                return -1;
                            }
                        }
                    }
                }

                // keep the exported variables in the module export entries (they are used when the eval function is deleted and cannot be initialized before in case imports are exported)
                {
                    let mut inner = self.0.borrow_mut();
                    for me in inner.export_entries.iter_mut() {
                        if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } =
                            &mut me.export_type
                        {
                            debug_assert_ne!(*var_idx, u16::MAX);
                            var_ref.replace(var_refs[*var_idx as usize].clone());
                        }
                    }

                    debug_assert!(inner.func_created);
                    inner.bytecode.clone()
                };
            }

            // initialize the global variables
            let ret_val = JS_Call(ctx, &JSValue::Object(func_obj), &JSValue::Bool(true), &[]);
            if ret_val.is_exception() {
                return -1;
            }
        }

        let (dfs_ancestor_index, dfs_index, self_id) = {
            let inner = self.0.borrow();
            (inner.dfs_ancestor_index, inner.dfs_index, inner.id())
        };
        debug_assert!(dfs_ancestor_index <= dfs_index);
        if dfs_index == dfs_ancestor_index {
            loop {
                // pop m1 from stack
                let m1 = pstack_top.take().unwrap();
                let mut m1 = m1.0.borrow_mut();

                m1.status = JSModuleStatus::JS_MODULE_STATUS_LINKED;
                *pstack_top = m1.stack_prev.take();

                if m1.id() == self_id {
                    break;
                }
            }
        }

        #[cfg(DUMP_MODULE_RESOLVE)]
        {
            println!("js_inner_module_linking done");
        }

        index
    }
}

#[deprecated(note = "use JSResolveState::find()")]
pub unsafe fn find_resolve_entry(
    s: *mut JSResolveState,
    m: &ModuleDef,
    name: JSAtom,
) -> Option<i32> {
    unreachable!("unused");
    // for i in 0..(*s).count {
    //     let re = (*s).array.add(i as usize).as_mut().unwrap();
    //     if re.module.id() == m.id() && re.name == name {
    //         return Some(i);
    //     }
    // }
    // None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSResolveResultEnum {
    JS_RESOLVE_RES_EXCEPTION = -1, /* memory alloc error */
    JS_RESOLVE_RES_FOUND = 0,
    JS_RESOLVE_RES_NOT_FOUND,
    JS_RESOLVE_RES_CIRCULAR,
    JS_RESOLVE_RES_AMBIGUOUS,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSModuleStatus {
    JS_MODULE_STATUS_UNLINKED,
    JS_MODULE_STATUS_LINKING,
    JS_MODULE_STATUS_LINKED,
    JS_MODULE_STATUS_EVALUATING,
    JS_MODULE_STATUS_EVALUATING_ASYNC,
    JS_MODULE_STATUS_EVALUATED,
}

pub unsafe fn add_req_module_entry(ctx: &JSContext, m: &ModuleDef, module_name: JSAtom) -> i32 {
    let mut m = m.0.borrow_mut();

    // no need to add the module request if it is already present
    for i in 0..m.req_module_entries_count {
        if m.req_module_entries
            .offset(i as isize)
            .as_ref()
            .unwrap()
            .module_name
            == module_name
        {
            return i as i32; // found index
        }
    }

    if js_resize_array(
        ctx,
        std::ptr::addr_of_mut!(m.req_module_entries).cast(),
        std::mem::size_of::<JSReqModuleEntry>() as _,
        std::ptr::addr_of_mut!(m.req_module_entries_size),
        m.req_module_entries_count as i32 + 1,
    ) != 0
    {
        return -1;
    }

    let rme = &mut *m
        .req_module_entries
        .offset(m.req_module_entries_count as isize);
    rme.module_name = module_name.dup(&ctx.rt());
    rme.module = None;

    m.req_module_entries_count += 1;
    m.req_module_entries_count as i32 - 1 // the last index is count-1
}

pub unsafe fn add_resolve_entry(
    ctx: &JSContext,
    s: *mut JSResolveState,
    m: &ModuleDef,
    name: JSAtom,
) -> Result<(), i32> {
    if js_resize_array(
        ctx,
        std::ptr::addr_of_mut!((*s).array).cast(),
        std::mem::size_of::<JSResolveEntry>() as _,
        std::ptr::addr_of_mut!((*s).size),
        (*s).count + 1,
    ) != 0
    {
        return Err(-1);
    }

    let re = (*s).array.add((*s).count as usize).as_mut().unwrap();
    (*s).count += 1;
    re.module = m.clone();
    re.name = name.dup(&ctx.rt());

    Ok(())
}

#[deprecated(note = "unused")]
pub unsafe fn js_resolve_export_throw_error(
    ctx: &JSContext,
    res: JSResolveResultEnum,
    m: &ModuleDef,
    export_name: JSAtom,
) {
    match res {
        JSResolveResultEnum::JS_RESOLVE_RES_EXCEPTION => {}
        JSResolveResultEnum::JS_RESOLVE_RES_NOT_FOUND => {
            JS_ThrowSyntaxError(
                ctx,
                &format!(
                    "Could not find export '{}' in module '{}'",
                    export_name.to_str(ctx.rt()),
                    m.0.borrow().module_name.to_str(ctx.rt()),
                ),
            );
        }
        JSResolveResultEnum::JS_RESOLVE_RES_CIRCULAR => {
            JS_ThrowSyntaxError(
                ctx,
                &format!(
                    "circular reference when looking for export '{}' in module '{}'",
                    export_name.to_str(ctx.rt()),
                    m.0.borrow().module_name.to_str(ctx.rt()),
                ),
            );
        }
        JSResolveResultEnum::JS_RESOLVE_RES_AMBIGUOUS => {
            JS_ThrowSyntaxError(
                ctx,
                &format!(
                    "export '{}' in module '{}' is ambiguous",
                    export_name.to_str(ctx.rt()),
                    m.0.borrow().module_name.to_str(ctx.rt()),
                ),
            );
        }
        _ => {}
    }
}

/// find the loaded module by module name
#[deprecated(note = "use JSContext::find_loaded_module() instead")]
pub fn js_find_loaded_module(ctx: &JSContext, name: JSAtom) -> Option<ModuleDef> {
    ctx.find_loaded_module(name)
}

// Load all the required modules for module 'm'
#[deprecated(note = "use JSContext::resolve_module() instead")]
pub unsafe fn js_resolve_module(ctx: &JSContext, m: &ModuleDef) -> i32 {
    ctx.resolve_module(m).map_or(-1, |_| 0)
}

pub fn js_host_resolve_imported_module(
    ctx: &JSContext,
    base_cname: &str,
    cname1: &str,
) -> Result<ModuleDef, i32> {
    let name = if let Some(cb) = ctx.rt().module_normalize_func {
        cb(ctx, base_cname, cname1, ctx.rt().module_loader_opaque)
    } else {
        js_default_module_normalize_name(ctx, base_cname, cname1)
    }?;
    let module_name = ctx.create_atom(&name)?;

    // look at the loaded modules
    if let Some(m) = ctx.find_loaded_module(module_name) {
        return Ok(m); // already loaded
    }

    // load the module
    if let Some(cb) = ctx.rt().module_loader_func {
        cb(ctx, &name, ctx.rt().module_loader_opaque)
    } else {
        log::warn!("module loader function is not set, can't import module.");
        JS_ThrowReferenceError(ctx, &format!("could not load module '{}'", name));
        Err(-1)
    }
}

pub fn js_host_resolve_imported_module_atom(
    ctx: &JSContext,
    base_module_name: JSAtom,
    module_name1: JSAtom,
) -> Result<ModuleDef, i32> {
    let base_cname = base_module_name.to_utf8_string(ctx)?;
    let cname = module_name1.to_utf8_string(ctx)?;
    js_host_resolve_imported_module(ctx, base_cname.as_ref(), &cname)
}

/// Prepare a module to be executed by resolving all the imported variables.
pub fn js_link_module(ctx: &JSContext, m: &ModuleDef) -> i32 {
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
        m.0.borrow().status,
        JSModuleStatus::JS_MODULE_STATUS_UNLINKED
            | JSModuleStatus::JS_MODULE_STATUS_LINKED
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
    ));

    let mut stack_top: Option<ModuleDef> = None;

    //if unsafe { js_inner_module_linking(ctx, m, &mut stack_top, 0) < 0 } {
    if unsafe { m.inner_module_linking(ctx, &mut stack_top, 0) < 0 } {
        while let Some(m1) = stack_top.take() {
            debug_assert_eq!(
                m1.0.borrow().status,
                JSModuleStatus::JS_MODULE_STATUS_LINKING
            );
            m1.0.borrow_mut().status = JSModuleStatus::JS_MODULE_STATUS_UNLINKED;
            stack_top = m1.0.borrow_mut().stack_prev.take();
        }
        return -1;
    }

    debug_assert!(stack_top.is_none());
    debug_assert!(matches!(
        m.status(),
        JSModuleStatus::JS_MODULE_STATUS_LINKED
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
    ));

    0
}

// Prepare a module to be executed by resolving all the imported variables.
#[deprecated(note = "use ModuleDef::inner_module_linking() instead")]
pub unsafe fn js_inner_module_linking(
    ctx: &JSContext,
    m: &ModuleDef, // FIXME: use ModuleInner instead
    pstack_top: &mut Option<ModuleDef>,
    index: i32,
) -> i32 {
    if js_check_stack_overflow(ctx.rt(), 0) {
        JS_ThrowStackOverflow(ctx);
        return -1;
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

    {
        let mut inner = m.0.borrow_mut();

        if matches!(
            inner.status,
            JSModuleStatus::JS_MODULE_STATUS_LINKING
                | JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ) {
            return index;
        }

        debug_assert!(inner.status == JSModuleStatus::JS_MODULE_STATUS_UNLINKED);
        inner.status = JSModuleStatus::JS_MODULE_STATUS_LINKING;
        inner.dfs_index = index;
        inner.dfs_ancestor_index = index;

        // push 'm' on stack
        inner.stack_prev = pstack_top.take();
        pstack_top.replace(m.clone());

        let mut index = index + 1;

        for i in 0..inner.req_module_entries_count {
            let rme: *mut JSReqModuleEntry = inner.req_module_entries.offset(i as isize);
            let m1 = (*rme).module.as_ref().unwrap();

            // FIXME: m.0 is borrowed_mut() at line 368, panic!
            index = js_inner_module_linking(ctx, m1, pstack_top, index);
            if index < 0 {
                return -1;
            }

            debug_assert!(matches!(
                m1.0.borrow().status,
                JSModuleStatus::JS_MODULE_STATUS_LINKING
                    | JSModuleStatus::JS_MODULE_STATUS_LINKED
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));

            if m1.0.borrow_mut().status == JSModuleStatus::JS_MODULE_STATUS_LINKING {
                inner.dfs_ancestor_index =
                    std::cmp::min(inner.dfs_ancestor_index, m1.0.borrow().dfs_ancestor_index);
            }
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
    {
        let inner = m.0.borrow();

        for me in inner.export_entries.iter() {
            if let JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) = me.export_type {
                if me.local_name != JS_ATOM__star_ {
                    let m1 = (*inner.req_module_entries.add(req_module_idx as usize))
                        .module
                        .as_ref()
                        .unwrap();

                    match m1.resolve_export(ctx, me.local_name, None) {
                        ModuleResolveExport::Found { .. } => {}
                        ret => {
                            ret.throw_error(ctx, &m, me.export_name);
                            return -1;
                        }
                    }
                }
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

    let is_c_module = m.0.borrow().init_func.is_some();
    if !is_c_module {
        let inner = m.0.borrow();
        let mut isa = inner.bytecode.object_unchecked().0.payload.borrow_mut();
        let var_refs = isa.func_mut().unwrap().var_refs.as_mut_slice();

        for i in 0..m.0.borrow().import_entries_count {
            let mi = m.0.borrow().import_entries.offset(i as isize);

            #[cfg(DUMP_MODULE_RESOLVE)]
            {
                print!("import var_idx={} name=", (*mi).var_idx);
                print_atom(ctx, (*mi).import_name);
                print!(": ");
            }

            let m1 = (*m
                .0
                .borrow()
                .req_module_entries
                .offset((*mi).req_module_idx as isize))
            .module
            .clone()
            .unwrap();

            if (*mi).import_name == JS_ATOM__star_ {
                // name space import
                if let Ok(val) = m1.ns(ctx) {
                    var_refs[(*mi).var_idx as usize].set_value(val);

                    #[cfg(DUMP_MODULE_RESOLVE)]
                    {
                        println!("namespace");
                    }
                } else {
                    return -1;
                }
            } else {
                match m1.resolve_export(ctx, (*mi).import_name, None) {
                    ModuleResolveExport::Found { module, entry } => {
                        match entry.export_type {
                            JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                                if entry.local_name == JS_ATOM__star_ {
                                    // name space import from
                                    let m2 = (*module
                                        .0
                                        .borrow()
                                        .req_module_entries
                                        .add(req_module_idx as usize))
                                    .module
                                    .clone()
                                    .unwrap();

                                    let val = if let Ok(o) = m2.ns(ctx) {
                                        o
                                    } else {
                                        return -1;
                                    };

                                    if let Ok(vref) = js_create_module_var(ctx, true) {
                                        vref.set_value(val);
                                        var_refs[(*mi).var_idx as usize] = vref;

                                        #[cfg(DUMP_MODULE_RESOLVE)]
                                        {
                                            println!("namespace from");
                                        }
                                    } else {
                                        return -1;
                                    }
                                }
                            }
                            JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } => {
                                debug_assert_ne!(entry.local_name, JS_ATOM__star_);
                                debug_assert_ne!(var_idx, u16::MAX);

                                var_refs[(*mi).var_idx as usize] = if let Some(v) = var_ref {
                                    v
                                } else {
                                    module
                                        .0
                                        .borrow()
                                        .bytecode
                                        .object_unchecked()
                                        .0
                                        .payload
                                        .borrow()
                                        .func()
                                        .unwrap()
                                        .var_refs[var_idx as usize]
                                        .clone()
                                };

                                #[cfg(DUMP_MODULE_RESOLVE)]
                                {
                                    println!("local export (var_ref={:?})", var_ref);
                                }
                            }
                        }
                    }
                    ret => {
                        ret.throw_error(ctx, &m1, (*mi).import_name);
                        return -1;
                    }
                }
            }
        }

        // keep the exported variables in the module export entries (they are used when the eval function is deleted and cannot be initialized before in case imports are exported)
        for me in m.0.borrow_mut().export_entries.iter_mut() {
            if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } = &mut me.export_type
            {
                debug_assert_ne!(*var_idx, u16::MAX);
                var_ref.replace(var_refs[*var_idx as usize].clone());
            }
        }

        // initialize the global variables
        let ret_val = JS_Call(ctx, &m.0.borrow_mut().bytecode, &JSValue::Bool(true), &[]);
        if ret_val.is_exception() {
            return -1;
        }
    }

    debug_assert!(m.0.borrow().dfs_ancestor_index <= m.0.borrow().dfs_index);

    if m.0.borrow().dfs_index == m.0.borrow().dfs_ancestor_index {
        loop {
            // pop m1 from stack
            let m1 = pstack_top.take().unwrap();
            let mut m1 = m1.0.borrow_mut();

            m1.status = JSModuleStatus::JS_MODULE_STATUS_LINKED;
            *pstack_top = m1.stack_prev.take();

            if m1.id() == m.id() {
                break;
            }
        }
    }

    #[cfg(DUMP_MODULE_RESOLVE)]
    {
        println!("js_inner_module_linking done");
    }

    index
}
