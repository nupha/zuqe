// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use libloading::Library;
use zuqe_zapi::ffi::{
    ZQ_ABI_VERSION, ZQ_API_VERSION, ZQ_CONST_VALUE_SLOT_EXCEPTION, ZQ_FIELD_BOOL, ZQ_FIELD_CLASS,
    ZQ_FIELD_FLOAT, ZQ_FIELD_FUNC, ZQ_FIELD_GETSET, ZQ_FIELD_INT, ZQ_FIELD_STR, ZQ_FIELD_UNDEFINED,
    ZQ_FIELD_VALUE, ZqApi, ZqCallInfoS, ZqFieldDef, ZqModuleInfo, ZqRawCtx, ZqRawValue,
};

use crate::{JSValue, ModuleDef, ScopedContext, class::ClassID, function::Callback};

use super::stub::{ZQ_API_TABLE, zq_to_js_prop_flags};

/// Translate a `ZqFieldDef` array into engine `FieldDef` entries (methods,
/// accessors, and constant data props), for class prototypes/constructors.
pub(crate) fn push_class_fields(
    ctx: &mut ScopedContext,
    entries: &mut Vec<crate::FieldDef>,
    fields: &[ZqFieldDef],
) -> Result<(), i32> {
    for f in fields {
        let name = f
            .name_str()
            .ok_or_else(|| ctx.throw_internal_error("zapi class member has no name"))?;
        let name: &'static str = Box::leak(name.to_string().into_boxed_str());
        // Attribute bits come from the FFI; translate them bit-by-bit so
        // `ZQ_PROP_*` values stay decoupled from engine `JS_PROP_*` bits.
        let prop_flags = zq_to_js_prop_flags(f.prop_flags);
        match f.kind {
            ZQ_FIELD_FUNC => {
                let Some(body) = f.body else {
                    continue;
                };
                let (_, token) = register_zapi_func_magic(ctx, body, f.magic);
                entries.push(crate::FieldDef::FUNC(
                    name,
                    f.arity,
                    zapi_trampoline_fn,
                    token as i16,
                ));
            }
            ZQ_FIELD_GETSET => {
                let magic = if let Some(body) = f.get.or(f.set) {
                    let (_, token) = register_zapi_func_magic(ctx, body, f.magic);
                    token as i16
                } else {
                    continue;
                };
                entries.push(crate::FieldDef::GETSET(
                    name,
                    f.get
                        .map(|_| zapi_trampoline_fn as crate::function::GetterFn),
                    f.set
                        .map(|_| zapi_trampoline_fn as crate::function::SetterFn),
                    magic,
                ));
            }
            ZQ_FIELD_STR => {
                let s = if f.str_val.is_null() {
                    ""
                } else {
                    unsafe { std::ffi::CStr::from_ptr(f.str_val) }
                        .to_str()
                        .map_err(|_| -1)?
                };
                let s: &'static str = Box::leak(s.to_string().into_boxed_str());
                entries.push(crate::FieldDef::STR(name, s, prop_flags));
            }
            ZQ_FIELD_INT => {
                entries.push(crate::FieldDef::INT32(name, f.i64_val as i32, prop_flags));
            }
            ZQ_FIELD_FLOAT => {
                entries.push(crate::FieldDef::FLOAT64(name, f.f64_val, prop_flags));
            }
            ZQ_FIELD_BOOL => {
                entries.push(crate::FieldDef::BOOL(name, f.u8_val != 0, prop_flags));
            }
            ZQ_FIELD_UNDEFINED => {
                entries.push(crate::FieldDef::UNDEFINED(name, prop_flags));
            }
            _ => {
                return Err(ctx.throw_internal_error(&format!(
                    "zapi class member '{name}': unsupported kind {}",
                    f.kind
                )));
            }
        }
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
    let serial = cc.magic();
    let (body, user_magic) = match ctx
        .rt()
        .zapi_body_registry
        .lock()
        .unwrap()
        .get(&serial)
        .copied()
    {
        Some((b, m)) => (b, m),
        None => return Err(-1),
    };

    let raw_args: Vec<ZqRawValue> = cc.arguments().iter().map(|v| v.0).collect();
    let mut call_info = ZqCallInfoS {
        target: 0,
        this: cc.this().0,
        args: raw_args.as_ptr(),
        argc: raw_args.len(),
        magic: user_magic,
    };

    let env = ctx as *mut ScopedContext as ZqRawCtx;
    let result = unsafe { body(env, &mut call_info as *mut _) };
    if result == ZQ_API_TABLE.const_values[ZQ_CONST_VALUE_SLOT_EXCEPTION] {
        Err(-1)
    } else {
        Ok(crate::JSValue(result, std::marker::PhantomData))
    }
}

/// 同 `register_zapi_func`，但显式指定用户 magic。
pub(crate) fn register_zapi_func_magic(
    ctx: &mut ScopedContext,
    body: unsafe extern "C" fn(env: ZqRawCtx, info: *mut ZqCallInfoS) -> ZqRawValue,
    magic: i32,
) -> (Callback, i32) {
    let rt = ctx.rt_mut();
    let token = rt
        .zapi_next_serial
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
        use $crate::{JSResult, JSValue, ScopedContext, function::CallContext};

        fn trampoline(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
            let mut env = $crate::zapi::host::env_from_scoped(ctx);
            let raw_args: Vec<u64> = cc.arguments().iter().map(|v| v.0).collect();
            let mut info = zuqe_zapi::ffi::ZqCallInfoS {
                target: 0,
                this: cc.this().0,
                args: raw_args.as_ptr(),
                argc: cc.length(),
                magic: 0,
            };
            let result = unsafe { ($body)(&mut env, &mut info) };
            if result
                == $crate::zapi::stub::ZQ_API_TABLE.const_values
                    [zuqe_zapi::ffi::ZQ_CONST_VALUE_SLOT_EXCEPTION]
            {
                Err(-1)
            } else {
                Ok(JSValue(result, std::marker::PhantomData))
            }
        }
        $crate::function::Callback::Func(trampoline)
    }};
}

