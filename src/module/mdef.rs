// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    atom::Atom,
    builtins::promise::Promise,
    class::ClassExoticMethods,
    context::{ContextImpl, ScopedContext},
    function::{
        BFuncPayload, DEFINE_GLOBAL_LEX_VAR, FieldDef, FunctionDef, JSVarKindEnum, VarRef,
        prop_str_to_atom,
    },
    gcref_wrapper,
    module::{ExportEntry, ExportType, ImportEntry, JSModuleStatus, ModuleDependency},
    object::{JS_PROP_C_W_E, JS_PROP_WRITABLE, Object, ObjectPayload},
    runtime::RuntimeImpl,
    value::JSValue,
};

use url::Url;

pub type ModuleNormalizeFn = fn(
    ctx: &mut ScopedContext,
    base_name: &Url,
    name: &str,
    opaque: *mut ::core::ffi::c_void,
) -> Result<Url, i32>;

pub type ModuleLoadFn = fn(
    ctx: &mut ScopedContext,
    module_name: &Url,
    attrs: JSValue,
    opaque: *mut ::core::ffi::c_void,
) -> Result<ModuleDef, i32>;

pub(crate) trait ModuleInitFn:
    Fn(&mut ScopedContext, &ModuleDef) -> Result<(), i32>
{
}
impl<F> ModuleInitFn for F where F: Fn(&mut ScopedContext, &ModuleDef) -> Result<(), i32> {}

pub struct ModuleDefImpl {
    pub(super) module_name: Atom,
    pub(super) dependencies: Vec<ModuleDependency>,

    pub(crate) import_entries: Vec<ImportEntry>,
    pub(crate) export_entries: Vec<ExportEntry>,
    /// list of index in dependencies
    pub(crate) star_export_entries: Vec<u16>,

    /// module namespace.
    pub(super) module_ns: Option<Object>,

    /// for js module, when func_created is:
    /// - true: bytecode field is a function object value
    /// - false: bytecode field is a bytecode value
    pub(crate) func_created: bool,

    /// for JS modules only, this value could be:
    /// * Bytecode, when func_created is false,
    /// * BCWrapper function, when func_created is true.
    /// * undefined for uninitialized.
    pub(crate) bytecode: JSValue,
    /// init callback for native module
    pub(super) init_func: Option<Box<dyn ModuleInitFn>>,

    /// true if func_obj contains await
    has_tla: bool,
    pub resolved: bool,
    pub status: JSModuleStatus,

     // temp use during js_module_link() & js_module_evaluate() 
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

    pub(super) promise: Option<Promise>,  // corresponds to spec field: capability 
    pub(super) resolving_funcs: [JSValue; 2],  // corresponds to spec field: capability 

    /// undefined means no exception
    eval_error: JSValue,
    /// the `import.meta` object of module
    pub meta_obj: Option<Object>,

    /// Import attributes (e.g. `type: "json"`)
    pub(super) attributes: JSValue,
}

impl GcTrace for ModuleDefImpl {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.module_name.trace(tr);
        self.dependencies.iter().for_each(|x| x.trace(tr));
        self.export_entries.iter().for_each(|x| x.trace(tr));
        self.import_entries.iter().for_each(|x| x.trace(tr));

        self.bytecode.trace(tr);
        self.async_parent_modules.iter().for_each(|x| x.trace(tr));

        if let Some(x) = self.promise {
            x.trace(tr)
        }
        self.resolving_funcs[0].trace(tr);
        self.resolving_funcs[1].trace(tr);

        if let Some(x) = self.stack_prev {
            x.trace(tr)
        }
        if let Some(x) = self.module_ns {
            x.trace(tr)
        }
        if let Some(x) = self.cycle_root {
            x.trace(tr)
        }
        if let Some(x) = self.meta_obj {
            x.trace(tr)
        }
        self.eval_error.trace(tr);
        self.attributes.trace(tr);
    }
}

