use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    path::PathBuf,
    rc::{Rc, Weak},
};

use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JS_FreeAtomRT, JSAtom},
    class::JSClassExoticMethods,
    context::{JSContext, JSContextInner},
    error::{JS_ThrowSyntaxError, JSResult},
    function::{JSFunctionDef, JSObjectFunc, JSVarKindEnum, JSVarRef},
    gc::GCObjectHeader,
    mem::js_free_rt,
    module::{
        JSExportEntry, JSExportTypeEnum, JSImportEntry, JSModuleStatus, JSReqModuleEntry,
        JSStarExportEntry,
    },
    object::{JS_PROP_C_W_E, JSObject, JSObjectIsa},
    runtime::JSRuntime,
    stack_frame::StackFrame,
    value::JSValue,
};

pub type JSModuleNormalizeFunc = for<'a> fn(
    ctx: &JSContext,
    module_base_name: &'a str,
    module_name: &'a str,
    opaque: *mut ::core::ffi::c_void,
) -> Result<String, i32>;

pub type JSModuleLoaderFunc = fn(
    ctx: &JSContext,
    module_name: &str,
    opaque: *mut ::core::ffi::c_void,
) -> Result<JSModuleDef, i32>;

pub type JSModuleInitFunc = fn(ctx: &JSContext, m: &JSModuleDef) -> i32;

#[derive(Debug)]
pub(crate) struct JSModuleDefInner {
    header: GCObjectHeader,

    pub module_name: JSAtom,

    // TODO: use Vec<JSReqModuleEntry>
    pub req_module_entries: *mut JSReqModuleEntry,
    pub req_module_entries_count: u16,
    pub req_module_entries_size: i32,

    pub export_entries: Vec<Box<JSExportEntry>>,

    // TODO: use Vec<JSStartExportEntry>
    pub star_export_entries: *mut JSStarExportEntry,
    pub star_export_entries_count: i32,
    pub star_export_entries_size: i32,

    // TODO: use Vec<JSImportEntry>
    pub import_entries: *mut JSImportEntry,
    pub import_entries_count: u16,
    pub import_entries_size: i32,

    /// module namespace.
    pub module_ns: JSValue,

    /// for JS modules only.
    /// TODO: use Option<Bytecode>
    pub bytecode: JSValue,
    /// for C modules only
    pub init_func: Option<JSModuleInitFunc>,

    /// true if func_obj contains await
    pub has_tla: bool,

    pub resolved: bool,
    pub func_created: bool,
    pub status: JSModuleStatus,

    /* temp use during js_module_link() & js_module_evaluate() */
    pub dfs_index: i32,
    pub dfs_ancestor_index: i32,

    pub stack_prev: Option<JSModuleDef>,
    pub cycle_root: Option<JSModuleDef>,

    //
    // temp use during js_module_evaluate()
    //
    pub async_parent_modules: Vec<JSModuleDef>,

    pub pending_async_dependencies: i32,
    pub async_evaluation: bool,
    pub async_evaluation_timestamp: i64,

    pub promise: JSValue, /* corresponds to spec field: capability */
    pub resolving_funcs: [JSValue; 2], /* corresponds to spec field: capability */

    /* true if evaluation yielded an exception. It is saved in
    eval_exception */
    pub eval_has_exception: bool,
    pub eval_exception: JSValue,

    /// the import.meta object
    pub meta_obj: JSValue,
}

impl Drop for JSModuleDefInner {
    fn drop(&mut self) {
        unsafe {
            self.finalize();
        }
    }
}

