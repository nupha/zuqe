use std::{cmp::Ordering, ops::Deref};

use crate::{
    atom::{
        JSAtom,
        consts::{JS_ATOM__star_, JS_ATOM_Module, JS_ATOM_Symbol_toStringTag, JS_ATOM_default},
    },
    context::JSContext,
    function::JSVarRef,
    mem::{js_free, js_resize_array},
    module::{ModuleDef, ModuleDefInner, ModuleResolveExport},
    object::{
        JS_PROP_ENUMERABLE, JS_PROP_VARREF, JS_PROP_WRITABLE, JSObject, JSProperty, LazyInit,
        ObjectDetail,
    },
};

#[derive(Clone)]
pub enum ExportedNameEntryEnum {
    EXPORTED_NAME_AMBIGUOUS,
    EXPORTED_NAME_NORMAL(JSVarRef),
    EXPORTED_NAME_NS(ModuleDef),
}

pub struct ExportedNameEntry {
    pub export_name: JSAtom,
    pub export_type: ExportedNameEntryEnum,

    /// using when the list is built
    pub me: *mut JSExportEntry,
}

struct GetExportNamesState {
    pub modules: Vec<ModuleDef>,

    // TODO: use Vec<ExportedNameEntry>,
    pub exported_names: *mut ExportedNameEntry,
    pub exported_names_size: i32,
    pub exported_names_count: i32,
}

impl Default for GetExportNamesState {
    fn default() -> Self {
        Self {
            exported_names: std::ptr::null_mut(),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone)]
pub enum JSExportTypeEnum {
    JS_EXPORT_TYPE_LOCAL {
        var_idx: u16,
        var_ref: Option<JSVarRef>,
    },

    /// indirect export with req_module_idx
    JS_EXPORT_TYPE_INDIRECT(i32),
}

#[derive(Debug, Clone)]
pub struct JSExportEntry {
    pub export_type: JSExportTypeEnum,

    /// '*' if export ns from. not used for local export after compilation
    pub local_name: JSAtom,

    /// exported variable name
    pub export_name: JSAtom,
}

pub struct JSStarExportEntry {
    /// index in dependencies
    pub req_module_idx: i32,
}

impl ModuleDef {
    /// find export by name
    pub fn find_export(&self, export_name: JSAtom) -> Option<JSExportEntry> {
        self.0
            .export_entries
            .borrow()
            .iter()
            .find(|x| x.export_name == export_name)
            .map(|v| *v.clone())
    }

    pub unsafe fn get_exported_names(
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

        s.modules.try_reserve(1).map_err(|_| -1)?;
        s.modules.push(self.clone());

        for me in self.0.export_entries.borrow().iter() {
            if from_star && me.export_name == JS_ATOM_default {
                continue;
            }

            if let Some(j) = find_exported_name(s, me.export_name) {
                let en = s.exported_names.add(j as usize).as_mut().unwrap();
                en.me = std::ptr::null_mut();
            } else {
                if js_resize_array(
                    ctx,
                    std::ptr::addr_of_mut!(s.exported_names).cast(),
                    std::mem::size_of::<ExportedNameEntry>() as _,
                    &mut s.exported_names_size,
                    s.exported_names_count + 1,
                ) != 0
                {
                    return Err(-1);
                }

                let en = s
                    .exported_names
                    .add(s.exported_names_count as usize)
                    .as_mut()
                    .unwrap();
                s.exported_names_count += 1;
                en.export_name = me.export_name;

                // avoid a second lookup for simple module exports
                en.me = if from_star
                    || !matches!(
                        me.export_type,
                        JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { .. }
                    ) {
                    std::ptr::null_mut()
                } else {
                    me.deref() as *const JSExportEntry as *mut JSExportEntry
                };
            }
        }

        for idx in self.0.star_export_entries.borrow().iter() {
            let req_module_idx = *idx;
            self.0.dependencies.borrow()[req_module_idx as usize]
                .module
                .as_ref()
                .unwrap()
                .get_exported_names(ctx, s, true)?;
        }

        Ok(())
    }

