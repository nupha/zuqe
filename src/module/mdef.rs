use std::{
    cmp::Ordering,
    ffi::{c_void, CStr},
    ops::Deref,
    path::PathBuf,
    ptr::NonNull,
};

use crate::{
    atom::{consts::*, JSAtom, JS_DupAtom, JS_FreeAtom, JS_ATOM_NULL},
    class::JSClassExoticMethods,
    context::JSContext,
    error::{
        JSResult, JS_ThrowReferenceError, JS_ThrowStackOverflow, JS_ThrowSyntaxError,
        JS_ThrowSyntaxErrorAtom,
    },
    function::{
        js_class_has_bytecode, JSFunctionBytecode, JSFunctionDef, JSVarKindEnum, JSVarRef, JS_Call,
    },
    gc::{add_gc_object, JSGCObjectTypeEnum},
    list_entry,
    mem::{js_free, js_malloc_typed, js_resize_array},
    runtime::{js_check_stack_overflow, JSRuntime},
    types::{
        list_add_tail, list_del, list_foreach_safe, list_foreach_with_ret, list_head,
        JSRefCountHeader,
    },
    value::{
        object::{
            JSAutoInitIDEnum, JSObject, JS_NewObjectProto, JS_PROP_C_W_E, JS_PROP_ENUMERABLE,
            JS_PROP_VARREF, JS_PROP_WRITABLE,
        },
        set_value, JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_IsException,
        JS_TAG_OBJECT, JS_VALUE_GET_OBJ, JS_VALUE_GET_PTR, JS_VALUE_GET_TAG,
    },
    JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_MODULE_NS, JS_MKPTR,
};

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

pub type JSModuleNormalizeFunc = for<'a> fn(
    ctx: *mut JSContext,
    module_base_name: &'a str,
    module_name: &'a str,
    opaque: *mut ::core::ffi::c_void,
) -> Result<String, i32>;

pub type JSModuleLoaderFunc = for<'a> fn(
    ctx: &'a mut JSContext,
    module_name: &'a str,
    opaque: *mut ::core::ffi::c_void,
) -> *mut JSModuleDef;

pub type JSModuleInitFunc =
    unsafe fn(ctx: *mut JSContext, m: *mut JSModuleDef) -> ::core::ffi::c_int;

#[repr(C)]
pub struct JSModuleDef {
    pub header: JSRefCountHeader, /* must come first, 32-bit */

    pub module_name: JSAtom,
    pub link: list_head,

    pub req_module_entries: *mut JSReqModuleEntry,
    pub req_module_entries_count: u16,
    pub req_module_entries_size: i32,

    pub export_entries: Vec<Box<JSExportEntry>>,
    // pub export_entries: *mut JSExportEntry,
    // pub export_entries_count: u16,
    // pub export_entries_size: i32,
    pub star_export_entries: *mut JSStarExportEntry,
    pub star_export_entries_count: i32,
    pub star_export_entries_size: i32,

    pub import_entries: *mut JSImportEntry,
    pub import_entries_count: u16,
    pub import_entries_size: i32,

    pub module_ns: JSValue,
    pub func_obj: JSValue,                   /* only used for JS modules */
    pub init_func: Option<JSModuleInitFunc>, /* only used for C modules */
    pub has_tla: bool,                       /* true if func_obj contains await */
    pub resolved: bool,
    pub func_created: bool,
    pub status: JSModuleStatus,

    /* temp use during js_module_link() & js_module_evaluate() */
    pub dfs_index: i32,
    pub dfs_ancestor_index: i32,
    pub stack_prev: *mut JSModuleDef,

    //
    // temp use during js_module_evaluate()
    //
    pub async_parent_modules: *mut *mut JSModuleDef,
    pub async_parent_modules_count: i32,
    pub async_parent_modules_size: i32,
    pub pending_async_dependencies: i32,
    pub async_evaluation: bool,
    pub async_evaluation_timestamp: i64,
    pub cycle_root: *mut JSModuleDef,
    pub promise: JSValue, /* corresponds to spec field: capability */
    pub resolving_funcs: [JSValue; 2], /* corresponds to spec field: capability */

    /* true if evaluation yielded an exception. It is saved in
    eval_exception */
    pub eval_has_exception: bool,
    pub eval_exception: JSValue,

    /// the import.meta object
    pub meta_obj: JSValue,
}

