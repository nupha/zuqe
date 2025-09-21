use crate::{
    atom::{JS_FreeAtom, JSAtom},
    context::JSContext,
    module::{ModuleDef, js_default_module_normalize_name},
};

pub struct ModuleDependency {
    pub module_name: JSAtom,
    /// None if module is not resolved.
    pub module: Option<ModuleDef>,
}

pub struct JSResolveEntry {
    pub module: ModuleDef,
    pub name: JSAtom,
}

impl ModuleDef {
    /// add module dependency, returns the dependency index.
    /// `module_name` is not consumed.
    pub fn add_dependency(&self, ctx: &JSContext, module_name: JSAtom) -> Result<u16, i32> {
        if let Some(i) = self
            .0
            .dependencies
            .borrow()
            .iter()
            .position(|req| req.module_name == module_name)
        {
            // dep is already present, returns dep index directly
            return Ok(i as u16);
        }

        if self.0.dependencies.borrow().len() == self.0.dependencies.borrow().capacity() {
            if self.0.dependencies.borrow_mut().try_reserve(4).is_err() {
                ctx.throw_out_of_memory();
                return Err(-1);
            }
        }

        self.0.dependencies.borrow_mut().push(ModuleDependency {
            module_name: module_name.dup(&ctx.rt()),
            module: None,
        });

        // return the new dep index
        Ok(self.0.dependencies.borrow().len() as u16 - 1)
    }
}

impl JSContext {
    /// find the loaded module by module name.
    /// Note: `name` is not consumed.
    pub fn find_loaded_module(&self, name: JSAtom) -> Option<ModuleDef> {
        self.0.borrow_mut().loaded_modules.iter().find_map(|w| {
            if let Some(m) = w.upgrade() {
                if m.0.module_name == name {
                    Some(m)
                } else {
                    None
                }
            } else {
                None
            }
        })
    }

    /// Load all the required modules (dependent modules) for module 'm'.
    /// TODO: migrate to ModuleDef::resolve()
    pub fn resolve_module(&self, m: &ModuleDef) -> Result<(), i32> {
        if m.0.resolved.get() {
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

        m.0.resolved.set(true);

        // resolve dep modules
        let module_name = m.0.module_name;
        for dep in m.0.dependencies.borrow_mut().iter_mut() {
            let m1 = js_host_resolve_imported_module_atom(self, module_name, dep.module_name)?;
            dep.module = Some(m1.clone());

            // already done in js_host_resolve_imported_module() except if
            // the module was loaded with JS_EvalBinary()
            self.resolve_module(&m1)?;
        }

        Ok(())
    }
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
    let module_name = JSAtom::new(ctx.rt_mut(), &name)?;

    // look at the loaded modules
    if let Some(m) = ctx.find_loaded_module(module_name) {
        JS_FreeAtom(ctx, module_name);
        return Ok(m); // already loaded
    }

    JS_FreeAtom(ctx, module_name);

    // load the module
    if let Some(cb) = ctx.rt().module_loader_func {
        cb(ctx, &name, ctx.rt().module_loader_opaque)
    } else {
        #[cfg(debug_assertions)]
        println!("module loader function is not set, can't import module.");

        ctx.throw_reference_error(&format!("could not load module '{}'", name));
        Err(-1)
    }
}

pub fn js_host_resolve_imported_module_atom(
    ctx: &JSContext,
    base_module_name: JSAtom,
    module_name1: JSAtom,
) -> Result<ModuleDef, i32> {
    let base_cname = base_module_name.to_utf8(ctx.rt());
    let cname = module_name1.to_utf8(ctx.rt());
    js_host_resolve_imported_module(ctx, base_cname.as_ref(), &cname)
}