impl JSModuleDefInner {
    /// 'name' is consumed
    pub fn new(ctx: &JSContext, name: JSAtom) -> Self {
        let o = Self {
            header: GCObjectHeader::new(ctx.rt_mut(), 1),

            module_name: name,

            req_module_entries: std::ptr::null_mut(),
            req_module_entries_count: 0,
            req_module_entries_size: 0,

            export_entries: Vec::new(),
            star_export_entries: std::ptr::null_mut(),
            star_export_entries_count: 0,
            star_export_entries_size: 0,

            import_entries: std::ptr::null_mut(),
            import_entries_count: 0,
            import_entries_size: 0,

            module_ns: JSValue::UNDEFINED,
            bytecode: JSValue::UNDEFINED,
            init_func: None,

            has_tla: false,
            resolved: false,
            func_created: false,
            status: JSModuleStatus::JS_MODULE_STATUS_UNLINKED,

            dfs_index: 0,
            dfs_ancestor_index: 0,

            cycle_root: None,
            stack_prev: None,
            async_parent_modules: Vec::new(),

            pending_async_dependencies: 0,
            async_evaluation: false,
            async_evaluation_timestamp: 0,

            promise: JSValue::UNDEFINED,
            resolving_funcs: [const { JSValue::UNDEFINED }; 2],
            eval_has_exception: false,
            eval_exception: JSValue::UNDEFINED,

            meta_obj: JSValue::UNDEFINED,
        };

        o
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const JSModuleDefInner as usize
    }

    /// get module import.meta object.
    pub fn import_meta(&mut self, ctx: &JSContext) -> JSResult {
        // allocate meta_obj only if requested to save memory
        let mut meta = self.meta_obj.clone();
        if meta.is_undefined() {
            meta = ctx.create_base_object_with_proto(&JSValue::NULL)?;
            self.meta_obj = meta.clone();
        }
        Ok(meta)
    }

    /// set module import.meta props.
    pub fn set_import_meta(
        &mut self,
        ctx: &JSContext,
        url: &str,
        is_main: bool,
    ) -> Result<(), i32> {
        let meta_obj = self.import_meta(ctx)?;

        let sval = ctx.create_string(url);
        meta_obj.define_property_value_str(ctx, "url", sval, JS_PROP_C_W_E)?;
        meta_obj.define_property_value_str(ctx, "main", is_main.into(), JS_PROP_C_W_E)?;
        Ok(())
    }

    unsafe fn finalize(&mut self) {
        let rt = self.header.rt_mut();

        JS_FreeAtomRT(rt, self.module_name);

        for i in 0..self.req_module_entries_count {
            let rme: *mut JSReqModuleEntry = self.req_module_entries.offset(i as isize);
            JS_FreeAtomRT(rt, (*rme).module_name);
        }
        js_free_rt(rt, self.req_module_entries.cast());

        for me in self.export_entries.drain(..) {
            if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_ref, .. } = me.export_type {
                debug_assert!(var_ref.is_some());
                drop(var_ref);
            }

            JS_FreeAtomRT(rt, me.export_name);
            JS_FreeAtomRT(rt, me.local_name);
        }

        js_free_rt(rt, self.star_export_entries.cast());

        for i in 0..self.import_entries_count {
            let mi: *mut JSImportEntry = self.import_entries.offset(i as isize);
            JS_FreeAtomRT(rt, (*mi).import_name);
        }
        js_free_rt(rt, self.import_entries.cast());
    }
}

#[derive(Debug, Clone)]
pub struct JSModuleDef(pub(crate) Rc<RefCell<JSModuleDefInner>>);

impl From<JSModuleDef> for JSValue {
    fn from(value: JSModuleDef) -> Self {
        Self::Module(value)
    }
}

impl JSModuleDef {
    /// 'name' is consumed
    pub fn new(ctx: &JSContext, name: JSAtom) -> Self {
        let inner = JSModuleDefInner::new(ctx, name);
        Self(Rc::new(RefCell::new(inner)))
    }

    pub fn rt(&self) -> &JSRuntime {
        unsafe { self.0.borrow().header.rt_ptr().as_ref() }
    }

    pub fn with<R>(&self, f: impl FnOnce(&JSModuleDefInner) -> R) -> R {
        f(self.0.borrow().deref())
    }

    pub fn with_mut<R>(&self, f: impl FnOnce(&mut JSModuleDefInner) -> R) -> R {
        f(self.0.borrow_mut().deref_mut())
    }

    /// get module `import.meta` object.
    pub fn import_meta(&self, ctx: &JSContext) -> JSResult {
        self.0.borrow_mut().import_meta(ctx)
    }

