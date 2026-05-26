// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    ClassDef, ScopedContext,
    atom::Atom,
    error::JSResult,
    function::{
        AsyncFunction, AsyncState, BoundFunction, CallContext, FieldDef, FunctionKind,
        NativeFunction, js_function_apply,
    },
    string::JS_ConcatString3,
    value::JSValue,
};

fn js_function_call(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if cc.length() > 0 {
        cc.this().call(
            ctx,
            cc.arg(0),
            &cc.arguments()[1..], // chop first argument (thisArg)
        )
    } else {
        cc.this().call(ctx, JSValue::UNDEFINED, &[])
    }
}

fn js_function_bind(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let func_obj = cc.this().into_callable(ctx)?;
    let arg_count = cc.length().saturating_sub(1);

    BoundFunction::create(
        ctx,
        func_obj,
        cc.arg(0), // use argument #0 as `this`,
        if arg_count > 0 {
            // collect arguments starting from #1
            cc.arguments()[1..].to_vec()
        } else {
            Vec::new()
        },
    )
    .map(JSValue::from)
}

fn js_function_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    this_val.ensure_callable(ctx)?;

    let p = unsafe { this_val.as_object_unchecked() };

    let func_kind = if let Some(func) = p.payload().bc_function() {
        let bc = func.bytecode();
        if let Some(debug) = bc.debug.as_ref()
            && let Some(s) = debug.source.as_deref()
        {
            return ctx.create_string_from_str(s).map(JSValue::from);
        }
        bc.func_kind()
    } else {
        FunctionKind::Normal
    };

    let pref = match func_kind {
        FunctionKind::Generator => c"function *",
        FunctionKind::Async => c"async function ",
        FunctionKind::AsyncGenerator => c"async function *",
        _ => c"function ",
    };
    let suff = c"() { [native code] }";
    let mut name = this_val.get_property(ctx, Atom::CONST_name)?;
    if name.is_undefined() {
        name = Atom::CONST_empty_string.force_string(ctx)?.into();
    }

    JS_ConcatString3(ctx, pref, name, suff).map(JSValue::from)
}

fn js_function_hasInstance(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(o) = cc.arg(0).as_object() {
        o.ordinary_instance_of(ctx, cc.this()).map(JSValue::Bool)
    } else {
        Ok(JSValue::Bool(false))
    }
}

fn js_function_proto_fileName(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(filename) = cc.this().as_object().and_then(|o| {
        o.payload()
            .bc_function()
            .and_then(|x| x.bytecode().debug.as_ref().map(|d| d.filename))
    }) {
        filename.force_string(ctx).map(JSValue::from)
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

fn js_function_proto_lineNumber(_: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(line_num) = cc.this().as_object().and_then(|o| {
        o.payload()
            .bc_function()
            .and_then(|x| x.bytecode().debug.as_ref().map(|d| d.line_num))
    }) {
        Ok(JSValue::Int(line_num))
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub(crate) const js_function_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("call", 1, js_function_call, 0),
    FieldDef::FUNC("apply", 2, js_function_apply, 0),
    FieldDef::FUNC("bind", 1, js_function_bind, 0),
    FieldDef::FUNC("toString", 0, js_function_toString, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::FUNC("[Symbol.hasInstance]", 1, js_function_hasInstance, 0),
    FieldDef::GETSET("fileName", Some(js_function_proto_fileName), None, 0),
    FieldDef::GETSET("lineNumber", Some(js_function_proto_lineNumber), None, 0),
];

pub(crate) static CLASS_DEF_BYTECODE_FUNCTION: ClassDef =
    ClassDef::builtin(Atom::CONST_Function, js_function_proto_funcs, None);

pub(crate) static CLASS_DEF_BOUND_FUNCTION: ClassDef =
    ClassDef::builtin(Atom::CONST_Function, &[], None).on_call(BoundFunction::class_call);

pub(crate) static CLASS_DEF_C_FUNCTION: ClassDef =
    ClassDef::builtin(Atom::CONST_Function, &[], None).on_call(NativeFunction::class_call);

pub(crate) static CLASS_DEF_C_FUNCTION_DATA: ClassDef =
    ClassDef::builtin(Atom::CONST_Function, &[], None)
        .on_call(NativeFunction::class_call_with_data);

pub(crate) static CLASS_DEF_ASYNC_FUNCTION: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_AsyncFunction, AsyncFunction::class_init)
        .on_call(AsyncFunction::class_call);

pub(crate) static CLASS_DEF_ASYNC_FUNCTION_RESOLVE: ClassDef =
    ClassDef::builtin(Atom::CONST_AsyncFunctionResolve, &[], None).on_call(AsyncState::class_call);

pub(crate) static CLASS_DEF_ASYNC_FUNCTION_REJECT: ClassDef =
    ClassDef::builtin(Atom::CONST_AsyncFunctionReject, &[], None).on_call(AsyncState::class_call);