impl<'a> ScopedContext<'a> {
    /// Load a zapi extension cdylib and register its exports as a native module.
    ///
    /// `path` is the filesystem path to the `.dylib`/`.so`/`.dll` file.
    ///
    /// Steps:
    /// 1. dlopen the library, resolve `zq_module_register`
    /// 2. Lazy-init Runtime's `ZqApi` table (created once, shared across extensions)
    /// 3. Call `zq_module_register` → extension fills `ZqModuleInfo` (exports + init)
    /// 4. Register classes declared as `ZQ_FIELD_CLASS` (ctors live in `class_slots`)
    /// 5. Return a `ModuleDef` whose exports (classes + functions + values) are
    ///    published to the module namespace at evaluation time, after the optional
    ///    module init hook runs, matching standard ESM semantics
    ///    (`import { X } from '...'` / `import * as m from '...'`).
    pub fn load_zapi_extension(&mut self, path: &std::path::Path) -> Result<ModuleDef, i32> {
        type ZqModuleRegisterFn = unsafe extern "C" fn(api: *mut ZqApi, info: *mut ZqModuleInfo);

        // Resolve absolute path, then try dlopen.
        let abs_path = path.canonicalize().map_err(|e| {
            self.throw_reference_error(&format!(
                "could not resolve extension path '{}': {e}",
                path.display()
            ))
        })?;

        let lib = unsafe { Library::new(&abs_path) }.map_err(|e| {
            self.throw_reference_error(&format!(
                "could not load library '{}': {e}",
                abs_path.display()
            ))
        })?;

        let module_register_fn: libloading::Symbol<ZqModuleRegisterFn> =
            unsafe { lib.get(c"zq_module_register") }.map_err(|_| {
                self.throw_reference_error(&format!(
                    "could not find zq_module_register in '{}'",
                    abs_path.display()
                ))
            })?;

        // Only build the URL after the library has loaded successfully.
        let file_url = crate::path_to_url(abs_path.to_str().unwrap_or("")).map_err(|_| {
            self.throw_reference_error(&format!(
                "could not convert path to URL: {}",
                abs_path.display()
            ))
        })?;

        let api: &ZqApi = &ZQ_API_TABLE;
        let api_ptr = api as *const ZqApi as *mut ZqApi;

        let mut md_info = ZqModuleInfo::default();

        // Hand the extension its own canonical load path before registration
        // (dlopen-filename pattern): extensions locate sibling
        // resources relative to it via `zuqe_zapi::module_path()`.
        //
        // Ownership: intentionally leaked — never freed by the process. The
        // extension stores this pointer in its own static (`module_path()`
        // promises `&'static str`) and may read it at any later point, even
        // during teardown, so freeing it here would be a use-after-free.
        // Bounded: one allocation per loaded module (cache-deduped).
        // Reclaimed wholesale by the OS at process exit, like the dylib
        // mapping kept alive by `mem::forget` below.
        let path_c = match std::ffi::CString::new(abs_path.to_string_lossy().as_ref()) {
            Ok(s) => s,
            Err(e) => {
                return Err(self.throw_internal_error(&format!(
                    "invalid extension path '{}': {e}",
                    abs_path.display()
                )));
            }
        };
        md_info.path = Box::leak(Box::new(path_c)).as_ptr();

        unsafe { module_register_fn(api_ptr, &mut md_info) };

        // Validate ABI compatibility: the extension's binary layout must match
        // the host's layout exactly. Mismatch means struct fields or function
        // signatures differ — using them would be UB.
        let ext_abi = md_info.abi_version();
        if ext_abi != ZQ_ABI_VERSION {
            return Err(self.throw_reference_error(&format!(
                "zapi module '{}': ABI version mismatch (host={}, ext={})",
                file_url, ZQ_ABI_VERSION, ext_abi
            )));
        }
        // Validate API capability: the host must provide at least the API tier
        // the extension requires. Lower host API means missing functions.
        let ext_api = md_info.api_version();
        if ext_api > ZQ_API_VERSION {
            return Err(self.throw_reference_error(&format!(
                "zapi module '{}': API version too new (host={}, ext={})",
                file_url, ZQ_API_VERSION, ext_api
            )));
        }

        // Pre-pass: snapshot the exports and register every CLASS field (host
        // needs the class registered before module evaluation). The `ZqFieldDef`
        // array is leaked by the extension, so names/pointers stay valid for the
        // process lifetime; the library is kept loaded via `mem::forget` below.
        let register_class = api.register_class;
        let fields: Vec<ZqFieldDef> = md_info.exports().to_vec();
        let mut class_ids: Vec<ClassID> = Vec::new();
        let mut export_names: Vec<String> = Vec::new();

        for def in &fields {
            let name = def
                .name_str()
                .ok_or_else(|| self.throw_type_error("invalid export name in zapi module"))?;
            if def.kind == ZQ_FIELD_CLASS {
                if def.class_def.is_null() {
                    return Err(self.throw_reference_error(&format!(
                        "zapi module '{file_url}': class export '{name}' has no class def"
                    )));
                }
                let env = self as *mut ScopedContext as ZqRawCtx;
                let r = unsafe { register_class(env, def.class_def) };
                if r < 0 {
                    return Err(self.throw_reference_error(&format!(
                        "zapi module '{file_url}': failed to register class '{name}'"
                    )));
                }
                class_ids.push(r as ClassID);
            }
            if !export_names.iter().any(|n| n == name) {
                export_names.push(name.to_string());
            }
        }

        // Inject `__file__` — the canonical load path as a plain string —
        // into the module namespace (Python `__file__` semantics) unless the
        // extension exports its own (author wins). Done host-side so prebuilt
        // extensions get it without recompiling.
        let inject_file = !export_names.iter().any(|n| n == "__file__");
        if inject_file {
            export_names.push("__file__".to_string());
        }
        let module_path_str = abs_path.to_string_lossy().into_owned();
        let module_path_clone = module_path_str.clone();

        // Keep the library alive.
        std::mem::forget(lib);

        let init_ptr = md_info.init;

        // Module init runs at evaluation time, after the export VarRefs exist:
        // run the extension's init hook first (module-load setup), then publish
        // each field (function / class ctor / value / constant) to the module
        // namespace instead of `globalThis`.
        let init_fn = move |ctx: &mut ScopedContext, md: &ModuleDef| -> Result<(), i32> {
            if let Some(init_f) = init_ptr {
                let env = ctx as *mut ScopedContext as ZqRawCtx;
                if unsafe { init_f(env) } != 0 {
                    return Err(ctx.throw_internal_error("zapi module init failed"));
                }
            }

            let mut class_iter = class_ids.iter();
            for def in &fields {
                let name = def
                    .name_str()
                    .ok_or_else(|| ctx.throw_internal_error("zapi module export has no name"))?;
                let class_id = if def.kind == ZQ_FIELD_CLASS {
                    class_iter.next().copied()
                } else {
                    None
                };
                let value = eval_field_value(ctx, def, class_id)?;
                set_module_export(ctx, md, name, value)?;
            }

            if inject_file {
                // export `__file__` field
                let sr = ctx
                    .create_string_from_utf8(module_path_clone.as_bytes().to_vec())
                    .map_err(|_| -1)?;
                set_module_export(ctx, md, "__file__", crate::JSValue::make_string(sr))?;
            }

            Ok(())
        };

        // Register under the canonical `file://` URL of the dylib (not the
        // extension-declared module name): module resolution keys `loaded_modules`
        // by the canonicalized URL, and dlopen is file-based — one file loaded once.
        ModuleDef::create(
            self,
            file_url.as_str(),
            export_names.iter().map(String::as_str),
            init_fn,
        )
    }
}

