// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTracable, GcTraceCtx};

use crate::{
    Callback,
    atom::Atom,
    builtins::promise::Promise,
    context::{ContextImpl, ContextPtr},
    error::JSResult,
    function::{CallContext, NativeFunction},
    module::{
        JSFreeModuleEnum, get_script_or_module_name, js_free_modules,
        js_host_resolve_imported_module,
    },
    value::JSValue,
};

#[derive(Debug)]
pub struct ImportEntry {
    pub var_idx: u16, // closure variable index
    pub import_name: Atom,
    pub req_module_idx: i32, // in dependencies
}

unsafe impl GcTracable for ImportEntry {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.import_name.trace(tr);
    }
}

pub fn js_dynamic_import(ctx: &mut ContextImpl, specifier: JSValue) -> JSResult {
    let basename_val: JSValue = if let Some(a) = get_script_or_module_name(ctx, 0) {
        a.to_value(ctx)?
    } else {
        JSValue::NULL
    };

    let (promise, resolving_funcs) = Promise::create(ctx)?;

    // cannot run JS_LoadModuleInternal synchronously because it would
    // cause an unexpected recursion in js_evaluate_module()
    let rr = resolving_funcs;
    let mut cx = unsafe { ContextPtr::from_ref_unchecked(ctx) };

    ctx.queue_job_with_trace(
        move || {
            js_dynamic_import_job(cx, resolving_funcs, basename_val, specifier);
            Ok(JSValue::UNDEFINED)
        },
        {
            // Note: job tick is performed in root scope,
            // should promote related gc nodes to root scope.
            let top = cx.rt_ref().gc_top_scope;
            let heap = cx.rt_mut().gc_heap_mut();
            if let Some(n) = basename_val.gc_node_ptr() {
                heap.set_xref(top, n);
            }
            if let Some(n) = specifier.gc_node_ptr() {
                heap.set_xref(top, n);
            }
            for n in rr.iter().filter_map(|v| v.gc_node_ptr()) {
                heap.set_xref(top, n);
            }

            move |tr| {
                basename_val.trace(tr);
                specifier.trace(tr);
                rr[0].trace(tr);
                rr[1].trace(tr);
            }
        },
    );

    Ok(promise.into())
}

fn js_dynamic_import_job(
    mut ctx: ContextPtr,
    resolving_funcs: [JSValue; 2],
    basename: JSValue,
    name: JSValue,
) {
    let r = if let JSValue::String(base) = basename {
        name.to_string(&ctx).map(|nm| {
            js_load_module_internal(
                ctx,
                &base.content().encode_utf8(false),
                &nm.content().encode_utf8(false),
                &resolving_funcs,
            );
        })
    } else {
        ctx.throw_type_error("no function filename for import()");
        Err(-1)
    };

    if r.is_err() {
        // call reject func if has error
        let err = ctx.rt_mut().take_exception();
        let _ = resolving_funcs[1].call(&ctx, JSValue::UNDEFINED, std::slice::from_ref(&err));
    }
}

fn js_load_module_internal(
    mut ctx: ContextPtr,
    basename: &str,
    filename: &str,
    resolving_funcs: &[JSValue; 2],
) {
    match js_host_resolve_imported_module(ctx, basename, filename) {
        Ok(mut md) => {
            if ctx.resolve_module(md).is_err() {
                js_free_modules(&mut ctx, JSFreeModuleEnum::JS_FREE_MODULE_NOT_RESOLVED);
                let err = ctx.rt_ptr().take_exception();
                let _ =
                    resolving_funcs[1].call(&ctx, JSValue::UNDEFINED, std::slice::from_ref(&err));
                return;
            }

            // eval module
            match md.eval(&mut ctx) {
                Ok(mut evaluate_promise) => {
                    let evaluate_resolving_funcs = [
                        NativeFunction::create(
                            &ctx,
                            Callback::Func(js_load_module_fulfilled),
                            "",
                            0,
                            0,
                            None,
                            Some([resolving_funcs[0], resolving_funcs[1], md.into()].into()),
                        )
                        .unwrap(),
                        NativeFunction::create(
                            &ctx,
                            Callback::Func(js_load_module_rejected),
                            "",
                            0,
                            0,
                            None,
                            Some([resolving_funcs[0], resolving_funcs[1], md.into()].into()),
                        )
                        .unwrap(),
                    ];

                    let _ = evaluate_promise.then(
                        &mut ctx,
                        evaluate_resolving_funcs[0].into(),
                        evaluate_resolving_funcs[1].into(),
                    );
                }
                Err(_) => {
                    let err = ctx.rt_ptr().take_exception();
                    let _ = resolving_funcs[1].call(
                        &ctx,
                        JSValue::UNDEFINED,
                        std::slice::from_ref(&err),
                    );
                }
            }
        }
        Err(_) => {
            let err = ctx.rt_ptr().take_exception();
            let _ = resolving_funcs[1].call(&ctx, JSValue::UNDEFINED, std::slice::from_ref(&err));
        }
    }
}

fn js_load_module_rejected(cc: CallContext) -> JSResult {
    let error = if cc.length() >= 1 {
        *cc.arg(0)
    } else {
        JSValue::UNDEFINED
    };
    let _ = cc
        .data(1)
        .call(cc.ctx(), JSValue::UNDEFINED, std::slice::from_ref(&error));
    Ok(JSValue::UNDEFINED)
}

fn js_load_module_fulfilled(cc: CallContext) -> JSResult {
    if let Ok(ns) = if let JSValue::Module(mut m) = cc.data(2) {
        m.ns(cc.ctx())
    } else {
        unreachable!()
    } {
        // resolve() with module namespace
        let _ = cc.data(0).call(cc.ctx(), JSValue::UNDEFINED, &[ns.into()]);
    } else {
        // reject() with error
        let err = cc.ctx().rt_ptr().take_exception();
        let _ = cc
            .data(1)
            .call(cc.ctx(), JSValue::UNDEFINED, std::slice::from_ref(&err));
    }

    Ok(JSValue::UNDEFINED)
}
