use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF,
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    function::{
        CallContext, JS_Call, JSBoundFunction, NativeFieldDef, JSFunctionKindEnum,
        js_function_apply,
    },
    object::{JS_PROP_CONFIGURABLE, JSObjectIsa},
    value::{JS_ConcatString3, JSValue},
};

pub fn js_function_call(ctx: &JSContext, args: CallContext) -> JSResult {
    if args.length() > 0 {
        JS_Call(
            ctx,
            args.this(),
            args.arg(0),
            &args.arguments()[1..], // chop first argument (thisArg)
        )
    } else {
        JS_Call(ctx, args.this(), &JSValue::UNDEFINED, &[])
    }
    .check_exception()
}

pub fn js_function_bind(ctx: &JSContext, args: CallContext) -> JSResult {
    let this_val = args.this();

    this_val.ensure_callable(ctx)?;

    let mut bf = Box::new(JSBoundFunction {
        func_obj: this_val.clone(),
        this_val: args.arg(0).clone(), // argument #0 is for `this`
        bind_args: Vec::new(),
    });

    let mut arg_count = args.length();
    if arg_count > 0 {
        arg_count -= 1;
    }
    if arg_count > 0 {
        // collect arguments starting from #1
        if bf.bind_args.try_reserve_exact(arg_count).is_err() {
            ctx.JS_ThrowOutOfMemory();
            return Err(-1);
        }
        for a in &args.arguments()[1..] {
            bf.bind_args.push(a.clone());
        }
    }

    let func_obj = ctx
        .create_instance_from_proto(
            &ctx.0.borrow().function_proto,
            JSObjectIsa::BOUND_FUNCTION(bf),
        )
        .map_err(|_| -1)?;

    let p = unsafe { func_obj.object_unchecked() };

    unsafe {
        (*p.0.detailed.as_ptr()).set_constructor(this_val.is_constructor());
    }

    unsafe {
        /* XXX: the spec could be simpler by only using GetOwnProperty */
        let len_val = if args
            .this_raw()
            .get_own_property(ctx, JS_ATOM_length)?
            .is_none()
        {
            JSValue::Int(0)
        } else {
            let len = this_val.get_property(ctx, JS_ATOM_length);
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
                    JSValue::Float64(d)
                }
                JSValue::EXCEPTION => {
                    return Err(-1);
                }
                _ => JSValue::Int(0),
            }
        };

        func_obj
            .define_property_value(ctx, JS_ATOM_length, len_val, JS_PROP_CONFIGURABLE)
            .unwrap();

        let mut name = args
            .this_raw()
            .get_property(ctx, JS_ATOM_name)
            .check_exception()?;

        if !name.is_string() {
            name = JS_ATOM_empty_string.to_value(ctx, true);
        }

        // make bound function name.
        let bf_name = JS_ConcatString3(ctx, c"bound ", name, c"").check_exception()?;

        func_obj.define_property_value(ctx, JS_ATOM_name, bf_name, JS_PROP_CONFIGURABLE)?;

        Ok(func_obj)
    }
}

pub fn js_function_toString(ctx: &JSContext, args: CallContext) -> JSResult {
    let this_val = args.this_raw();

    this_val.ensure_callable(ctx)?;
    let p = unsafe { this_val.object_unchecked() };

    let func_kind = if let Some(func) = p.0.isa.borrow().func() {
        let bc = func.bytecode.as_ref().unwrap();
        let b = bc.0.borrow();
        if b.has_debug {
            if let Some(s) = b.debug.source.as_deref() {
                return ctx.create_string(s).check_exception();
            }
        }
        b.func_kind
    } else {
        JSFunctionKindEnum::JS_FUNC_NORMAL
    };

    let pref = match func_kind {
        JSFunctionKindEnum::JS_FUNC_GENERATOR => c"function *",
        JSFunctionKindEnum::JS_FUNC_ASYNC => c"async function ",
        JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR => c"async function *",
        _ => c"function ",
    };
    let suff = c"() {\n    [native code]\n}";
    let mut name = this_val.get_property(ctx, JS_ATOM_name);
    if name.is_undefined() {
        name = JS_ATOM_empty_string.to_value(ctx, true);
    }

    unsafe { JS_ConcatString3(ctx, pref, name, suff).check_exception() }
}

pub fn js_function_hasInstance(ctx: &JSContext, args: CallContext) -> JSResult {
    Ok(JSValue::Bool(
        args.this_raw()
            .ordinary_instance_of(ctx, &args.get_raw(0))?,
    ))
}

pub fn js_function_proto_fileName(ctx: &JSContext, args: CallContext) -> JSResult {
    if let Some(b) = args.this_raw().object().and_then(|o| {
        o.0.isa
            .borrow()
            .func()
            .and_then(|f| f.bytecode.clone())
    }) {
        let b = b.0.borrow();
        if b.has_debug {
            b.debug.filename.to_value(ctx, true).check_exception()
        } else {
            Ok(JSValue::UNDEFINED)
        }
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub fn js_function_proto_lineNumber(_: &JSContext, args: CallContext) -> JSResult {
    if let Some(b) = args.this_raw().object().and_then(|o| {
        o.0.isa
            .borrow()
            .func()
            .and_then(|f| f.bytecode.clone())
    }) {
        let b = b.0.borrow();
        if b.has_debug {
            Ok(JSValue::Int(b.debug.line_num as i32))
        } else {
            Ok(JSValue::UNDEFINED)
        }
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub const js_function_proto_funcs: [NativeFieldDef; 7] = [
    JS_CFUNC_NATIVE_DEF!("call", 1, js_function_call, 0),
    JS_CFUNC_NATIVE_DEF!("apply", 2, js_function_apply, 0),
    JS_CFUNC_NATIVE_DEF!("bind", 1, js_function_bind, 0),
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_function_toString, 0),
    JS_CFUNC_NATIVE_DEF!("[Symbol.hasInstance]", 1, js_function_hasInstance, 0),
    JS_CGETSET_NATIVE_DEF!("fileName", js_function_proto_fileName, NONE, 0),
    JS_CGETSET_NATIVE_DEF!("lineNumber", js_function_proto_lineNumber, NONE, 0),
];
