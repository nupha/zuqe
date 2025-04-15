use smallvec::SmallVec;

use crate::{
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    eval::{JS_EvalObject, JS_EVAL_TYPE_INDIRECT},
    function::{
        func_kind_to_class_id, js_class_has_bytecode, js_function_apply, CallArgs, JSBoundFunction,
        JSCFunctionListEntry, JSFunctionKindEnum, JS_Call, JS_GetFunctionBytecode,
        JS_GetFunctionRealm,
    },
    mem::js_malloc,
    value::{
        object::{
            JS_GetOwnProperty, JS_IsConstructor, JS_NewObjectProtoClass, JS_OrdinaryIsInstanceOf,
            JS_SetPrototypeInternal, JS_PROP_CONFIGURABLE,
        },
        JSTag, JSValue, JSValueConst, JS_ConcatString3, JS_FreeValue, JS_IsException,
        JS_IsUndefined, JS_VALUE_GET_FLOAT64, JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG,
    },
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_BOUND_FUNCTION,
};

// pub unsafe fn js_function_call(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     if argc <= 0 {
//         JS_Call(
//             ctx.as_mut().unwrap(),
//             this_val,
//             JSValue::UNDEFINED,
//             0,
//             std::ptr::null_mut(),
//         )
//     } else {
//         JS_Call(
//             ctx.as_mut().unwrap(),
//             this_val,
//             *argv.offset(0),
//             argc - 1,
//             argv.offset(1),
//         )
//     }
// }