impl ModuleDefImpl {
    fn new(module_name: Atom) -> Self {
        Self {
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
            eval_error: JSValue::UNDEFINED,

            meta_obj: None,
            attributes: JSValue::UNDEFINED,
        }
    }

    /// create a native module def with module init func.
    fn create(
        ctx: &mut ScopedContext,
        module_name: &str,
        init_fn: impl ModuleInitFn + 'static,
    ) -> Result<Self, i32> {
        let atom = prop_str_to_atom(ctx, module_name)?;
        let mut md = Self::new(atom);
        md.init_func = Some(Box::new(init_fn));
        Ok(md)
    }

    /// get module name atom.
    /// returned atom is not duplicated, caller should clone it before keep it referenced.
    #[inline(always)]
    pub const fn module_name(&self) -> Atom {
        self.module_name
    }

    /// True if this is a native (Rust-backed) module rather than a bytecode/file module.
    #[inline(always)]
    pub const fn is_native(&self) -> bool {
        // Native modules carry an `init_func`; bytecode/file modules do not.
        self.init_func.is_some()
    }

    #[inline(always)]
    pub const fn has_tla(&self) -> bool {
        self.has_tla
    }

    #[inline(always)]
    pub(crate) const fn set_tla(&mut self, b: bool) {
        self.has_tla = b;
    }

    /// get eval error. undefined means no exception.
    #[inline(always)]
    pub(crate) const fn get_eval_error(&self) -> JSValue {
        self.eval_error
    }

    /// set eval error, can't be undefined or null
    pub(crate) fn set_eval_error(&mut self, err_val: JSValue) {
        debug_assert!(!err_val.is_undefined_or_null());
        self.eval_error = err_val;
    }

