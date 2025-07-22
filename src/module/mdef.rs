use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use crate::{
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_BYTECODE_FUNCTION,
    atom::{JS_ATOM_NULL, JS_FreeAtom, JS_FreeAtomRT, JSAtom},
    class::JSClassExoticMethods,
    context::{JSContext, JSContextInner},
    error::{JS_ThrowSyntaxError, JSResult},
    function::{JSFunctionDef, JSVarKindEnum, JSVarRef, ObjFunc},
    mem::js_free_rt,
    module::{
        JSExportEntry, JSExportTypeEnum, JSImportEntry, JSModuleStatus, JSStarExportEntry,
        ModuleDependency,
    },
    object::{JS_PROP_C_W_E, JSObject, ObjectDetail},
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
) -> Result<ModuleDef, i32>;

pub type JSModuleInitFunc = fn(ctx: &JSContext, m: &ModuleDef) -> i32;

pub(crate) struct ModuleDefInner {
    rt: NonNull<JSRuntime>,
    gc_mark: Cell<bool>,

    pub module_name: JSAtom,

    pub dependencies: RefCell<Vec<ModuleDependency>>,
    pub import_entries: RefCell<Vec<JSImportEntry>>,
    pub export_entries: RefCell<Vec<Box<JSExportEntry>>>,
    /// list of index in dependencies
    pub star_export_entries: RefCell<Vec<u16>>,

    /// module namespace.
    pub module_ns: Cell<Option<JSObject>>,

    /// for js module, when func_created is:
    /// - true: bytecode field is a function object value
    /// - false: bytecode field is a bytecode value
    pub func_created: Cell<bool>,

    /// for JS modules only, this value could be:
    /// * bytecode, when func_created is false,
    /// * function object, when func_created is true.
    /// * undefined for initially unknown.
    pub bytecode: Cell<JSValue>,
    /// for C modules only
    pub init_func: Option<JSModuleInitFunc>,

    /// true if func_obj contains await
    pub has_tla: Cell<bool>,
    pub resolved: Cell<bool>,
    pub status: Cell<JSModuleStatus>,

    /* temp use during js_module_link() & js_module_evaluate() */
    pub dfs_index: Cell<i32>,
    pub dfs_ancestor_index: Cell<i32>,

    pub stack_prev: RefCell<Option<ModuleDef>>,
    pub cycle_root: RefCell<Option<ModuleDef>>,

    //
    // temp use during js_module_evaluate()
    //
    pub async_parent_modules: RefCell<Vec<ModuleDef>>,

    pub pending_async_dependencies: Cell<i32>,
    pub async_evaluation: Cell<bool>,
    pub async_evaluation_timestamp: Cell<i64>,

    pub promise: Cell<JSValue>, /* corresponds to spec field: capability */
    pub resolving_funcs: Cell<[JSValue; 2]>, /* corresponds to spec field: capability */

    /* true if evaluation yielded an exception. It is saved in
    eval_exception */
    pub eval_has_exception: Cell<bool>,
    pub eval_exception: Cell<JSValue>,

    /// the `import.meta` object of module
    pub meta_obj: Cell<Option<JSObject>>,
}

