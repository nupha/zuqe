use std::cmp::Ordering;

use gc_lite::GcTracable;

use crate::{
    atom::Atom,
    context::ContextImpl,
    error::ErrorKind,
    function::{FieldDef, FieldDetail, JSVarRef},
    module::{ModuleDef, ModuleDefImpl},
    object::{
        JS_PROP_ENUMERABLE, JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty, LazyInit, ObjectPayload,
        ObjectRef,
    },
    utils::unlikely,
    value::JSValue,
};

#[derive(Debug, Clone)]
enum ExportedNameKind {
    EXPORTED_NAME_AMBIGUOUS,
    EXPORTED_NAME_NORMAL(JSVarRef),
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

// impl ExportedNameEntry {
//     pub fn dup(&self, rt: &RuntimeImpl) -> Self {
//         Self {
//             export_name: self.export_name.dup(rt),
//             export_type: self.export_type.clone(),
//             cached: self.cached.as_ref().map(|x| x.dup(rt)),
//         }
//     }
// }

#[derive(Debug, Clone, Copy)]
pub(crate) enum ExportType {
    JS_EXPORT_TYPE_LOCAL {
        var_idx: u16,
        /// None means var ref is not linked yet.
        var_ref: Option<JSVarRef>,
    },

    /// indirect export with req_module_idx
    JS_EXPORT_TYPE_INDIRECT(i32),
}

unsafe impl GcTracable for ExportType {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        if let Self::JS_EXPORT_TYPE_LOCAL {
            var_ref: Some(vref),
            ..
        } = self
        {
            vref.trace(tracer);
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

unsafe impl GcTracable for ExportEntry {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        self.local_name.trace(tracer);
        self.export_name.trace(tracer);
        self.export_type.trace(tracer);
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

unsafe impl GcTracable for ResolveExport {
    #[inline]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        if let Self::Found {
            export_name,
            local_name,
            export_type,
            module,
        } = self
        {
            export_name.trace(tracer);
            local_name.trace(tracer);
            export_type.trace(tracer);
            module.trace(tracer);
        }
    }
}

impl ResolveExport {
    pub(super) fn throw_syntax_error_export(
        &self,
        ctx: &ContextImpl,
        m: &ModuleDefImpl,
        export_name: &Atom,
    ) {
        match self {
            Self::Found { .. } => {}
            Self::NotFound => {
                ctx.throw_syntax_error(&format!(
                    "Could not find export '{}' in module '{}'",
                    export_name.to_utf8(),
                    m.module_name.to_utf8(),
                ));
            }
            Self::Circular => {
                ctx.throw_syntax_error(&format!(
                    "circular reference when looking for export '{}' in module '{}'",
                    export_name.to_utf8(),
                    m.module_name.to_utf8(),
                ));
            }
            Self::Ambiguous => {
                ctx.throw_syntax_error(&format!(
                    "export '{}' in module '{}' is ambiguous",
                    export_name.to_utf8(),
                    m.module_name.to_utf8(),
                ));
            }
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
        ctx: &ContextImpl,
        local_name: Atom,
        export_name: Atom,
        export_type: ExportType,
    ) -> Result<(), i32> {
        if unlikely(self.find_export(export_name).is_some()) {
            ctx.throw_syntax_error(&format!(
                "duplicate exported name '{}'",
                export_name.to_utf8(),
            ));
            Err(ErrorKind::Syntax as i32)
        } else {
            if self.export_entries.len() == self.export_entries.capacity() {
                self.export_entries.try_reserve(1).map_err(|_| {
                    ctx.throw_out_of_memory();
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

    /// declare a module export name
    pub fn add_export(&mut self, ctx: &ContextImpl, export_name: &str) -> Result<(), i32> {
        let atom = Atom::new(ctx.rt(), export_name)?;
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
    pub fn set_export_values(&self, ctx: &ContextImpl, entries: &[FieldDef]) -> Result<(), i32> {
        for entry in entries {
            let val: JSValue = match &entry.detail {
                FieldDetail::Func { handler, length } => ctx
                    .create_native_function(*handler, entry.name, *length, 0)?
                    .into(),
                FieldDetail::Object(fields) => {
                    let mut o = ctx.create_base_object()?;
                    o.install_field_defs(ctx, fields)?;
                    o.into()
                }
                FieldDetail::Str(s) => ctx.create_string_from_str(s)?.into(),
                FieldDetail::Int32(i) => JSValue::Int(*i),
                FieldDetail::Float64(d) => JSValue::from(*d),
                _ => {
                    unreachable!();
                }
            };

            self.set_export_value(ctx, entry.name, val)?;
        }

        Ok(())
    }

    /// update or add a module export entry named `export_name`, with given value
    pub fn set_export_value(
        &self,
        ctx: &ContextImpl,
        export_name: &str,
        value: JSValue,
    ) -> Result<(), i32> {
        let atom = Atom::new(ctx.rt(), export_name)?;

        if let Some(me) = self.find_export(atom) {
            if let ExportType::JS_EXPORT_TYPE_LOCAL { var_ref, .. } = me.export_type {
                debug_assert!(
                    var_ref.is_some(),
                    "exported local entry must have been linked"
                );
                var_ref.unwrap().set_value(value);
                Ok(())
            } else {
                // not a local export entry
                unreachable!()
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
        ctx: &ContextImpl,
        req_module_idx: u16,
    ) -> Result<(), i32> {
        if self.star_export_entries.len() == self.star_export_entries.capacity() {
            self.star_export_entries.try_reserve(4).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }

        self.star_export_entries.push(req_module_idx);
        Ok(())
    }

    fn collect_exported_names(
        &self,
        ctx: &ContextImpl,
        s: &mut GetExportNamesState,
        from_star: bool,
    ) -> Result<(), i32> {
        // check circular reference
        if s.modules.iter().any(|m| m == self) {
            return Ok(());
        }

        s.modules.try_reserve(1).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        s.modules.push(*self);

        for exp in self.export_entries.iter() {
            #[cfg(debug_assertions)]
            {
                debug_assert!(
                    exp.export_name.is_alive(),
                    "[O.o] local_name {:?} must be alive",
                    exp.local_name
                );
                debug_assert!(
                    exp.export_name.is_alive(),
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
                    s.exported_names.try_reserve(1).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
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
    fn build_module_ns(&self, ctx: &ContextImpl) -> Result<ObjectRef, i32> {
        let mut obj = ctx
            .create_object(ObjectPayload::MODULE_NS)
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
                    ExportType::JS_EXPORT_TYPE_INDIRECT(dep_idx) => {
                        if local_name == Atom::CONST__star_ {
                            export.export_type = ExportedNameKind::EXPORTED_NAME_NS(
                                module.dependencies[dep_idx as usize].module.unwrap(),
                            );
                        }
                    }
                    ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } => {
                        debug_assert_ne!(local_name, Atom::CONST__star_);

                        export.export_type =
                            ExportedNameKind::EXPORTED_NAME_NORMAL(if let Some(vr) = var_ref {
                                vr
                            } else {
                                debug_assert_ne!(var_idx, u16::MAX);
                                let p1 = module
                                    .bytecode
                                    .as_object_BCFunction()
                                    .copied()
                                    .expect("BCFunction expected");
                                p1.payload().var_refs[var_idx as usize]
                            });
                    }
                },
                ResolveExport::Ambiguous => {
                    export.export_type = ExportedNameKind::EXPORTED_NAME_AMBIGUOUS;
                }
                ResolveExport::NotFound | ResolveExport::Circular => {
                    resolv.throw_syntax_error_export(ctx, self, &export.export_name);
                    return Err(-1);
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

        obj.define_property_value(
            ctx,
            Atom::CONST_Symbol_toStringTag,
            Atom::CONST_Module.force_string(ctx)?.into(),
            0,
        )?;
        obj.set_flag_extensible(false);

        Ok(obj)
    }

    /// get module ns object.
    /// if ns object is not initialized, initialize it and return.
    pub fn ns(&mut self, ctx: &ContextImpl) -> Result<ObjectRef, i32> {
        if let Some(ns) = self.module_ns {
            Ok(ns)
        } else {
            let ns = self.build_module_ns(ctx)?;
            self.module_ns.replace(ns);
            Ok(ns)
        }
    }
}