/// Evaluate a `ZqFieldDef` to a JS value for a module-namespace export.
/// `class_id` is the registered `ClassID` for `ZQ_FIELD_CLASS` fields.
fn eval_field_value(
    ctx: &mut ScopedContext,
    def: &ZqFieldDef,
    class_id: Option<ClassID>,
) -> Result<JSValue, i32> {
    match def.kind {
        ZQ_FIELD_FUNC => {
            let body = def.body.ok_or(-1)?;
            let (cb, token) = register_zapi_func_magic(ctx, body, def.magic);
            let name = def.name_str().unwrap_or("");
            let fn_obj = ctx.create_native_function(cb, name, def.arity, token as i16)?;
            Ok(fn_obj.into())
        }
        ZQ_FIELD_CLASS => {
            let cid = class_id.ok_or(-1)?;
            let Some(ctor) = ctx.class_slots[cid as usize].ctor else {
                return Err(ctx.throw_internal_error("zapi class export has no constructor"));
            };
            Ok(ctor.into())
        }
        ZQ_FIELD_VALUE => {
            let value_fn = def.value_fn.ok_or(-1)?;
            let env = ctx as *mut ScopedContext as ZqRawCtx;
            let mut out: ZqRawValue = 0;
            if unsafe { value_fn(env, &mut out) } != 0 {
                return Err(ctx.throw_internal_error(&format!(
                    "zapi module export '{}' failed",
                    def.name_str().unwrap_or("<unnamed>")
                )));
            }
            Ok(JSValue(out, std::marker::PhantomData))
        }
        ZQ_FIELD_STR => {
            let bytes = if def.str_val.is_null() {
                Vec::new()
            } else {
                unsafe { std::ffi::CStr::from_ptr(def.str_val) }
                    .to_bytes()
                    .to_vec()
            };
            let sr = ctx.create_string_from_utf8(bytes).map_err(|_| -1)?;
            Ok(crate::JSValue::make_string(sr))
        }
        ZQ_FIELD_INT => Ok(crate::JSValue::from(def.i64_val as i32)),
        ZQ_FIELD_FLOAT => Ok(crate::JSValue::from(def.f64_val)),
        ZQ_FIELD_BOOL => {
            if def.u8_val != 0 {
                Ok(crate::JSValue::TRUE)
            } else {
                Ok(crate::JSValue::FALSE)
            }
        }
        ZQ_FIELD_UNDEFINED => Ok(crate::JSValue::UNDEFINED),
        _ => Err(ctx.throw_internal_error(&format!(
            "zapi module export '{}': unsupported kind {}",
            def.name_str().unwrap_or("<unnamed>"),
            def.kind
        ))),
    }
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
