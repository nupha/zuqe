use crate::{
    atom::{
        JS_FreeAtom, JSAtom,
        consts::{JS_ATOM__star_, JS_ATOM_default},
    },
    context::JSContext,
    error::JS_ThrowSyntaxError,
    module::{JSExportEntry, JSExportTypeEnum, JSResolveEntry, ModuleDef, ModuleDefInner},
};

pub enum ModuleResolveExport {
    Found {
        module: ModuleDef,
        entry: JSExportEntry,
    },
    NotFound,
    Ambiguous,
    Circular,
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
            Self::NotFound => {
                JS_ThrowSyntaxError(
                    ctx,
                    &format!(
                        "Could not find export '{}' in module '{}'",
                        export_name.to_utf8(ctx.rt()),
                        m.module_name.to_utf8(ctx.rt()),
                    ),
                );
            }
            Self::Circular => {
                JS_ThrowSyntaxError(
                    ctx,
                    &format!(
                        "circular reference when looking for export '{}' in module '{}'",
                        export_name.to_utf8(ctx.rt()),
                        m.module_name.to_utf8(ctx.rt()),
                    ),
                );
            }
            Self::Ambiguous => {
                JS_ThrowSyntaxError(
                    ctx,
                    &format!(
                        "export '{}' in module '{}' is ambiguous",
                        export_name.to_utf8(ctx.rt()),
                        m.module_name.to_utf8(ctx.rt()),
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
    unsafe fn resolve_export_inner(
        &self,
        ctx: &JSContext,
        export_name: JSAtom,
        expect: Option<(&ModuleDef, JSAtom)>, // (module, local_name)
        resolved: &mut Vec<JSResolveEntry>,
    ) -> Result<ModuleResolveExport, i32> {
        if resolved
            .iter()
            .find(|re| re.module.id() == self.id() && re.name == export_name)
            .is_some()
        {
            return Ok(ModuleResolveExport::Circular);
        }

        // add resolve
        if resolved.len() == resolved.capacity() {
            resolved.try_reserve(4).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
        resolved.push(JSResolveEntry {
            module: self.clone(),
            name: export_name.dup(&ctx.rt()),
        });

        if let Some(me) = self.find_export(export_name) {
            match me.export_type {
                JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { .. } => Ok(ModuleResolveExport::Found {
                    module: self.clone(),
                    entry: me.clone(),
                }),
                JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                    if me.local_name == JS_ATOM__star_ {
                        // export ns from
                        Ok(ModuleResolveExport::Found {
                            module: self.clone(),
                            entry: me.clone(),
                        })
                    } else {
                        let m1 = self.0.dependencies.borrow()[req_module_idx as usize]
                            .module
                            .clone()
                            .unwrap();

                        m1.resolve_export_inner(ctx, me.local_name, expect, resolved)
                    }
                }
            }
        } else {
            if export_name != JS_ATOM_default {
                // not found in direct or indirect exports: try star exports
                for idx in self.0.star_export_entries.borrow().iter() {
                    let m1 = self.0.dependencies.borrow()[*idx as usize]
                        .module
                        .clone()
                        .unwrap();

                    match m1.resolve_export_inner(ctx, export_name, expect, resolved)? {
                        ModuleResolveExport::Found { module, entry } => {
                            return Ok(
                                if let Some((test_module, test_local_name)) = expect
                                    && (test_local_name != entry.local_name
                                        || test_module.id() != module.id())
                                {
                                    ModuleResolveExport::Ambiguous
                                } else {
                                    ModuleResolveExport::Found { module, entry }
                                },
                            );
                        }
                        ModuleResolveExport::NotFound | ModuleResolveExport::Circular => {
                            return Ok(ModuleResolveExport::NotFound);
                        }
                        ModuleResolveExport::Ambiguous => {
                            return Ok(ModuleResolveExport::Ambiguous);
                        }
                    }
                }
            }

            Ok(ModuleResolveExport::NotFound)
        }
    }

    pub fn resolve_export(
        &self,
        ctx: &JSContext,
        export_name: JSAtom,
        expect: Option<(&ModuleDef, JSAtom)>, // expected (module, local_name)
    ) -> Result<ModuleResolveExport, i32> {
        let mut lst = Vec::<JSResolveEntry>::with_capacity(8);
        let ret = unsafe { self.resolve_export_inner(ctx, export_name, expect, &mut lst) };
        for ent in lst.drain(..) {
            JS_FreeAtom(ctx, ent.name);
        }
        ret
    }
}