    /// get module name atom. returned atom is not duplicated.
    pub fn module_name_atom(&self) -> JSAtom {
        self.0.borrow().module_name
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    pub fn downgrade(&self) -> JSModuleDefWeakref {
        JSModuleDefWeakref(Rc::downgrade(&self.0))
    }

    pub fn gc_has_mark(&self) -> bool {
        self.0.borrow().header.gc_has_mark()
    }

    pub fn gc_set_mark(&self, mark: bool) {
        self.0.borrow_mut().header.gc_set_mark(mark);
    }
}

#[derive(Debug, Clone)]
pub struct JSModuleDefWeakref(Weak<RefCell<JSModuleDefInner>>);

impl JSModuleDefWeakref {
    pub fn upgrade(&self) -> Option<JSModuleDef> {
        self.0.upgrade().map(JSModuleDef)
    }
}

/// Note: 'name' is freed on error
#[deprecated(note = "use JSModuleDefInner::new() instead")]
pub unsafe fn js_new_module_def(ctx: &JSContext, name: JSAtom) -> *mut JSModuleDefInner {
    unreachable!("unused");
    // if let Ok(m) = JSModuleDefInner::new(ctx, name) {
    //     Box::into_raw(m) // take ownership
    // } else {
    //     std::ptr::null_mut()
    // }
}

#[deprecated(note = "see impl Drop for JSModuleDefInner")]
pub unsafe fn js_free_module_def(ctx: &JSContext, m: *mut JSModuleDefInner) {
    unreachable!("unused")

    // JS_FreeAtom(ctx, (*m).module_name);

    // for i in 0..(*m).req_module_entries_count {
    //     let rme: *mut JSReqModuleEntry = (*m).req_module_entries.offset(i as isize);
    //     JS_FreeAtom(ctx, (*rme).module_name);
    // }
    // js_free(ctx, (*m).req_module_entries.cast());

    // for me in (*m).export_entries.drain(..) {
    //     if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_ref, .. } = me.export_type {
    //         debug_assert!(var_ref.is_some());
    //         if let Some(vr) = var_ref {
    //             JSVarRef::free(ctx.rt_mut(), vr.cast());
    //         }
    //     }

    //     JS_FreeAtom(ctx, me.export_name);
    //     JS_FreeAtom(ctx, me.local_name);
    // }

    // js_free(ctx, (*m).star_export_entries.cast());

    // for i in 0..(*m).import_entries_count {
    //     let mi: *mut JSImportEntry = (*m).import_entries.offset(i as isize);
    //     JS_FreeAtom(ctx, (*mi).import_name);
    // }
    // js_free(ctx, (*m).import_entries.cast());
    // js_free(ctx, (*m).async_parent_modules.cast());

    // JS_FreeValue(ctx, &(*m).module_ns);
    // JS_FreeValue(ctx, &(*m).func_obj);
    // JS_FreeValue(ctx, (*m).eval_exception);
    // JS_FreeValue(ctx, (*m).meta_obj);
    // JS_FreeValue(ctx, (*m).promise);
    // JS_FreeValue(ctx, (*m).resolving_funcs[0]);
    // JS_FreeValue(ctx, (*m).resolving_funcs[1]);
    // list_del(&mut (*m).link);

    // drop(Box::from_raw(m));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSFreeModuleEnum {
    JS_FREE_MODULE_ALL,
    JS_FREE_MODULE_NOT_RESOLVED,
}

// XXX: would be more efficient with separate module lists
pub fn js_free_modules(ctx: &mut JSContextInner, flag: JSFreeModuleEnum) {
    // To free modules that meet condition:
    // flag == JSFreeModuleEnum::JS_FREE_MODULE_ALL || (flag == JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED && m.resolved)

    // retain modules of negative condition shown above.
    // dropped module will be finalized by Drop trait
    ctx.loaded_modules.retain(|w| {
        if let Some(m) = w.upgrade() {
            !(flag == JSFreeModuleEnum::JS_FREE_MODULE_ALL
                || (flag == JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED && m.0.borrow().resolved))
        } else {
            false
        }
    });
}

pub unsafe fn add_module_variables(ctx: &JSContext, fd: &mut JSFunctionDef) -> i32 {
    // The imported global variables were added as closure variables
    // in js_parse_import(). We add here the module global
    // variables.
    for i in 0..fd.global_vars.len() {
        let hf = &fd.global_vars[i];
        if fd
            .add_closure_var(
                ctx,
                true,
                false,
                i as _,
                hf.var_name,
                hf.is_const(),
                hf.is_lexical(),
                JSVarKindEnum::JS_VAR_NORMAL,
            )
            .is_err()
        {
            return -1;
        }
    }

    let m = fd.module.as_ref().unwrap();

    // resolve the variable names of the local exports
    for me in m.0.borrow_mut().export_entries.iter_mut() {
        if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_idx, .. } = &mut me.export_type {
            if let Some(idx) = fd.find_closure_var(me.local_name) {
                *var_idx = idx;
            } else {
                JS_ThrowSyntaxError(
                    ctx,
                    &format!(
                        "exported variable '{}' does not exist",
                        me.local_name.to_str(ctx.rt())
                    ),
                );
                return -1;
            }
        }
    }

