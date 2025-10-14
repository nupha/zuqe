use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF,
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    function::{BoundFuncObjPayload, CallContext, FieldDef, JSFunctionKindEnum, js_function_apply},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    string::JS_ConcatString3,
    value::JSValue,
};

pub fn js_function_call(ctx: &JSContext, cc: CallContext) -> JSResult {
    if cc.length() > 0 {
        cc.this().call(
            ctx,
            cc.arg(0),
            &cc.arguments()[1..], // chop first argument (thisArg)
        )
    } else {
        cc.this().call(ctx, &JSValue::UNDEFINED, &[])
    }
}

pub fn js_function_bind(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    this_val.ensure_function(ctx)?;

    let mut bf = Box::new(BoundFuncObjPayload {
        func_obj: this_val.clone(),
        this_val: cc.arg(0).clone(), // argument #0 is for `this`
        bind_args: Vec::new(),
    });

    let mut arg_count = cc.length();
    if arg_count > 0 {
        arg_count -= 1;
    }
    if arg_count > 0 {
        // collect arguments starting from #1
        if bf.bind_args.try_reserve_exact(arg_count).is_err() {
            ctx.throw_out_of_memory();
            return Err(-1);
        }
        for a in &cc.arguments()[1..] {
            bf.bind_args.push(a.clone());
        }
    }

    let func_obj = ctx
        .create_object_from_proto(
            ctx.0.borrow().function_proto.clone(),
            ObjectPayload::BOUND_FUNCTION(bf),
        )
        .map_err(|_| -1)?;

    func_obj.set_flag_constructor(this_val.is_constructor());

    /* XXX: the spec could be simpler by only using GetOwnProperty */
    let len_val = if cc
        .this_raw()
        .get_own_property(ctx, JS_ATOM_length)?
        .is_none()
    {
        JSValue::Int(0)
    } else {
        let len = this_val.get_property(ctx, JS_ATOM_length)?;
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

    func_obj
        .define_property_value(ctx, JS_ATOM_length, len_val, JS_PROP_CONFIGURABLE)
        .unwrap();

    let name = cc.this_raw().get_property(ctx, JS_ATOM_name)?;
    let name = if let JSValue::String(s) = name {
        s
    } else {
        JS_ATOM_empty_string.force_string(ctx)?
    };

    // make bound function name.
    let bf_name = JS_ConcatString3(ctx, c"bound ", name.into(), c"")?;
    func_obj.define_property_value(ctx, JS_ATOM_name, bf_name.into(), JS_PROP_CONFIGURABLE)?;

    Ok(func_obj.into())
}

pub fn js_function_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    this_val.ensure_function(ctx)?;

    let p = unsafe { this_val.as_object_unchecked() };

    let func_kind = if let Some(func) = p.payload().func() {
        let bc = func.bytecode();
        if bc.0.has_debug {
            if let Some(s) = bc.0.debug.source.as_deref() {
                return ctx.create_string_from_str(s).map(JSValue::String);
            }
        }
        bc.func_kind()
    } else {
        JSFunctionKindEnum::JS_FUNC_NORMAL
    };

    let pref = match func_kind {
        JSFunctionKindEnum::JS_FUNC_GENERATOR => c"function *",
        JSFunctionKindEnum::JS_FUNC_ASYNC => c"async function ",
        JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR => c"async function *",
        _ => c"function ",
    };
    let suff = c"() { [native code] }";
    let mut name = this_val.get_property(ctx, JS_ATOM_name)?;
    if name.is_undefined() {
        name = JS_ATOM_empty_string.force_string(ctx)?.into();
    }

    JS_ConcatString3(ctx, pref, name, suff).map(JSValue::String)
}

pub fn js_function_hasInstance(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let Some(o) = cc.arg(0).as_object() {
        o.ordinary_instance_of(ctx, cc.this()).map(JSValue::Bool)
    } else {
        Ok(JSValue::Bool(false))
    }
}

pub fn js_function_proto_fileName(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let Some((has_debug, filename)) = cc.this().as_object().and_then(|o| {
        o.payload().func().map(|x| {
            let b = &x.bytecode().0;
            (b.has_debug, b.debug.filename)
        })
    }) {
        if has_debug {
            filename.force_string(ctx).map(JSValue::String)
        } else {
            Ok(JSValue::UNDEFINED)
        }
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub fn js_function_proto_lineNumber(_: &JSContext, cc: CallContext) -> JSResult {
    if let Some((has_debug, line_num)) = cc.this().as_object().and_then(|o| {
        o.payload().func().map(|x| {
            let b = &x.bytecode().0;
            (b.has_debug, b.debug.line_num)
        })
    }) {
        if has_debug {
            Ok(JSValue::Int(line_num))
        } else {
            Ok(JSValue::UNDEFINED)
        }
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub static js_function_proto_funcs: [FieldDef; 7] = [
    JS_CFUNC_NATIVE_DEF!("call", 1, js_function_call, 0),
    JS_CFUNC_NATIVE_DEF!("apply", 2, js_function_apply, 0),
    JS_CFUNC_NATIVE_DEF!("bind", 1, js_function_bind, 0),
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_function_toString, 0),
    JS_CFUNC_NATIVE_DEF!("[Symbol.hasInstance]", 1, js_function_hasInstance, 0),
    JS_CGETSET_NATIVE_DEF!("fileName", js_function_proto_fileName, NONE, 0),
    JS_CGETSET_NATIVE_DEF!("lineNumber", js_function_proto_lineNumber, NONE, 0),
];