    /// get module `import.meta` object.
    pub fn import_meta(&mut self, ctx: &mut ScopedContext) -> Result<Object, i32> {
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
    ///
    /// The `url` property is the module's canonical name, which is always a
    /// `file://` URL (except for native modules). It is taken directly from
    /// `module_name`, so `import.meta.url === module_name` by construction.
    pub fn set_import_meta(
        &mut self,
        ctx: &mut ScopedContext,
        is_main: bool,
    ) -> Result<(), i32> {
        let url = self.module_name().to_utf8(ctx.rt());
        let mut meta_obj = self.import_meta(ctx)?;
        let sval = ctx.create_string_from_str(&url)?;
        meta_obj.define_property_value_str(ctx, "url", sval.into(), JS_PROP_C_W_E)?;
        meta_obj.define_property_value_str(ctx, "main", is_main.into(), JS_PROP_C_W_E)?;
        Ok(())
    }

    // Make the <eval> function associated with the module, for bytecode module.
    fn create_module_bytecode_function(&mut self, ctx: &mut ScopedContext) -> Result<(), i32> {
        debug_assert!(!self.func_created);

        match self.bytecode {
            JSValue::Bytecode(bc) => {
                // create BytecodeFunction object embedding Bytecode
                let mut var_refs = Vec::<VarRef>::new();
                let gvar_count = bc.global_var_ref_count;
                let cvar_count = bc.closure_var_count;
                let total_count = gvar_count as usize + cvar_count as usize;

                if total_count != 0 {
                    var_refs.try_reserve_exact(total_count).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;

                    // First: global variable refs (initialized via realm_scope fallback)
                    for cv in bc.global_var_refs() {
                        if cv.is_global_decl() {
                            // define the global property at closure-creation time, matching declare_global_var.
                            let def_flags = if cv.is_lexical() {
                                (DEFINE_GLOBAL_LEX_VAR as u32)
                                    | if !cv.is_const() { JS_PROP_WRITABLE } else { 0 }
                            } else {
                                0 // Module code is never eval; var/func decl is non-configurable.
                            };
                            ctx.define_global_var(cv.var_name, def_flags)?;
                            if let Some((idx, _)) =
                                ctx.global_var_obj().find_shape_property(cv.var_name)
                            {
                                var_refs.push(VarRef::create_prop_value(
                                    ctx,
                                    ctx.global_var_obj(),
                                    cv.var_name,
                                    std::ptr::null(),
                                    0,
                                    idx as u16,
                                )?);
                            } else {
                                var_refs.push(VarRef::create(ctx, JSValue::UNINITIALIZED)?);
                            }
                        } else {
                            var_refs.push(VarRef::create(ctx, JSValue::UNINITIALIZED)?);
                        }
                    }

                    // Then: closure variables
                    // (module's own variables + variables imported from other modules)
                    for cv in bc.closure_vars() {
                        var_refs.push(if cv.is_local() {
                            js_create_module_var(ctx, cv.is_lexical())?
                        } else {
                            // The other variables are imported from other modules.
                            // Add slot with a placeholder.
                            VarRef::create(ctx, JSValue::UNDEFINED)?
                        });
                    }
                }

                self.bytecode = {
                    let p = ObjectPayload::BYTECODE_FUNCTION(BFuncPayload::new(bc, var_refs));
                    ctx.create_instance_with_proto_payload(Some(ctx.function_proto()), p)
                        .map_err(|_| -1)?
                        .into()
                };
            }

            _ => {
                unreachable!("module bytecode function already created");
            }
        }

        Ok(())
    }

    /// make module <eval> function from bytecode
    /// must be done before module link because of cyclic references
    pub(super) fn create_module_function(&mut self, ctx: &mut ScopedContext) -> Result<(), i32> {
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
#[derive(Clone, Copy, PartialEq)]
pub struct ModuleDef(GcRef<ModuleDefImpl>);

gcref_wrapper!(ModuleDef, ModuleDefImpl);

impl From<ModuleDef> for JSValue {
    fn from(value: ModuleDef) -> Self {
        Self::Module(value)
    }
}

impl std::fmt::Debug for ModuleDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<ModuleDef {:?}>", self.gc_ref())
    }
}

impl ModuleDef {
    /// alloc an uninitialized (usually bytecode) module.
    pub(crate) fn new(ctx: &mut ScopedContext, name: Atom) -> Result<Self, i32> {
        let md = ctx
            .alloc_local(ModuleDefImpl::new(name))
            .map(Self)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        ctx.loaded_modules.push(md);
        Ok(md)
    }

    /// Create native module def with initialization callback. Declare exported names.
    pub(crate) fn create<'a, N: Iterator<Item = &'a str>>(
        ctx: &mut ScopedContext,
        module_name: &str,
        export_names: N,
        init_fn: impl ModuleInitFn + 'static,
    ) -> Result<Self, i32> {
        let payload = ModuleDefImpl::create(ctx, module_name, init_fn)?;
        let mut md = ctx.alloc_local(payload).map(Self).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        ctx.loaded_modules.push(md);

        // declare export names
        for name in export_names {
            md.declare_export_name(ctx, name)?;
        }

        Ok(md)
    }
}

impl<'c> ScopedContext<'c> {
    /// Create native module def with initialization callback. Declare exported names to module.
    pub fn create_module(
        &mut self,
        name: &str,
        entries: &'static [FieldDef],
    ) -> Result<ModuleDef, i32> {
        ModuleDef::create(
            self,
            name,
            entries.iter().map(|e| e.name),
            |cx, m| -> Result<(), i32> { m.set_export_values(cx, entries) },
        )
    }