    0
}

// default module filename normalizer
pub fn js_default_module_normalize_name(
    ctx: &JSContext,
    base_name: &str,
    name: &str,
) -> Result<String, i32> {
    debug_assert!(!name.is_empty());

    let path = if let Some(base) = PathBuf::from(base_name).parent() {
        base.join(name)
    } else {
        PathBuf::from(name)
    };

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => {
                normalized.push(component.as_os_str());
            }
        }
    }

    Ok(normalized.to_string_lossy().to_string())
}

/* Unfortunately, the spec gives a different behavior from GetOwnProperty ! */
fn js_module_ns_has(ctx: &JSContext, obj: &JSObject, prop: JSAtom) -> i32 {
    (!obj.has_shape_property(prop)) as i32
}

pub static js_module_ns_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    has_property: Some(js_module_ns_has),
    get_own_property: None,
    get_own_property_names: None,
    delete_property: None,
    define_own_property: None,
    get_property: None,
    set_property: None,
};

#[deprecated]
pub unsafe fn JS_NewModuleValue(m: JSModuleDef) -> JSValue {
    m.into()
}

// must be done before js_link_module() because of cyclic references
pub unsafe fn js_create_module_function(ctx: &JSContext, m: &JSModuleDef) -> i32 {
    if m.0.borrow().func_created {
        return 0;
    }

    let is_c_module = m.0.borrow().init_func.is_some();
    if is_c_module {
        // initialize the exported variables
        for me in m.0.borrow_mut().export_entries.iter_mut() {
            if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_ref, .. } = &mut me.export_type {
                if let Ok(vr) = js_create_module_var(ctx, false) {
                    var_ref.replace(vr);
                } else {
                    return -1;
                }
            }
        }
    } else {
        if js_create_module_bytecode_function(ctx, m).is_err() {
            return -1;
        }
    }

    m.0.borrow_mut().func_created = true;

    // do it on the dependencies
    for i in 0..m.0.borrow().req_module_entries_count {
        let rme = m.0.borrow().req_module_entries.offset(i as isize);
        if js_create_module_function(ctx, (*rme).module.as_ref().unwrap()) < 0 {
            return -1;
        }
    }

    0
}

pub fn js_create_module_var(ctx: &JSContext, is_lexical: bool) -> Result<JSVarRef, i32> {
    let var_ref = JSVarRef::new(
        ctx.rt_mut(),
        if is_lexical {
            JSValue::UNINITIALIZED
        } else {
            JSValue::UNDEFINED
        },
    );

    ctx.rt_mut()
        .gc_add_object(var_ref.id(), var_ref.downgrade().into());
    Ok(var_ref)
}

