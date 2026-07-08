// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    Callback, JobGcTrace,
    atom::Atom,
    builtins::promise::Promise,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, NativeFunction},
    module::{
        JSFreeModuleEnum, get_script_or_module_name, js_free_modules,
        js_host_resolve_imported_module,
    },
    value::JSValue,
};

/// JobGcTrace wrapper for 5 values (used by js_dynamic_import).
struct JobGcTrace5<A, B, C, D, E> {
    a: A,
    b: B,
    c: C,
    d: D,
    e: E,
}
impl<A: JobGcTrace, B: JobGcTrace, C: JobGcTrace, D: JobGcTrace, E: JobGcTrace> JobGcTrace
    for JobGcTrace5<A, B, C, D, E>
{
    fn job_trace_values(&self, gcx: &mut GcTraceCtx) {
        self.a.job_trace_values(gcx);
        self.b.job_trace_values(gcx);
        self.c.job_trace_values(gcx);
        self.d.job_trace_values(gcx);
        self.e.job_trace_values(gcx);
    }
}

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

    // cannot run js_dynamic_import_job synchronously because it would
    // cause an unexpected recursion in js_evaluate_module()
    let rr = resolving_funcs;
    ctx.queue_oneshot_job(
        move |cx| {
            js_dynamic_import_job(cx, resolving_funcs, basename_val, specifier, options);
            Ok(())
        },
        JobGcTrace5 {
            a: basename_val,
            b: specifier,
            c: options,
            d: rr[0],
            e: rr[1],
        },
    );

    Ok(promise.into())
}

fn js_dynamic_import_job(
    ctx: &mut ScopedContext,
    resolving_funcs: [JSValue; 2],
    basename: JSValue,
    name: JSValue,
    options: JSValue,
) {
    // Extract import attributes from options.with
    let attrs = if options.is_object() {
        match options.get_property_str(ctx, "with") {
            Ok(v) if v.is_object() => v,
            Ok(JSValue::UNDEFINED) | Ok(JSValue::NULL) => JSValue::UNDEFINED,
            Ok(_) => {
                ctx.throw_type_error("import() options.with must be an object");
                let err_val = ctx.take_exception().to_value(ctx);
                let _ = resolving_funcs[1].call(
                    ctx,
                    JSValue::UNDEFINED,
                    std::slice::from_ref(&err_val),
                );
                return;
            }
            Err(_) => {
                let err_val = ctx.take_exception().to_value(ctx);
                let _ = resolving_funcs[1].call(
                    ctx,
                    JSValue::UNDEFINED,
                    std::slice::from_ref(&err_val),
                );
                return;
            }
        }
    } else {
        JSValue::UNDEFINED
    };

    let r = if let JSValue::String(base) = basename {
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
    let JSValue::Module(mut m) = cc.data(2) else {
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
