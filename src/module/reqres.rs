// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};
use url::Url;

use std::path::Path;

use crate::{
    atom::Atom,
    context::{ContextImpl, ScopedContext},
    module::{ModuleDef, path_to_url, url_to_abspath},
    value::JSValue,
};

#[derive(Debug)]
pub struct ModuleDependency {
    pub module_name: Atom,
    /// None when module is not resolved.
    pub module: Option<ModuleDef>,
    /// Import attributes from `with { ... }` clause, or JSValue::UNDEFINED if absent.
    /// A JS object mapping string keys to string values (e.g. { type: "json" }).
    pub attributes: JSValue,
}

impl GcTrace for ModuleDependency {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.module_name.trace(tr);
        if let Some(x) = self.module {
            x.trace(tr)
        }
        self.attributes.trace(tr);
    }
}

impl ModuleDef {
    /// add module dependency, returns the dependency index.
    pub fn add_dependency(
        &mut self,
        ctx: &mut ScopedContext,
        module_name: Atom,
        attributes: JSValue,
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
            attributes,
        });

        // return the new dep index
        Ok(self.dependencies.len() as u16 - 1)
    }
}

impl ContextImpl {
    /// find the loaded module by module name.
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
    pub fn resolve_module(&mut self, mut md: ModuleDef) -> Result<(), i32> {
        if !md.resolved {
            md.resolved = true;
            let base_utf8 = md.module_name.to_utf8(self.rt());

            // resolve dependency modules
            for dep in md.dependencies.iter_mut() {
                let dep_utf8 = dep.module_name.to_utf8(self.rt());
                let m1 =
                    js_host_resolve_imported_module(self, &base_utf8, &dep_utf8, dep.attributes)?;

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
    attrs: JSValue,
) -> Result<ModuleDef, i32> {
    // Native modules are registered in `loaded_modules` under their verbatim
    // name. Check this *before* normalization: normalizing would otherwise mangle
    // a bare native name like "os" into an absolute path (e.g. "C:/cwd/os") and the
    // registry lookup below would miss it. Running before any host normalizer also
    // makes native imports detected uniformly for both the default and custom
    // normalizers. After this early return, only non-native specifiers reach the
    // normalize + load path below.
    let name_atom = Atom::from_str(ctx, name)?;
    if let Some(m) = ctx.find_loaded_module(name_atom) {
        if m.is_native() {
            return Ok(m);
        }
    }

    // normalize module name (non-native specifiers only)
    let norm_name = if let Some(cb) = ctx.rt().module_normalize_func {
        cb(ctx, base_name, name, ctx.rt_ptr().module_load_opaque)
    } else {
        js_default_module_normalize_name(base_name, name)
    }?;

    // search in loaded_modules
    let module_name = Atom::from_str(ctx, &norm_name)?;
    if let Some(m) = ctx.find_loaded_module(module_name) {
        return Ok(m);
    }

    // load module
    if let Some(cb) = ctx.rt_ptr().module_load_func {
        cb(ctx, &norm_name, attrs, ctx.rt_ptr().module_load_opaque)
    } else {
        #[cfg(debug_assertions)]
        println!("WARN: module loader function is not set, can't import module.");

        ctx.throw_reference_error(&format!("could not load module '{}'", norm_name));
        Err(-1)
    }
}

/// default module filename normalizer
///
/// Called only for non-native specifiers: native modules are resolved earlier in
/// `js_host_resolve_imported_module`, before normalization runs. By the time we
/// reach here, `base_name` is always a canonical `file://` URL (the importing
/// module's name). The import specifier itself may be:
///
/// * a relative reference (`./foo`, `../bar`) — resolved via `Url::join`, which
///   handles `.`/`..` and works uniformly for `file:` and `http:` bases;
/// * an absolute filesystem path (`C:/x.js`, `/usr/x.js`) — converted to a URL
///   directly;
/// * a full URL (`file://...`, future `https://...`) — passed through.
///
/// The resulting path is canonicalized (symlinks / `.` / `..` resolved) so two
/// different textual paths to the same file collapse to a single `module_name`
/// cache key.
fn js_default_module_normalize_name(base_name: &str, name: &str) -> Result<String, i32> {
    let url = if Path::new(name).is_absolute() {
        // A bare absolute filesystem path: URL-ify it directly.
        path_to_url(name)
    } else if let Ok(base) = Url::parse(base_name) {
        // Resolve relative to the importing module's URL.
        let joined = base.join(name).map_err(|_| -1)?;
        let path = url_to_abspath(joined.as_str())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| joined.as_str().to_string());
        path_to_url(&path)
    } else {
        // `base_name` is not a URL (should not happen for non-native modules);
        // fall back to treating `name` as a path.
        path_to_url(name)
    };

    Ok(url)
}
