// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cmp::Ordering;

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::ErrorKind,
    function::{FieldDef, FieldDetail, VarRef, prop_str_to_atom},
    module::{ModuleDef, ModuleDefImpl},
    object::{
        JS_PROP_ENUMERABLE, JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty, LazyInit, Object,
        ObjectPayload,
    },
    utils::unlikely,
    value::JSValue,
};

#[derive(Debug)]
enum ExportedNameKind {
    EXPORTED_NAME_AMBIGUOUS,
    EXPORTED_NAME_NORMAL(VarRef),
    EXPORTED_NAME_NS(ModuleDef),
}

#[derive(Debug)]
struct ExportedNameEntry {
    export_name: Atom,
    export_type: ExportedNameKind,
    /// (export_type, local_name, export_name)
    /// using when the list is built.
    cached: Option<(ExportType, Atom, Atom)>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ExportType {
    JS_EXPORT_TYPE_LOCAL {
        var_idx: u16,
        /// None means var ref is not linked yet.
        var_ref: Option<VarRef>,
    },

    /// indirect export with req_module_idx
    JS_EXPORT_TYPE_INDIRECT(i32),
}

impl GcTrace for ExportType {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Self::JS_EXPORT_TYPE_LOCAL {
            var_ref: Some(vref),
            ..
        } = self
        {
            vref.trace(tr);
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExportEntry {
    pub(crate) export_type: ExportType,
    /// '*' if export ns from. not used for local export after compilation
    pub local_name: Atom,
    /// exported variable name
    pub export_name: Atom,
}

impl GcTrace for ExportEntry {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.export_type.trace(gcx);
        self.local_name.trace(gcx);
        self.export_name.trace(gcx);
    }
}

/// resolving module export result
#[derive(Debug)]
pub(super) enum ResolveExport {
    Found {
        module: ModuleDef,
        export_type: ExportType,
        export_name: Atom,
        local_name: Atom,
    },
    NotFound,
    Ambiguous,
    Circular,
}

impl GcTrace for ResolveExport {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Self::Found {
            export_name,
            local_name,
            export_type,
            module,
        } = self
        {
            export_name.trace(tr);
            local_name.trace(tr);
            export_type.trace(tr);
            module.trace(tr);
        }
    }
}

impl ResolveExport {
    pub(super) fn throw_syntax_error_export(
        &self,
        ctx: &mut ScopedContext,
        m: &ModuleDefImpl,
        export_name: &Atom,
    ) -> i32 {
        match self {
            Self::Found { .. } => -1,
            Self::NotFound => ctx.throw_syntax_error(&format!(
                "Could not find export '{}' in module '{}'",
                export_name.to_utf8(ctx.rt()),
                m.module_name.to_utf8(ctx.rt()),
            )),
            Self::Circular => ctx.throw_syntax_error(&format!(
                "circular reference when looking for export '{}' in module '{}'",
                export_name.to_utf8(ctx.rt()),
                m.module_name.to_utf8(ctx.rt()),
            )),
            Self::Ambiguous => ctx.throw_syntax_error(&format!(
                "export '{}' in module '{}' is ambiguous",
                export_name.to_utf8(ctx.rt()),
                m.module_name.to_utf8(ctx.rt()),
            )),
        }
    }
}

impl ModuleDefImpl {
    /// find export entry by name
    pub fn find_export(&self, export_name: Atom) -> Option<&ExportEntry> {
        self.export_entries
            .iter()
            .find(|x| x.export_name == export_name)
    }

    /// add an export entry on `self`.
    ///
    /// Note: `local_name`, `export_name` are not consumed.
    pub(crate) fn add_export_internal(
        &mut self,
        ctx: &mut ScopedContext,
        local_name: Atom,
        export_name: Atom,
        export_type: ExportType,
    ) -> Result<(), i32> {
        if unlikely(self.find_export(export_name).is_some()) {
            let rt = ctx.rt_ptr();
            let _ = ctx.throw_syntax_error(&format!(
                "duplicate exported name '{}'",
                export_name.to_utf8(&rt),
            ));
            Err(ErrorKind::Syntax as i32)
        } else {
            if self.export_entries.len() == self.export_entries.capacity() {
                self.export_entries.try_reserve(1).map_err(|_| {
                    let _ = ctx.throw_out_of_memory();
                    ErrorKind::Internal as i32
                })?;
            }

            self.export_entries.push(ExportEntry {
                export_type,
                local_name,
                export_name,
            });

            Ok(())
        }
    }

    /// Declare a module export name
    pub fn declare_export_name(
        &mut self,
        ctx: &mut ScopedContext,
        export_name: &str,
    ) -> Result<(), i32> {
        let atom = prop_str_to_atom(ctx, export_name)?;
        self.add_export_internal(
            ctx,
            Atom::NULL,
            atom,
            ExportType::JS_EXPORT_TYPE_LOCAL {
                var_idx: u16::MAX, // unknown
                var_ref: None,
            },
        )
    }

