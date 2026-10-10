// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::path::Path;

use gc_lite::{GcTrace, GcTraceCtx};
use url::Url;

use crate::{
    atom::Atom,
    context::{ContextImpl, ScopedContext},
    module::{ModuleDef, ModuleNameFilter, path_to_url, url_to_abspath},
    value::JSValue,
};

#[derive(Debug)]
pub(crate) struct ModuleDependency {
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
            self.dependencies
                .try_reserve(1)
                .map_err(|_| ctx.throw_out_of_memory())?;
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
    /// Names of the modules loaded in this context, in load order, deduplicated.
    ///
    /// This is the authoritative registry behind module resolution, so a host
    /// that needs to know "which modules already exist here" should ask here
    /// rather than keep a parallel list of its own — the two drift apart the
    /// moment a module is registered by any path the host does not own
    /// (`load_zapi_extension`, a future builtin, …).
    ///
    /// Names are returned as `String` rather than borrowed slices: an `Atom`
    /// may be a rope, which has no single contiguous `&str`, so a zero-copy
    /// view is not always available.
    ///
    /// `loaded_modules` never deduplicates, so registering the same name twice
    /// (e.g. a host asking for the same capability twice) yields one entry
    /// here — matching [`ContextImpl::find_module_def`], which resolves such a
    /// name to the first record.
    pub fn loaded_module_names(&self, filter: ModuleNameFilter) -> Vec<String> {
        let rt = self.rt();
        let mut names: Vec<String> = Vec::new();
        for m in self.loaded_modules.iter() {
            let keep = match filter {
                ModuleNameFilter::All => true,
                ModuleNameFilter::NativeOnly => m.is_native(),
                ModuleNameFilter::BytecodeOnly => !m.is_native(),
            };
            if keep {
                let name = m.module_name().to_utf8(rt);
                // `loaded_modules` is tiny (builtins plus the live graph), so a
                // linear scan beats hashing, and appending preserves load order.
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// find the loaded module_def by name.
    pub(super) fn find_module_def(&self, name: Atom) -> Option<ModuleDef> {
        self.loaded_modules.iter().find_map(|m| {
            if m.module_name == name {
                Some(*m)
            } else {
                None
            }
        })
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
    let name_atom = Atom::intern_str(ctx, name)?;
    if let Some(m) = ctx.find_module_def(name_atom)
        && m.is_native()
    {
        return Ok(m);
    }

    // normalize module name (non-native specifiers only). `base_name` is expected
    // to be a canonical `file://` (or future `http(s)://`) URL — that is what the
    // module loaders pass. A plain *script* entry, however, may still hand a bare
    // filesystem path here (its `entry_name` is the raw filename). Turn such a
    // value into a URL so `Url::join` works uniformly: a Windows path like
    // `C:\x.js` would otherwise parse under a bogus bare scheme (`c:`) and later
    // fail every `scheme() == "file"` check, causing dynamic `import()` from a
    // script to be rejected before the target module is even loaded.
    let base_url = match Url::parse(base_name) {
        Ok(u) if matches!(u.scheme(), "file" | "http" | "https") => u,
        _ => path_to_url(base_name).map_err(|e| {
            ctx.throw_reference_error(&format!(
                "could not resolve module base '{}': {}",
                base_name, e
            ))
        })?,
    };

    let norm_name = (ctx.rt().module_normalize_func)(ctx, &base_url, name)?;

    // Best-effort canonicalization of `file://` URLs so symlink aliases
    // (e.g. `link/foo.js` → `real/foo.js`) collapse to a single cache key.
    // This applies uniformly to both custom and default normalizers.
    let norm_name = if norm_name.scheme() == "file"
        && let Some(p) = url_to_abspath(&norm_name)
        && let Ok(canon) = std::fs::canonicalize(&p)
    {
        path_to_url(&canon.to_string_lossy()).unwrap_or(norm_name)
    } else {
        norm_name
    };

    // search in loaded_modules (match both name and import attributes).
    // Two modules loaded from the same URL with different import attributes
    // are distinct module records per-spec.
    let module_name = Atom::intern_str(ctx, norm_name.as_str())?;
    if let Some(m) = ctx.find_module_def(module_name)
        && attrs_are_same(ctx, m.attributes, attrs)
    {
        return Ok(m);
    }

    let mut md = (ctx.rt_ptr().module_load_func)(ctx, &norm_name, attrs)?;

    // Store import attributes on the module so future cache lookups
    // can distinguish between modules loaded from the same URL with
    // different attribute types (e.g. `type: "text"` vs no attrs).
    md.attributes = attrs;
    Ok(md)
}

/// Builtin module loader for a runtime that registered none: every import
/// fails.
///
/// This is the initial value of `RuntimeImpl::module_load_func`. Reading modules
/// from anywhere is host policy — the engine has no filesystem, no registry and
/// no `require` semantics of its own — so the honest default is a stub that
/// throws rather than one that guesses. Register a real loader with
/// [`RuntimeImpl::set_module_loader`](crate::RuntimeImpl::set_module_loader).
pub(crate) fn js_default_module_loader(
    ctx: &mut ScopedContext,
    module_name: &Url,
    _attrs: JSValue,
) -> Result<ModuleDef, i32> {
    Err(ctx.throw_reference_error(&format!("could not load module '{}'", module_name.as_str())))
}

/// Compare two import attribute objects for semantic equality, used when
/// looking up modules in the loaded-module cache. Two modules loaded from
/// the same URL but with different import attributes are distinct modules
/// per-spec; this helper distinguishes them by comparing their `type` value.
///
/// Returns `true` if both are `undefined`, or both have the same `type`
/// string value (e.g. `"text"`, `"json"`).
fn attrs_are_same(ctx: &mut ScopedContext, a: JSValue, b: JSValue) -> bool {
    match (a.is_undefined(), b.is_undefined()) {
        (true, true) => {
            return true;
        }
        (false, false) => {}
        _ => {
            return false;
        }
    }

    // Both are non-undefined objects: compare `type` property values.
    // Silently treat get/parse failures as a mismatch (conservative).
    let type_a: Option<String> = a.get_property_str(ctx, "type").ok().and_then(|v| {
        v.as_string(true)
            .map(|s| s.content().encode_utf8(false).to_string())
    });
    let type_b: Option<String> = b.get_property_str(ctx, "type").ok().and_then(|v| {
        v.as_string(true)
            .map(|s| s.content().encode_utf8(false).to_string())
    });
    type_a == type_b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Context, Runtime};

    /// Names come from the registry itself, so anything registered here is
    /// visible — a host does not have to record modules separately.
    #[test]
    fn native_names_come_from_the_registry() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let ctx: Context = rt.create_context(|cx| {
            cx.create_module("fs", &[])?;
            cx.create_module("path", &[])?;
            Ok(())
        })?;

        assert_eq!(
            ctx.loaded_module_names(ModuleNameFilter::NativeOnly),
            vec!["fs".to_string(), "path".to_string()]
        );
        // Nothing was registered under this name, so a host asking about it
        // learns "absent" rather than guessing.
        assert!(!ctx
            .loaded_module_names(ModuleNameFilter::NativeOnly)
            .iter()
            .any(|n| n == "nope"));
        Ok(())
    }

    /// `loaded_modules` does not deduplicate, but the reported list does —
    /// a name registered twice is one module as far as `find_module_def` is
    /// concerned, so it must read as one here too.
    #[test]
    fn duplicate_registration_is_reported_once() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let ctx: Context = rt.create_context(|cx| {
            cx.create_module("fs", &[])?;
            cx.create_module("fs", &[])?;
            Ok(())
        })?;

        assert_eq!(
            ctx.loaded_module_names(ModuleNameFilter::NativeOnly),
            vec!["fs".to_string()]
        );
        Ok(())
    }

    /// A native module and a loaded JS module are distinguishable, which is
    /// what lets a host separate builtins from the module graph.
    #[test]
    fn filter_separates_native_from_bytecode() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let ctx: Context = rt.create_context(|cx| {
            cx.create_module("fs", &[])?;
            // A module allocated without an init function is the bytecode kind.
            let name = Atom::intern_str(cx, "file:///a.js")?;
            ModuleDef::new(cx, name)?;
            Ok(())
        })?;

        assert_eq!(
            ctx.loaded_module_names(ModuleNameFilter::NativeOnly),
            vec!["fs".to_string()]
        );
        assert_eq!(
            ctx.loaded_module_names(ModuleNameFilter::BytecodeOnly),
            vec!["file:///a.js".to_string()]
        );
        assert_eq!(
            ctx.loaded_module_names(ModuleNameFilter::All).len(),
            2,
            "All is the union, in load order"
        );
        Ok(())
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
/// Note: `file://` canonicalization (symlink resolution) is done centrally in
/// `js_host_resolve_imported_module` so it applies to custom normalizers too.
///
/// This is the initial value of `RuntimeImpl::module_normalize_func`, so it needs
/// no registration.
///
/// `_ctx` is unused — resolution here is pure URL algebra — but the parameter is
/// what makes this a `ModuleNormalizeFn` rather than a private helper, so the
/// builtin default can live in the same field as any host override.
///
/// Crate-internal on purpose: a host picks its resolution policy once, at
/// startup, and never reads the current value back. So there is no supported way
/// to *restore* this default after overriding it — keep a host-side copy of the
/// pointer if a reset path is needed.
pub(crate) fn js_default_module_normalizer(
    _: &mut ScopedContext,
    base_name: &Url,
    name: &str,
) -> Result<Url, i32> {
    if Path::new(name).is_absolute() {
        // A bare absolute filesystem path: URL-ify it directly.
        path_to_url(name).map_err(|_| -1)
    } else {
        // Resolve relative to the importing module's URL.
        base_name.join(name).map_err(|_| -1)
    }
}