impl Drop for ModuleDefInner {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl ModuleDefInner {
    /// 'name' is consumed
    pub fn new(ctx: &JSContext, name: JSAtom) -> Self {
        let o = Self {
            rt: ctx.rt_ptr(),
            gc_mark: Cell::new(false),

            module_name: name,

            dependencies: RefCell::new(Vec::new()),
            import_entries: RefCell::new(Vec::new()),
            export_entries: RefCell::new(Vec::new()),
            star_export_entries: RefCell::new(Vec::new()),

            module_ns: Cell::new(None),
            bytecode: Cell::new(JSValue::UNDEFINED),
            init_func: None,

            has_tla: Cell::new(false),
            resolved: Cell::new(false),
            func_created: Cell::new(false),
            status: Cell::new(JSModuleStatus::JS_MODULE_STATUS_UNLINKED),

            dfs_index: Cell::new(0),
            dfs_ancestor_index: Cell::new(0),

            cycle_root: RefCell::new(None),
            stack_prev: RefCell::new(None),
            async_parent_modules: RefCell::new(Vec::new()),

            pending_async_dependencies: Cell::new(0),
            async_evaluation: Cell::new(false),
            async_evaluation_timestamp: Cell::new(0),

            promise: Cell::new(JSValue::UNDEFINED),
            resolving_funcs: Cell::new([const { JSValue::UNDEFINED }; 2]),
            eval_has_exception: Cell::new(false),
            eval_exception: Cell::new(JSValue::UNDEFINED),

            meta_obj: Cell::new(None),
        };

        o
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    pub fn rt(&self) -> &JSRuntime {
        unsafe { self.rt.as_ref() }
    }

    pub fn rt_mut(&self) -> &mut JSRuntime {
        unsafe { self.rt.as_ptr().as_mut().unwrap() }
    }

    /// get module import.meta object.
    pub fn import_meta(&self, ctx: &JSContext) -> Result<JSObject, i32> {
        if let Some(o) = self.meta_obj.take() {
            self.meta_obj.replace(Some(o.clone()));
            Ok(o)
        } else {
            // lazy initialize meta object
            let o = ctx
                .create_base_object_from_proto(JSValue::NULL)
                .inspect_err(|_| {
                    ctx.throw_type_error("import.meta not supported in this context");
                })?;
            self.meta_obj.replace(Some(o.clone()));
            Ok(o)
        }
    }

    /// set module import.meta props.
    pub fn set_import_meta(&self, ctx: &JSContext, url: &str, is_main: bool) -> Result<(), i32> {
        let meta_obj = self.import_meta(ctx)?;
        let sval = ctx.create_string(url);
        meta_obj.define_property_value_str(ctx, "url", sval, JS_PROP_C_W_E)?;
        meta_obj.define_property_value_str(ctx, "main", is_main.into(), JS_PROP_C_W_E)?;
        Ok(())
    }

    fn finalize(&mut self) {
        let rt = self.rt_mut();

        JS_FreeAtomRT(rt, self.module_name);

        for rme in self.dependencies.borrow_mut().drain(..) {
            JS_FreeAtomRT(rt, rme.module_name);
        }

        for me in self.export_entries.borrow_mut().drain(..) {
            // if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_ref, .. } = me.export_type {
            //     debug_assert!(var_ref.is_some());
            //     drop(var_ref);
            // }

            JS_FreeAtomRT(rt, me.export_name);
            JS_FreeAtomRT(rt, me.local_name);
        }

        for mi in self.import_entries.borrow_mut().drain(..) {
            JS_FreeAtomRT(rt, mi.import_name);
        }
    }
}

#[derive(Clone)]
pub struct ModuleDef(pub(crate) Rc<ModuleDefInner>);

impl std::fmt::Debug for ModuleDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "<ModuleDef id={:p}>",
            self.id() as *const std::ffi::c_void
        )
    }
}

impl From<ModuleDef> for JSValue {
    fn from(value: ModuleDef) -> Self {
        Self::Module(value)
    }
}

impl ModuleDef {
    /// create bytecode module def.
    /// 'name' is consumed
    pub fn new(ctx: &JSContext, name: JSAtom) -> Self {
        let inner = ModuleDefInner::new(ctx, name);
        Self(Rc::new(inner))
    }

    /// get module `import.meta` object.
    pub fn import_meta(&self, ctx: &JSContext) -> Result<JSObject, i32> {
        self.0.import_meta(ctx)
    }

    /// get module name atom. returned atom is not duplicated.
    pub fn module_name_atom(&self) -> JSAtom {
        self.0.module_name
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.id()
    }

    pub fn downgrade(&self) -> JSModuleDefWeakref {
        JSModuleDefWeakref(Rc::downgrade(&self.0))
    }

    pub fn gc_has_mark(&self) -> bool {
        self.0.gc_mark.get()
    }

    pub fn gc_set_mark(&self, mark: bool) {
        self.0.gc_mark.set(mark);
    }
}

#[derive(Debug, Clone)]
pub struct JSModuleDefWeakref(Weak<ModuleDefInner>);

impl JSModuleDefWeakref {
    pub fn upgrade(&self) -> Option<ModuleDef> {
        self.0.upgrade().map(ModuleDef)
    }
}

/// Note: 'name' is freed on error
#[deprecated(note = "use JSModuleDefInner::new() instead")]
pub unsafe fn js_new_module_def(ctx: &JSContext, name: JSAtom) -> *mut ModuleDefInner {
    unreachable!("unused");
    // if let Ok(m) = JSModuleDefInner::new(ctx, name) {
    //     Box::into_raw(m) // take ownership
    // } else {
    //     std::ptr::null_mut()
    // }
}

