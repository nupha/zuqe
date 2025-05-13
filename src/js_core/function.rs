use crate::{
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    function::{
        js_class_has_bytecode, js_function_apply, CallArgs, JSBoundFunction, JSCFunctionListEntry,
        JSFunctionKindEnum, JS_Call, JS_GetFunctionBytecode,
    },
    value::{
        object::{JS_GetOwnProperty, JS_OrdinaryIsInstanceOf, JS_PROP_CONFIGURABLE},
        JSTag, JSValue, JS_ConcatString3, JS_FreeValue, JS_VALUE_GET_FLOAT64, JS_VALUE_GET_INT,
        JS_VALUE_GET_NORM_TAG,
    },
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_BOUND_FUNCTION,
};

pub fn js_function_call(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let r = if args.length() == 0 {
        unsafe {
            JS_Call(
                ctx,
                args.this_value,
                JSValue::UNDEFINED,
                args.length() as _,
                std::ptr::null_mut(),
            )
        }
    } else {
        unsafe {
            JS_Call(
                ctx,
                args.this_value,
                args.get_raw(0),
                args.length() as i32 - 1,
                args.arguments_raw().as_ptr().add(1) as _,
            )
        }
    };

    if r.is_exception() {
        Err(-1)
    } else {
        Ok(r)
    }
}

pub fn js_function_bind(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    args.this_value.ensure_callable(ctx)?;

    let func_obj =
        ctx.create_object_with_proto_and_classid(ctx.function_proto, JS_CLASS_BOUND_FUNCTION)?;

    let p = unsafe { func_obj.object_unchecked() };
    p.detailed.set_constructor(args.this_value.is_constructor());

    let mut bf = Box::new(JSBoundFunction {
        func_obj: args.this_value.dup(),
        this_val: args.get_raw(0).dup(), // argument #0 is for `this`
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
        for a in &args.arguments_raw()[1..] {
            bf.bind_args.push(a.dup());
        }
    }

    unsafe {
        (*p).u.bound_function = Box::into_raw(bf);

        /* XXX: the spec could be simpler by only using GetOwnProperty */
        let ret = JS_GetOwnProperty(ctx, std::ptr::null_mut(), args.this_value, JS_ATOM_length);
        if ret < 0 {
            JS_FreeValue(ctx, func_obj);
            return Err(-1);
        }

        let len_val = if ret == 0 {
            JSValue::ZERO
        } else {
            let length = args.this_value.get_property(ctx, JS_ATOM_length);
            match JS_VALUE_GET_NORM_TAG(length) {
                JSTag::Int => {
                    let n = JS_VALUE_GET_INT(length);
                    JSValue::from_i32(std::cmp::max(0, n - arg_count as i32))
                }
                JSTag::Float64 => {
                    let mut d = JS_VALUE_GET_FLOAT64(length);
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
                    JSValue::from_f64(d)
                }
                JSTag::Exception => {
                    JS_FreeValue(ctx, func_obj);
                    return Err(-1);
                }
                _ => {
                    JS_FreeValue(ctx, length);
                    JSValue::ZERO
                }
            }
        };

        func_obj
            .define_property_value(ctx, JS_ATOM_length, len_val, JS_PROP_CONFIGURABLE)
            .unwrap();

        let mut name = args.this_value.get_property(ctx, JS_ATOM_name);
        if name.is_exception() {
            JS_FreeValue(ctx, func_obj);
            return Err(-1);
        }
        if !name.is_string() {
            JS_FreeValue(ctx, name);
            name = JS_ATOM_empty_string.to_value(ctx, true);
        }

        // make bound function name.
        let bf_name = JS_ConcatString3(ctx, c"bound ", name, c"");
        if bf_name.is_exception() {
            JS_FreeValue(ctx, func_obj);
            return Err(-1);
        }

        func_obj
            .define_property_value(ctx, JS_ATOM_name, bf_name, JS_PROP_CONFIGURABLE)
            .unwrap();

        Ok(func_obj)
    }
}

pub fn js_function_toString(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    args.this_value.ensure_callable(ctx)?;

    let p = unsafe { args.this_value.object_unchecked() };

    let func_kind = if js_class_has_bytecode(p.class_id()) {
        unsafe {
            let b = p.u.func.function_bytecode;
            if (*b).has_debug && !(*b).debug.source.is_null() {
                let rv = ctx.create_string(
                    std::str::from_utf8_unchecked(std::slice::from_raw_parts(
                        (*b).debug.source,
                        (*b).debug.source_len as _,
                    ))
                    .as_ref(),
                );
                return if rv.is_exception() { Err(-1) } else { Ok(rv) };
            }
            (*b).func_kind
        }
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
    let mut name = args.this_value.get_property(ctx, JS_ATOM_name);
    if name.is_undefined() {
        name = JS_ATOM_empty_string.to_value(ctx, true);
    }

    let rv = unsafe { JS_ConcatString3(ctx, pref, name, suff) };
    if rv.is_exception() {
        Err(-1)
    } else {
        Ok(rv)
    }
}

pub fn js_function_hasInstance(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let b = unsafe { JS_OrdinaryIsInstanceOf(ctx, args.get_raw(0), args.this_value)? };
    Ok(if b { JSValue::TRUE } else { JSValue::FALSE })
}

pub fn js_function_proto_fileName(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let b = unsafe { JS_GetFunctionBytecode(args.this_value) };

    unsafe {
        if !b.is_null() && (*b).has_debug {
            (*b).debug.filename.to_value(ctx, true).check_exception()
        } else {
            Ok(JSValue::UNDEFINED)
        }
    }
}

pub fn js_function_proto_lineNumber(_: &mut JSContext, args: CallArgs) -> JSResult {
    let b = unsafe { JS_GetFunctionBytecode(args.this_value) };

    unsafe {
        if !b.is_null() && (*b).has_debug {
            Ok(JSValue::from_i32((*b).debug.line_num as _))
        } else {
            Ok(JSValue::UNDEFINED)
        }
    }
}

pub const js_function_proto_funcs: [JSCFunctionListEntry; 7] = [
    JS_CFUNC_NATIVE_DEF!(c"call", 1, js_function_call, 0),
    JS_CFUNC_NATIVE_DEF!(c"apply", 2, js_function_apply, 0),
    JS_CFUNC_NATIVE_DEF!(c"bind", 1, js_function_bind, 0),
    JS_CFUNC_NATIVE_DEF!(c"toString", 0, js_function_toString, 0),
    JS_CFUNC_NATIVE_DEF!(c"[Symbol.hasInstance]", 1, js_function_hasInstance, 0),
    JS_CGETSET_NATIVE_DEF!(c"fileName", js_function_proto_fileName, NONE, 0),
    JS_CGETSET_NATIVE_DEF!(c"lineNumber", js_function_proto_lineNumber, NONE, 0),
];
