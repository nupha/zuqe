// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    Callback,
    atom::Atom,
    builtins::promise::{JobGcTrace5, Promise},
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, NativeFunction},
    module::{
        JSFreeModuleEnum, get_script_or_module_name, js_free_modules,
        js_host_resolve_imported_module,
    },
    object::{JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JS_PROP_C_W_E, Object, PropertyKeyKind},
    value::JSValue,
};

#[derive(Debug)]
pub struct ImportEntry {
    /// closure variable index
    pub var_idx: u16,
    pub import_name: Atom,
    /// in dependencies
    pub req_module_idx: i32,
}

impl GcTrace for ImportEntry {
    #[inline(always)]
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.import_name.trace(gcx);
    }
}

/// Synchronously extract import attributes from `options.with`.
///
/// Per spec, this must happen synchronously during `import()` evaluation
/// so that Proxy traps (ownKeys, get, getOwnPropertyDescriptor) fire
/// before the call returns.
fn extract_import_attrs(ctx: &mut ScopedContext, options: JSValue) -> JSResult {
    if !options.is_object() {
        return Ok(JSValue::UNDEFINED);
    }

    let attrs = match options.get_property_str(ctx, "with")? {
        v if v.is_object() => v,
        v if v.is_undefined_or_null() => {
            return Ok(JSValue::UNDEFINED);
        }
        _ => {
            ctx.throw_type_error("import() options.with must be an object");
            return Err(-1);
        }
    };

    let mut obj = Object::try_from(attrs).map_err(|_| {
        ctx.throw_type_error("import() options.with must be an object");
        -1
    })?;

    let keys = obj.get_own_property_names(ctx, JS_GPN_ENUM_ONLY | JS_GPN_STR_OR_INT_MASK)?;

    if keys.is_empty() {
        return Ok(JSValue::UNDEFINED);
    }

    let mut plain_obj = ctx.create_base_object()?;
    for pk in keys.iter() {
        // Only include string keys (import attribute keys must be strings)
        if !matches!(pk.kind, PropertyKeyKind::String) {
            continue;
        }
        // Get the value (triggers Proxy get trap, may throw)
        let val = obj.get_property(ctx, pk.atom)?;
        plain_obj.define_property_value(ctx, pk.atom, val, JS_PROP_C_W_E)?;
    }

    Ok(JSValue::from(plain_obj))
}

pub(crate) fn js_dynamic_import(
    ctx: &mut ScopedContext,
    specifier: JSValue,
    options: JSValue,
) -> JSResult {
    let basename_val: JSValue = if let Some(a) = get_script_or_module_name(ctx, 0) {
        a.to_value(ctx)?
    } else {
        JSValue::NULL
    };

    let (promise, resolving_funcs) = Promise::create(ctx)?;

    // Extract attributes synchronously (triggers Proxy traps per spec)
    let attrs = match extract_import_attrs(ctx, options) {
        Ok(a) => a,
        Err(_) => {
            let err_val = ctx.take_exception().to_value(ctx);
            let _ =
                resolving_funcs[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val));
            return Ok(promise.into());
        }
    };

    // cannot run js_dynamic_import_job synchronously because it would
    // cause an unexpected recursion in js_evaluate_module()
    ctx.queue_oneshot_job(
        move |cx| {
            js_dynamic_import_job(cx, resolving_funcs, basename_val, specifier, attrs);
            Ok(())
        },
        JobGcTrace5 {
            a: basename_val,
            b: specifier,
            c: attrs,
            d: resolving_funcs[0],
            e: resolving_funcs[1],
        },
    );

    Ok(promise.into())
}

fn js_dynamic_import_job(
    ctx: &mut ScopedContext,
    resolving_funcs: [JSValue; 2],
    basename: JSValue,
    name: JSValue,
    attrs: JSValue,
) {
    let r = if let Some(base) = basename.as_string() {
        name.to_string(ctx).map(|nm| {
            js_load_module_internal(
                ctx,
                &base.content().encode_utf8(false),
                &nm.content().encode_utf8(false),
                &resolving_funcs,
                attrs,
            );
        })
    } else {
        ctx.throw_type_error("no function filename for import()");
        Err(-1)
    };

    if r.is_err() {
        // call reject func if has error
        let err_val = ctx.take_exception().to_value(ctx);
        let _ = resolving_funcs[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val));
    }
}

fn js_load_module_internal(
    ctx: &mut ScopedContext,
    basename: &str,
    filename: &str,
    resolving_funcs: &[JSValue; 2],
    attrs: JSValue,
) {
    match js_host_resolve_imported_module(ctx, basename, filename, attrs) {
        Ok(mut md) => {
            md.attributes = attrs;
            if ctx.resolve_module(md).is_err() {
                js_free_modules(ctx, JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED);
                let err_val = ctx.take_exception().to_value(ctx);
                let _ = resolving_funcs[1].call(
                    ctx,
                    JSValue::UNDEFINED,
                    std::slice::from_ref(&err_val),
                );
                return;
            }

            match md.evaluate(ctx) {
                Ok(mut promise) => {
                    let evaluate_resolving_funcs = [
                        NativeFunction::create(
                            ctx,
                            Callback::Func(js_load_module_fulfilled),
                            Atom::CONST_empty_string,
                            0,
                            0,
                            ctx.function_proto(),
                            Some([resolving_funcs[0], resolving_funcs[1], md.into()].into()),
                        )
                        .unwrap(),
                        NativeFunction::create(
                            ctx,
                            Callback::Func(js_load_module_rejected),
                            Atom::CONST_empty_string,
                            0,
                            0,
                            ctx.function_proto(),
                            Some([resolving_funcs[0], resolving_funcs[1], md.into()].into()),
                        )
                        .unwrap(),
                    ];

                    let _ = promise.then(
                        ctx,
                        evaluate_resolving_funcs[0].into(),
                        evaluate_resolving_funcs[1].into(),
                    );
                }
                Err(_) => {
                    let err_val = ctx.take_exception().to_value(ctx);
                    let _ = resolving_funcs[1].call(
                        ctx,
                        JSValue::UNDEFINED,
                        std::slice::from_ref(&err_val),
                    );
                }
            }
        }
        Err(_) => {
            let err_val = ctx.take_exception().to_value(ctx);
            let _ =
                resolving_funcs[1].call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val));
        }
    }
}

fn js_load_module_rejected(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let error = if cc.length() >= 1 {
        cc.arg(0)
    } else {
        JSValue::UNDEFINED
    };
    let _ = cc
        .data(1)
        .call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&error));

    debug_assert_eq!(
        ctx.rt().exception(),
        crate::Exception::No,
        "[import js_load_module_rejected] left exception behind"
    );
    Ok(JSValue::UNDEFINED)
}

fn js_load_module_fulfilled(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(mut m) = cc.data(2).as_module() else {
        unreachable!()
    };

    match m.ns(ctx) {
        Ok(ns) => {
            // resolve with module namespace
            let _ = cc.data(0).call(ctx, JSValue::UNDEFINED, &[ns.into()]);
        }
        Err(_) => {
            // reject with error
            let err_val = ctx.take_exception().to_value(ctx);
            let _ = cc
                .data(1)
                .call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val));
        }
    }

    debug_assert_eq!(
        ctx.rt().exception(),
        crate::Exception::No,
        "[import js_load_module_fulfilled] left exception behind"
    );

    Ok(JSValue::UNDEFINED)
}
