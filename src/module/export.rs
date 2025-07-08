use std::{cmp::Ordering, ops::Deref};

use crate::{
    atom::{
        JSAtom,
        consts::{JS_ATOM__star_, JS_ATOM_Module, JS_ATOM_Symbol_toStringTag, JS_ATOM_default},
    },
    context::JSContext,
    error::JSResult,
    function::JSVarRef,
    mem::{js_free, js_resize_array},
    module::{JSResolveResultEnum, JSResolveState, ModuleDef, ModuleDefInner, ModuleResolveExport},
    object::{
        JS_PROP_ENUMERABLE, JS_PROP_VARREF, JS_PROP_WRITABLE, ObjectDetail, JSProperty, LazyInit,
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

#[repr(C)]
pub struct JSStarExportEntry {
    pub req_module_idx: i32, // in req_module_entries
}

impl ModuleDef {
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

        for me in self.0.borrow().export_entries.iter() {
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

        for i in 0..self.0.borrow().star_export_entries_count {
            let idx = (*self.0.borrow().star_export_entries.add(i as usize)).req_module_idx;
            let m1 = (*self.0.borrow().req_module_entries.add(idx as usize))
                .module
                .as_ref()
                .unwrap();
            m1.get_exported_names(ctx, s, true)?;
        }

        Ok(())
    }

    /// build module ns object
    unsafe fn build_ns(&self, ctx: &JSContext) -> JSResult {
        let mut s_s = GetExportNamesState::default();
        let s = &mut s_s;

        let obj = ctx
            .create_instance(ObjectDetail::MODULE_NS)
            .map_err(|_| -1)?;
        let p = obj.object_unchecked();

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
                self.resolve_export(ctx, en.export_name, None)
            };

            match res {
                ModuleResolveExport::Found { module, entry } => match entry.export_type {
                    JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
                        if entry.local_name == JS_ATOM__star_ {
                            en.export_type = ExportedNameEntryEnum::EXPORTED_NAME_NS(
                                (*module
                                    .0
                                    .borrow()
                                    .req_module_entries
                                    .add(req_module_idx as usize))
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
                                let minner = module.0.borrow();
                                let p1 = minner.bytecode.object_unchecked();
                                p1.0.detail.borrow().func().unwrap().var_refs[var_idx as usize].clone()
                            },
                        );
                    }
                },
                ModuleResolveExport::Ambiguous => {
                    en.export_type = ExportedNameEntryEnum::EXPORTED_NAME_AMBIGUOUS;
                }
                ModuleResolveExport::NotFound
                | ModuleResolveExport::Circular
                | ModuleResolveExport::Error => {
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
                    if p.add_shape_property_then(
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
                        .object_unchecked()
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
            (*p.0.detailed.as_ptr()).set_extensible(false);
        }
        Ok(obj)
    }

    /// get module ns object.
    /// if ns object is not initialized, initialize it and return.
    pub fn ns(&self, ctx: &JSContext) -> JSResult {
        let mut ns = self.0.borrow().module_ns.clone();
        if ns.is_undefined() {
            ns = unsafe { self.build_ns(ctx)? };
            self.0.borrow_mut().module_ns = ns.clone();
        }
        Ok(ns)
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

pub unsafe fn find_export_entry(m: &ModuleDef, export_name: JSAtom) -> Option<JSExportEntry> {
    m.0.borrow()
        .export_entries
        .iter()
        .find(|me| me.export_name == export_name)
        .map(|v| *v.clone())
}

#[deprecated(note = "use ModuleDef::resolve_export1() instead")]
pub unsafe fn js_resolve_export1(
    ctx: &JSContext,
    pmodule: *mut *mut ModuleDefInner,
    pme: *mut *mut JSExportEntry,
    m: &ModuleDef,
    export_name: JSAtom,
    s: *mut JSResolveState,
) -> JSResolveResultEnum {
    unreachable!("unused");

    // *pmodule = std::ptr::null_mut();
    // *pme = std::ptr::null_mut();

    // if (*s).find(m, export_name).is_some() {
    //     return JSResolveResultEnum::JS_RESOLVE_RES_CIRCULAR;
    // }
    // if add_resolve_entry(ctx, s, m, export_name).is_err() {
    //     return JSResolveResultEnum::JS_RESOLVE_RES_EXCEPTION;
    // }

    // if let Some(me) = find_export_entry(m, export_name) {
    //     match me.export_type {
    //         JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL { .. } => {
    //             *pmodule = m;
    //             *pme = me as *const JSExportEntry as *mut JSExportEntry;
    //             return JSResolveResultEnum::JS_RESOLVE_RES_FOUND;
    //         }
    //         JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT(req_module_idx) => {
    //             let m1 = (*m
    //                 .0
    //                 .borrow()
    //                 .req_module_entries
    //                 .offset(req_module_idx as isize))
    //             .module
    //             .as_ref()
    //             .unwrap();

    //             return if (*me).local_name == JS_ATOM__star_ {
    //                 // export ns from
    //                 *pmodule = m.clone();
    //                 *pme = me as *const JSExportEntry as *mut JSExportEntry;
    //                 JSResolveResultEnum::JS_RESOLVE_RES_FOUND
    //             } else {
    //                 js_resolve_export1(ctx, pmodule, pme, m1, (*me).local_name, s)
    //             };
    //         }
    //     }
    // } else {
    //     if export_name != JS_ATOM_default {
    //         // not found in direct or indirect exports: try star exports
    //         for i in 0..m.0.borrow().star_export_entries_count {
    //             let idx = (*m.0.borrow_mut().star_export_entries.offset(i as isize)).req_module_idx;
    //             let m1 = (*m.0.borrow().req_module_entries.add(idx as usize))
    //                 .module
    //                 .clone()
    //                 .unwrap();

    //             let mut res_m: *mut JSModuleDefInner = std::ptr::null_mut();
    //             let mut res_me: *mut JSExportEntry = std::ptr::null_mut();
    //             let ret = js_resolve_export1(ctx, &mut res_m, &mut res_me, &m1, export_name, s);
    //             if ret == JSResolveResultEnum::JS_RESOLVE_RES_AMBIGUOUS
    //                 || ret == JSResolveResultEnum::JS_RESOLVE_RES_EXCEPTION
    //             {
    //                 return ret;
    //             } else if ret == JSResolveResultEnum::JS_RESOLVE_RES_FOUND {
    //                 if !(*pme).is_null() {
    //                     if *pmodule != res_m || (*res_me).local_name != (**pme).local_name {
    //                         *pmodule = std::ptr::null_mut();
    //                         *pme = std::ptr::null_mut();
    //                         return JSResolveResultEnum::JS_RESOLVE_RES_AMBIGUOUS;
    //                     }
    //                 } else {
    //                     *pmodule = res_m;
    //                     *pme = res_me;
    //                 }
    //             }
    //         }
    //         if !(*pme).is_null() {
    //             return JSResolveResultEnum::JS_RESOLVE_RES_FOUND;
    //         }
    //     }

    //     return JSResolveResultEnum::JS_RESOLVE_RES_NOT_FOUND;
    // }
}

// If the return value is JS_RESOLVE_RES_FOUND, return the module
// (*pmodule) and the corresponding local export entry
// (*pme). Otherwise return (NULL, NULL)
#[deprecated(note = "use ModuleDef::resolve_export() instead")]
pub unsafe fn js_resolve_export(
    ctx: &JSContext,
    pmodule: *mut *mut ModuleDefInner,
    pme: *mut *mut JSExportEntry,
    m: &mut ModuleDef,
    export_name: JSAtom,
) -> JSResolveResultEnum {
    unreachable!("unused");

    // let mut ss = JSResolveState {
    //     array: std::ptr::null_mut(),
    //     size: 0,
    //     count: 0,
    // };

    // let s = &mut ss;
    // let ret: JSResolveResultEnum = js_resolve_export1(ctx, pmodule, pme, m, export_name, s);

    // for i in 0..s.count {
    //     JS_FreeAtom(ctx, (*(*s).array.offset(i as isize)).name);
    // }
    // js_free(ctx, s.array.cast());

    // ret
}

pub unsafe fn add_star_export_entry(
    ctx: &JSContext,
    m: *mut ModuleDefInner,
    req_module_idx: i32,
) -> i32 {
    if js_resize_array(
        ctx,
        std::ptr::addr_of_mut!((*m).star_export_entries).cast(),
        std::mem::size_of::<JSStarExportEntry>() as _,
        std::ptr::addr_of_mut!((*m).star_export_entries_size),
        (*m).star_export_entries_count + 1,
    ) != 0
    {
        return -1;
    }

    let se = (*m)
        .star_export_entries
        .offset((*m).star_export_entries_count as _);

    (*se).req_module_idx = req_module_idx;
    (*m).star_export_entries_count += 1;

    0
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