impl JSModuleDef {
    /// 'name' is consumed
    pub unsafe fn new(ctx: &mut JSContext, name: JSAtom) -> Result<Box<Self>, i32> {
        match js_malloc_typed::<JSModuleDef>(ctx, 1, true) {
            Some(ptr) => {
                let mut m = Box::from_raw(ptr.as_ptr());
                m.header.ref_count = 1;
                m.module_name = name;
                m.module_ns = JSValue::UNDEFINED;
                m.func_obj = JSValue::UNDEFINED;
                m.eval_exception = JSValue::UNDEFINED;
                m.meta_obj = JSValue::UNDEFINED;
                m.promise = JSValue::UNDEFINED;
                m.resolving_funcs[0] = JSValue::UNDEFINED;
                m.resolving_funcs[1] = JSValue::UNDEFINED;

                m.export_entries = Vec::new();
                m.star_export_entries = std::ptr::null_mut();
                m.import_entries = std::ptr::null_mut();
                m.req_module_entries = std::ptr::null_mut();

                list_add_tail(&mut m.link, &mut ctx.loaded_modules);
                Ok(m)
            }
            None => {
                JS_FreeAtom(ctx, name);
                Err(-1)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSExportTypeEnum {
    JS_EXPORT_TYPE_LOCAL,
    JS_EXPORT_TYPE_INDIRECT,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportedNameEntryEnum {
    EXPORTED_NAME_AMBIGUOUS,
    EXPORTED_NAME_NORMAL,
    EXPORTED_NAME_NS,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSResolveResultEnum {
    JS_RESOLVE_RES_EXCEPTION = -1, /* memory alloc error */
    JS_RESOLVE_RES_FOUND = 0,
    JS_RESOLVE_RES_NOT_FOUND,
    JS_RESOLVE_RES_CIRCULAR,
    JS_RESOLVE_RES_AMBIGUOUS,
}

#[repr(C)]
pub struct ExportedNameEntry {
    pub export_name: JSAtom,
    pub export_type: ExportedNameEntryEnum,
    pub u: ExportedNameEntryUnion,
}

#[repr(C)]
pub union ExportedNameEntryUnion {
    pub me: *mut JSExportEntry, /* using when the list is built */

    // TODO: NonNull<JSVarRef>
    pub var_ref: *mut JSVarRef, /* EXPORTED_NAME_NORMAL */

    pub module: *mut JSModuleDef, /* for EXPORTED_NAME_NS */
}

#[repr(C)]
pub struct GetExportNamesState {
    // TODO: use Vec<&mut JSModuleDef>,
    pub modules: *mut *mut JSModuleDef,
    pub modules_size: i32,
    pub modules_count: i32,

    // TODO: use Vec<ExportedNameEntry>,
    pub exported_names: *mut ExportedNameEntry,
    pub exported_names_size: i32,
    pub exported_names_count: i32,
}

#[repr(C)]
pub struct JSReqModuleEntry {
    pub module_name: JSAtom,
    pub module: *mut JSModuleDef, /* used using resolution */
}

#[repr(C)]
pub struct JSExportEntry {
    pub u: JSExportEntry_u,
    pub export_type: JSExportTypeEnum,
    // '*' if export ns from. not used for local export after compilation
    pub local_name: JSAtom,
    // exported variable name
    pub export_name: JSAtom,
}

#[repr(C)]
pub union JSExportEntry_u {
    // for local export
    pub local: JSExportEntry_u_local,
    // module for indirect export
    pub req_module_idx: i32,
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct JSExportEntry_u_local {
    // closure variable index
    pub var_idx: u16,
    // if != NULL, reference to the variable
    // TODO use Option<NonNull<JSVarRef>>
    pub var_ref: *mut JSVarRef,
}

#[repr(C)]
pub struct JSStarExportEntry {
    pub req_module_idx: i32, // in req_module_entries
}

#[repr(C)]
pub struct JSImportEntry {
    pub var_idx: u16, // closure variable index
    pub import_name: JSAtom,
    pub req_module_idx: i32, // in req_module_entries
}

#[repr(C)]
pub struct JSResolveEntry {
    module: *mut JSModuleDef,
    name: JSAtom,
}

#[repr(C)]
pub struct JSResolveState {
    array: *mut JSResolveEntry,
    size: i32,
    count: i32,
}

impl JSModuleDef {
    pub unsafe fn get_exported_names(
        &self,
        ctx: &mut JSContext,
        s: &mut GetExportNamesState,
        from_star: bool,
    ) -> Result<(), i32> {
        // check circular reference
        for i in 0..s.modules_count {
            if *s.modules.add(i as usize) as *const JSModuleDef == self {
                return Ok(());
            }
        }

        if js_resize_array(
            ctx,
            std::ptr::addr_of_mut!(s.modules).cast(),
            std::mem::size_of::<*mut JSModuleDef>() as _,
            &mut s.modules_size,
            s.modules_count + 1,
        ) != 0
        {
            return Err(-1);
        }

        *s.modules.add(s.modules_count as usize) = self as *const JSModuleDef as *mut JSModuleDef;
        s.modules_count += 1;

        for me in self.export_entries.iter() {
            if from_star && me.export_name == JS_ATOM_default {
                continue;
            }

            if let Some(j) = find_exported_name(s, me.export_name) {
                let en = s.exported_names.add(j as usize).as_mut().unwrap();
                en.u.me = std::ptr::null_mut();
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
                en.u.me = if from_star || me.export_type != JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                    std::ptr::null_mut()
                } else {
                    me.deref() as *const JSExportEntry as *mut JSExportEntry
                };
            }
        }

        for i in 0..self.star_export_entries_count {
            let se = self.star_export_entries.add(i as usize);
            let m1 = (*self.req_module_entries.add((*se).req_module_idx as usize)).module;
            (*m1).get_exported_names(ctx, s, true)?;
        }

        Ok(())
    }

    /// build module ns object
    pub(crate) unsafe fn build_ns(&mut self, ctx: &mut JSContext) -> JSResult {
        let mut s_s: GetExportNamesState = std::mem::zeroed();
        let s = &mut s_s;

        let obj = ctx.create_object_with_classid(JS_CLASS_MODULE_NS)?;
        let p = obj.object_unchecked();

        let ret = self.get_exported_names(ctx, s, false);
        js_free(ctx, s.modules.cast());
        if ret.is_err() {
            js_free(ctx, s.exported_names.cast());
            JS_FreeValue(ctx, obj);
            return Err(-1);
        }

        /* Resolve the exported names. The ambiguous exports are removed */
        for i in 0..s.exported_names_count {
            let en = s.exported_names.add(i as usize).as_mut().unwrap();

            let mut res_me = std::ptr::null_mut::<JSExportEntry>();
            let mut res_m = std::ptr::null_mut::<JSModuleDef>();
            let res = if !en.u.me.is_null() {
                // fast case: no resolution needed
                res_me = en.u.me;
                res_m = self;
                JSResolveResultEnum::JS_RESOLVE_RES_FOUND
            } else {
                js_resolve_export(ctx, &mut res_m, &mut res_me, self, en.export_name)
            };

            if res != JSResolveResultEnum::JS_RESOLVE_RES_FOUND {
                if res != JSResolveResultEnum::JS_RESOLVE_RES_AMBIGUOUS {
                    js_resolve_export_throw_error(ctx, res, self, en.export_name);
                    js_free(ctx, s.exported_names.cast());
                    JS_FreeValue(ctx, obj);
                    return Err(-1);
                }
                en.export_type = ExportedNameEntryEnum::EXPORTED_NAME_AMBIGUOUS;
            } else {
                if (*res_me).local_name == JS_ATOM__star_ {
                    en.export_type = ExportedNameEntryEnum::EXPORTED_NAME_NS;
                    en.u.module = (*(*res_m)
                        .req_module_entries
                        .add((*res_me).u.req_module_idx as usize))
                    .module;
                } else {
                    en.export_type = ExportedNameEntryEnum::EXPORTED_NAME_NORMAL;
                    if !(*res_me).u.local.var_ref.is_null() {
                        en.u.var_ref = (*res_me).u.local.var_ref;
                    } else {
                        let p1 = (*res_m).func_obj.object_unchecked();
                        en.u.var_ref =
                            p1.u.func.var_refs[(*res_me).u.local.var_idx as usize].as_ptr();
                    }
                }
            }
        }

        // sort the exported names
        let names =
            std::slice::from_raw_parts_mut(s.exported_names, s.exported_names_count as usize);
        names.sort_by(|a, b| {
            // XXX: should avoid allocation memory in atom comparison
            let str1 = a.export_name.to_value(ctx, true);
            let str2 = b.export_name.to_value(ctx, true);

            let res = if str1.is_exception() || str2.is_exception() {
                // XXX: raise an error ?
                Ordering::Equal
            } else {
                str1.string().content().cmp(str2.string().content())
            };

            JS_FreeValue(ctx, str1);
            JS_FreeValue(ctx, str2);

            res
        });

        // original c code
        // rqsort(
        //     (*s).exported_names,
        //     (*s).exported_names_count as usize,
        //     std::mem::size_of::<ExportedNameEntry>(),
        //     exported_names_cmp,
        //     ctx,
        // );

        for i in 0..s.exported_names_count {
            let en = s.exported_names.add(i as usize).as_mut().unwrap();

            match en.export_type {
                ExportedNameEntryEnum::EXPORTED_NAME_NORMAL => {
                    let var_ref = en.u.var_ref;
                    if let Ok(pr) = p.add_property(
                        ctx,
                        en.export_name,
                        JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_VARREF,
                    ) {
                        (*var_ref).u0.header.ref_count += 1;
                        (*pr).u.var_ref = var_ref;
                    } else {
                        js_free(ctx, s.exported_names.cast());
                        JS_FreeValue(ctx, obj);
                        return Err(-1);
                    }
                }
                ExportedNameEntryEnum::EXPORTED_NAME_NS => {
                    // the exported namespace must be created on demand
                    if obj
                        .define_auto_init_property(
                            ctx,
                            en.export_name,
                            JSAutoInitIDEnum::JS_AUTOINIT_ID_MODULE_NS,
                            en.u.module.cast(),
                            JS_PROP_ENUMERABLE | JS_PROP_WRITABLE,
                        )
                        .is_err()
                    {
                        js_free(ctx, s.exported_names.cast());
                        JS_FreeValue(ctx, obj);
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

        p.detailed.set_extensible(false);

        Ok(obj)
    }

    /// get module ns object. if ns object is not initialized, initialize it and return.
    pub fn ns(&mut self, ctx: &mut JSContext) -> JSResult {
        if self.module_ns.is_undefined() {
            let ns = unsafe { self.build_ns(ctx)? };
            self.module_ns = ns;
        }

        Ok(self.module_ns.dup())
    }

    /// get module import.meta object.
    pub fn import_meta(&mut self, ctx: &mut JSContext) -> JSResult {
        // allocate meta_obj only if requested to save memory
        let mut meta = self.meta_obj;
        if meta.is_undefined() {
            meta = ctx.create_object_with_proto(JSValue::NULL)?;
            self.meta_obj = meta;
        }
        Ok(meta.dup())
    }

    /// set module import.meta props.
    pub fn set_import_meta(
        &mut self,
        ctx: &mut JSContext,
        url: &str,
        is_main: bool,
    ) -> Result<(), i32> {
        let meta_obj = self.import_meta(ctx)?.into_rc(ctx.rt());

        let sval = ctx.create_string(url);
        meta_obj.define_property_value_str(ctx, "url", sval, JS_PROP_C_W_E)?;
        meta_obj.define_property_value_str(ctx, "main", is_main.into(), JS_PROP_C_W_E)?;
        Ok(())
    }
}

/// Note: 'name' is freed on error
#[deprecated(note = "use JSModuleDef::new() instead")]
pub unsafe fn js_new_module_def(ctx: *mut JSContext, name: JSAtom) -> *mut JSModuleDef {
    if let Ok(m) = JSModuleDef::new(ctx.as_mut().unwrap(), name) {
        Box::into_raw(m) // take ownership
    } else {
        std::ptr::null_mut()
    }
}

pub unsafe fn js_free_module_def(ctx: *mut JSContext, m: *mut JSModuleDef) {
    JS_FreeAtom(ctx, (*m).module_name);

    for i in 0..(*m).req_module_entries_count {
        let rme: *mut JSReqModuleEntry = (*m).req_module_entries.offset(i as isize);
        JS_FreeAtom(ctx, (*rme).module_name);
    }
    js_free(ctx, (*m).req_module_entries.cast());

    for me in (*m).export_entries.drain(..) {
        if me.export_type == JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
            debug_assert!(!me.u.local.var_ref.is_null());
            debug_assert!(me.u.local.var_ref.is_aligned());

            if let Some(_) = Box::from_raw(me.u.local.var_ref).free((*ctx).rt_mut()) {
                unreachable!();
                // std::mem::forget(_vr);
            }
        }
        JS_FreeAtom(ctx, me.export_name);
        JS_FreeAtom(ctx, me.local_name);
    }

    // for i in 0..(*m).export_entries_count {
    //     let me = (*m).export_entries.offset(i as isize);
    //     debug_assert!(!me.is_null());

    //     if (*me).export_type == JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
    //         if let Some(vr) = Box::from_raw((*me).u.local.var_ref).free((*ctx).rt_mut()) {
    //             unreachable!();
    //             std::mem::forget(vr);
    //         }
    //     }
    //     JS_FreeAtom(ctx, (*me).export_name);
    //     JS_FreeAtom(ctx, (*me).local_name);
    // }
    // js_free(ctx, (*m).export_entries.cast());

    js_free(ctx, (*m).star_export_entries.cast());

    for i in 0..(*m).import_entries_count {
        let mi: *mut JSImportEntry = (*m).import_entries.offset(i as isize);
        JS_FreeAtom(ctx, (*mi).import_name);
    }
    js_free(ctx, (*m).import_entries.cast());
    js_free(ctx, (*m).async_parent_modules.cast());

    JS_FreeValue(ctx, (*m).module_ns);
    JS_FreeValue(ctx, (*m).func_obj);
    JS_FreeValue(ctx, (*m).eval_exception);
    JS_FreeValue(ctx, (*m).meta_obj);
    JS_FreeValue(ctx, (*m).promise);
    JS_FreeValue(ctx, (*m).resolving_funcs[0]);
    JS_FreeValue(ctx, (*m).resolving_funcs[1]);
    list_del(&mut (*m).link);

    drop(Box::from_raw(m));
}

pub unsafe fn find_export_entry<'a>(
    ctx: *mut JSContext,
    m: *mut JSModuleDef,
    export_name: JSAtom,
) -> Option<&'a JSExportEntry> {
    (*m).export_entries
        .iter()
        .find(|me| me.export_name == export_name)
        .map(|me| me.deref())
}

pub unsafe fn add_req_module_entry(
    ctx: *mut JSContext,
    m: *mut JSModuleDef,
    module_name: JSAtom,
) -> i32 {
    // no need to add the module request if it is already present
    for i in 0..(*m).req_module_entries_count {
        if (*m)
            .req_module_entries
            .offset(i as isize)
            .as_ref()
            .unwrap()
            .module_name
            == module_name
        {
            return i as i32; // found index
        }
    }

    if js_resize_array(
        ctx,
        std::ptr::addr_of_mut!((*m).req_module_entries).cast(),
        std::mem::size_of::<JSReqModuleEntry>() as _,
        std::ptr::addr_of_mut!((*m).req_module_entries_size),
        (*m).req_module_entries_count as i32 + 1,
    ) != 0
    {
        return -1;
    }

    let rme = &(*m)
        .req_module_entries
        .offset((*m).req_module_entries_count as isize);
    (*m).req_module_entries_count += 1;
    (*rme).as_mut().unwrap().module_name = JS_DupAtom(ctx, module_name);
    (*rme).as_mut().unwrap().module = std::ptr::null_mut();

    (*m).req_module_entries_count as i32 - 1 // the last index is count-1
}

pub unsafe fn add_star_export_entry(
    ctx: *mut JSContext,
    m: *mut JSModuleDef,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSFreeModuleEnum {
    JS_FREE_MODULE_ALL,
    JS_FREE_MODULE_NOT_RESOLVED,
}

// XXX: would be more efficient with separate module lists
pub unsafe fn js_free_modules(ctx: &mut JSContext, flag: JSFreeModuleEnum) {
    // let mut cnt = (*ctx).loaded_modules.len();
    // println!("loaded_modules: {cnt}");
    // while let Some(mut m) = (*ctx).loaded_modules.pop_front() {
    //     match flag {
    //         JSFreeModuleEnum::JS_FREE_MODULE_ALL => {
    //             js_free_module_def(ctx, m.deref_mut());
    //         }
    //         JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED => {
    //             if m.resolved != 0 {
    //                 js_free_module_def(ctx, m.deref_mut());
    //             } else {
    //                 (*ctx).loaded_modules.push_back(m);
    //                 cnt -= 1;
    //                 if cnt == 0 {
    //                     break;
    //                 }
    //             }
    //         }
    //     }
    // }

    list_foreach_safe(&mut ctx.loaded_modules, |el| {
        let m = list_entry!(el, JSModuleDef, link);
        if flag == JSFreeModuleEnum::JS_FREE_MODULE_ALL
            || (flag == JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED && (*m).resolved)
        {
            js_free_module_def(ctx, m);
        }
    });
}

pub unsafe fn find_resolve_entry(
    s: *mut JSResolveState,
    m: *mut JSModuleDef,
    name: JSAtom,
) -> Option<i32> {
    for i in 0..(*s).count {
        let re = (*s).array.add(i as usize).as_mut().unwrap();
        if re.module == m && re.name == name {
            return Some(i);
        }
    }
    None
}

pub unsafe fn add_resolve_entry(
    ctx: *mut JSContext,
    s: *mut JSResolveState,
    m: *mut JSModuleDef,
    name: JSAtom,
) -> Result<(), i32> {
    if js_resize_array(
        ctx,
        std::ptr::addr_of_mut!((*s).array).cast(),
        std::mem::size_of::<JSResolveEntry>() as _,
        std::ptr::addr_of_mut!((*s).size),
        (*s).count + 1,
    ) != 0
    {
        return Err(-1);
    }

    let re = (*s).array.add((*s).count as usize).as_mut().unwrap();
    (*s).count += 1;
    re.module = m;
    re.name = JS_DupAtom(ctx, name);

    Ok(())
}

pub unsafe fn js_resolve_export1(
    ctx: *mut JSContext,
    pmodule: *mut *mut JSModuleDef,
    pme: *mut *mut JSExportEntry,
    m: *mut JSModuleDef,
    export_name: JSAtom,
    s: *mut JSResolveState,
) -> JSResolveResultEnum {
    *pmodule = std::ptr::null_mut();
    *pme = std::ptr::null_mut();

    if find_resolve_entry(s, m, export_name).is_some() {
        return JSResolveResultEnum::JS_RESOLVE_RES_CIRCULAR;
    }
    if add_resolve_entry(ctx, s, m, export_name).is_err() {
        return JSResolveResultEnum::JS_RESOLVE_RES_EXCEPTION;
    }

    if let Some(me) = find_export_entry(ctx, m, export_name) {
        if me.export_type == JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
            // local export
            *pmodule = m;
            *pme = me as *const JSExportEntry as *mut JSExportEntry;
            return JSResolveResultEnum::JS_RESOLVE_RES_FOUND;
        } else {
            // indirect export
            let m1 = (*(*m)
                .req_module_entries
                .offset((*me).u.req_module_idx as isize))
            .module;

            if (*me).local_name == JS_ATOM__star_ {
                // export ns from
                *pmodule = m;
                *pme = me as *const JSExportEntry as *mut JSExportEntry;
                return JSResolveResultEnum::JS_RESOLVE_RES_FOUND;
            } else {
                return js_resolve_export1(ctx, pmodule, pme, m1, (*me).local_name, s);
            }
        }
    } else {
        if export_name != JS_ATOM_default {
            // not found in direct or indirect exports: try star exports
            for i in 0..(*m).star_export_entries_count {
                let se = (*m).star_export_entries.offset(i as isize);

                let m1 = (*(*m)
                    .req_module_entries
                    .offset((*se).req_module_idx as isize))
                .module;

                let mut res_m: *mut JSModuleDef = std::ptr::null_mut();
                let mut res_me: *mut JSExportEntry = std::ptr::null_mut();
                let ret = js_resolve_export1(ctx, &mut res_m, &mut res_me, m1, export_name, s);
                if ret == JSResolveResultEnum::JS_RESOLVE_RES_AMBIGUOUS
                    || ret == JSResolveResultEnum::JS_RESOLVE_RES_EXCEPTION
                {
                    return ret;
                } else if ret == JSResolveResultEnum::JS_RESOLVE_RES_FOUND {
                    if !(*pme).is_null() {
                        if *pmodule != res_m || (*res_me).local_name != (**pme).local_name {
                            *pmodule = std::ptr::null_mut();
                            *pme = std::ptr::null_mut();
                            return JSResolveResultEnum::JS_RESOLVE_RES_AMBIGUOUS;
                        }
                    } else {
                        *pmodule = res_m;
                        *pme = res_me;
                    }
                }
            }
            if !(*pme).is_null() {
                return JSResolveResultEnum::JS_RESOLVE_RES_FOUND;
            }
        }

        return JSResolveResultEnum::JS_RESOLVE_RES_NOT_FOUND;
    }
}

// If the return value is JS_RESOLVE_RES_FOUND, return the module
// (*pmodule) and the corresponding local export entry
// (*pme). Otherwise return (NULL, NULL)
pub unsafe fn js_resolve_export(
    ctx: *mut JSContext,
    pmodule: *mut *mut JSModuleDef,
    pme: *mut *mut JSExportEntry,
    m: *mut JSModuleDef,
    export_name: JSAtom,
) -> JSResolveResultEnum {
    let mut ss: JSResolveState = JSResolveState {
        array: std::ptr::null_mut(),
        size: 0,
        count: 0,
    };

    let s = &mut ss;
    let ret: JSResolveResultEnum = js_resolve_export1(ctx, pmodule, pme, m, export_name, s);

    for i in 0..s.count {
        JS_FreeAtom(ctx, (*(*s).array.offset(i as isize)).name);
    }
    js_free(ctx, s.array.cast());

    ret
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

#[deprecated(note = "use JSModuleDef::get_exportd_names() instead")]
pub unsafe fn get_exported_names(
    ctx: *mut JSContext,
    s: *mut GetExportNamesState,
    m: *mut JSModuleDef,
    from_star: bool,
) -> Result<(), i32> {
    (*m).get_exported_names(ctx.as_mut().unwrap(), s.as_mut().unwrap(), from_star)
}

pub unsafe fn add_module_variables(ctx: *mut JSContext, fd: *mut JSFunctionDef) -> i32 {
    let m: *mut JSModuleDef = (*fd).module;

    // The imported global variables were added as closure variables
    // in js_parse_import(). We add here the module global
    // variables.

    for i in 0..(*fd).global_var_count {
        let hf = ((*fd).global_vars).offset(i as isize);
        if (*fd)
            .add_closure_var(
                ctx.as_mut().unwrap(),
                true,
                false,
                i,
                (*hf).var_name,
                (*hf).is_const(),
                (*hf).is_lexical(),
                JSVarKindEnum::JS_VAR_NORMAL,
            )
            .is_err()
        {
            return -1;
        }
    }

    // resolve the variable names of the local exports
    for me in (*m).export_entries.iter_mut() {
        if me.export_type == JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
            if let Some(idx) = (*fd).find_closure_var(me.local_name) {
                me.u.local.var_idx = idx;
            } else {
                JS_ThrowSyntaxErrorAtom(
                    ctx,
                    c"exported variable '%s' does not exist",
                    me.local_name,
                );
                return -1;
            }
        }
    }

    // for i in 0..(*m).export_entries_count {
    //     let me = ((*m).export_entries).offset(i as isize);
    //     if (*me).export_type == JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
    //         if let Some(idx) = (*fd).find_closure_var((*me).local_name) {
    //             (*me).u.local.var_idx = idx;
    //         } else {
    //             JS_ThrowSyntaxErrorAtom(
    //                 ctx,
    //                 c"exported variable '%s' does not exist",
    //                 (*me).local_name,
    //             );
    //             return -1;
    //         }
    //     }
    // }

    0
}

pub unsafe fn js_find_loaded_module(ctx: *mut JSContext, name: JSAtom) -> *mut JSModuleDef {
    // first look at the loaded modules
    let r = list_foreach_with_ret(&mut (*ctx).loaded_modules, |el| {
        let m = (el as *mut u8)
            .offset(-(std::mem::offset_of!(JSModuleDef, link) as isize))
            .cast::<JSModuleDef>();
        if (*m).module_name == name {
            return Some(m);
        }
        None
    });
    if let Some(m) = r {
        return m;
    }

    std::ptr::null_mut()
}

pub unsafe fn js_resolve_export_throw_error(
    ctx: *mut JSContext,
    res: JSResolveResultEnum,
    m: *mut JSModuleDef,
    export_name: JSAtom,
) {
    match res {
        JSResolveResultEnum::JS_RESOLVE_RES_EXCEPTION => {}
        JSResolveResultEnum::JS_RESOLVE_RES_NOT_FOUND => {
            JS_ThrowSyntaxError(
                ctx,
                CStr::from_ptr(
                    format!(
                        "Could not find export '{}' in module '{}'\0",
                        export_name.to_str((*ctx).rt()),
                        (*m).module_name.to_str((*ctx).rt()),
                    )
                    .as_ptr()
                    .cast(),
                ),
            );
        }
        JSResolveResultEnum::JS_RESOLVE_RES_CIRCULAR => {
            JS_ThrowSyntaxError(
                ctx,
                CStr::from_ptr(
                    format!(
                        "circular reference when looking for export '{}' in module '{}'\0",
                        export_name.to_str((*ctx).rt()),
                        (*m).module_name.to_str((*ctx).rt()),
                    )
                    .as_ptr()
                    .cast(),
                ),
            );
        }
        JSResolveResultEnum::JS_RESOLVE_RES_AMBIGUOUS => {
            JS_ThrowSyntaxError(
                ctx,
                CStr::from_ptr(
                    format!(
                        "export '{}' in module '{}' is ambiguous\0",
                        export_name.to_str((*ctx).rt()),
                        (*m).module_name.to_str((*ctx).rt()),
                    )
                    .as_ptr()
                    .cast(),
                ),
            );
        }
        _ => {}
    }
}

// Load all the required modules for module 'm'
pub unsafe fn js_resolve_module(ctx: *mut JSContext, m: *mut JSModuleDef) -> i32 {
    if (*m).resolved {
        return 0;
    }

    #[cfg(DUMP_MODULE_RESOLVE)]
    {
        let buf1: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];
        println!(
            "resolving module '{}':\n",
            JS_AtomGetStr(
                ctx,
                buf1.as_mut_ptr(),
                std::mem::size_of::<[u8; ATOM_GET_STR_BUF_SIZE]>(),
                (*m).module_name
            )
        );
    }

    (*m).resolved = true;

    // resolve each requested module
    for i in 0..(*m).req_module_entries_count {
        let rme = (*m).req_module_entries.add(i as usize);
        let m1 = js_host_resolve_imported_module_atom(ctx, (*m).module_name, (*rme).module_name);
        if m1.is_null() {
            return -1;
        }

        (*rme).module = m1;

        // already done in js_host_resolve_imported_module() except if
        // the module was loaded with JS_EvalBinary()
        if js_resolve_module(ctx, m1) < 0 {
            return -1;
        }
    }

    0
}

// return NULL in case of exception (e.g. module could not be loaded)
pub unsafe fn js_host_resolve_imported_module(
    ctx: *mut JSContext,
    base_cname: &str,
    cname1: &str,
) -> *mut JSModuleDef {
    let rt = (*ctx).rt.as_ref();

    let cname = if let Some(cb) = rt.module_normalize_func {
        cb(ctx, base_cname, cname1, rt.module_loader_opaque)
    } else {
        js_default_module_normalize_name(ctx, base_cname, cname1)
    };
    let cname = if let Ok(n) = cname {
        n
    } else {
        return std::ptr::null_mut();
    };

    let module_name = if let Ok(a) = (*ctx).create_atom(&cname) {
        a.into_rc((*ctx).rt())
    } else {
        return std::ptr::null_mut();
    };

    // first look at the loaded modules
    let m = js_find_loaded_module(ctx, *module_name);
    if !m.is_null() {
        return m; // already loaded
    }

    // load the module
    if let Some(cb) = rt.module_loader_func {
        let m = cb(ctx.as_mut().unwrap(), &cname, rt.module_loader_opaque);
        m
    } else {
        log::warn!("module loader function is not set, can't import module.");

        JS_ThrowReferenceError(
            ctx,
            CStr::from_ptr(
                format!("could not load module '{}'\0", cname)
                    .as_ptr()
                    .cast(),
            ),
        );
        std::ptr::null_mut()
    }
}

pub unsafe fn js_host_resolve_imported_module_atom(
    ctx: *mut JSContext,
    base_module_name: JSAtom,
    module_name1: JSAtom,
) -> *mut JSModuleDef {
    let base_cname = if let Ok(s) = base_module_name.to_utf8_string(ctx.into()) {
        s
    } else {
        return std::ptr::null_mut();
    };

    let cname = if let Ok(s) = module_name1.to_utf8_string(ctx.into()) {
        s
    } else {
        return std::ptr::null_mut();
    };

    js_host_resolve_imported_module(ctx, base_cname.as_ref(), cname.as_ref())
}

// default module filename normalizer
pub fn js_default_module_normalize_name(
    ctx: *const JSContext,
    base_name: &str,
    name: &str,
) -> Result<String, i32> {
    debug_assert!(!name.is_empty());

    let path = if let Some(base) = PathBuf::from(base_name).parent() {
        base.join(name)
    } else {
        PathBuf::from(name)
    };

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => {
                normalized.push(component.as_os_str());
            }
        }
    }

    Ok(normalized.to_string_lossy().to_string())
}

pub unsafe fn js_module_ns_autoinit(
    ctx: *mut JSContext,
    p: *mut JSObject,
    atom: JSAtom,
    opaque: *mut c_void,
) -> JSValue {
    (*opaque.cast::<JSModuleDef>())
        .ns(ctx.as_mut().unwrap())
        .unwrap_or(JSValue::EXCEPTION)
}

/* Unfortunately, the spec gives a different behavior from GetOwnProperty ! */
unsafe fn js_module_ns_has(ctx: *mut JSContext, obj: JSValueConst, atom: JSAtom) -> i32 {
    obj.object_unchecked().find_own_property1(atom).is_none() as i32
}

pub static js_module_ns_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    has_property: Some(js_module_ns_has),
    get_own_property: None,
    get_own_property_names: None,
    delete_property: None,
    define_own_property: None,
    get_property: None,
    set_property: None,
};

pub unsafe fn JS_NewModuleValue(ctx: *mut JSContext, m: *mut JSModuleDef) -> JSValue {
    JS_DupValue(ctx, JS_MKPTR!(JSTag::Module, m.cast()))
}

// must be done before js_link_module() because of cyclic references
pub unsafe fn js_create_module_function(ctx: *mut JSContext, m: *mut JSModuleDef) -> i32 {
    if (*m).func_created {
        return 0;
    }

    let is_c_module = (*m).init_func.is_some();
    if is_c_module {
        // initialize the exported variables
        for me in (*m).export_entries.iter_mut() {
            if me.export_type == JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                if let Ok(var_ref) = js_create_module_var(ctx, false) {
                    me.u.local.var_ref = Box::into_raw(var_ref);
                } else {
                    return -1;
                }
            }
        }
        // for i in 0..(*m).export_entries_count {
        //     let me = (*m).export_entries.offset(i as isize);
        //     if (*me).export_type == JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
        //         if let Ok(var_ref) = js_create_module_var(ctx, false) {
        //             (*me).u.local.var_ref = Box::into_raw(var_ref);
        //         } else {
        //             return -1;
        //         }
        //     }
        // }
    } else {
        if js_create_module_bytecode_function(ctx, m) != 0 {
            return -1;
        }
    }
    (*m).func_created = true;

    // do it on the dependencies
    for i in 0..(*m).req_module_entries_count {
        let rme = (*m).req_module_entries.offset(i as isize);
        if js_create_module_function(ctx, (*rme).module) < 0 {
            return -1;
        }
    }

    0
}

pub unsafe fn js_create_module_var(
    ctx: *mut JSContext,
    is_lexical: bool,
) -> Result<Box<JSVarRef>, i32> {
    let mut var_ref = if let Some(p) = js_malloc_typed::<JSVarRef>(ctx, 1, true) {
        Box::from_raw(p.as_ptr())
    } else {
        return Err(-1);
    };

    var_ref.u0.header.ref_count = 1;
    var_ref.set_detached(true);
    var_ref.value_or_links.value = if is_lexical {
        JSValue::UNINITIALIZED
    } else {
        JSValue::UNDEFINED
    };
    var_ref.pvalue = &mut (*var_ref).value_or_links.value;
    add_gc_object(
        (*ctx).rt_mut(),
        &mut var_ref.u0.header,
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_VAR_REF,
    );

    Ok(var_ref)
}

// Create the <eval> function associated with the module
pub unsafe fn js_create_module_bytecode_function(ctx: *mut JSContext, m: *mut JSModuleDef) -> i32 {
    let bfunc = (*m).func_obj;
    let func_obj = if let Ok(o) = (*ctx)
        .create_object_with_proto_and_classid((*ctx).function_proto, JS_CLASS_BYTECODE_FUNCTION)
    {
        o
    } else {
        return -1;
    };

    let b = JS_VALUE_GET_PTR(bfunc).cast::<JSFunctionBytecode>();
    let p = func_obj.object_unchecked();

    (*b).header.ref_count += 1;
    (*p.u.func).function_bytecode = b;
    (*p.u.func).home_object = std::ptr::null_mut();
    (*p.u.func).var_refs = Vec::new();

    if (*b).closure_var_count != 0 {
        if (*p.u.func)
            .var_refs
            .try_reserve_exact((*b).closure_var_count as usize)
            .is_err()
        {
            (*ctx).JS_ThrowOutOfMemory();
            JS_FreeValue(ctx, func_obj);
            return -1;
        };

        // create the global variables. The other variables are imported from other modules
        for cv in (*b).closure_vars_slice() {
            if cv.is_local() {
                if let Ok(vr) = js_create_module_var(ctx, (*cv).is_lexical()) {
                    #[cfg(DUMP_MODULE_RESOLVE)]
                    println!("local {}: {:p}", i, var_ref);

                    (*p.u.func)
                        .var_refs
                        .push(NonNull::new(Box::into_raw(vr)).unwrap());
                } else {
                    JS_FreeValue(ctx, func_obj);
                    return -1;
                }
            } else {
                // The other variables are imported from other modules.
                // Add slot with a placeholder.
                (*p.u.func).var_refs.push(NonNull::dangling());
            }
        }
    }

    (*m).func_obj = func_obj;
    JS_FreeValue(ctx, bfunc);

    0
}

// Prepare a module to be executed by resolving all the imported
// variables.
pub unsafe fn js_link_module(ctx: *mut JSContext, m: *mut JSModuleDef) -> i32 {
    #[cfg(DUMP_MODULE_RESOLVE)]
    {
        let buf1: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];
        println!(
            "js_link_module '{}':\n",
            JS_AtomGetStr(
                ctx,
                buf1.as_mut_ptr(),
                std::mem::size_of::<[u8; ATOM_GET_STR_BUF_SIZE]>(),
                (*m).module_name
            )
        );
    }

    debug_assert!(matches!(
        (*m).status,
        JSModuleStatus::JS_MODULE_STATUS_UNLINKED
            | JSModuleStatus::JS_MODULE_STATUS_LINKED
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
    ));

    let mut stack_top = std::ptr::null_mut::<JSModuleDef>();
    if js_inner_module_linking(ctx, m, &mut stack_top, 0) < 0 {
        while !stack_top.is_null() {
            let m1 = stack_top;
            debug_assert_eq!((*m1).status, JSModuleStatus::JS_MODULE_STATUS_LINKING);
            (*m1).status = JSModuleStatus::JS_MODULE_STATUS_UNLINKED;
            stack_top = (*m1).stack_prev;
        }
        return -1;
    }

    debug_assert!(stack_top.is_null());
    debug_assert!(matches!(
        (*m).status,
        JSModuleStatus::JS_MODULE_STATUS_LINKED
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
    ));

    0
}

// Prepare a module to be executed by resolving all the imported variables.
pub unsafe fn js_inner_module_linking(
    ctx: *mut JSContext,
    m: *mut JSModuleDef,
    pstack_top: *mut *mut JSModuleDef,
    index: i32,
) -> i32 {
    if js_check_stack_overflow((*ctx).rt(), 0) {
        JS_ThrowStackOverflow(ctx);
        return -1;
    }

    #[cfg(DUMP_MODULE_RESOLVE)]
    {
        let buf1: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];
        println!(
            "js_inner_module_linking '{}':\n",
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                JS_AtomGetStr(ctx, buf1.as_mut_ptr(), buf1.len(), (*m).module_name),
                buf1.len()
            ))
        );
    }

    if matches!(
        (*m).status,
        JSModuleStatus::JS_MODULE_STATUS_LINKING
            | JSModuleStatus::JS_MODULE_STATUS_LINKED
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
    ) {
        return index;
    }

    debug_assert!((*m).status == JSModuleStatus::JS_MODULE_STATUS_UNLINKED);
    (*m).status = JSModuleStatus::JS_MODULE_STATUS_LINKING;
    (*m).dfs_index = index;
    (*m).dfs_ancestor_index = index;

    // push 'm' on stack
    (*m).stack_prev = *pstack_top;
    *pstack_top = m;

    let mut index = index + 1;

    for i in 0..(*m).req_module_entries_count {
        let rme: *mut JSReqModuleEntry = (*m).req_module_entries.offset(i as isize);
        let m1 = (*rme).module;

        index = js_inner_module_linking(ctx, m1, pstack_top, index);
        if index < 0 {
            return -1;
        }

        debug_assert!(matches!(
            (*m1).status,
            JSModuleStatus::JS_MODULE_STATUS_LINKING
                | JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));

        if (*m1).status == JSModuleStatus::JS_MODULE_STATUS_LINKING {
            (*m).dfs_ancestor_index =
                std::cmp::min((*m).dfs_ancestor_index, (*m1).dfs_ancestor_index);
        }
    }

    #[cfg(DUMP_MODULE_RESOLVE)]
    {
        let buf1: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];
        println!(
            "instantiating module '{}':\n",
            std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                JS_AtomGetStr(ctx, buf1.as_mut_ptr(), buf1.len(), (*m).module_name),
                buf1.len()
            ))
        );
    }

    // check the indirect exports
    for me in (*m).export_entries.iter_mut() {
        if me.export_type == JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT
            && me.local_name != JS_ATOM__star_
        {
            let mut res_me: *mut JSExportEntry = std::ptr::null_mut();
            let mut res_m: *mut JSModuleDef = std::ptr::null_mut();
            let m1 = (*(*m).req_module_entries.offset(me.u.req_module_idx as isize)).module;
            let ret = js_resolve_export(ctx, &mut res_m, &mut res_me, m1, (*me).local_name);
            if ret != JSResolveResultEnum::JS_RESOLVE_RES_FOUND {
                js_resolve_export_throw_error(ctx, ret, m, me.export_name);
                return -1;
            }
        }
    }
    // for i in 0..(*m).export_entries_count {
    //     let me: *mut JSExportEntry = (*m).export_entries.offset(i as isize);
    //     if (*me).export_type == JSExportTypeEnum::JS_EXPORT_TYPE_INDIRECT
    //         && (*me).local_name != JS_ATOM__star_
    //     {
    //         let mut res_me: *mut JSExportEntry = std::ptr::null_mut();
    //         let mut res_m: *mut JSModuleDef = std::ptr::null_mut();
    //         m1 = (*(*m)
    //             .req_module_entries
    //             .offset((*me).u.req_module_idx as isize))
    //         .module;
    //         let ret = js_resolve_export(ctx, &mut res_m, &mut res_me, m1, (*me).local_name);
    //         if ret != JSResolveResultEnum::JS_RESOLVE_RES_FOUND {
    //             js_resolve_export_throw_error(ctx, ret, m, (*me).export_name);
    //             return -1;
    //         }
    //     }
    // }

    #[cfg(DUMP_MODULE_RESOLVE)]
    {
        println!("exported bindings:\n");
        i = 0;
        while i < (*m).export_entries_count {
            let me: *mut JSExportEntry = (*m).export_entries.offset(i as isize);
            print!(" name=");
            print_atom(ctx, (*me).export_name);
            print!(" local=");
            print_atom(ctx, (*me).local_name);
            println!(" type={} idx={}", (*me).export_type, (*me).u.local.var_idx);
            i += 1;
        }
    }

    let is_c_module = (*m).init_func.is_some();
    if !is_c_module {
        let p = JS_VALUE_GET_OBJ((*m).func_obj);
        let var_refs = (*(*p).u.func).var_refs.as_mut_slice();

        for i in 0..(*m).import_entries_count {
            let mi = (*m).import_entries.offset(i as isize);

            #[cfg(DUMP_MODULE_RESOLVE)]
            {
                print!("import var_idx={} name=", (*mi).var_idx);
                print_atom(ctx, (*mi).import_name);
                print!(": ");
            }

            let m1 = (*(*m)
                .req_module_entries
                .offset((*mi).req_module_idx as isize))
            .module;
            if (*mi).import_name == JS_ATOM__star_ {
                // name space import
                let val = if let Ok(o) = (*m1).ns(ctx.as_mut().unwrap()) {
                    o
                } else {
                    return -1;
                };

                set_value(
                    ctx,
                    &mut var_refs[(*mi).var_idx as usize]
                        .as_mut()
                        .value_or_links
                        .value,
                    val,
                );

                #[cfg(DUMP_MODULE_RESOLVE)]
                {
                    println!("namespace");
                }
            } else {
                let mut res_me: *mut JSExportEntry = std::ptr::null_mut();
                let mut res_m: *mut JSModuleDef = std::ptr::null_mut();
                let ret = js_resolve_export(ctx, &mut res_m, &mut res_me, m1, (*mi).import_name);
                if ret != JSResolveResultEnum::JS_RESOLVE_RES_FOUND {
                    js_resolve_export_throw_error(ctx, ret, m1, (*mi).import_name);
                    return -1;
                }

                if (*res_me).local_name == JS_ATOM__star_ {
                    // name space import from
                    let m2 = (*(*res_m)
                        .req_module_entries
                        .offset((*res_me).u.req_module_idx as isize))
                    .module;

                    let val = if let Ok(o) = (*m2).ns(ctx.as_mut().unwrap()) {
                        o
                    } else {
                        return -1;
                    };

                    if let Ok(mut var_ref) = js_create_module_var(ctx, true) {
                        set_value(ctx, &mut var_ref.value_or_links.value, val);
                        var_refs[(*mi).var_idx as usize] =
                            NonNull::new(Box::into_raw(var_ref)).unwrap();

                        #[cfg(DUMP_MODULE_RESOLVE)]
                        {
                            println!("namespace from");
                        }
                    } else {
                        JS_FreeValue(ctx, val);
                        return -1;
                    }
                } else {
                    let mut vr = (*res_me).u.local.var_ref;
                    if vr.is_null() {
                        let p1 = JS_VALUE_GET_OBJ((*res_m).func_obj);
                        vr = (*p1).u.func.var_refs[(*res_me).u.local.var_idx as usize].as_ptr();
                    }
                    (*vr).u0.header.ref_count += 1;
                    var_refs[(*mi).var_idx as usize] = NonNull::new(vr).unwrap();

                    #[cfg(DUMP_MODULE_RESOLVE)]
                    {
                        println!("local export (var_ref={:?})", var_ref);
                    }
                }
            }
        }

        // keep the exported variables in the module export entries (they are used when the eval function is deleted and cannot be initialized before in case imports are exported)
        for me in (*m).export_entries.iter_mut() {
            if me.export_type == JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                let mut var_ref = var_refs[me.u.local.var_idx as usize];
                var_ref.as_mut().u0.header.ref_count += 1;
                me.u.local.var_ref = var_ref.as_ptr();
            }
        }
        // for i in 0..(*m).export_entries_count {
        //     let me: *mut JSExportEntry = (*m).export_entries.offset(i as isize);
        //     if (*me).export_type == JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
        //         let mut var_ref = var_refs[(*me).u.local.var_idx as usize];
        //         var_ref.as_mut().u0.header.ref_count += 1;
        //         (*me).u.local.var_ref = var_ref.as_ptr();
        //     }
        // }

        // initialize the global variables
        let ret_val = JS_Call(
            ctx.as_mut().unwrap(),
            (*m).func_obj,
            JSValue::TRUE,
            0,
            std::ptr::null_mut(),
        );
        if JS_IsException(ret_val) {
            return -1;
        }
        JS_FreeValue(ctx, ret_val);
    }

    debug_assert!((*m).dfs_ancestor_index <= (*m).dfs_index);
    if (*m).dfs_index == (*m).dfs_ancestor_index {
        loop {
            // pop m1 from stack
            let m1 = *pstack_top;
            *pstack_top = (*m1).stack_prev;
            (*m1).status = JSModuleStatus::JS_MODULE_STATUS_LINKED;
            if m1 == m {
                break;
            }
        }
    }

    #[cfg(DUMP_MODULE_RESOLVE)]
    {
        println!("js_inner_module_linking done");
    }

    index
}