    /// update export value
    pub(super) fn set_export_values(
        &self,
        ctx: &mut ScopedContext,
        entries: &[FieldDef],
    ) -> Result<(), i32> {
        for entry in entries {
            let atom = prop_str_to_atom(ctx, entry.name)?;

            let val: JSValue = match entry.detail {
                FieldDetail::LazyValueFn(f) => f(ctx)?,
                FieldDetail::LazyStr(s) => {
                    ctx.create_string_from_str(s).map(JSValue::make_string)?
                }
                FieldDetail::LazyFunc {
                    callback,
                    length,
                    magic,
                } => ctx
                    .create_native_function(callback, entry.name, length, magic)
                    .map(JSValue::from)?,
                FieldDetail::LazyObject(tab) => {
                    let mut o = ctx.create_base_object()?;
                    o.install_fields(ctx, tab)?;
                    o.into()
                }
                FieldDetail::LazyClass(f) => {
                    let class_id = f();
                    ctx.class_slots
                        .get(class_id as usize)
                        .and_then(|x| x.ctor)
                        .map(JSValue::from)
                        .ok_or_else(|| {
                            let name = ctx
                                .rt()
                                .class_registry
                                .get(class_id as usize)
                                .map(|d| d.name)
                                .unwrap_or("<unknown>");

                            ctx.throw_internal_error(&format!(
                                "can't get constructor for class id {class_id} ({name}): class is not registered, not used or has no constructor"
                            ))
                        })?
                }

                FieldDetail::Bool(b) => JSValue::from(b),
                FieldDetail::Int32(i) => JSValue::from(i),
                FieldDetail::Float64(d) => JSValue::from(d),

                FieldDetail::GetSet { .. } => {
                    #[cfg(debug_assertions)]
                    unreachable!("Export get/set for a module is not supported");
                    #[cfg(not(debug_assertions))]
                    JSValue::UNDEFINED
                }
                FieldDetail::Undefined | FieldDetail::Alias { .. } => JSValue::UNDEFINED,
            };

            if !val.is_undefined() {
                self.set_export_value(ctx, atom, val)?;
            }
        }

        Ok(())
    }

    /// update a module export entry
    fn set_export_value(
        &self,
        ctx: &mut ScopedContext,
        atom: Atom,
        value: JSValue,
    ) -> Result<(), i32> {
        if let Some(me) = self.find_export(atom) {
            if let ExportType::JS_EXPORT_TYPE_LOCAL { var_ref, .. } = me.export_type {
                debug_assert!(
                    var_ref.is_some(),
                    "exported local entry must have been linked"
                );
                var_ref.unwrap().set_value(&mut ctx.rt_ptr(), value);
                Ok(())
            } else {
                unreachable!("not a local export")
            }
        } else {
            // export_name is not declared
            Err(-1)
        }
    }
}

#[derive(Debug, Default)]
struct GetExportNamesState {
    modules: Vec<ModuleDef>,
    exported_names: Vec<ExportedNameEntry>,
}

impl ModuleDef {
    pub(crate) fn add_star_export_entry(
        &mut self,
        ctx: &mut ScopedContext,
        req_module_idx: u16,
    ) -> Result<(), i32> {
        if self.star_export_entries.len() == self.star_export_entries.capacity() {
            self.star_export_entries
                .try_reserve(4)
                .map_err(|_| ctx.throw_out_of_memory())?;
        }

        self.star_export_entries.push(req_module_idx);
        Ok(())
    }

    fn collect_exported_names(
        &self,
        ctx: &mut ScopedContext,
        s: &mut GetExportNamesState,
        from_star: bool,
    ) -> Result<(), i32> {
        // check circular reference
        if s.modules.iter().any(|m| m == self) {
            return Ok(());
        }

        s.modules
            .try_reserve(1)
            .map_err(|_| ctx.throw_out_of_memory())?;
        s.modules.push(*self);

        for exp in self.export_entries.iter() {
            #[cfg(debug_assertions)]
            {
                debug_assert!(
                    exp.export_name.is_alive(ctx.rt()),
                    "[O.o] local_name {:?} must be alive",
                    exp.local_name
                );
                debug_assert!(
                    exp.export_name.is_alive(ctx.rt()),
                    "[O.o] export_name {:?} must be alive",
                    exp.export_name
                );
            }

            if from_star && exp.export_name == Atom::CONST_default {
                continue;
            }

            if let Some(entry) = s
                .exported_names
                .iter_mut()
                .find(|x| x.export_name == exp.export_name)
            {
                entry.cached.take();
            } else {
                if s.exported_names.len() == s.exported_names.capacity() {
                    s.exported_names
                        .try_reserve(1)
                        .map_err(|_| ctx.throw_out_of_memory())?;
                }
                s.exported_names.push(ExportedNameEntry {
                    export_name: exp.export_name,
                    export_type: ExportedNameKind::EXPORTED_NAME_AMBIGUOUS,
                    cached: if from_star
                        || !matches!(exp.export_type, ExportType::JS_EXPORT_TYPE_LOCAL { .. })
                    {
                        None
                    } else {
                        Some((exp.export_type, exp.local_name, exp.export_name))
                    },
                });
            }
        }

        for idx in self.star_export_entries.iter() {
            self.dependencies[*idx as usize]
                .module
                .unwrap()
                .collect_exported_names(ctx, s, true)?;
        }

        Ok(())
    }