#[deprecated(note = "see impl Drop for JSModuleDefInner")]
pub unsafe fn js_free_module_def(ctx: &JSContext, m: *mut ModuleDefInner) {
    unreachable!("unused")

    // JS_FreeAtom(ctx, (*m).module_name);

    // for i in 0..(*m).req_module_entries_count {
    //     let rme: *mut ModuleDependency = (*m).dependencies.offset(i as isize);
    //     JS_FreeAtom(ctx, (*rme).module_name);
    // }
    // js_free(ctx, (*m).dependencies.cast());

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
                || (flag == JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED && m.0.resolved.get()))
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
    for me in m.0.export_entries.borrow_mut().iter_mut() {
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
pub unsafe fn JS_NewModuleValue(m: ModuleDef) -> JSValue {
    m.into()
}

// must be done before js_link_module() because of cyclic references
pub unsafe fn js_create_module_function(ctx: &JSContext, m: &ModuleDef) -> i32 {
    if m.0.func_created.get() {
        return 0;
    }

    let is_c_module = m.0.init_func.is_some();
    if is_c_module {
        // initialize the exported variables
        for me in m.0.export_entries.borrow_mut().iter_mut() {
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

    m.0.func_created.set(true);

    // create module <eval> function for dependencies
    for dep in m.0.dependencies.borrow().iter() {
        if js_create_module_function(ctx, dep.module.as_ref().unwrap()) < 0 {
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
    m: &ModuleDef,
) -> Result<(), i32> {
    debug_assert!(!m.0.func_created.get());

    let module_bc = {
        let bc = m.0.bytecode.replace(JSValue::UNDEFINED);
        m.0.bytecode.replace(bc.clone());
        bc
    };

    if let JSValue::Bytecode(b) = module_bc {
        // create function from bytecode
        let (isa, class_id) = {
            let mut var_refs = Vec::<JSVarRef>::new();
            let cvar_count = b.0.borrow().closure_var_count;
            if cvar_count != 0 {
                var_refs
                    .try_reserve_exact(cvar_count as usize)
                    .map_err(|_| {
                        ctx.throw_out_of_memory();
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

            // create bytecode function when has_tla=false;
            // create async function when has_tla=true;
            if m.0.has_tla.get() {
                // b.0.borrow_mut().js_mode |= JS_MODE_ASYNC as u8;
                // (
                //     ObjectDetail::ASYNC_FUNCTION(ObjFunc {
                //         bytecode: Some(b.clone()),
                //         var_refs,
                //         home_object: None,
                //     }),
                //     JS_CLASS_BYTECODE_FUNCTION,
                // )
                (
                    ObjectDetail::BYTECODE_FUNCTION(ObjFunc {
                        bytecode: Some(b.clone()),
                        var_refs,
                        home_object: None,
                    }),
                    JS_CLASS_ASYNC_FUNCTION,
                )
            } else {
                (
                    ObjectDetail::BYTECODE_FUNCTION(ObjFunc {
                        bytecode: Some(b.clone()),
                        var_refs,
                        home_object: None,
                    }),
                    JS_CLASS_ASYNC_FUNCTION,
                )
            }
        };

        // create function object
        let proto = ctx.0.borrow().class_proto[class_id as usize].clone();
        let func_obj = ctx.create_object_from_proto(proto, isa).map_err(|_| -1)?;
        m.0.bytecode.replace(func_obj.into());
    } else {
        debug_assert!(
            matches!(
                module_bc.object().map(|o| o.class_id()),
                Some(JS_CLASS_BYTECODE_FUNCTION)
            ),
            "module bytecode has been created into function object",
        );
    }

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
                p.0.payload
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
pub unsafe fn JS_GetImportMeta(ctx: &JSContext, m: *mut ModuleDefInner) -> JSValue {
    unreachable!("unused");
}

/// get module import.meta object of context current execution.
pub unsafe fn js_import_meta(ctx: &JSContext) -> Result<JSObject, i32> {
    let filename = JS_GetScriptOrModuleName(ctx, 0);
    if filename == JS_ATOM_NULL {
        ctx.throw_type_error("import.meta not supported in this context");
        return Err(-1);
    }

    // XXX: inefficient, need to add a module or script pointer in
    // Bytecode
    let m = ctx.find_loaded_module(filename);
    JS_FreeAtom(ctx, filename);
    if let Some(m) = m {
        m.import_meta(ctx)
    } else {
        ctx.throw_type_error("import.meta not supported in this context");
        Err(-1)
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