// return JS_ATOM_NULL if the name cannot be found. Only works with
// not striped bytecode functions.
pub unsafe fn JS_GetScriptOrModuleName(ctx: *mut JSContext, n_stack_levels: i32) -> JSAtom {
    // XXX: currently we just use the filename of the englobing
    // function from the debug info. May need to add a ScriptOrModule
    // info in JSFunctionBytecode.
    let mut sf = (*ctx).rt().current_stack_frame;
    if sf.is_null() {
        return JS_ATOM_NULL;
    }
    let mut levels = n_stack_levels;
    while levels > 0 {
        sf = (*sf).prev_frame;
        if sf.is_null() {
            return JS_ATOM_NULL;
        }
        levels -= 1;
    }

    loop {
        if JS_VALUE_GET_TAG((*sf).cur_func) != JS_TAG_OBJECT {
            return JS_ATOM_NULL;
        }

        let p = JS_VALUE_GET_OBJ((*sf).cur_func);
        if !js_class_has_bytecode((*p).class_id()) {
            return JS_ATOM_NULL;
        }

        let b = (*p).u.func.function_bytecode;
        if !(*b).is_direct_or_indirect_eval {
            if !(*b).has_debug {
                return JS_ATOM_NULL;
            }
            return JS_DupAtom(ctx, (*b).debug.filename);
        } else {
            sf = (*sf).prev_frame;
            if sf.is_null() {
                return JS_ATOM_NULL;
            }
        }
    }
}

