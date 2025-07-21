use crate::module::{JSResolveEntry, ModuleDefInner};
use crate::{
    atom::{
        JS_FreeAtom, JSAtom,
        consts::{JS_ATOM__star_, JS_ATOM_default},
    },
    context::JSContext,
    error::JS_ThrowSyntaxError,
    module::{
        JSExportEntry, JSExportTypeEnum, JSResolveState, ModuleDef, add_resolve_entry,
        find_export_entry,
    },
};

pub enum ModuleResolveExport {
    Found {
        module: ModuleDef,
        entry: JSExportEntry,
    },
    NotFound,
    Ambiguous,
    Circular,
    Error,
}

impl ModuleResolveExport {
    pub(super) fn throw_error_inner(
        &self,
        ctx: &JSContext,
        m: &ModuleDefInner,
        export_name: JSAtom,
    ) {
        match self {
            Self::Found { .. } => {}
            Self::Error => {}
            Self::NotFound => {
                JS_ThrowSyntaxError(
                    ctx,
                    &format!(
                        "Could not find export '{}' in module '{}'",
                        export_name.to_str(ctx.rt()),
                        m.module_name.to_str(ctx.rt()),
                    ),
                );
            }
            Self::Circular => {
                JS_ThrowSyntaxError(
                    ctx,
                    &format!(
                        "circular reference when looking for export '{}' in module '{}'",
                        export_name.to_str(ctx.rt()),
                        m.module_name.to_str(ctx.rt()),
                    ),
                );
            }
            Self::Ambiguous => {
                JS_ThrowSyntaxError(
                    ctx,
                    &format!(
                        "export '{}' in module '{}' is ambiguous",
                        export_name.to_str(ctx.rt()),
                        m.module_name.to_str(ctx.rt()),
                    ),
                );
            }
        }
    }

    /// put exception to current_exception according to status
    pub fn throw_error(&self, ctx: &JSContext, m: &ModuleDef, export_name: JSAtom) {
        self.throw_error_inner(ctx, &m.0, export_name);
    }
}

impl ModuleDef {
    unsafe fn resolve_export1(
        &self,
        ctx: &JSContext,
        export_name: JSAtom,
        expect: Option<(&ModuleDef, JSAtom)>, // (module, local_name)
        lst: &mut Vec<JSResolveEntry>,
    ) -> ModuleResolveExport {
        if lst
            .iter()
            .find(|re| re.module.id() == self.id() && re.name == export_name)
            .is_some()
        {
            return ModuleResolveExport::Circular;
        }

        if add_resolve_entry(ctx, lst, self, export_name).is_err() {
            return ModuleResolveExport::Error;
        }

        if let Some(me) = find_export_entry(self, export_name) {
            match me.export_type {
                JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { .. } => {
                    return ModuleResolveExport::Found {
                        module: self.clone(),
                        entry: me.clone(),
                    };
                }
                JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                    return if me.local_name == JS_ATOM__star_ {
                        // export ns from
                        ModuleResolveExport::Found {
                            module: self.clone(),
                            entry: me.clone(),
                        }
                    } else {
                        let m1 = self.0.req_module_entries.borrow()[req_module_idx as usize]
                            .module
                            .clone()
                            .unwrap();

                        m1.resolve_export1(ctx, me.local_name, expect, lst)
                    };
                }
            }
        } else {
            if export_name != JS_ATOM_default {
                // not found in direct or indirect exports: try star exports
                for i in 0..self.0.star_export_entries_count {
                    let idx = (*self.0.star_export_entries.offset(i as isize)).req_module_idx;

                    let m1 = self.0.req_module_entries.borrow()[idx as usize]
                        .module
                        .clone()
                        .unwrap();

                    match m1.resolve_export1(ctx, export_name, expect, lst) {
                        ModuleResolveExport::Found { module, entry } => {
                            if let Some((test_module, test_local_name)) = expect {
                                if test_module.id() != module.id()
                                    || test_local_name != entry.local_name
                                {
                                    return ModuleResolveExport::Ambiguous;
                                }
                            }
                            return ModuleResolveExport::Found { module, entry };
                        }
                        ModuleResolveExport::NotFound | ModuleResolveExport::Circular => {
                            return ModuleResolveExport::NotFound;
                        }
                        ModuleResolveExport::Ambiguous => {
                            return ModuleResolveExport::Ambiguous;
                        }
                        ModuleResolveExport::Error => {
                            return ModuleResolveExport::Error;
                        }
                    }
                }
            }

            return ModuleResolveExport::NotFound;
        }
    }

    pub fn resolve_export(
        &self,
        ctx: &JSContext,
        export_name: JSAtom,
        expect: Option<(&ModuleDef, JSAtom)>, // expected (module, local_name)
    ) -> ModuleResolveExport {
        let mut lst = Vec::<JSResolveEntry>::with_capacity(4);
        let ret = unsafe { self.resolve_export1(ctx, export_name, expect, &mut lst) };

        for ent in lst.drain(..) {
            JS_FreeAtom(ctx, ent.name);
        }

        ret
    }
}
