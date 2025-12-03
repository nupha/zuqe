use gc_lite::{Collect, GcRef};

use crate::{
    atom::Atom,
    builtins::promise::Promise,
    class::JSClassExoticMethods,
    context::{ContextHandle, ContextImpl, ContextRef},
    function::{BCFuncPayload, FieldDef, JSFunctionDef, JSVarKindEnum, JSVarRef},
    gcref_wrapper,
    module::{ExportEntry, ExportType, ImportEntry, JSModuleStatus, ModuleDependency},
    object::{JS_PROP_C_W_E, ObjectPayload, ObjectRef},
    runtime::{RuntimeImpl, RuntimePtr},
    stack_frame::StackFrameParentIter,
    value::JSValue,
};

pub type ModuleNormalizeFn = fn(
    ctx: ContextHandle,
    base_name: &str,
    name: &str,
    opaque: *mut ::core::ffi::c_void,
) -> Result<String, i32>;

pub type ModuleLoadFn = fn(
    ctx: ContextHandle,
    module_name: &str,
    opaque: *mut ::core::ffi::c_void,
) -> Result<ModuleDef, i32>;

pub(crate) trait ModuleInitFn: Fn(&ContextImpl, &ModuleDef) -> Result<(), i32> {}
impl<F> ModuleInitFn for F where F: Fn(&ContextImpl, &ModuleDef) -> Result<(), i32> {}

pub struct ModuleDefImpl {
    #[deprecated(note = "seems no use, should be removed")]
    rt: RuntimePtr,

    /// hold a refcount of the atom
    pub module_name: Atom,

    pub(super) dependencies: Vec<ModuleDependency>,
    pub(crate) import_entries: Vec<ImportEntry>,
    pub(crate) export_entries: Vec<ExportEntry>,

    /// list of index in dependencies
    pub(crate) star_export_entries: Vec<u16>,

    /// module namespace.
    pub(super) module_ns: Option<ObjectRef>,

    /// for js module, when func_created is:
    /// - true: bytecode field is a function object value
    /// - false: bytecode field is a bytecode value
    pub(crate) func_created: bool,
    /// for JS modules only, this value could be:
    /// * Bytecode, when func_created is false,
    /// * BCFunction object, when func_created is true.
    /// * undefined for uninitialized.
    pub(crate) bytecode: JSValue,
    /// init callback for native module
    pub(super) init_func: Option<Box<dyn ModuleInitFn>>,

    /// true if func_obj contains await
    has_tla: bool,
    pub resolved: bool,
    pub status: JSModuleStatus,

    /* temp use during js_module_link() & js_module_evaluate() */
    pub(super) dfs_index: i32,
    pub(super) dfs_ancestor_index: i32,

    pub(super) stack_prev: Option<ModuleDef>,
    pub(super) cycle_root: Option<ModuleDef>,

    //
    // temp use during js_module_evaluate()
    //
    pub(super) async_parent_modules: Vec<ModuleDef>,
    pub(super) async_evaluation: bool,
    pub(super) async_evaluation_timestamp: i64,
    pub(super) pending_async_dependencies: i32,

    pub promise: Option<Promise>, /* corresponds to spec field: capability */
    pub resolving_funcs: [JSValue; 2], /* corresponds to spec field: capability */
    pub eval_exception: Option<JSValue>,
    /// the `import.meta` object of module
    pub meta_obj: Option<ObjectRef>,
}

unsafe impl Collect for ModuleDefImpl {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.module_name.trace(tracer);
        self.dependencies.trace(tracer);
        self.export_entries.trace(tracer);
        self.import_entries.trace(tracer);

        self.bytecode.trace(tracer);
        self.async_parent_modules.trace(tracer);
        self.promise.trace(tracer);
        self.resolving_funcs[0].trace(tracer);
        self.resolving_funcs[1].trace(tracer);

        self.stack_prev.trace(tracer);
        self.module_ns.trace(tracer);
        self.cycle_root.trace(tracer);
        self.eval_exception.trace(tracer);
        self.meta_obj.trace(tracer);
    }
}

impl PartialEq for ModuleDefImpl {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl ModuleDefImpl {
    fn new(ctx: &ContextImpl, module_name: Atom) -> Self {
        Self {
            rt: ctx.rt(),

            module_name,

            dependencies: Vec::new(),
            import_entries: Vec::new(),
            export_entries: Vec::new(),
            star_export_entries: Vec::new(),

            module_ns: None,
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

            promise: None,
            resolving_funcs: [const { JSValue::UNDEFINED }; 2],
            eval_exception: None,

            meta_obj: None,
        }
    }

