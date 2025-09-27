use std::{
    cell::{Cell, RefCell, UnsafeCell},
    path::PathBuf,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JS_FreeAtomRT, JSAtom},
    class::JSClassExoticMethods,
    context::{JSContext, JSContextInner},
    error::JSResult,
    function::{BCFuncPayload, JSFunctionDef, JSVarKindEnum, JSVarRef},
    module::{ExportEntry, ExportType, JSImportEntry, JSModuleStatus, ModuleDependency},
    object::{JS_PROP_C_W_E, JSObject, ObjectPayload},
    promise::Promise,
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

    pub(super) dependencies: RefCell<Vec<ModuleDependency>>,
    pub import_entries: RefCell<Vec<JSImportEntry>>,
    pub export_entries: RefCell<Vec<Box<ExportEntry>>>,
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
    has_tla: Cell<bool>,
    pub resolved: Cell<bool>,
    pub status: Cell<JSModuleStatus>,

    /* temp use during js_module_link() & js_module_evaluate() */
    pub dfs_index: Cell<i32>,
    pub dfs_ancestor_index: Cell<i32>,

    pub(super) stack_prev: RefCell<Option<ModuleDef>>,
    pub(super) cycle_root: RefCell<Option<ModuleDef>>,

    //
    // temp use during js_module_evaluate()
    //
    pub(super) async_parent_modules: RefCell<Vec<ModuleDef>>,
    pub(super) async_evaluation: Cell<bool>,
    pub(super) async_evaluation_timestamp: Cell<i64>,
    pub(super) pending_async_dependencies: Cell<i32>,

    pub promise: Cell<Option<Promise>>, /* corresponds to spec field: capability */
    pub resolving_funcs: Cell<[JSValue; 2]>, /* corresponds to spec field: capability */

    pub eval_exception: UnsafeCell<Option<JSValue>>,

    /// the `import.meta` object of module
    pub meta_obj: Cell<Option<JSObject>>,
}

impl PartialEq for ModuleDefInner {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Drop for ModuleDefInner {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl ModuleDefInner {
    /// 'name' is consumed
    fn new(ctx: &JSContext, name: JSAtom) -> Self {
        let md = Self {
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

            promise: Cell::new(None),
            resolving_funcs: Cell::new([const { JSValue::UNDEFINED }; 2]),
            eval_exception: UnsafeCell::new(None),

            meta_obj: Cell::new(None),
        };

        md
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
            // create meta object
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
        let sval = ctx.create_string_from_str(url)?;
        meta_obj.define_property_value_str(ctx, "url", sval.into(), JS_PROP_C_W_E)?;
        meta_obj.define_property_value_str(ctx, "main", is_main.into(), JS_PROP_C_W_E)?;
        Ok(())
    }

    // Make the <eval> function associated with the module, for bytecode module.
    fn create_module_bytecode_function(&self, ctx: &JSContext) -> Result<(), i32> {
        debug_assert!(!self.func_created.get());

        match unsafe { (*self.bytecode.as_ptr()).clone() } {
            JSValue::Bytecode(bc) => {
                // create bytecode function with bytecode
                let mut var_refs = Vec::<JSVarRef>::new();
                let cvar_count = bc.0.borrow().closure_var_count;
                if cvar_count != 0 {
                    var_refs
                        .try_reserve_exact(cvar_count as usize)
                        .map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;

                    // create the global variables.
                    // The other variables are imported from other modules
                    for cv in bc.closure_vars_slice().iter() {
                        if cv.is_local() {
                            let vr = js_create_module_var(ctx, cv.is_lexical())?;
                            var_refs.push(vr);
                        } else {
                            // The other variables are imported from other modules.
                            // Add slot with a placeholder.
                            var_refs.push(JSVarRef::new(ctx.rt_mut(), JSValue::UNDEFINED));
                        }
                    }
                }

                let isa = ObjectPayload::BYTECODE_FUNCTION(BCFuncPayload {
                    bytecode: Some(bc.clone()),
                    var_refs,
                    home_object: None,
                });
                let proto = ctx.0.borrow().function_proto.clone();
                let func_obj = ctx.create_object_from_proto(proto, isa).map_err(|_| -1)?;
                self.bytecode.replace(func_obj.into());
            }

            _ => {
                unreachable!("module bytecode function already created");
            }
        }

        Ok(())
    }

