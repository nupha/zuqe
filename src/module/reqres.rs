// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    atom::Atom,
    context::{ContextImpl, ScopedContext},
    module::ModuleDef,
};

#[derive(Debug)]
pub struct ModuleDependency {
    pub module_name: Atom,
    /// None when module is not resolved.
    pub module: Option<ModuleDef>,
}

impl GcTrace for ModuleDependency {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.module_name.trace(tr);
        if let Some(x) = self.module {
            x.trace(tr)
        }
    }
}

impl ModuleDef {
    /// add module dependency, returns the dependency index.
    /// `module_name` is not consumed.
    pub fn add_dependency(
        &mut self,
        ctx: &mut ScopedContext,
        module_name: Atom,
    ) -> Result<u16, i32> {
        if let Some(i) = self
            .dependencies
            .iter()
            .position(|req| req.module_name.eq(&module_name))
        {
            // dep is already present, returns dep index directly
            return Ok(i as u16);
        }

        if self.dependencies.len() == self.dependencies.capacity() {
            self.dependencies.try_reserve(1).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }

        self.dependencies.push(ModuleDependency {
            module_name,
            module: None,
        });

        // return the new dep index
        Ok(self.dependencies.len() as u16 - 1)
    }
}

impl ContextImpl {
    /// find the loaded module by module name.
    /// Note: `name` is not consumed.
    pub fn find_loaded_module(&self, name: Atom) -> Option<ModuleDef> {
        self.loaded_modules.iter().find_map(|m| {
            if m.module_name == name {
                Some(*m)
            } else {
                None
            }
        })
    }
}

impl<'c> ScopedContext<'c> {
    /// Load all the required modules (dependent modules) for module 'm'.
    /// TODO: migrate to ModuleDef::resolve()
    pub fn resolve_module(&mut self, mut m: ModuleDef) -> Result<(), i32> {
        if !m.resolved {
            m.resolved = true;
            let base_module_name = m.module_name;

            // resolve dependency modules
            for dep in m.dependencies.iter_mut() {
                let m1 = js_host_resolve_imported_module_atom(
                    self,
                    &base_module_name,
                    &dep.module_name,
                )?;
                dep.module = Some(m1);
                self.resolve_module(m1)?;
            }
        }

        Ok(())
    }
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

pub(super) fn js_host_resolve_imported_module(
    ctx: &mut ScopedContext,
    base_name: &str,
    name: &str,
) -> Result<ModuleDef, i32> {
    // normalize module name
    let norm_name = if let Some(cb) = ctx.rt().module_normalize_func {
        cb(ctx, base_name, name, ctx.rt_ptr().module_load_opaque)
    } else {
        js_default_module_normalize_name(ctx, base_name, name)
    }?;

    let module_name = Atom::from_str(ctx, &norm_name)?;

    // search in loaded_modules
    if let Some(m) = ctx.find_loaded_module(module_name) {
        return Ok(m);
    }

    // load module
    if let Some(cb) = ctx.rt_ptr().module_load_func {
        cb(ctx, &norm_name, ctx.rt_ptr().module_load_opaque)
    } else {
        #[cfg(debug_assertions)]
        println!("WARN: module loader function is not set, can't import module.");

        ctx.throw_reference_error(&format!("could not load module '{}'", norm_name));
        Err(-1)
    }
}

#[inline(always)]
fn js_host_resolve_imported_module_atom(
    ctx: &mut ScopedContext,
    base_module_name: &Atom,
    module_name1: &Atom,
) -> Result<ModuleDef, i32> {
    let base_cname = base_module_name.to_utf8(ctx.rt());
    let cname = module_name1.to_utf8(ctx.rt());
    js_host_resolve_imported_module(ctx, base_cname.as_ref(), &cname)
}

/// default module filename normalizer
fn js_default_module_normalize_name(
    _: &mut ScopedContext,
    base_name: &str,
    name: &str,
) -> Result<String, i32> {
    // Bare names (not starting with '.') are global, not relative to base
    if !name.starts_with('.') {
        return Ok(name.to_string());
    }

    let base_dir = std::path::Path::new(base_name)
        .parent()
        .unwrap_or(std::path::Path::new(""));

    let resolved = base_dir.join(name);

    // Canonicalize if the file exists, otherwise use absolute to resolve ./ and ../
    if let Ok(p) = resolved.canonicalize() {
        Ok(p.to_string_lossy().into_owned())
    } else if let Ok(p) = std::path::absolute(&resolved) {
        Ok(p.to_string_lossy().into_owned())
    } else {
        Ok(resolved.to_string_lossy().into_owned())
    }
}
