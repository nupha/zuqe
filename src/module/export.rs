use std::cmp::Ordering;

use crate::{
    atom::{
        JSAtom,
        consts::{JS_ATOM__star_, JS_ATOM_Module, JS_ATOM_Symbol_toStringTag, JS_ATOM_default},
    },
    branch_hints::unlikely,
    context::JSContext,
    function::JSVarRef,
    module::{ModuleDef, ModuleDefInner},
    object::{
        JS_PROP_ENUMERABLE, JS_PROP_VARREF, JS_PROP_WRITABLE, JSObject, JSProperty, LazyInit,
        ObjectPayload,
    },
};

#[derive(Clone)]
enum ExportedNameKind {
    EXPORTED_NAME_AMBIGUOUS,
    EXPORTED_NAME_NORMAL(JSVarRef),
    EXPORTED_NAME_NS(ModuleDef),
}

struct ExportedNameEntry {
    export_name: JSAtom,
    export_type: ExportedNameKind,

    /// using when the list is built
    me: Option<ExportEntry>,
}

struct GetExportNamesState {
    modules: Vec<ModuleDef>,
    exported_names: Vec<ExportedNameEntry>,
}

impl Default for GetExportNamesState {
    fn default() -> Self {
        Self {
            modules: Vec::new(),
            exported_names: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ExportType {
    JS_EXPORT_TYPE_LOCAL {
        var_idx: u16,
        var_ref: Option<JSVarRef>,
    },

    /// indirect export with req_module_idx
    JS_EXPORT_TYPE_INDIRECT(i32),
}

#[derive(Debug, Clone)]
pub struct ExportEntry {
    pub export_type: ExportType,
    /// '*' if export ns from. not used for local export after compilation
    pub local_name: JSAtom,
    /// exported variable name
    pub export_name: JSAtom,
}

struct StarExportEntry {
    /// index in dependencies
    req_module_idx: i32,
}

pub(super) enum ResolveExport {
    Found {
        module: ModuleDef,
        entry: ExportEntry,
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
        export_name: JSAtom,
    ) {
        match self {
            Self::Found { .. } => {}
            Self::NotFound => {
                ctx.throw_syntax_error(&format!(
                    "Could not find export '{}' in module '{}'",
                    export_name.to_utf8(ctx.rt()),
                    m.module_name.to_utf8(ctx.rt()),
                ));
            }
            Self::Circular => {
                ctx.throw_syntax_error(&format!(
                    "circular reference when looking for export '{}' in module '{}'",
                    export_name.to_utf8(ctx.rt()),
                    m.module_name.to_utf8(ctx.rt()),
                ));
            }
            Self::Ambiguous => {
                ctx.throw_syntax_error(&format!(
                    "export '{}' in module '{}' is ambiguous",
                    export_name.to_utf8(ctx.rt()),
                    m.module_name.to_utf8(ctx.rt()),
                ));
            }
        }
    }
}

impl ModuleDef {
    /// find export by name
    pub fn find_export(&self, export_name: JSAtom) -> Option<ExportEntry> {
        self.0
            .export_entries
            .borrow()
            .iter()
            .find(|x| x.export_name == export_name)
            .map(|v| *v.clone())
    }

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
        for m in s.modules.iter() {
            if m.id() == self.id() {
                return Ok(());
            }
        }

        s.modules.try_reserve(1).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        s.modules.push(self.clone());

        for exp in self.0.export_entries.borrow().iter() {
            if from_star && exp.export_name == JS_ATOM_default {
                continue;
            }

            if let Some(entry) = s
                .exported_names
                .iter_mut()
                .find(|m| m.export_name == exp.export_name)
            {
                entry.me.take();
            } else {
                if unlikely(s.exported_names.len() == s.exported_names.capacity()) {
                    s.exported_names.try_reserve(1).map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                }
                s.exported_names.push(ExportedNameEntry {
                    export_name: exp.export_name,
                    export_type: ExportedNameKind::EXPORTED_NAME_AMBIGUOUS,
                    me: if from_star
                        || !matches!(exp.export_type, ExportType::JS_EXPORT_TYPE_LOCAL { .. })
                    {
                        None
                    } else {
                        Some(*exp.clone())
                    },
                });
            }
        }

        for idx in self.0.star_export_entries.borrow().iter() {
            let req_module_idx = *idx;
            self.0.dependencies.borrow()[req_module_idx as usize]
                .module
                .as_ref()
                .unwrap()
                .collect_exported_names(ctx, s, true)?;
        }

        Ok(())
    }

    /// build module ns object
    fn make_ns(&self, ctx: &JSContext) -> Result<JSObject, i32> {
        let obj = ctx
            .create_object(ObjectPayload::MODULE_NS)
            .map_err(|_| -1)?;

        let mut state = GetExportNamesState::default();
        self.collect_exported_names(ctx, &mut state, false)?;

        // Resolve the exported names. The ambiguous exports are removed
        for en in state.exported_names.iter_mut() {
            let res = if let Some(me) = en.me.as_ref() {
                // fast case: no resolution needed
                ResolveExport::Found {
                    module: self.clone(),
                    entry: me.clone(),
                }
            } else {
                self.resolve_export(ctx, en.export_name, None)?
            };

            match res {
                ResolveExport::Found { module, entry } => match entry.export_type {
                    ExportType::JS_EXPORT_TYPE_INDIRECT(dep_idx) => {
                        if entry.local_name == JS_ATOM__star_ {
                            en.export_type = ExportedNameKind::EXPORTED_NAME_NS(
                                module.0.dependencies.borrow()[dep_idx as usize]
                                    .module
                                    .clone()
                                    .unwrap(),
                            );
                        }
                    }
                    ExportType::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } => {
                        debug_assert_ne!(entry.local_name, JS_ATOM__star_);
                        debug_assert_ne!(var_idx, u16::MAX);

                        en.export_type =
                            ExportedNameKind::EXPORTED_NAME_NORMAL(if let Some(vr) = var_ref {
                                vr
                            } else {
                                let p1 =
                                    unsafe { (*module.0.bytecode.as_ptr()).as_object_unchecked() };
                                p1.0.payload.borrow().func().unwrap().var_refs[var_idx as usize]
                                    .clone()
                            });
                    }
                },
                ResolveExport::Ambiguous => {
                    en.export_type = ExportedNameKind::EXPORTED_NAME_AMBIGUOUS;
                }
                ResolveExport::NotFound | ResolveExport::Circular => {
                    res.throw_syntax_error_export(ctx, &self.0, en.export_name);
                    return Err(-1);
                }
            }
        }

        // sort the exported names
        state.exported_names.sort_by(|a, b| {
            // XXX: should avoid allocation memory in atom comparison
            let str1 = a.export_name.force_string(ctx);
            let str2 = b.export_name.force_string(ctx);

            let res = if str1.is_err() || str2.is_err() {
                // XXX: raise an error ?
                Ordering::Equal
            } else {
                str1.unwrap().content().cmp(str2.unwrap().content())
            };

            res
        });

        for en in state.exported_names {
            match &en.export_type {
                ExportedNameKind::EXPORTED_NAME_NORMAL(vref) => {
                    obj.add_own_property(
                        ctx,
                        en.export_name,
                        JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_VARREF,
                        JSProperty::VarRef(vref.clone()),
                    )?;
                }
                ExportedNameKind::EXPORTED_NAME_NS(module) => {
                    // the exported namespace must be created on demand
                    obj.define_lazy_property(
                        ctx,
                        en.export_name,
                        LazyInit::ModuleNS(module.clone()),
                        JS_PROP_ENUMERABLE | JS_PROP_WRITABLE,
                    )?;
                }
                _ => {}
            }
        }

        obj.define_property_value(
            ctx,
            JS_ATOM_Symbol_toStringTag,
            JS_ATOM_Module.force_string(ctx)?.into(),
            0,
        )?;

        unsafe {
            (*obj.0.obj_flags.as_ptr()).set_extensible(false);
        }

        Ok(obj.into())
    }

    /// get module ns object.
    /// if ns object is not initialized, initialize it and return.
    pub fn ns(&self, ctx: &JSContext) -> Result<JSObject, i32> {
        if let Some(ns) = unsafe { (*self.0.module_ns.as_ptr()).clone() } {
            Ok(ns)
        } else {
            let ns = self.make_ns(ctx)?;
            self.0.module_ns.replace(Some(ns.clone()));
            Ok(ns)
        }
    }
}