    /// create a native module def with module init func.
    fn create(
        ctx: &ContextImpl,
        name: &str,
        init: impl ModuleInitFn + 'static,
    ) -> Result<Self, i32> {
        let name = Atom::new(&ctx.rt(), name)?;
        let mut md = ModuleDefImpl::new(ctx, name);
        md.init_func.replace(Box::new(init));
        Ok(md)
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    /// get module name atom.
    /// returned atom is not duplicated, caller should clone it before keep it referenced.
    #[inline(always)]
    pub const fn module_name(&self) -> Atom {
        self.module_name
    }

    #[inline(always)]
    pub const fn has_tla(&self) -> bool {
        self.has_tla
    }

    #[inline(always)]
    pub(crate) const fn set_tla(&mut self, b: bool) {
        self.has_tla = b;
    }

    /// get eval exception. None means no exception.
    #[inline(always)]
    pub(crate) const fn eval_exception(&self) -> Option<JSValue> {
        self.eval_exception
    }

    /// set eval exception
    pub(crate) fn set_eval_exception(&mut self, exc: Option<JSValue>) {
        debug_assert!(exc.as_ref().is_none_or(|v| !v.is_undefined_or_null()));
        self.eval_exception = exc;
    }

    /// get module import.meta object.
    pub fn import_meta(&mut self, ctx: &ContextImpl) -> Result<ObjectRef, i32> {
        if let Some(o) = self.meta_obj {
            Ok(o)
        } else {
            // create meta object
            let o = ctx.create_base_object_with_proto(None).inspect_err(|_| {
                ctx.throw_type_error("import.meta not supported in this context");
            })?;
            self.meta_obj.replace(o);
            Ok(o)
        }
    }

    /// set module import.meta props.
    pub fn set_import_meta(
        &mut self,
        ctx: &ContextImpl,
        url: &str,
        is_main: bool,
    ) -> Result<(), i32> {
        let mut meta_obj = self.import_meta(ctx)?;
        let sval = ctx.create_string_from_str(url)?;
        meta_obj.define_property_value_str(ctx, "url", sval.into(), JS_PROP_C_W_E)?;
        meta_obj.define_property_value_str(ctx, "main", is_main.into(), JS_PROP_C_W_E)?;
        Ok(())
    }

    // Make the <eval> function associated with the module, for bytecode module.
    fn create_module_bytecode_function(&mut self, ctx: &ContextImpl) -> Result<(), i32> {
        debug_assert!(!self.func_created);

        match self.bytecode {
            JSValue::Bytecode(bc) => {
                // create bytecode function with bytecode
                let mut var_refs = Vec::<JSVarRef>::new();
                let cvar_count = bc.closure_var_count;
                if cvar_count != 0 {
                    var_refs
                        .try_reserve_exact(cvar_count as usize)
                        .map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;

                    // create the global variables.
                    // The other variables are imported from other modules
                    for cv in bc.closure_vars().iter() {
                        if cv.is_local() {
                            let vr = js_create_module_var(ctx, cv.is_lexical())?;
                            var_refs.push(vr);
                        } else {
                            // The other variables are imported from other modules.
                            // Add slot with a placeholder.
                            var_refs.push(JSVarRef::new(ctx, JSValue::UNDEFINED)?);
                        }
                    }
                }

                let payload = ObjectPayload::BYTECODE_FUNCTION(BCFuncPayload::new(bc, var_refs));
                let func = ctx
                    .create_object_with_proto(Some(ctx.function_proto()), payload)
                    .map_err(|_| -1)?;

                self.bytecode = func.into();
            }

            _ => {
                unreachable!("module bytecode function already created");
            }
        }

        Ok(())
    }

    /// make module <eval> function from bytecode
    /// must be done before module link because of cyclic references
    pub(super) fn create_module_function(&mut self, ctx: &ContextImpl) -> Result<(), i32> {
        if self.func_created {
            return Ok(());
        }

        if self.init_func.is_some() {
            // is native module: initialize exported variables
            for me in self.export_entries.iter_mut() {
                if let ExportType::JS_EXPORT_TYPE_LOCAL { var_ref, .. } = &mut me.export_type {
                    var_ref.replace(js_create_module_var(ctx, false)?);
                }
            }
        } else {
            // is bytecode module
            self.create_module_bytecode_function(ctx)?;
        }

        self.func_created = true;

        // make <eval> function for dependency modules
        for dep in self.dependencies.iter_mut() {
            dep.module.as_mut().unwrap().create_module_function(ctx)?;
        }

        Ok(())
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct ModuleDef(GcRef<ModuleDefImpl>);

gcref_wrapper!(ModuleDef, ModuleDefImpl);

impl From<ModuleDef> for JSValue {
    fn from(value: ModuleDef) -> Self {
        Self::Module(value)
    }
}

impl PartialEq for ModuleDef {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl std::fmt::Debug for ModuleDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<ModuleDef {:p}>", self.id() as *const std::ffi::c_void)
    }
}

impl ModuleDef {
    /// alloc an uninitialized (usually bytecode) module.
    pub(crate) fn new(ctx: &mut ContextImpl, name: Atom) -> Result<Self, i32> {
        let inner = ModuleDefImpl::new(ctx, name);
        let gc_ref = ctx.rt().gc_alloc(inner).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        let m = ModuleDef(gc_ref);
        ctx.loaded_modules.push(m);

        Ok(m)
    }

    /// create a native module def.
    pub(crate) fn create(
        ctx: &mut ContextImpl,
        name: &str,
        init: impl ModuleInitFn + 'static,
    ) -> Result<Self, i32> {
        let inner = ModuleDefImpl::create(ctx, name, init)?;
        let gc_ref = ctx.rt().gc_alloc(inner).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        let m = Self(gc_ref);
        ctx.loaded_modules.push(m);
        Ok(m)
    }
}

impl ContextImpl {
    /// register a native module
    pub fn register_module(&mut self, name: &str, entries: &'static [FieldDef]) -> Result<(), i32> {
        let mut md = ModuleDef::create(
            self,
            name,
            |cx: &ContextImpl, m: &ModuleDef| -> Result<(), i32> {
                m.set_export_values(cx, entries)
            },
        )?;

        // declare export names
        for me in entries {
            md.add_export(self, me.name)?;
        }

        Ok(())
    }
}

impl RuntimeImpl {
    /// set module loader callbacks
    pub fn set_module_loader(
        &mut self,
        norm_func: Option<ModuleNormalizeFn>,
        load_func: ModuleLoadFn,
        opaque: *mut std::ffi::c_void,
    ) {
        self.module_normalize_func = norm_func;
        self.module_load_func = Some(load_func);
        self.module_load_opaque = opaque;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSFreeModuleEnum {
    JS_FREE_MODULE_ALL,
    JS_FREE_MODULE_NOT_RESOLVED,
}

// XXX: would be more efficient with separate module lists
pub fn js_free_modules(ctx: &mut ContextImpl, flag: JSFreeModuleEnum) {
    // To free modules that meet condition:
    // flag == JSFreeModuleEnum::JS_FREE_MODULE_ALL || (flag == JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED && m.resolved)
    ctx.loaded_modules.retain(|m| {
        !(flag == JSFreeModuleEnum::JS_FREE_MODULE_ALL
            || (flag == JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED && m.resolved))
    });
}

pub fn add_module_variables(ctx: &ContextImpl, fd: &mut JSFunctionDef) -> i32 {
    // The imported global variables were added as closure variables
    // in js_parse_import(). We add here the module global
    // variables.
    for i in 0..fd.global_vars.len() {
        let (var_name, is_const, is_lexical) = {
            let v = &fd.global_vars[i];
            (v.var_name, v.is_const(), v.is_lexical())
        };

        if fd
            .add_closure_var(
                ctx,
                true,
                false,
                i as _,
                var_name,
                is_const,
                is_lexical,
                JSVarKindEnum::JS_VAR_NORMAL,
            )
            .is_err()
        {
            return -1;
        }
    }

    let mut m = fd.module.unwrap();

    // resolve the variable names of the local exports
    for me in m.export_entries.iter_mut() {
        if let ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, .. } = &mut me.export_type {
            if let Some(idx) = fd.find_closure_var(me.local_name) {
                *var_idx = idx;
            } else {
                ctx.throw_syntax_error(&format!(
                    "exported variable '{}' does not exist",
                    me.local_name.to_utf8()
                ));
                return -1;
            }
        }
    }

    0
}

/* Unfortunately, the spec gives a different behavior from GetOwnProperty ! */
fn js_module_ns_has(_: ContextRef, obj: ObjectRef, prop: Atom) -> Result<bool, i32> {
    Ok(obj.has_shape_property(prop))
}

pub(crate) const js_module_ns_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    has_property: Some(js_module_ns_has),
    get_own_property: None,
    get_own_property_names: None,
    delete_property: None,
    define_own_property: None,
    get_property: None,
    set_property: None,
};

pub fn js_create_module_var(ctx: &ContextImpl, is_lexical: bool) -> Result<JSVarRef, i32> {
    JSVarRef::new(
        ctx,
        if is_lexical {
            JSValue::UNINITIALIZED
        } else {
            JSValue::UNDEFINED
        },
    )
}

/// returns None if the name cannot be found.
/// Only works with not striped bytecode functions.
pub(super) fn get_script_or_module_name(ctx: &ContextImpl, n_stack_levels: usize) -> Option<Atom> {
    // XXX: currently we just use the filename of the englobing
    // function from the debug info. May need to add a ScriptOrModule
    // info in Bytecode.
    for sf in ctx
        .rt()
        .current_stack_frame()
        .map_or(StackFrameParentIter::EMPTY, |f| f.parent_iter(true))
        .skip(n_stack_levels)
    {
        if let Some(bcf) = sf.cur_func.as_object_BCFunction() {
            let bc = bcf.payload().bytecode();
            if !bc.is_direct_or_indirect_eval {
                return bc.debug.as_ref().map(|d| d.filename);
            }
        } else {
            break;
        }
    }

    None
}

/// get module import.meta object of context current execution.
pub fn js_import_meta(ctx: &ContextImpl) -> Result<ObjectRef, i32> {
    if let Some(filename) = get_script_or_module_name(ctx, 0) {
        // XXX: inefficient, need to add a module or script pointer in Bytecode
        if let Some(mut m) = ctx.find_loaded_module(filename) {
            m.import_meta(ctx)
        } else {
            ctx.throw_type_error("import.meta not supported in this context");
            Err(-1)
        }
    } else {
        ctx.throw_type_error("import.meta not supported in this context");
        Err(-1)
    }
}