// Create the <eval> function associated with the module
pub unsafe fn js_create_module_bytecode_function(
    ctx: &JSContext,
    m: &JSModuleDef,
) -> Result<(), i32> {
    // get bytecode
    let b = unsafe {
        m.0.borrow()
            .bytecode
            .object_unchecked()
            .0
            .isa
            .borrow()
            .func()
            .and_then(|f| f.bytecode.clone())
            .unwrap()
    };

    // make isa to instance
    let isa = {
        let mut var_refs = Vec::<JSVarRef>::new();
        let cvar_count = b.0.borrow().closure_var_count;
        if cvar_count != 0 {
            var_refs
                .try_reserve_exact(cvar_count as usize)
                .map_err(|_| {
                    ctx.JS_ThrowOutOfMemory();
                    -1
                })?;

            // create the global variables.
            // The other variables are imported from other modules
            for cv in b.closure_vars_slice().iter() {
                if cv.is_local() {
                    let vr = js_create_module_var(ctx, cv.is_lexical())?;
                    #[cfg(DUMP_MODULE_RESOLVE)]
                    println!("local {}: {:p}", i, var_ref);

                    var_refs.push(vr);
                } else {
                    // The other variables are imported from other modules.
                    // Add slot with a placeholder.
                    var_refs.push(JSVarRef::new(ctx.rt_mut(), JSValue::UNDEFINED));
                }
            }
        }

        JSObjectIsa::BYTECODE_FUNCTION(JSObjectFunc {
            bytecode: Some(b),
            var_refs,
            home_object: None,
        })
    };

    // create JS_CLASS_BYTECODE_FUNCTION object
    let func_obj = ctx
        .create_instance_from_proto(&ctx.0.borrow().function_proto, isa)
        .map_err(|_| -1)?;

    m.0.borrow_mut().bytecode = func_obj;
    Ok(())
}

// return JS_ATOM_NULL if the name cannot be found. Only works with
// not striped bytecode functions.
pub unsafe fn JS_GetScriptOrModuleName(ctx: &JSContext, n_stack_levels: i32) -> JSAtom {
    // XXX: currently we just use the filename of the englobing
    // function from the debug info. May need to add a ScriptOrModule
    // info in Bytecode.
    let mut sf = ctx.rt().cur_stack_frame.cast::<StackFrame>();
    if sf.is_null() {
        return JS_ATOM_NULL;
    }

    let mut levels = n_stack_levels;

    while levels > 0 {
        sf = (*sf).prev_frame.cast::<StackFrame>();
        if sf.is_null() {
            return JS_ATOM_NULL;
        }
        levels -= 1;
    }

    loop {
        if let JSValue::Object(p) = &(*sf).cur_running {
            if let Some(b) =
                p.0.isa
                    .borrow()
                    .func()
                    .map(|f| f.bytecode.as_ref().unwrap())
            {
                let b = b.0.borrow();
                if !b.is_direct_or_indirect_eval {
                    if !b.has_debug {
                        return JS_ATOM_NULL;
                    }
                    return b.debug.filename.dup(ctx.rt());
                } else {
                    sf = (*sf).prev_frame.cast::<StackFrame>(); // walk up
                    if sf.is_null() {
                        return JS_ATOM_NULL;
                    }
                }
            } else {
                return JS_ATOM_NULL;
            }
        } else {
            return JS_ATOM_NULL;
        }
    }
}

#[deprecated(note = "use JSModuleDefInner::import_meta() instead")]
pub unsafe fn JS_GetImportMeta(ctx: &JSContext, m: *mut JSModuleDefInner) -> JSValue {
    (*m).import_meta(ctx).unwrap_or(JSValue::EXCEPTION)
}

/// get module import.meta object of context current execution.
pub unsafe fn js_import_meta(ctx: &JSContext) -> JSValue {
    let filename = JS_GetScriptOrModuleName(ctx, 0);
    if filename == JS_ATOM_NULL {
        ctx.JS_ThrowTypeError("import.meta not supported in this context ");
        return JSValue::EXCEPTION;
    }

    // XXX: inefficient, need to add a module or script pointer in
    // Bytecode
    let m = ctx.find_loaded_module(filename);
    JS_FreeAtom(ctx, filename);
    if let Some(m) = m {
        m.0.borrow_mut()
            .import_meta(ctx)
            .unwrap_or(JSValue::EXCEPTION)
    } else {
        ctx.JS_ThrowTypeError("import.meta not supported in this context");
        JSValue::EXCEPTION
    }
}

pub unsafe fn JS_SetModuleLoaderFunc(
    rt: *mut JSRuntime,
    module_normalize: Option<JSModuleNormalizeFunc>,
    module_loader: JSModuleLoaderFunc,
    opaque: *mut std::ffi::c_void,
) {
    (*rt).module_normalize_func = module_normalize;
    (*rt).module_loader_func = Some(module_loader);
    (*rt).module_loader_opaque = opaque;
}

pub fn js_dynamic_import(ctx: &JSContext, specifier: &JSValue) -> JSResult {
    unimplemented!()
}
