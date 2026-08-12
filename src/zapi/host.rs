// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use libloading::Library;
use zapi::ffi::{
    ZQ_EXPORT_CLASS, ZQ_EXPORT_FUNC, ZQ_EXPORT_VALUE, ZapiBodyFn, ZqApi, ZqCallbackInfoS,
    ZqClassAccessor, ZqEnv, ZqEnvS, ZqExportValueFn, ZqFuncInfo, ZqModuleInfo, ZqRawValue,
};

use crate::{JSValue, ModuleDef, ScopedContext, class::ClassID, function::Callback};

use super::stub::{ZQ_API_TABLE, env_from_ctx};

pub(crate) fn push_method_fields(
    ctx: &mut ScopedContext,
    entries: &mut Vec<crate::FieldDef>,
    methods: &[ZqFuncInfo],
) -> Result<(), i32> {
    for m in methods {
        let Some(body) = m.body else {
            continue;
        };
        let name = unsafe { std::ffi::CStr::from_ptr(m.name) };
        let name = name.to_str().map_err(|_| -1)?;
        let name: &'static str = Box::leak(name.to_string().into_boxed_str());
        let (_, token) = register_zapi_func_magic(ctx, body, m.magic);
        entries.push(crate::FieldDef::FUNC(
            name,
            m.arity,
            zapi_trampoline_fn,
            token as i16,
        ));
    }
    Ok(())
}

pub(crate) fn push_accessor_fields(
    ctx: &mut ScopedContext,
    entries: &mut Vec<crate::FieldDef>,
    accessors: &[ZqClassAccessor],
) -> Result<(), i32> {
    for a in accessors {
        let name = unsafe { std::ffi::CStr::from_ptr(a.name) };
        let name = name.to_str().map_err(|_| -1)?;
        let name: &'static str = Box::leak(name.to_string().into_boxed_str());
        // A get+set accessor shares one body (one magic); register whichever is present.
        let magic = if let Some(body) = a.get.or(a.set) {
            let (_, token) = register_zapi_func_magic(ctx, body, a.magic);
            token as i16
        } else {
            continue;
        };
        entries.push(crate::FieldDef::GETSET(
            name,
            a.get
                .map(|_| zapi_trampoline_fn as crate::function::GetterFn),
            a.set
                .map(|_| zapi_trampoline_fn as crate::function::SetterFn),
            magic,
        ));
    }
    Ok(())
}

// ──────────────────────────────────────
// Thunk
// ──────────────────────────────────────

/// 通用 zapi 跳板函数：从 `cc.magic()` 读取 token，在 RuntimeImpl 的
/// `zapi_body_registry` 中查找对应的扩展 body 及其用户 magic。
pub(crate) fn zapi_trampoline_fn(
    ctx: &mut ScopedContext,
    cc: crate::function::CallContext,
) -> crate::JSResult {
    let token = cc.magic();
    let (body, user_magic) = match ctx
        .rt()
        .zapi_body_registry
        .lock()
        .unwrap()
        .get(&token)
        .copied()
    {
        Some((b, m)) => (b, m),
        None => return Err(-1),
    };

    let mut env = unsafe { env_from_ctx(ctx) };

    // Pass arguments as proper engine JSValues (numbers included) so the
    // extension can inspect them with `type_of` / `number_value` etc. An
    // extension that needs a numeric view converts with `api.number_value`.
    let raw_args: Vec<ZqRawValue> = cc.arguments().iter().map(|v| v.0).collect();

    let mut info = ZqCallbackInfoS {
        target: 0,
        this: cc.this().0,
        args: raw_args.as_ptr(),
        argc: cc.length() as u32,
        ret: 0,
        threw: 0,
        magic: user_magic,
    };

    unsafe { body(&mut env, &mut info) };

    if info.threw != 0 {
        Err(-1)
    } else {
        Ok(crate::JSValue(info.ret, std::marker::PhantomData))
    }
}