    /// build module ns object
    unsafe fn make_ns(&self, ctx: &JSContext) -> Result<JSObject, i32> {
        let obj = ctx.create_object(ObjectDetail::MODULE_NS).map_err(|_| -1)?;

        let mut s_s = GetExportNamesState::default();
        let s = &mut s_s;

        let ret = self.get_exported_names(ctx, s, false);
        s.modules.clear();
        if ret.is_err() {
            js_free(ctx, s.exported_names.cast());
            return Err(-1);
        }

        /* Resolve the exported names. The ambiguous exports are removed */
        for i in 0..s.exported_names_count {
            let en = s.exported_names.add(i as usize).as_mut().unwrap();
            let res = if let Some(me) = en.me.as_ref() {
                // fast case: no resolution needed
                ModuleResolveExport::Found {
                    module: self.clone(),
                    entry: me.clone(),
                }
            } else {
                self.resolve_export(ctx, en.export_name, None)?
            };

            match res {
                ModuleResolveExport::Found { module, entry } => match entry.export_type {
                    JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(dep_idx) => {
                        if entry.local_name == JS_ATOM__star_ {
                            en.export_type = ExportedNameEntryEnum::EXPORTED_NAME_NS(
                                module.0.dependencies.borrow()[dep_idx as usize]
                                    .module
                                    .clone()
                                    .unwrap(),
                            );
                        }
                    }
                    JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { var_idx, var_ref } => {
                        debug_assert_ne!(entry.local_name, JS_ATOM__star_);
                        debug_assert_ne!(var_idx, u16::MAX);

                        en.export_type = ExportedNameEntryEnum::EXPORTED_NAME_NORMAL(
                            if let Some(vr) = var_ref {
                                vr
                            } else {
                                let p1 = (*module.0.bytecode.as_ptr()).object_unchecked();
                                p1.0.payload.borrow().func().unwrap().var_refs[var_idx as usize]
                                    .clone()
                            },
                        );
                    }
                },
                ModuleResolveExport::Ambiguous => {
                    en.export_type = ExportedNameEntryEnum::EXPORTED_NAME_AMBIGUOUS;
                }
                ModuleResolveExport::NotFound | ModuleResolveExport::Circular => {
                    res.throw_error(ctx, self, en.export_name);
                    js_free(ctx, s.exported_names.cast());
                    return Err(-1);
                }
            }
        }

        // sort the exported names
        let names = unsafe {
            std::slice::from_raw_parts_mut(s.exported_names, s.exported_names_count as usize)
        };
        names.sort_by(|a, b| {
            // XXX: should avoid allocation memory in atom comparison
            let str1 = a.export_name.to_value(ctx, true);
            let str2 = b.export_name.to_value(ctx, true);

            let res = if str1.is_exception() || str2.is_exception() {
                // XXX: raise an error ?
                Ordering::Equal
            } else {
                str1.string_unchecked()
                    .content()
                    .cmp(str2.string_unchecked().content())
            };

            res
        });

        for i in 0..s.exported_names_count {
            let en = s.exported_names.add(i as usize).as_ref().unwrap();
            match en.export_type.clone() {
                ExportedNameEntryEnum::EXPORTED_NAME_NORMAL(vref) => {
                    if obj
                        .add_shape_property_then(
                            ctx,
                            en.export_name,
                            JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_VARREF,
                            |pr| {
                                *pr = JSProperty::VarRef(vref);
                            },
                        )
                        .is_err()
                    {
                        js_free(ctx, s.exported_names.cast());
                        return Err(-1);
                    }
                }
                ExportedNameEntryEnum::EXPORTED_NAME_NS(module) => {
                    // the exported namespace must be created on demand
                    if obj
                        .define_lazy_property(
                            ctx,
                            en.export_name,
                            LazyInit::ModuleNS(module),
                            JS_PROP_ENUMERABLE | JS_PROP_WRITABLE,
                        )
                        .is_err()
                    {
                        js_free(ctx, s.exported_names.cast());
                        return Err(-1);
                    }
                }
                _ => {}
            }
        }

        js_free(ctx, s.exported_names.cast());

        let ms = JS_ATOM_Module.to_value(ctx, true);
        obj.define_property_value(ctx, JS_ATOM_Symbol_toStringTag, ms, 0)
            .unwrap();

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
            let ns = unsafe { self.make_ns(ctx)? };
            self.0.module_ns.replace(Some(ns.clone()));
            Ok(ns)
        }
    }
}

#[inline(always)]
pub unsafe fn find_exported_name(s: *mut GetExportNamesState, name: JSAtom) -> Option<i32> {
    for i in 0..(*s).exported_names_count {
        if (*(*s).exported_names.offset(i as isize)).export_name == name {
            return Some(i);
        }
    }

    return None;
}

#[deprecated(note = "use ModuleDef::find_export() instead")]
pub unsafe fn find_export_entry(m: &ModuleDef, export_name: JSAtom) -> Option<JSExportEntry> {
    m.find_export(export_name)
    // m.0.export_entries
    //     .borrow()
    //     .iter()
    //     .find(|me| me.export_name == export_name)
    //     .map(|v| *v.clone())
}

pub fn add_star_export_entry(
    ctx: &JSContext,
    m: &ModuleDef,
    req_module_idx: u16,
) -> Result<(), i32> {
    if m.0.star_export_entries.borrow().len() == m.0.star_export_entries.borrow().capacity() {
        m.0.star_export_entries
            .borrow_mut()
            .try_reserve(4)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
    }

    m.0.star_export_entries.borrow_mut().push(req_module_idx);
    Ok(())
}

#[deprecated(note = "use JSModuleDefInner::get_exportd_names() instead")]
pub unsafe fn get_exported_names(
    ctx: &JSContext,
    s: *mut GetExportNamesState,
    m: *mut ModuleDefInner,
    from_star: bool,
) -> Result<(), i32> {
    unreachable!("unused");
    // (*m).get_exported_names(ctx, s.as_mut().unwrap(), from_star)
}