pub fn js_function_call(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let r = if args.length() == 0 {
        unsafe {
            JS_Call(
                ctx,
                args.this,
                JSValue::UNDEFINED,
                args.length() as _,
                std::ptr::null_mut(),
            )
        }
    } else {
        unsafe {
            JS_Call(
                ctx,
                args.this,
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
    args.this.ensure_callable(ctx)?;

    let func_obj =
        unsafe { JS_NewObjectProtoClass(ctx, ctx.function_proto, JS_CLASS_BOUND_FUNCTION) };
    if func_obj.is_exception() {
        return Err(-1);
    }

    let p = unsafe { func_obj.object() };
    p.detailed
        .set_constructor(unsafe { JS_IsConstructor(ctx, args.this) });

    let arg_count = std::cmp::max(0, args.length() - 1);

    unsafe {
        let bf = js_malloc(
            ctx,
            std::mem::size_of::<JSBoundFunction>() + arg_count * std::mem::size_of::<JSValue>(),
        )
        .cast::<JSBoundFunction>();
        if bf.is_null() {
            JS_FreeValue(ctx, func_obj);
            return Err(-1);
        }

        (*bf).func_obj = args.this.dup();
        (*bf).this_val = args.get_raw(0).dup();
        (*bf).argc = arg_count as _;
        for i in 0..arg_count {
            *(*bf).argv.as_mut_ptr().add(i) = args.get_raw(i + 1).dup();
        }
        (*p).u.bound_function = bf;

        /* XXX: the spec could be simpler by only using GetOwnProperty */
        let ret = JS_GetOwnProperty(ctx, std::ptr::null_mut(), args.this, JS_ATOM_length);
        if ret < 0 {
            JS_FreeValue(ctx, func_obj);
            return Err(-1);
        }

        let len_val = if ret == 0 {
            JSValue::ZERO
        } else {
            let length = args.this.get_property(ctx, JS_ATOM_length);
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

        let mut name = args.this.get_property(ctx, JS_ATOM_name);
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

// pub unsafe fn js_function_bind(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     if check_function(ctx, this_val).is_err() {
//         return JSValue::EXCEPTION;
//     }

//     let func_obj = JS_NewObjectProtoClass(ctx, (*ctx).function_proto, JS_CLASS_BOUND_FUNCTION);
//     if func_obj.is_exception() {
//         return JSValue::EXCEPTION;
//     }

//     let p = JS_VALUE_GET_OBJ(func_obj);
//     (*p).detailed
//         .set_constructor(JS_IsConstructor(ctx, this_val));

//     let arg_count = std::cmp::max(0, argc - 1);

//     let bf = js_malloc(
//         ctx,
//         std::mem::size_of::<JSBoundFunction>()
//             + (arg_count as usize) * std::mem::size_of::<JSValue>(),
//     )
//     .cast::<JSBoundFunction>();
//     if bf.is_null() {
//         JS_FreeValue(ctx, func_obj);
//         return JSValue::EXCEPTION;
//     }

//     (*bf).func_obj = this_val.dup();
//     (*bf).this_val = (*argv.offset(0)).dup();
//     (*bf).argc = arg_count;
//     for i in 0..arg_count {
//         *(*bf).argv.as_mut_ptr().add(i as usize) = JS_DupValue(ctx, *argv.offset((i + 1) as isize));
//     }
//     (*p).u.bound_function = bf;

//     /* XXX: the spec could be simpler by only using GetOwnProperty */
//     let ret = JS_GetOwnProperty(ctx, std::ptr::null_mut(), this_val, JS_ATOM_length);
//     if ret < 0 {
//         JS_FreeValue(ctx, func_obj);
//         return JSValue::EXCEPTION;
//     }

//     let len_val = if ret == 0 {
//         JSValue::ZERO
//     } else {
//         let length = this_val.get_property(ctx.as_mut().unwrap(), JS_ATOM_length);
//         match JS_VALUE_GET_NORM_TAG(length) {
//             JSTag::Int => {
//                 let n = JS_VALUE_GET_INT(length);
//                 JSValue::from_i32(std::cmp::max(0, n - arg_count))
//             }
//             JSTag::Float64 => {
//                 let mut d = JS_VALUE_GET_FLOAT64(length);
//                 if d.is_nan() {
//                     d = 0.0;
//                 } else {
//                     d = d.trunc();
//                     if d <= arg_count as f64 {
//                         d = 0.0;
//                     } else {
//                         d -= arg_count as f64; /* also converts -0 to +0 */
//                     }
//                 }
//                 JSValue::from_f64(d)
//             }
//             JSTag::Exception => {
//                 JS_FreeValue(ctx, func_obj);
//                 return JSValue::EXCEPTION;
//             }
//             _ => {
//                 JS_FreeValue(ctx, length);
//                 JSValue::ZERO
//             }
//         }
//     };

//     func_obj
//         .define_property_value(
//             ctx.as_mut().unwrap(),
//             JS_ATOM_length,
//             len_val,
//             JS_PROP_CONFIGURABLE,
//         )
//         .unwrap();

//     let mut name = this_val.get_property(ctx.as_mut().unwrap(), JS_ATOM_name);
//     if name.is_exception() {
//         JS_FreeValue(ctx, func_obj);
//         return JSValue::EXCEPTION;
//     }
//     if !JS_IsString(name) {
//         JS_FreeValue(ctx, name);
//         name = JS_ATOM_empty_string.to_value(ctx.as_mut().unwrap(), true);
//     }

//     // make bound function name.
//     let bf_name = JS_ConcatString3(ctx, c"bound ", name, c"");
//     if bf_name.is_exception() {
//         JS_FreeValue(ctx, func_obj);
//         return JSValue::EXCEPTION;
//     }

//     func_obj
//         .define_property_value(
//             ctx.as_mut().unwrap(),
//             JS_ATOM_name,
//             bf_name,
//             JS_PROP_CONFIGURABLE,
//         )
//         .unwrap();

//     func_obj
// }

// pub unsafe fn js_function_toString(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     if check_function(ctx, this_val).is_err() {
//         return JSValue::EXCEPTION;
//     }

//     let p = JS_VALUE_GET_OBJ(this_val);

//     let func_kind = if js_class_has_bytecode((*p).class_id()) {
//         let b = (*p).u.func.function_bytecode;
//         if (*b).has_debug && !(*b).debug.source.is_null() {
//             return (*ctx).create_string(
//                 std::str::from_utf8_unchecked(std::slice::from_raw_parts(
//                     (*b).debug.source,
//                     (*b).debug.source_len as _,
//                 ))
//                 .as_ref(),
//             );
//         }
//         (*b).func_kind
//     } else {
//         JSFunctionKindEnum::JS_FUNC_NORMAL
//     };

//     {
//         let pref = match func_kind {
//             JSFunctionKindEnum::JS_FUNC_GENERATOR => c"function *",
//             JSFunctionKindEnum::JS_FUNC_ASYNC => c"async function ",
//             JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR => c"async function *",
//             _ => c"function ",
//         };
//         let suff = c"() {\n    [native code]\n}";
//         let mut name = this_val.get_property(ctx.as_mut().unwrap(), JS_ATOM_name);
//         if name.is_undefined() {
//             name = JS_ATOM_empty_string.to_value(ctx.as_mut().unwrap(), true);
//         }

//         JS_ConcatString3(ctx, pref, name, suff)
//     }
// }

pub fn js_function_toString(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    args.this.ensure_callable(ctx)?;

    let p = unsafe { args.this.object() };

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
    let mut name = args.this.get_property(ctx, JS_ATOM_name);
    if name.is_undefined() {
        name = unsafe { JS_ATOM_empty_string.to_value(ctx, true) };
    }

    let rv = unsafe { JS_ConcatString3(ctx, pref, name, suff) };
    if rv.is_exception() {
        Err(-1)
    } else {
        Ok(rv)
    }
}

// pub unsafe fn js_function_hasInstance(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
// ) -> JSValue {
//     if let Ok(b) = JS_OrdinaryIsInstanceOf(ctx, (*argv.offset(0)), this_val) {
//         JS_NewBool(b)
//     } else {
//         JSValue::EXCEPTION
//     }
// }

pub fn js_function_hasInstance(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let b = unsafe { JS_OrdinaryIsInstanceOf(ctx, args.get_raw(0), args.this)? };
    Ok(if b { JSValue::TRUE } else { JSValue::FALSE })
}

// pub unsafe fn js_function_proto_fileName(ctx: *mut JSContext, this_val: JSValueConst) -> JSValue {
//     let b = JS_GetFunctionBytecode(this_val);
//     if !b.is_null() && (*b).has_debug {
//         (*b).debug.filename.to_value(ctx.as_mut().unwrap(), true)
//     } else {
//         JSValue::UNDEFINED
//     }
// }

pub fn js_function_proto_fileName(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let b = unsafe { JS_GetFunctionBytecode(args.this) };

    unsafe {
        if !b.is_null() && (*b).has_debug {
            (*b).debug.filename.to_value(ctx, true).check_exception()
        } else {
            Ok(JSValue::UNDEFINED)
        }
    }
}

// pub unsafe fn js_function_proto_lineNumber(ctx: *mut JSContext, this_val: JSValueConst) -> JSValue {
//     let b = JS_GetFunctionBytecode(this_val);
//     if !b.is_null() && (*b).has_debug {
//         JSValue::from_i32((*b).debug.line_num as _)
//     } else {
//         JSValue::UNDEFINED
//     }
// }

pub fn js_function_proto_lineNumber(_: &mut JSContext, args: CallArgs) -> JSResult {
    let b = unsafe { JS_GetFunctionBytecode(args.this) };

    unsafe {
        if !b.is_null() && (*b).has_debug {
            Ok(JSValue::from_i32((*b).debug.line_num as _))
        } else {
            Ok(JSValue::UNDEFINED)
        }
    }
}

// XXX: add a specific eval mode so that Function("}), ({") is rejected
pub unsafe fn js_function_constructor(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    debug_assert!((0..4).contains(&magic));
    let func_kind = std::mem::transmute::<u8, JSFunctionKindEnum>(magic as u8);

    let mut buf = SmallVec::<[u8; 64]>::new();
    buf.push(b'(');

    if matches!(
        func_kind,
        JSFunctionKindEnum::JS_FUNC_ASYNC | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
    ) {
        buf.extend_from_slice(b"async ");
    }
    buf.extend_from_slice(b"function");

    if matches!(
        func_kind,
        JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
    ) {
        buf.push(b'*');
    }
    buf.extend_from_slice(b" anonymous(");

    let n = argc - 1;
    for i in 0..n {
        if i != 0 {
            buf.push(b',');
        }

        let v = *argv.offset(i as _);
        if let Ok(sv) = v.to_utf8_string(ctx.as_mut().unwrap()) {
            buf.extend_from_slice(sv.as_bytes());
        } else {
            return JSValue::EXCEPTION;
        }
    }

    buf.extend_from_slice(b") {\n");
    if n >= 0 {
        if let Ok(sv) = (*argv.offset(n as _)).to_utf8_string(ctx.as_mut().unwrap()) {
            buf.extend_from_slice(sv.as_bytes());
        } else {
            return JSValue::EXCEPTION;
        }
    }
    buf.extend_from_slice(b"\n})");

    // create script string
    let src = (*ctx).create_string_from_utf8(buf.to_vec());
    if src.is_exception() {
        return JSValue::EXCEPTION;
    }

    // eval script
    let obj = JS_EvalObject(ctx, (*ctx).global_obj, src, JS_EVAL_TYPE_INDIRECT, -1);
    JS_FreeValue(ctx, src);

    if JS_IsException(obj) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    if !JS_IsUndefined(new_target) {
        // set the prototype
        let mut proto = new_target.get_property(ctx.as_mut().unwrap(), JS_ATOM_prototype);
        if proto.is_exception() {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
        if !proto.is_object() {
            let realm = JS_GetFunctionRealm(ctx, new_target);
            if realm.is_null() {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
            proto = (*realm).class_proto[func_kind_to_class_id[func_kind as usize] as usize].dup();
        }

        let ret = JS_SetPrototypeInternal(ctx, obj, proto, true);
        JS_FreeValue(ctx, proto);
        if ret.is_err() {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    obj
}

pub const js_function_proto_funcs: [JSCFunctionListEntry; 7] = [
    JS_CFUNC_NATIVE_DEF!(c"call", 1, js_function_call, 0),
    JS_CFUNC_NATIVE_DEF!(c"apply", 2, js_function_apply, 0),
    JS_CFUNC_NATIVE_DEF!(c"bind", 1, js_function_bind, 0),
    JS_CFUNC_NATIVE_DEF!(c"toString", 0, js_function_toString, 0),
    JS_CFUNC_NATIVE_DEF!(c"[Symbol.hasInstance]", 1, js_function_hasInstance, 0),
    JS_CGETSET_NATIVE_DEF!(c"fileName", js_function_proto_fileName, NONE, 0),
    JS_CGETSET_NATIVE_DEF!(c"lineNumber", js_function_proto_lineNumber, NONE, 0),
    // JS_CFUNC_DEF!(c"call", 1, js_function_call),
    // JS_CFUNC_MAGIC_DEF!(c"apply", 2, js_function_apply_old, 0),
    // JS_CFUNC_DEF!(c"bind", 1, js_function_bind),
    // JS_CFUNC_DEF!(c"toString", 0, js_function_toString),
    // JS_CFUNC_DEF!(c"[Symbol.hasInstance]", 1, js_function_hasInstance),
    // JS_CGETSET_DEF!(c"fileName", js_function_proto_fileName, NONE),
    // JS_CGETSET_DEF!(c"lineNumber", js_function_proto_lineNumber, NONE),
];
