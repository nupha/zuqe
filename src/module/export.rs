use std::{cmp::Ordering, ops::Deref};

use crate::{
    atom::{
        Atom,
        consts::{JS_ATOM__star_, JS_ATOM_Module, JS_ATOM_Symbol_toStringTag, JS_ATOM_default},
    },
    branch_hints::unlikely,
    context::JSContext,
    error::ErrorKind,
    function::{FieldDef, JSVarRef, NativeFieldDetail},
    module::{ModuleDef, ModuleDefInner},
    object::{
        JS_PROP_ENUMERABLE, JS_PROP_VARREF, JS_PROP_WRITABLE, JSObject, JSProperty, LazyInit,
        ObjectPayload,
    },
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
//     pub fn dup(&self, rt: &JSRuntime) -> Self {
//         Self {
//             export_name: self.export_name.dup(rt),
//             export_type: self.export_type.clone(),
//             cached: self.cached.as_ref().map(|x| x.dup(rt)),
//         }
//     }
// }

#[derive(Debug, Default)]
struct GetExportNamesState {
    modules: Vec<ModuleDef>,
    exported_names: Vec<ExportedNameEntry>,
}

#[derive(Debug, Clone)]
pub(crate) enum ExportType {
    JS_EXPORT_TYPE_LOCAL {
        var_idx: u16,
        /// None means var ref is not linked yet.
        var_ref: Option<JSVarRef>,
    },

    /// indirect export with req_module_idx
    JS_EXPORT_TYPE_INDIRECT(i32),
}

#[derive(Debug, Clone)]
pub struct ExportEntry {
    pub(crate) export_type: ExportType,
    /// '*' if export ns from. not used for local export after compilation
    pub local_name: Atom,
    /// exported variable name
    pub export_name: Atom,
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

impl ResolveExport {
    pub(super) fn throw_syntax_error_export(
        &self,
        ctx: &JSContext,
        m: &ModuleDefInner,
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

impl ModuleDefInner {
    /// find export entry by name
    pub fn find_export(&self, export_name: &Atom) -> Option<ExportEntry> {
        self.export_entries
            .borrow()
            .iter()
            .find(|x| x.export_name.eq(export_name))
            .cloned()
    }

    /// add an export entry on `self`.
    ///
    /// Note: `local_name`, `export_name` are not consumed.
    pub(crate) fn add_export_internal(
        &self,
        ctx: &JSContext,
        local_name: &Atom,
        export_name: &Atom,
        export_type: ExportType,
    ) -> Result<(), i32> {
        if unlikely(self.find_export(export_name).is_some()) {
            ctx.throw_syntax_error(&format!(
                "duplicate exported name '{}'",
                export_name.to_utf8(),
            ));
            Err(ErrorKind::Syntax as i32)
        } else {
            let mut lst = self.export_entries.borrow_mut();

            if lst.len() == lst.capacity() {
                lst.try_reserve(1).map_err(|_| {
                    ctx.throw_out_of_memory();
                    ErrorKind::Internal as i32
                })?;
            }

            lst.push(ExportEntry {
                export_type,
                local_name: local_name.clone(),
                export_name: export_name.clone(),
            });

            Ok(())
        }
    }

    /// declare a module export name
    pub fn add_export(&self, ctx: &JSContext, export_name: &str) -> Result<(), i32> {
        let atom = Atom::new(ctx.rt(), export_name)?;
        self.add_export_internal(
            ctx,
            &Atom::NULL,
            &atom,
            ExportType::JS_EXPORT_TYPE_LOCAL {
                var_idx: u16::MAX, // unknown
                var_ref: None,
            },
        )
    }

    /// update export value
    pub fn set_export_values(&self, ctx: &JSContext, entries: &[FieldDef]) -> Result<(), i32> {
        for entry in entries {
            let val: JSValue = match &entry.detail {
                NativeFieldDetail::Func { handler, length } => {
                    ctx.create_native_function(*handler, entry.name, *length, 0)?
                }
                NativeFieldDetail::Object(fields) => {
                    let o = ctx.create_base_object()?;
                    o.install_field_defs(ctx, fields.as_ref())?;
                    o.into()
                }
                NativeFieldDetail::Str(s) => ctx.create_string_from_str(s)?.into(),
                NativeFieldDetail::Int32(i) => JSValue::Int(*i),
                NativeFieldDetail::Float64(d) => JSValue::from(*d),
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
        ctx: &JSContext,
        export_name: &str,
        value: JSValue,
    ) -> Result<(), i32> {
        let atom = Atom::new(ctx.rt(), export_name)?;
        if let Some(me) = self.find_export(&atom) {
            if let ExportType::JS_EXPORT_TYPE_LOCAL { var_ref, .. } = me.export_type {
                debug_assert!(
                    var_ref.is_some(),
                    "exported local entry must have been linked"
                );
                var_ref.as_ref().unwrap().set_value(value);
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

impl ModuleDef {
    pub(crate) fn add_star_export_entry(
        &self,
        ctx: &JSContext,
        req_module_idx: u16,
    ) -> Result<(), i32> {
        let mut b = self.0.star_export_entries.borrow_mut();

        if b.len() == b.capacity() {
            b.try_reserve(4).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }

        b.push(req_module_idx);
        Ok(())
    }

    fn collect_exported_names(
        &self,
        ctx: &JSContext,
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
        s.modules.push(self.clone());

        for exp in self.0.export_entries.borrow().iter() {
            if from_star && exp.export_name.to_ref() == JS_ATOM_default {
                continue;
            }

            if let Some(entry) = s
                .exported_names
                .iter_mut()
                .find(|x| x.export_name.to_ref() == exp.export_name.to_ref())
            {
                entry.cached.take();
                // if let Some(en) = entry.cached.take() {
                //     en.export_name.free(ctx.rt_mut());
                //     en.local_name.free(ctx.rt_mut());
                // }
            } else {
                if s.exported_names.len() == s.exported_names.capacity() {
                    s.exported_names.try_reserve(1).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                }
                s.exported_names.push(ExportedNameEntry {
                    export_name: exp.export_name.clone(),
                    export_type: ExportedNameKind::EXPORTED_NAME_AMBIGUOUS,
                    cached: if from_star
                        || !matches!(exp.export_type, ExportType::JS_EXPORT_TYPE_LOCAL { .. })
                    {
                        None
                    } else {
                        Some((
                            exp.export_type.clone(),
                            exp.local_name.clone(),
                            exp.export_name.clone(),
                        ))
                    },
                });
            }
        }

        for idx in self.0.star_export_entries.borrow().iter() {
            self.0.dependencies.borrow()[*idx as usize]
                .module
                .as_ref()
                .unwrap()
                .collect_exported_names(ctx, s, true)?;
        }

        Ok(())
    }

    /// build module ns object
    fn build_module_ns(&self, ctx: &JSContext) -> Result<JSObject, i32> {
        let obj = ctx
            .create_object(ObjectPayload::MODULE_NS)
            .map_err(|_| -1)?;

        let mut state = GetExportNamesState::default();
        self.collect_exported_names(ctx, &mut state, false)?;

        // Resolve the exported names. The ambiguous exports are removed
        for export in state.exported_names.iter_mut() {
            let resolv =
                if let Some((export_type, local_name, export_name)) = export.cached.as_ref() {
                    // fast case: use cached, no resolution needed
                    ResolveExport::Found {
                        module: self.clone(),
                        export_type: export_type.clone(),
                        local_name: local_name.clone(),
                        export_name: export_name.clone(),
                    }
                } else {
                    self.resolve_export(ctx, &export.export_name, None)?
                };

            match resolv {
                ResolveExport::Found {
                    module,
                    export_type,
                    local_name,
                    ..
                } => match export_type {
                    ExportType::JS_EXPORT_TYPE_INDIRECT(dep_idx) => {
                        if local_name.to_ref() == JS_ATOM__star_ {
                            export.export_type = ExportedNameKind::EXPORTED_NAME_NS(
                                module.0.dependencies.borrow()[dep_idx as usize]
                                    .module
                                    .clone()
                                    .unwrap(),
                            );
                        }
                    }
                    ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } => {
                        debug_assert_ne!(local_name.to_ref(), JS_ATOM__star_);

                        export.export_type =
                            ExportedNameKind::EXPORTED_NAME_NORMAL(if let Some(vr) = var_ref {
                                vr
                            } else {
                                debug_assert_ne!(var_idx, u16::MAX);
                                let p1 = unsafe {
                                    (*module.0.bytecode.as_ptr())
                                        .as_object_BCFunction()
                                        .expect("BCFunction expected")
                                };
                                p1.payload().var_refs.borrow()[var_idx as usize].clone()
                            });
                    }
                },
                ResolveExport::Ambiguous => {
                    export.export_type = ExportedNameKind::EXPORTED_NAME_AMBIGUOUS;
                }
                ResolveExport::NotFound | ResolveExport::Circular => {
                    resolv.throw_syntax_error_export(ctx, &self.0, &export.export_name);
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
            match &en.export_type {
                ExportedNameKind::EXPORTED_NAME_NORMAL(vref) => {
                    obj.add_own_property(
                        ctx,
                        en.export_name.clone(),
                        JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_VARREF,
                        JSProperty::VarRef(vref.clone()),
                    )?;
                }
                ExportedNameKind::EXPORTED_NAME_NS(module) => {
                    // the exported namespace must be created on demand
                    obj.define_lazy_property(
                        ctx,
                        &en.export_name,
                        LazyInit::ModuleNS(module.clone()),
                        JS_PROP_ENUMERABLE | JS_PROP_WRITABLE,
                    )?;
                }
                _ => {}
            }
        }

        obj.define_property_value(
            ctx,
            JS_ATOM_Symbol_toStringTag.deref(),
            JS_ATOM_Module.force_string(ctx)?.into(),
            0,
        )?;
        obj.set_flag_extensible(false);

        Ok(obj)
    }

    /// get module ns object.
    /// if ns object is not initialized, initialize it and return.
    pub fn ns(&self, ctx: &JSContext) -> Result<JSObject, i32> {
        if let Some(ns) = unsafe { (*self.0.module_ns.as_ptr()).clone() } {
            Ok(ns)
        } else {
            let ns = self.build_module_ns(ctx)?;
            self.0.module_ns.replace(Some(ns.clone()));
            Ok(ns)
        }
    }
}
