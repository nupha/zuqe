use std::{ffi::CStr, ops::Deref};

use crate::{
    atom::{
        JS_FreeAtom, JSAtom,
        consts::{JS_ATOM__star_, JS_ATOM_default},
    },
    context::JSContext,
    error::JS_ThrowSyntaxError,
    mem::js_free,
    module::{
        JSExportEntry, JSExportTypeEnum, JSModuleDef, JSResolveState, add_resolve_entry,
        find_export_entry,
    },
};

#[derive(Debug)]
pub enum ModuleResolveExport {
    Found {
        module: JSModuleDef,
        entry: JSExportEntry,
    },
    NotFound,
    Ambiguous,
    Circular,
    Error,
}

impl ModuleResolveExport {
    /// put exception to current_exception according to status
    pub fn throw_error(&self, ctx: &JSContext, m: &JSModuleDef, export_name: JSAtom) {
        match self {
            Self::Found { .. } => {}
            Self::Error => {}
            Self::NotFound => {
                JS_ThrowSyntaxError(
                    ctx,
                    &format!(
                        "Could not find export '{}' in module '{}'",
                        export_name.to_str(ctx.rt()),
                        m.0.borrow().module_name.to_str(ctx.rt()),
                    ),
                );
            }
            Self::Circular => {
                JS_ThrowSyntaxError(
                    ctx,
                    &format!(
                        "circular reference when looking for export '{}' in module '{}'",
                        export_name.to_str(ctx.rt()),
                        m.0.borrow().module_name.to_str(ctx.rt()),
                    ),
                );
            }
            Self::Ambiguous => {
                JS_ThrowSyntaxError(
                    ctx,
                    &format!(
                        "export '{}' in module '{}' is ambiguous",
                        export_name.to_str(ctx.rt()),
                        m.0.borrow().module_name.to_str(ctx.rt()),
                    ),
                );
            }
        }
    }
}

impl JSModuleDef {
    unsafe fn resolve_export1(
        &self,
        ctx: &JSContext,
        export_name: JSAtom,
        expect: Option<(&JSModuleDef, JSAtom)>, // expected (module, local_name)
        s: &mut JSResolveState,
    ) -> ModuleResolveExport {
        if s.find(self, export_name).is_some() {
            return ModuleResolveExport::Circular;
        }

        if add_resolve_entry(ctx, s, self, export_name).is_err() {
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
                    let m1 = (*self
                        .0
                        .borrow()
                        .req_module_entries
                        .offset(req_module_idx as isize))
                    .module
                    .as_ref()
                    .unwrap();

                    return if me.local_name == JS_ATOM__star_ {
                        // export ns from
                        ModuleResolveExport::Found {
                            module: self.clone(),
                            entry: me.clone(),
                        }
                    } else {
                        m1.resolve_export1(ctx, me.local_name, expect, s)
                    };
                }
            }
        } else {
            if export_name != JS_ATOM_default {
                // not found in direct or indirect exports: try star exports
                for i in 0..self.0.borrow().star_export_entries_count {
                    let idx = (*self.0.borrow_mut().star_export_entries.offset(i as isize))
                        .req_module_idx;

                    let m1 = (*self.0.borrow().req_module_entries.add(idx as usize))
                        .module
                        .clone()
                        .unwrap();

                    match m1.resolve_export1(ctx, export_name, expect, s) {
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
        expect: Option<(&JSModuleDef, JSAtom)>, // expected (module, local_name)
    ) -> ModuleResolveExport {
        let mut ss = JSResolveState {
            array: std::ptr::null_mut(),
            size: 0,
            count: 0,
        };
        let s = &mut ss;
        let ret = unsafe { self.resolve_export1(ctx, export_name, expect, s) };

        unsafe {
            for i in 0..s.count {
                JS_FreeAtom(ctx, (*(*s).array.offset(i as isize)).name);
            }
            js_free(ctx, s.array.cast());
        }

        ret
    }
}