    /// make module <eval> function from bytecode
    /// must be done before js_link_module() because of cyclic references
    fn create_module_function(&self, ctx: &JSContext) -> Result<(), i32> {
        if self.func_created.get() {
            return Ok(());
        }

        if self.init_func.is_some() {
            // is native module.
            // initialize the exported variables
            for exp in self.export_entries.borrow_mut().iter_mut() {
                if let ExportType::JS_EXPORT_TYPE_LOCAL { var_ref, .. } = &mut exp.export_type {
                    var_ref.replace(js_create_module_var(ctx, false)?);
                }
            }
        } else {
            // is bytecode module
            self.create_module_bytecode_function(ctx)?;
        }

        self.func_created.set(true);

        // make <eval> function for dependency modules
        for dep in self.dependencies.borrow().iter() {
            dep.module.as_ref().unwrap().create_module_function(ctx)?;
        }

        Ok(())
    }

    fn finalize(&mut self) {
        let rt = self.rt_mut();

        JS_FreeAtomRT(rt, self.module_name);

        for dep in self.dependencies.borrow_mut().drain(..) {
            JS_FreeAtomRT(rt, dep.module_name);
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

#[repr(transparent)]
#[derive(Clone, PartialEq)]
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
    pub(crate) fn new(ctx: &JSContext, name: JSAtom) -> Self {
        let m = Self(Rc::new(ModuleDefInner::new(ctx, name)));
        ctx.0.borrow_mut().loaded_modules.push(m.downgrade());
        m
    }

    /// get module `import.meta` object.
    pub fn import_meta(&self, ctx: &JSContext) -> Result<JSObject, i32> {
        self.0.import_meta(ctx)
    }

    /// get module name atom. returned atom is not duplicated.
    #[inline(always)]
    pub fn module_name(&self) -> JSAtom {
        self.0.module_name
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.id()
    }

    #[inline(always)]
    pub fn has_tla(&self) -> bool {
        self.0.has_tla.get()
    }

    #[inline(always)]
    pub fn set_tla(&self, b: bool) {
        self.0.has_tla.set(b);
    }

    /// get module loading promise
    #[inline(always)]
    pub(crate) fn promise(&self) -> Option<Promise> {
        unsafe { (*self.0.promise.as_ptr()).as_ref().cloned() }
    }

    /// get eval exception. None means no exception.
    #[inline(always)]
    pub fn eval_exception(&self) -> Option<JSValue> {
        unsafe {
            let p = &*self.0.eval_exception.get();
            p.clone()
        }
    }

    /// set eval exception
    pub fn set_eval_exception(&self, exc: Option<JSValue>) {
        debug_assert!(exc.as_ref().map_or(true, |v| !v.is_undefined_or_null()));

        unsafe {
            let p = &mut *self.0.eval_exception.get();
            *p = exc;
        }
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

    /// make module <eval> function from bytecode
    /// must be done before js_link_module() because of cyclic references
    #[inline(always)]
    pub(super) fn create_module_function(&self, ctx: &JSContext) -> Result<(), i32> {
        self.0.create_module_function(ctx)
    }
}

#[derive(Debug, Clone)]
pub struct JSModuleDefWeakref(Weak<ModuleDefInner>);

impl JSModuleDefWeakref {
    pub fn upgrade(&self) -> Option<ModuleDef> {
        self.0.upgrade().map(ModuleDef)
    }
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
        if let ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, .. } = &mut me.export_type {
            if let Some(idx) = fd.find_closure_var(me.local_name) {
                *var_idx = idx;
            } else {
                ctx.throw_syntax_error(&format!(
                    "exported variable '{}' does not exist",
                    me.local_name.to_utf8(ctx.rt())
                ));
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
fn js_module_ns_has(ctx: &JSContext, obj: &JSObject, prop: JSAtom) -> Result<bool, i32> {
    Ok(obj.has_shape_property(prop))
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

// return JS_ATOM_NULL if the name cannot be found. Only works with
// not striped bytecode functions.
pub fn JS_GetScriptOrModuleName(ctx: &JSContext, n_stack_levels: i32) -> JSAtom {
    // XXX: currently we just use the filename of the englobing
    // function from the debug info. May need to add a ScriptOrModule
    // info in Bytecode.
    let mut sf = ctx.rt().cur_stack_frame.cast::<StackFrame>();
    if sf.is_null() {
        return JS_ATOM_NULL;
    }

    let mut levels = n_stack_levels;

    while levels > 0 {
        sf = unsafe { (&*sf).prev_frame.cast::<StackFrame>() };
        if sf.is_null() {
            return JS_ATOM_NULL;
        }
        levels -= 1;
    }

    loop {
        if let JSValue::Object(p) = unsafe { &(&*sf).cur_func } {
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
                    sf = unsafe { (&*sf).prev_frame.cast::<StackFrame>() }; // walk up
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
pub fn js_import_meta(ctx: &JSContext) -> Result<JSObject, i32> {
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
    ctx.throw_internal_error("import() is not implemented");
    Err(-1)
}