#[deprecated(note = "use JSModuleDef::import_meta() instead")]
pub unsafe fn JS_GetImportMeta(ctx: *mut JSContext, m: *mut JSModuleDef) -> JSValue {
    (*m).import_meta(ctx.as_mut().unwrap())
        .unwrap_or(JSValue::EXCEPTION)
}

/// get module import.meta object of context current execution.
pub unsafe fn js_import_meta(ctx: *mut JSContext) -> JSValue {
    let filename = JS_GetScriptOrModuleName(ctx, 0);
    if filename == JS_ATOM_NULL {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"import.meta not supported in this context ");
        return JSValue::EXCEPTION;
    }

    // XXX: inefficient, need to add a module or script pointer in
    // JSFunctionBytecode
    let m = js_find_loaded_module(ctx, filename);
    JS_FreeAtom(ctx, filename);
    if m.is_null() {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"import.meta not supported in this context");
        JSValue::EXCEPTION
    } else {
        (*m).import_meta(ctx.as_mut().unwrap())
            .unwrap_or(JSValue::EXCEPTION)
    }
}

pub unsafe fn JS_SetModuleLoaderFunc(
    rt: *mut JSRuntime,
    module_normalize: Option<JSModuleNormalizeFunc>,
    module_loader: JSModuleLoaderFunc,
    opaque: *mut std::ffi::c_void,
) {
    (*rt).module_normalize_func = module_normalize;
    (*rt).module_loader_func = Some(module_loader);
    (*rt).module_loader_opaque = opaque;
}

pub unsafe fn js_dynamic_import(ctx: *mut JSContext, specifier: JSValueConst) -> JSValue {
    unimplemented!()
}