    /// build module ns object
    fn build_module_ns(&self, ctx: &mut ScopedContext) -> Result<Object, i32> {
        let mut obj = ctx
            .create_instance_with_payload(ObjectPayload::MODULE_NS)
            .map_err(|_| -1)?;

        let mut state = GetExportNamesState::default();
        self.collect_exported_names(ctx, &mut state, false)?;

        // Resolve the exported names. The ambiguous exports are removed
        for export in state.exported_names.iter_mut() {
            let resolv = if let Some((export_type, local_name, export_name)) = export.cached {
                // fast case: use cached, no resolution needed
                ResolveExport::Found {
                    module: *self,
                    export_type,
                    local_name,
                    export_name,
                }
            } else {
                self.resolve_export(ctx, export.export_name, None)?
            };

            match resolv {
                ResolveExport::Found {
                    module,
                    export_type,
                    local_name,
                    ..
                } => match export_type {
                    ExportType::JS_EXPORT_TYPE_INDIRECT(_) => {
                        if local_name == Atom::CONST__star_ {
                            // After the resolve fix, `module` is already the
                            // imported (target) module, not `self`. Use it
                            // directly instead of module.dependencies[dep_idx].
                            export.export_type = ExportedNameKind::EXPORTED_NAME_NS(module);
                        }
                    }
                    ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } => {
                        debug_assert_ne!(local_name, Atom::CONST__star_);
                        export.export_type =
                            ExportedNameKind::EXPORTED_NAME_NORMAL(if let Some(vr) = var_ref {
                                vr
                            } else {
                                debug_assert_ne!(var_idx, u16::MAX);
                                module
                                    .bytecode
                                    .as_object_BCWrapper()
                                    .unwrap()
                                    .payload()
                                    .get_closure(var_idx as usize)
                            });
                    }
                },
                ResolveExport::Ambiguous => {
                    export.export_type = ExportedNameKind::EXPORTED_NAME_AMBIGUOUS;
                }
                ResolveExport::NotFound | ResolveExport::Circular => {
                    return Err(resolv.throw_syntax_error_export(ctx, self, &export.export_name));
                }
            }
        }

        // sort the exported names
        state.exported_names.sort_by(|a, b| {
            // XXX: should avoid allocation memory in atom comparison
            let str1 = a.export_name.force_string(ctx);
            let str2 = b.export_name.force_string(ctx);

            match (str1, str2) {
                (Ok(str1), Ok(str2)) => str1.content().cmp(str2.content()),
                _ => {
                    // XXX: raise an error ?
                    Ordering::Equal
                }
            }
        });

        for en in state.exported_names {
            match en.export_type {
                ExportedNameKind::EXPORTED_NAME_NORMAL(vref) => {
                    obj.add_own_property(
                        ctx,
                        en.export_name,
                        JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_VARREF,
                        JSProperty::VarRef(vref),
                    )?;
                }
                ExportedNameKind::EXPORTED_NAME_NS(module) => {
                    // the exported namespace must be created on demand
                    obj.define_lazy_property(
                        ctx,
                        en.export_name,
                        LazyInit::ModuleNS(module),
                        JS_PROP_ENUMERABLE | JS_PROP_WRITABLE,
                    )?;
                }
                _ => {}
            }
        }

        let mod_str = Atom::CONST_Module.force_string(ctx)?;
        obj.define_property_value(
            ctx,
            Atom::CONST_Symbol_toStringTag,
            JSValue::make_string(mod_str),
            0,
        )?;

        obj.set_flag_extensible(false);

        Ok(obj)
    }

    /// get module ns object.
    /// if ns object is not initialized, initialize it and return.
    pub(crate) fn ns_object(&mut self, ctx: &mut ScopedContext) -> Result<Object, i32> {
        if let Some(ns) = self.module_ns {
            Ok(ns)
        } else {
            let ns = self.build_module_ns(ctx)?;
            self.module_ns.replace(ns);
            Ok(ns)
        }
    }
}