/// 将 zapi 扩展的 `body` FFI 入口注册到当前 Runtime 的跳板，并返回
/// `(Callback, token)`。token 由 host 内部递增分配（与用户 magic 无关）；
/// `magic` 只是透传给扩展的 `ZqCallInfo::magic()` 的分发值（默认 0）。
pub fn register_zapi_func(
    ctx: &mut ScopedContext,
    body: unsafe extern "C" fn(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue,
) -> (Callback, i32) {
    register_zapi_func_magic(ctx, body, 0)
}

/// 同 `register_zapi_func`，但显式指定用户 magic。
pub(crate) fn register_zapi_func_magic(
    ctx: &mut ScopedContext,
    body: unsafe extern "C" fn(env: ZqEnv, info: *mut ZqCallbackInfoS) -> ZqRawValue,
    magic: i32,
) -> (Callback, i32) {
    let rt = ctx.rt_mut();
    let token = rt
        .zapi_next_token
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    rt.zapi_body_registry
        .lock()
        .unwrap()
        .insert(token, (body, magic));
    (Callback::Func(zapi_trampoline_fn), token)
}

/// 将 zapi 扩展的 body FFI 入口包装为 zuqe 引擎的 `Callback`。
///
/// 因为 `CallbackFn = fn(...)` 是函数指针（无法捕获环境），该宏为每次调用
/// 生成一个独立的跳板函数，将 `body` 编译时内联到函数体中。
///
/// 使用方式：
/// ```ignore
/// let cb = zapi_make_thunk!(my_body);
/// ```
#[macro_export]
macro_rules! zapi_make_thunk {
    ($body:expr) => {{
        use zapi::ffi::ZqCallbackInfoS;
        use $crate::{JSResult, JSValue, ScopedContext, function::CallContext};

        fn trampoline(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
            let mut env = $crate::zapi::host::env_from_scoped(ctx);
            let raw_args: Vec<u64> = cc.arguments().iter().map(|v| v.0).collect();
            let mut info = ZqCallbackInfoS {
                target: 0,
                this: cc.this().0,
                args: raw_args.as_ptr(),
                argc: cc.length() as u32,
                ret: 0,
                threw: 0,
                magic: 0,
            };
            let _result_raw = unsafe { ($body)(&mut env, &mut info) };
            if info.threw != 0 {
                Err(-1)
            } else {
                Ok(JSValue(info.ret, std::marker::PhantomData))
            }
        }
        $crate::function::Callback::Func(trampoline)
    }};
}

// ──────────────────────────────────────
// 扩展加载
// ──────────────────────────────────────

/// Load a zapi extension cdylib and register its exports as a native module.
///
/// `path` is the filesystem path to the `.dylib`/`.so`/`.dll` file.
///
/// Steps:
/// 1. dlopen the library, resolve `zq_module_register`
/// 2. Lazy-init Runtime's `ZqApi` table (created once, shared across extensions)
/// 3. Call `zq_module_register` → extension fills `ZqModuleInfo` (exports + init)
/// 4. Register classes declared as `ZQ_EXPORT_CLASS` (ctors live in `class_slots`)
/// 5. Return a `ModuleDef` whose exports (classes + functions + values) are
///    published to the module namespace at evaluation time, after the optional
///    module init hook runs, matching standard ESM semantics
///    (`import { X } from '...'` / `import * as m from '...'`).
pub fn load_extension(ctx: &mut ScopedContext, path: &std::path::Path) -> Result<ModuleDef, i32> {
    type ZqModuleRegisterFn = unsafe extern "C" fn(api: *mut ZqApi, info: *mut ZqModuleInfo);

    // Resolve absolute path, then try dlopen.
    let abs_path = path.canonicalize().map_err(|e| {
        ctx.throw_reference_error(&format!(
            "could not resolve extension path '{}': {e}",
            path.display()
        ))
    })?;

    let lib = unsafe { Library::new(&abs_path) }.map_err(|e| {
        ctx.throw_reference_error(&format!(
            "could not load library '{}': {e}",
            abs_path.display()
        ))
    })?;

    let module_register_fn: libloading::Symbol<ZqModuleRegisterFn> =
        unsafe { lib.get(c"zq_module_register") }.map_err(|_| {
            ctx.throw_reference_error(&format!(
                "could not find zq_module_register in '{}'",
                abs_path.display()
            ))
        })?;

    // Only build the URL after the library has loaded successfully.
    let file_url = crate::path_to_url(abs_path.to_str().unwrap_or("")).map_err(|_| {
        ctx.throw_reference_error(&format!(
            "could not convert path to URL: {}",
            abs_path.display()
        ))
    })?;

    let api: &ZqApi = &ZQ_API_TABLE;
    let api_ptr = api as *const ZqApi as *mut ZqApi;

    let mut md_info = ZqModuleInfo::default();
    unsafe { module_register_fn(api_ptr, &mut md_info) };

    // Split the exports into per-kind owned lists (the `ZqExportDef` array is
    // leaked by the extension, so function pointers and names stay valid for the
    // process lifetime; the library is kept loaded via `mem::forget` below).
    let register_class = api.register_class;
    let mut class_exports: Vec<(String, ClassID)> = Vec::new();
    let mut func_exports: Vec<(String, ZapiBodyFn, u8, i32)> = Vec::new();
    let mut value_exports: Vec<(String, ZqExportValueFn)> = Vec::new();

    for def in md_info.exports() {
        let name = def
            .name_str()
            .ok_or_else(|| ctx.throw_type_error("invalid export name in zapi module"))?;
        match def.kind {
            ZQ_EXPORT_CLASS => {
                if def.class_def.is_null() {
                    return Err(ctx.throw_reference_error(&format!(
                        "zapi module '{file_url}': class export '{name}' has no class def"
                    )));
                }
                let mut env = ZqEnvS {
                    ctx: (ctx as *mut ScopedContext).cast::<c_void>(),
                    trace: std::ptr::null_mut(),
                    rt: std::ptr::null_mut(),
                };
                let r = unsafe { register_class(&mut env, def.class_def) };
                if r < 0 {
                    return Err(ctx.throw_reference_error(&format!(
                        "zapi module '{file_url}': failed to register class '{name}'"
                    )));
                }
                class_exports.push((name.to_string(), r as ClassID));
            }
            ZQ_EXPORT_FUNC => {
                let Some(body) = def.body else { continue };
                func_exports.push((name.to_string(), body, def.arity, def.magic));
            }
            ZQ_EXPORT_VALUE => {
                let Some(value_fn) = def.value_fn else {
                    continue;
                };
                value_exports.push((name.to_string(), value_fn));
            }
            _ => {}
        }
    }

    let mut export_names: Vec<String> = Vec::new();
    for (name, _) in &class_exports {
        export_names.push(name.clone());
    }
    for (name, _, _, _) in &func_exports {
        if !export_names.iter().any(|n| n == name) {
            export_names.push(name.clone());
        }
    }
    for (name, _) in &value_exports {
        if !export_names.iter().any(|n| n == name) {
            export_names.push(name.clone());
        }
    }

    // Keep the library alive.
    std::mem::forget(lib);

    let init_ptr = md_info.init;

    // Module init runs at evaluation time, after the export VarRefs exist:
    // run the extension's init hook first (module-load setup), then publish each
    // class constructor / function / value to the module namespace instead of
    // `globalThis`.
    let init_fn = move |ctx: &mut ScopedContext, md: &ModuleDef| -> Result<(), i32> {
        if let Some(init_f) = init_ptr {
            let mut env = unsafe { env_from_ctx(ctx) };
            if unsafe { init_f(&mut env) } != 0 {
                return Err(ctx.throw_internal_error("zapi module init failed"));
            }
        }
        for (name, class_id) in &class_exports {
            let Some(ctor) = ctx.class_slots[*class_id as usize].ctor else {
                continue; // class without a constructor body has no ctor
            };
            set_module_export(ctx, md, name, ctor.into())?;
        }
        for (name, body, arity, magic) in &func_exports {
            let (cb, token) = register_zapi_func_magic(ctx, *body, *magic);
            let fn_obj = ctx.create_native_function(cb, name, *arity, token as i16)?;
            set_module_export(ctx, md, name, fn_obj.into())?;
        }
        for (name, value_fn) in &value_exports {
            let mut env = unsafe { env_from_ctx(ctx) };
            let mut out: ZqRawValue = 0;
            if unsafe { value_fn(&mut env, &mut out) } != 0 {
                return Err(
                    ctx.throw_internal_error(&format!("zapi module export '{name}' failed"))
                );
            }
            set_module_export(ctx, md, name, crate::JSValue(out, std::marker::PhantomData))?;
        }
        Ok(())
    };

    // Register under the canonical `file://` URL of the dylib (not the
    // extension-declared module name): module resolution keys `loaded_modules`
    // by the canonicalized URL, and dlopen is file-based — one file loaded once.
    ModuleDef::create(
        ctx,
        file_url.as_str(),
        export_names.iter().map(String::as_str),
        init_fn,
    )
}

/// Set one module export binding to `value` during native module init.
fn set_module_export(
    ctx: &mut ScopedContext,
    md: &ModuleDef,
    name: &str,
    value: JSValue,
) -> Result<(), i32> {
    let atom = crate::function::prop_str_to_atom(ctx, name)?;
    let Some(me) = md.find_export(atom) else {
        return Err(
            ctx.throw_internal_error(&format!("zapi module: export '{name}' is not declared"))
        );
    };
    let crate::module::ExportType::JS_EXPORT_TYPE_LOCAL {
        var_ref: Some(mut vr),
        ..
    } = me.export_type
    else {
        return Err(ctx.throw_internal_error(&format!(
            "zapi module: export '{name}' has no local binding"
        )));
    };
    vr.set_value(&mut ctx.rt_ptr(), value);
    Ok(())
}
