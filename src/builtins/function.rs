// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    error::JSResult,
    function::{BoundFuncObjPayload, CallContext, FieldDef, FunctionKind, js_function_apply},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    string::JS_ConcatString3,
    value::JSValue,
};

fn js_function_call(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    if cc.length() > 0 {
        cc.this().call(
            ctx,
            *cc.arg(0),
            &cc.arguments()[1..], // chop first argument (thisArg)
        )
    } else {
        cc.this().call(ctx, JSValue::UNDEFINED, &[])
    }
}

fn js_function_bind(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let this_val = *cc.this();

    this_val.ensure_function(ctx)?;

    let mut bf = Box::new(BoundFuncObjPayload {
        func_obj: this_val,
        this_val: *cc.arg(0), // argument #0 is for `this`
        bind_args: Vec::new(),
    });

    let mut arg_count = cc.length();
    arg_count = arg_count.saturating_sub(1);

    if arg_count > 0 {
        // collect arguments starting from #1
        bf.bind_args.try_reserve_exact(arg_count).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        for a in &cc.arguments()[1..] {
            bf.bind_args.push(*a);
        }
    }

    let mut func_obj = ctx
        .create_object_with_proto(
            Some(ctx.function_proto()),
            ObjectPayload::BOUND_FUNCTION(bf),
        )
        .map_err(|_| -1)?;

    func_obj.set_flag_constructor(this_val.is_constructor());

    /* XXX: the spec could be simpler by only using GetOwnProperty */
    let len_val = if cc
        .this()
        .clone()
        .get_own_property(ctx, Atom::CONST_length)?
        .is_none()
    {
        JSValue::Int(0)
    } else {
        let len = this_val.get_property(ctx, Atom::CONST_length)?;
        match len {
            JSValue::Int(n) => JSValue::Int(std::cmp::max(0, n - arg_count as i32)),
            JSValue::Float64(mut d) => {
                if d.is_nan() {
                    d = 0.0;
                } else {
                    d = d.trunc();
                    if d <= arg_count as f64 {
                        d = 0.0;
                    } else {
                        d -= arg_count as f64; /* also converts -0 to +0 */
                    }
                }
                JSValue::from(d)
            }
            _ => JSValue::Int(0),
        }
    };

    func_obj.define_property_value(ctx, Atom::CONST_length, len_val, JS_PROP_CONFIGURABLE)?;

    let name = cc.this().get_property(ctx, Atom::CONST_name)?;
    let name = if let JSValue::String(s) = name {
        s
    } else {
        Atom::CONST_empty_string.force_string(ctx)?
    };

    // make bound function name.
    let bf_name = JS_ConcatString3(ctx, c"bound ", name.into(), c"")?;
    func_obj.define_property_value(ctx, Atom::CONST_name, bf_name.into(), JS_PROP_CONFIGURABLE)?;

    Ok(func_obj.into())
}

fn js_function_toString(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let this_val = cc.this();

    this_val.ensure_function(ctx)?;

    let p = unsafe { this_val.as_object_unchecked() };

    let func_kind = if let Some(func) = p.payload().bc_function() {
        let bc = func.bytecode();
        if let Some(debug) = bc.debug.as_ref()
            && let Some(s) = debug.source.as_deref()
        {
            return ctx.create_string_from_str(s).map(JSValue::String);
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

    JS_ConcatString3(ctx, pref, name, suff).map(JSValue::String)
}

fn js_function_hasInstance(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    if let Some(o) = cc.arg(0).as_object() {
        o.ordinary_instance_of(ctx, *cc.this()).map(JSValue::Bool)
    } else {
        Ok(JSValue::Bool(false))
    }
}

fn js_function_proto_fileName(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    if let Some(filename) = cc.this().as_object().and_then(|o| {
        o.payload()
            .bc_function()
            .and_then(|x| x.bytecode().debug.as_ref().map(|d| d.filename))
    }) {
        filename.force_string(ctx).map(JSValue::String)
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

fn js_function_proto_lineNumber(cc: CallContext) -> JSResult {
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

pub const js_function_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("call", 1, js_function_call, 0),
    FieldDef::CALL("apply", 2, js_function_apply, 0),
    FieldDef::CALL("bind", 1, js_function_bind, 0),
    FieldDef::CALL("toString", 0, js_function_toString, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::CALL("[Symbol.hasInstance]", 1, js_function_hasInstance, 0),
    FieldDef::GETSET("fileName", Some(js_function_proto_fileName), None, 0),
    FieldDef::GETSET("lineNumber", Some(js_function_proto_lineNumber), None, 0),
];