    #[deprecated(note = "use ::create_module() instead")]
    pub fn register_module(
        &mut self,
        name: &str,
        entries: &'static [FieldDef],
    ) -> Result<ModuleDef, i32> {
        self.create_module(name, entries)
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
pub(crate) enum JSFreeModuleEnum {
    JS_FREE_MODULE_ALL,
    JS_FREE_MODULE_NOT_RESOLVED,
}

// XXX: would be more efficient with separate module lists
pub(crate) fn js_free_modules(ctx: &mut ContextImpl, flag: JSFreeModuleEnum) {
    // To free modules that meet condition:
    // flag == JSFreeModuleEnum::JS_FREE_MODULE_ALL || (flag == JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED && m.resolved)
    ctx.loaded_modules.retain(|m| {
        !(flag == JSFreeModuleEnum::JS_FREE_MODULE_ALL
            || (flag == JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED && m.resolved))
    });
}

pub(crate) fn add_module_variables(ctx: &mut ScopedContext, fdef: &mut FunctionDef) -> i32 {
    // The imported global variables were added as closure variables
    // in js_parse_import(). We add here the module global variables.
    for i in 0..fdef.global_vars.len() {
        let (var_name, is_const, is_lexical) = {
            let v = &fdef.global_vars[i];
            (v.var_name, v.is_const(), v.is_lexical())
        };

        if fdef
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

    let mut m = fdef.module.unwrap();

    // resolve the variable names of the local exports
    for me in m.export_entries.iter_mut() {
        if let ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, .. } = &mut me.export_type {
            if let Some(idx) = fdef.find_closure_var(me.local_name) {
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

 // Unfortunately, the spec gives a different behavior from GetOwnProperty ! 
fn js_module_ns_has(_: &mut ScopedContext, obj: Object, prop: Atom) -> Result<bool, i32> {
    Ok(obj.has_shape_property(prop))
}

pub(crate) const js_module_ns_exotic_methods: ClassExoticMethods = ClassExoticMethods {
    has_property: Some(js_module_ns_has),
    get_own_property: None,
    get_own_property_names: None,
    delete_property: None,
    define_own_property: None,
    get_property: None,
    set_property: None,
    get_prototype: None,
    set_prototype: None,
};

pub(crate) fn js_create_module_var(
    ctx: &mut ScopedContext,
    is_lexical: bool,
) -> Result<VarRef, i32> {
    VarRef::create(
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
pub(super) fn get_script_or_module_name(
    ctx: &mut ContextImpl,
    n_stack_levels: usize,
) -> Option<Atom> {
    let rt = ctx.rt();

    if ctx.is_arena_mode() {
        // Arena path: walk the arena frame chain via FrameHead::prev links.
        // Skip `n_stack_levels` eval frames first, then return the first
        // non-eval frame's filename.  Start from the *current* frame (the
        // innermost one being executed, which for dynamic import is the
        // eval'd code's frame or the module-evaluation frame).
        let arena = rt.stack_frame_arena();
        let mut cur = rt.active_frame_index;
        let mut skipped = 0;

        while !cur.is_null() {
            let head = arena.frame(cur);
            if let Some(bcf) = head.func_obj.as_BCWrapper() {
                let bc = bcf.payload().bytecode();
                if bc.is_direct_or_indirect_eval {
                    cur = head.prev;
                    continue;
                }
                if skipped < n_stack_levels {
                    skipped += 1;
                    cur = head.prev;
                    continue;
                }
                return bc.debug.as_ref().map(|d| d.filename);
            }
            // Native function or non-bytecode: stop walking.
            break;
        }

        None
    } else {
        // Legacy fallback (preserved for reference):
        // if let Some(sf) = rt.current_stack_frame() {
        //     for sf in sf.parent_iter(true).skip(n_stack_levels) {
        //         if let Some(bcf) = sf.func_obj.as_BCWrapper() {
        //             let bc = bcf.payload().bytecode();
        //             if !bc.is_direct_or_indirect_eval {
        //                 return bc.debug.as_ref().map(|d| d.filename);
        //             }
        //         } else {
        //             break;
        //         }
        //     }
        // }
        unreachable!(
            "BytecodeFunction::call always uses arena mode; legacy StackFrame chain should not be reachable"
        );
    }
}

/// get module import.meta object of context current execution.
pub fn js_import_meta(ctx: &mut ScopedContext) -> Result<Object, i32> {
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
