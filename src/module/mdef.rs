// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::marker::PhantomData;

use gc_lite::{GcRef, GcTrace, GcTraceCtx};
use url::Url;

use crate::{
    JS_CLASS_BYTECODE_FUNCTION,
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
    object::{JS_PROP_C_W_E, JS_PROP_WRITABLE, Object, PayloadPtr},
    runtime::RuntimeImpl,
    value::JSValue,
};

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
    pub(crate) module_name: Atom,
    pub(crate) dependencies: Vec<ModuleDependency>,

    pub(crate) import_entries: Vec<ImportEntry>,
    pub(crate) export_entries: Vec<ExportEntry>,
    /// list of index in dependencies
    pub(crate) star_export_entries: Vec<u16>,

    /// module namespace.
    pub(crate) module_ns: Option<Object>,

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

    pub(super) promise: Option<Promise>, // corresponds to spec field: capability
    pub(super) resolving_funcs: [JSValue; 2], // corresponds to spec field: capability

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
                let _ = ctx.throw_type_error("import.meta not supported in this context");
            })?;
            self.meta_obj.replace(o);
            Ok(o)
        }
    }

    /// set module import.meta props.
    ///
    /// The `url` property is the module's canonical name, which is always an
    /// URL (except for native modules). It is taken directly from
    /// `module_name`, so `import.meta.url === module_name` by construction.
    pub(crate) fn set_import_meta(
        &mut self,
        ctx: &mut ScopedContext,
        is_main: bool,
    ) -> Result<(), i32> {
        let name = self.module_name().to_utf8(ctx.rt());

        let mut meta_obj = self.import_meta(ctx)?;
        let sval = ctx.create_string_from_str(&name)?;
        meta_obj.define_property_value_str(
            ctx,
            "url",
            JSValue::make_string(sval),
            JS_PROP_C_W_E,
        )?;
        meta_obj.define_property_value_str(ctx, "main", is_main.into(), JS_PROP_C_W_E)?;

        Ok(())
    }

    // Make the <eval> function associated with the module, for bytecode module.
    fn create_module_bytecode_function(&mut self, ctx: &mut ScopedContext) -> Result<(), i32> {
        debug_assert!(!self.func_created);

        let Some(bc) = self.bytecode.as_bytecode() else {
            unreachable!("module bytecode function already created");
        };

        // Global variable declarations: define the global property at
        // closure-creation time (declaration side effect only). No VarRefs
        // are created for globals — GET_VAR/PUT_VAR resolve by name at
        // runtime, like QuickJS OP_get_var/OP_put_var.
        for cv in bc.global_var_refs() {
            if cv.is_global_decl() {
                let def_flags = if cv.is_lexical() {
                    (DEFINE_GLOBAL_LEX_VAR as u32)
                        | if !cv.is_const() { JS_PROP_WRITABLE } else { 0 }
                } else {
                    0 // Module code is never eval; var/func decl is non-configurable.
                };
                ctx.define_global_var(cv.var_name, def_flags)?;
            }
        }

        // create BytecodeFunction object embedding Bytecode
        let cvar_count = bc.closure_var_count as usize;
        let mut var_refs = Vec::<VarRef>::with_capacity(cvar_count);

        if cvar_count != 0 {
            // Closure variables
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
            let p = BFuncPayload::new(bc, var_refs);
            ctx.create_instance_using_proto(
                Some(ctx.function_proto()),
                PayloadPtr::new(JS_CLASS_BYTECODE_FUNCTION, p),
            )?
            .into()
        };

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
    #[inline(always)]
    fn from(value: ModuleDef) -> Self {
        Self(
            crate::JSValueTag::Module.mknode(value.gc_node_ptr()),
            PhantomData,
        )
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
            .map_err(|_| ctx.throw_out_of_memory())?;

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
        let mut md = ctx
            .alloc_local(payload)
            .map(Self)
            .map_err(|_| ctx.throw_out_of_memory())?;

        ctx.loaded_modules.push(md);

        // declare export names
        for name in export_names {
            md.declare_export_name(ctx, name)?;
        }

        Ok(md)
    }

    /// Load all dependency modules for module `self`.
    pub(crate) fn resolve_deps(&mut self, ctx: &mut ScopedContext) -> Result<(), i32> {
        if self.resolved {
            // Per ECMAScript spec §16.2.1.7: if module is in "errored" state,
            // re-throw the cached error on subsequent import attempts.
            if !self.eval_error.is_undefined() {
                return Err(ctx.throw_value(self.eval_error));
            }
            return Ok(());
        }

        self.resolved = true;
        let base_utf8 = self.module_name.to_utf8(ctx.rt());

        // resolve dependency modules
        let result = (|| -> Result<(), i32> {
            for dep in self.dependencies.iter_mut() {
                let dep_utf8 = dep.module_name.to_utf8(ctx.rt());
                let mut m1 = super::js_host_resolve_imported_module(
                    ctx,
                    &base_utf8,
                    &dep_utf8,
                    dep.attributes,
                )?;

                dep.module = Some(m1);
                m1.resolve_deps(ctx)?;
            }
            Ok(())
        })();

        if result.is_err() {
            // Per ECMAScript spec §16.2.1.7: cache the error so subsequent
            // imports throw the same error without retrying.
            let e = ctx.rt().exception(); // peek only
            debug_assert_ne!(e, crate::Exception::No);
            let err_val = e.to_value(ctx);
            self.set_eval_error(err_val);
            return Err(-1); // current exception is still there
        }

        result
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

    /// Name of the script or module currently executing on the call stack.
    ///
    /// Returns the filename/URL of the innermost bytecode frame (the one
    /// actually running), skipping JS `eval()` frames but honoring
    /// `evalModule`/`evalScript` frames. For module mode this is the value
    /// that backs `import.meta.url`, so relative `import`/`fetch`/`fs`
    /// specifiers resolve against it (Node/Bun semantics).
    ///
    /// Returns `None` when the caller is not inside any script/module frame
    /// — i.e. a pure native (host) call with no active bytecode on the stack,
    /// or the very top level driven directly from Rust with no script name.
    ///
    /// Non-module cases:
    /// - inside `Aike.evalModule(code, baseUrl)` → `Some(baseUrl)`
    /// - inside `Aike.evalScript(code, {filename})` → `Some(filename)` (default `"<evalScript>"`)
    /// - inside a JS `eval()` call → the enclosing module/script name, or `None` if none
    /// - pure native / host top level → `None`
    pub fn current_module_name(&mut self) -> Option<String> {
        // Prefer the module pointer carried by the executing bytecode (works
        // inside async callbacks). Fall back to the active-frame filename for
        // plain script / eval code that has no module — including `evalScript`
        // code, whose `debug.filename` is the injected `baseUrl` and must NOT be
        // skipped as an eval wrapper here (that is only for stack-trace naming).
        current_module_def(self)
            .map(|m| m.module_name().to_utf8(self.rt()).to_string())
            .or_else(|| current_bytecode_filename(self).map(|a| a.to_utf8(self.rt()).to_string()))
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
                return ctx.throw_syntax_error(&format!(
                    "exported variable '{}' does not exist",
                    me.local_name.to_utf8(ctx.rt())
                ));
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

/// Resolve the module that owns the *currently executing* user code.
///
/// Each module's bytecode carries its module URL as `debug.filename`, so we
/// first look up the module from the active frame's filename — this is what
/// gives each module its own distinct `import.meta` object even for a nested/
/// fixture module. If no bytecode frame carries a filename (e.g. an async
/// callback run as an independent root frame, or a pure native call), fall back
/// to `eval_base_url`, the URL the host bound at the `eval` entry point (set
/// in `eval.rs`), which survives across the whole host eval.
///
/// Returns `None` when neither a bytecode frame nor `eval_base_url` resolves to
/// a loaded module.
fn current_module_def(ctx: &ScopedContext) -> Option<ModuleDef> {
    // Module URL carried by the executing bytecode frame.
    if let Some(filename) = current_bytecode_filename(ctx)
        && let Some(m) = ctx.find_module_def(filename)
    {
        return Some(m);
    }

    // Base URL bound at the host `eval` entry (fallback for async/native frames).
    let eval_base_url = ctx.rt().eval_base_url;
    if !eval_base_url.is_null()
        && let Some(m) = ctx.find_module_def(eval_base_url)
    {
        return Some(m);
    }

    None
}

/// Walks the active frame chain, skipping native frames, and returns the
/// `debug.filename` of the *first* bytecode frame (including eval/script frames,
/// which carry the `baseUrl` injected by `evalScript`/`evalModule`).
///
/// Unlike `get_script_or_module_name`, this does NOT skip eval frames: the
/// eval'd user code's own `debug.filename` *is* the base URL we want for
/// relative `fs`/`fetch` resolution, even when called from inside a native
/// function (e.g. `fetch`). Returns `None` only when there is no bytecode frame
/// at all (pure native / host top level).
pub(crate) fn current_bytecode_filename(ctx: &ScopedContext) -> Option<Atom> {
    let rt = ctx.rt();
    let arena = rt.stack_frame_arena();

    let mut cur = rt.active_frame_index;
    while !cur.is_null() {
        let sf = arena.frame(cur);
        if let Some(bcf) = sf.func_obj.as_BCWrapper()
            && let Some(d) = bcf.payload().bytecode().debug.as_ref()
            && !d.filename.is_null()
        {
            return Some(d.filename);
        }

        cur = sf.prev;
    }

    None
}

/// Returns the filename/URL of the script or module containing the current
/// execution frame.
///
/// Walks the active frame stack starting from the innermost frame, skipping
/// `n_stack_levels` frames (used to ignore the caller's own frame) and any
/// native frames, and returns the first non-eval bytecode frame's
/// `debug.filename`. Native frames are skipped (not stopping the walk) so that
/// callers invoked *from* a native function — e.g. `fetch` resolving a relative
/// URL via `current_module_name` — still see the user script/module base URL
/// carried by the bytecode frame below.
///
/// Behavior in non-module contexts:
/// - Inside `Aike.evalModule(code, baseUrl)` → returns `baseUrl` (the module
///   name), since the module's top-level frame carries it.
/// - Inside `Aike.evalScript(code, {filename})` → returns `filename` (default
///   `"<evalScript>"`); the script frame's `debug.filename` is the supplied
///   name.
/// - Inside a JS `eval()` call → the eval frame is skipped and the enclosing
///   frame's name is returned (or `None` if there is no enclosing script/module).
/// - Pure native (host) call, or top level driven directly from Rust with no
///   script name → returns `None` (no bytecode frame is active).
///
/// Returns `None` if no suitable frame is found. Only works with non-stripped
/// bytecode functions (stripped bytecode has no `debug.filename`).
///
/// This is the fallback used by `ScopedContext::current_module_name` for
/// script/eval code; module code is resolved directly via `current_module_def`
/// (which survives async callbacks, unlike this frame-walking path).
pub(crate) fn get_script_or_module_name(ctx: &ContextImpl, n_stack_levels: usize) -> Option<Atom> {
    debug_assert!(
        ctx.is_arena_mode(),
        "BytecodeFunction::call() always run with arena stack frame bump"
    );

    let rt = ctx.rt();

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
        // Native function or non-bytecode: skip it. `current_module_name` is
        // also called from inside native functions (e.g. `fetch`), whose active
        // frame is native; the user code that owns the base URL sits below.
        cur = head.prev;
        continue;
    }

    None
}

/// get module `import.meta` object of context current execution.
pub(crate) fn js_import_meta(ctx: &mut ScopedContext) -> Result<Object, i32> {
    // Prefer the module pointer carried by the executing bytecode (set at
    // compile time), so `import.meta` works even inside async callbacks.
    // Fall back to frame-stack lookup for script / eval code without a module.
    if let Some(mut m) = current_module_def(ctx) {
        return m.import_meta(ctx);
    }

    if let Some(filename) = get_script_or_module_name(ctx, 0)
        && let Some(mut m) = ctx.find_module_def(filename)
    {
        return m.import_meta(ctx);
    }

    Err(ctx.throw_type_error("import.meta not supported in this context"))
}
