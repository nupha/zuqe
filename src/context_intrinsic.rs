use std::ffi::CStr;

use crate::{
    atom::{consts::*, JS_NewAtomString},
    class::{JS_NewGlobalCConstructor, JS_NewGlobalCConstructor2},
    context::{JSContext, JS_MODE_STRICT},
    error::{js_error_proto_funcs, native_error_name, JSErrorEnum, JS_NATIVE_ERROR_COUNT},
    function::{
        js_function_proto, JSCFunction, JSCFunctionEnum, JSCFunctionMagic, JSFunctionKindEnum,
        JS_GetFunctionBytecode, JS_NewCFunction, JS_NewCFunction3, JS_NewCFunctionMagic,
        JS_SetPropertyFunctionList,
    },
    js_core::{
        array::{
            js_array_constructor, js_array_funcs, js_array_iterator_proto_funcs,
            js_array_proto_funcs,
        },
        boolean::{js_boolean_constructor, js_boolean_proto_funcs},
        error::js_error_constructor,
        function::{js_function_constructor, js_function_proto_funcs},
        global::{js_global_eval, js_global_props},
        number::{js_number_constructor, js_number_funcs, js_number_proto_funcs},
        object::{js_object_constructor, js_object_funcs, js_object_proto_funcs},
        string::{
            js_string_constructor, js_string_funcs, js_string_iterator_proto_funcs,
            js_string_proto_funcs,
        },
    },
    value::{
        object::{
            get_proto_obj, js_new_shape2, js_object_seal, JS_DefinePropertyValueStr,
            JS_GetProperty, JS_NewObject, JS_NewObjectProto, JS_NewObjectProtoClass,
            JS_SetObjectData, JS_PROP_CONFIGURABLE, JS_PROP_C_W_E, JS_PROP_HAS_CONFIGURABLE,
            JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_INITIAL_HASH_SIZE, JS_PROP_LENGTH,
            JS_PROP_WRITABLE,
        },
        JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_NewInt32,
    },
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_ITERATOR, JS_CLASS_BOOLEAN, JS_CLASS_BYTECODE_FUNCTION,
    JS_CLASS_ERROR, JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_NUMBER,
    JS_CLASS_OBJECT, JS_CLASS_STRING, JS_CLASS_STRING_ITERATOR, JS_CLASS_SYMBOL,
};

// Minimum amount of objects to be able to compile code and display
// error messages. No JSAtom should be allocated by this function.
pub unsafe fn JS_AddIntrinsicBasicObjects(ctx: &mut JSContext) {
    let object_proto = JS_NewObjectProto(ctx, JSValue::NULL);

    // Object
    ctx.class_proto[JS_CLASS_OBJECT as usize] = object_proto;

    // Function
    {
        ctx.function_proto = JS_NewCFunction3(
            ctx,
            js_function_proto,
            c"",
            0,
            JSCFunctionEnum::Generic,
            0,
            ctx.class_proto[JS_CLASS_OBJECT as usize],
        );
        ctx.class_proto[JS_CLASS_BYTECODE_FUNCTION as usize] = (*ctx).function_proto.dup();
    }

    // Error
    {
        let err_proto = JS_NewObject(ctx);
        JS_SetPropertyFunctionList(
            ctx,
            err_proto,
            js_error_proto_funcs.as_ptr(),
            js_error_proto_funcs.len(),
        );
        ctx.class_proto[JS_CLASS_ERROR as usize] = err_proto;

        // standard Errors
        for i in 0..JS_NATIVE_ERROR_COUNT {
            let proto = JS_NewObjectProto(ctx, err_proto);

            let name = JS_NewAtomString(ctx, native_error_name[i]);
            proto
                .define_property_value(
                    ctx,
                    JS_ATOM_name,
                    name,
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

            let msg = JS_ATOM_empty_string.to_value(ctx, true);
            proto
                .define_property_value(
                    ctx,
                    JS_ATOM_message,
                    msg,
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

            ctx.native_error_proto[i as usize] = proto;
        }
    }

    // Array
    {
        // the array prototype is an array
        ctx.class_proto[JS_CLASS_ARRAY as usize] =
            JS_NewObjectProtoClass(ctx, object_proto, JS_CLASS_ARRAY);
        ctx.array_shape = js_new_shape2(
            ctx,
            get_proto_obj(ctx.class_proto[JS_CLASS_ARRAY as usize]),
            JS_PROP_INITIAL_HASH_SIZE,
            1,
        );
        ctx.array_shape.as_mut().unwrap().add_shape_property(
            ctx,
            None,
            JS_ATOM_length,
            JS_PROP_WRITABLE | JS_PROP_LENGTH,
        );
    }

    /* XXX: could test it on first context creation to ensure that no
    new atoms are created in JS_AddIntrinsicBasicObjects(). It is
    necessary to avoid useless renumbering of atoms after
    JS_EvalBinary() if it is done just after JS_AddIntrinsicBasicObjects(). */
    // assert_eq!((*(*ctx).rt).atom_count as u32, JS_ATOM_INIT_COUNT);
}

pub unsafe fn JS_AddIntrinsicBaseObjects(ctx: *mut JSContext) {
    // throw TypeError()
    {
        (*ctx).throw_type_error = JS_NewCFunction(ctx, js_throw_type_error, c"", 0);

        // add caller and arguments properties to throw a TypeError
        let obj1 = JS_NewCFunction(ctx, js_function_proto_caller, c"", 0);
        (*ctx)
            .function_proto
            .define_property(
                ctx.as_mut().unwrap(),
                JS_ATOM_caller,
                JSValue::EXCEPTION,
                obj1,
                (*ctx).throw_type_error,
                JS_PROP_HAS_GET | JS_PROP_HAS_SET | JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
        (*ctx)
            .function_proto
            .define_property(
                ctx.as_mut().unwrap(),
                JS_ATOM_arguments,
                JSValue::EXCEPTION,
                obj1,
                (*ctx).throw_type_error,
                JS_PROP_HAS_GET | JS_PROP_HAS_SET | JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        JS_FreeValue(ctx, obj1);
        JS_FreeValue(
            ctx,
            js_object_seal(ctx, JSValue::EXCEPTION, 1, &mut (*ctx).throw_type_error, 1),
        );
    }

    // global
    (*ctx).global_var_obj = JS_NewObjectProto(ctx, JSValue::NULL);
    (*ctx).global_obj = JS_NewObject(ctx);

    // Object
    {
        let obj_ctor = JS_NewGlobalCConstructor(
            ctx,
            c"Object",
            js_object_constructor,
            1,
            (*ctx).class_proto[JS_CLASS_OBJECT as usize],
        );
        JS_SetPropertyFunctionList(
            ctx,
            obj_ctor,
            js_object_funcs.as_ptr(),
            js_object_funcs.len(),
        );

        JS_SetPropertyFunctionList(
            ctx,
            (*ctx).class_proto[JS_CLASS_OBJECT as usize],
            js_object_proto_funcs.as_ptr(),
            js_object_proto_funcs.len(),
        );
    }

    // Function
    {
        JS_SetPropertyFunctionList(
            ctx,
            (*ctx).function_proto,
            js_function_proto_funcs.as_ptr(),
            js_function_proto_funcs.len(),
        );
        (*ctx).function_ctor = JS_NewCFunctionMagic(
            ctx,
            js_function_constructor,
            c"Function",
            1,
            JSCFunctionEnum::ConstructorOrFuncMagic,
            JSFunctionKindEnum::JS_FUNC_NORMAL as _,
        );
        JS_NewGlobalCConstructor2(
            ctx,
            JS_DupValue(ctx, (*ctx).function_ctor),
            c"Function",
            (*ctx).function_proto,
        );
    }

    // Error
    {
        let obj1 = JS_NewCFunctionMagic(
            ctx,
            js_error_constructor,
            c"Error",
            1,
            JSCFunctionEnum::ConstructorOrFuncMagic,
            -1,
        );
        JS_NewGlobalCConstructor2(
            ctx,
            obj1,
            c"Error",
            (*ctx).class_proto[JS_CLASS_ERROR as usize],
        );

        let f: JSCFunctionMagic = js_error_constructor;
        let g: JSCFunction = *(f as *mut JSCFunction);
        for i in 0..JS_NATIVE_ERROR_COUNT {
            let n_args = 1 + (i == JSErrorEnum::Aggregate as usize) as i32;
            let func_obj = JS_NewCFunction3(
                ctx,
                g,
                native_error_name[i as usize],
                n_args as _,
                JSCFunctionEnum::ConstructorOrFuncMagic,
                i as _,
                obj1,
            );

            JS_NewGlobalCConstructor2(
                ctx,
                func_obj,
                native_error_name[i as usize],
                (*ctx).native_error_proto[i as usize],
            );
        }
    }

    // // Iterator prototype
    // (*ctx).iterator_proto = JS_NewObject(ctx);
    // JS_SetPropertyFunctionList(
    //     ctx,
    //     (*ctx).iterator_proto,
    //     js_iterator_proto_funcs.as_ptr(),
    //     std::mem::size_of_val(js_iterator_proto_funcs) / std::mem::size_of::<JSFunction>(),
    // );

    // Array
    {
        JS_SetPropertyFunctionList(
            ctx,
            (*ctx).class_proto[JS_CLASS_ARRAY as usize],
            js_array_proto_funcs.as_ptr(),
            js_array_proto_funcs.len(),
        );

        let obj = JS_NewGlobalCConstructor(
            ctx,
            c"Array",
            js_array_constructor,
            1,
            (*ctx).class_proto[JS_CLASS_ARRAY as usize],
        );
        (*ctx).array_ctor = obj.dup();
        JS_SetPropertyFunctionList(ctx, obj, js_array_funcs.as_ptr(), js_array_funcs.len());

        // XXX: create auto_initializer
        {
            // initialize Array.prototype[Symbol.unscopables]
            let unscopables = [
                "copyWithin\0",
                "entries\0",
                "fill\0",
                "find\0",
                "findIndex\0",
                "findLast\0",
                "findLastIndex\0",
                "flat\0",
                "flatMap\0",
                "includes\0",
                "keys\0",
                "toReversed\0",
                "toSorted\0",
                "toSpliced\0",
                "values\0",
            ];
            let obj1 = JS_NewObjectProto(ctx, JSValue::NULL);
            for s in unscopables {
                JS_DefinePropertyValueStr(
                    ctx,
                    obj1,
                    CStr::from_bytes_with_nul_unchecked(s.as_bytes()),
                    JSValue::TRUE,
                    JS_PROP_C_W_E,
                );
            }

            // let unscopables: &[u8] = b"copyWithin\0entries\0fill\0find\0findIndex\0findLast\0findLastIndex\0flat\0flatMap\0includes\0keys\0toReversed\0toSorted\0toSpliced\0values\0";
            // let mut p: *const u8 = unscopables.as_ptr();
            // let obj1 = JS_NewObjectProto(ctx, JSValue::NULL);
            // let mut p_start = p;
            // while *p != 0 {
            //     let len = libc::strlen(p as *const i8);
            //     p = p.add(len + 1);
            //     JS_DefinePropertyValueStr(
            //         ctx,
            //         obj1,
            //         CStr::from_ptr(p_start.cast()),
            //         JSValue::TRUE,
            //         JS_PROP_C_W_E,
            //     );
            //     p_start = p;
            // }

            (*ctx).class_proto[JS_CLASS_ARRAY as usize]
                .define_property_value(
                    ctx.as_mut().unwrap(),
                    JS_ATOM_Symbol_unscopables,
                    obj1,
                    JS_PROP_CONFIGURABLE,
                )
                .unwrap();
        }

        // needed to initialize arguments[Symbol.iterator]
        (*ctx).array_proto_values = JS_GetProperty(
            ctx,
            (*ctx).class_proto[JS_CLASS_ARRAY as usize],
            JS_ATOM_values,
        );

        (*ctx).class_proto[JS_CLASS_ARRAY_ITERATOR as usize] =
            JS_NewObjectProto(ctx, (*ctx).iterator_proto);
        JS_SetPropertyFunctionList(
            ctx,
            (*ctx).class_proto[JS_CLASS_ARRAY_ITERATOR as usize],
            js_array_iterator_proto_funcs.as_ptr(),
            js_array_iterator_proto_funcs.len(),
        );
    }

    //
    // define global properties (isNan, parseInt, undefined, NaN etc)
    //
    JS_SetPropertyFunctionList(
        ctx,
        (*ctx).global_obj,
        js_global_props.as_ptr(),
        js_global_props.len(),
    );

    // Number
    {
        let number_proto = JS_NewObjectProtoClass(
            ctx,
            (*ctx).class_proto[JS_CLASS_OBJECT as usize],
            JS_CLASS_NUMBER,
        );
        JS_SetObjectData(ctx, number_proto, JS_NewInt32(ctx, 0));
        (*ctx).class_proto[JS_CLASS_NUMBER as usize] = number_proto;
        JS_SetPropertyFunctionList(
            ctx,
            number_proto,
            js_number_proto_funcs.as_ptr(),
            js_number_proto_funcs.len(),
        );
        let number_obj =
            JS_NewGlobalCConstructor(ctx, c"Number", js_number_constructor, 1, number_proto);
        JS_SetPropertyFunctionList(
            ctx,
            number_obj,
            js_number_funcs.as_ptr(),
            js_number_funcs.len(),
        );
    }

    // Boolean
    {
        let bool_proto = JS_NewObjectProtoClass(
            ctx,
            (*ctx).class_proto[JS_CLASS_OBJECT as usize],
            JS_CLASS_BOOLEAN,
        );
        (*ctx).class_proto[JS_CLASS_BOOLEAN as usize] = bool_proto;
        JS_SetObjectData(ctx, bool_proto, JSValue::FALSE);
        JS_SetPropertyFunctionList(
            ctx,
            bool_proto,
            js_boolean_proto_funcs.as_ptr(),
            js_boolean_proto_funcs.len(),
        );
        JS_NewGlobalCConstructor(ctx, c"Boolean", js_boolean_constructor, 1, bool_proto);
    }

    // String
    {
        (*ctx).class_proto[JS_CLASS_STRING as usize] = JS_NewObjectProtoClass(
            ctx,
            (*ctx).class_proto[JS_CLASS_OBJECT as usize],
            JS_CLASS_STRING,
        );
        JS_SetObjectData(
            ctx,
            (*ctx).class_proto[JS_CLASS_STRING as usize],
            JS_ATOM_empty_string.to_value(ctx.as_mut().unwrap(), true),
        );
        let obj = JS_NewGlobalCConstructor(
            ctx,
            c"String",
            js_string_constructor,
            1,
            (*ctx).class_proto[JS_CLASS_STRING as usize],
        );
        JS_SetPropertyFunctionList(ctx, obj, js_string_funcs.as_ptr(), js_string_funcs.len());
        JS_SetPropertyFunctionList(
            ctx,
            (*ctx).class_proto[JS_CLASS_STRING as usize],
            js_string_proto_funcs.as_ptr(),
            js_string_proto_funcs.len(),
        );

        (*ctx).class_proto[JS_CLASS_STRING_ITERATOR as usize] =
            JS_NewObjectProto(ctx, (*ctx).iterator_proto);
        JS_SetPropertyFunctionList(
            ctx,
            (*ctx).class_proto[JS_CLASS_STRING_ITERATOR as usize],
            js_string_iterator_proto_funcs.as_ptr(),
            js_string_iterator_proto_funcs.len(),
        );
    }

    // Math: create as autoinit object
    // js_random_init(ctx);
    // JS_SetPropertyFunctionList(
    //     ctx,
    //     (*ctx).global_obj,
    //     js_math_obj.as_ptr(),
    //     std::mem::size_of_val(js_math_obj) / std::mem::size_of::<JSFunction>(),
    // );

    // ES6 Reflect: create as autoinit object
    // JS_SetPropertyFunctionList(
    //     ctx,
    //     (*ctx).global_obj,
    //     js_reflect_obj.as_ptr(),
    //     std::mem::size_of_val(js_reflect_obj) / std::mem::size_of::<JSFunction>(),
    // );

    // ES6 Symbol
    // (*ctx).class_proto[JS_CLASS_SYMBOL] = JS_NewObject(ctx);
    // JS_SetPropertyFunctionList(
    //     ctx,
    //     (*ctx).class_proto[JS_CLASS_SYMBOL],
    //     js_symbol_proto_funcs.as_ptr(),
    //     std::mem::size_of_val(js_symbol_proto_funcs) / std::mem::size_of::<JSFunction>(),
    // );
    // obj = JS_NewGlobalCConstructor(
    //     ctx,
    //     c"Symbol",
    //     js_symbol_constructor,
    //     0,
    //     (*ctx).class_proto[JS_CLASS_SYMBOL],
    // );
    // JS_SetPropertyFunctionList(
    //     ctx,
    //     obj,
    //     js_symbol_funcs.as_ptr(),
    //     std::mem::size_of_val(js_symbol_funcs) / std::mem::size_of::<JSFunction>(),
    // );
    // for i in JS_ATOM_Symbol_toPrimitive..JS_ATOM_END {
    //     let mut buf: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];
    //     let str: *const i8;
    //     let mut p: *const i8;
    //     str = JS_AtomGetStr(ctx, buf.as_mut_ptr(), std::mem::size_of_val(&buf) as u32, i);
    //     // skip "Symbol."
    //     p = strchr(str, '.' as i32);
    //     if !p.is_null() {
    //         str = p.offset(1);
    //     }
    //     JS_DefinePropertyValueStr(ctx, obj, str, JS_AtomToValue(ctx, i), 0);
    // }

    // ES6 Generator
    // (*ctx).class_proto[JS_CLASS_GENERATOR] = JS_NewObjectProto(ctx, (*ctx).iterator_proto);
    // JS_SetPropertyFunctionList(
    //     ctx,
    //     (*ctx).class_proto[JS_CLASS_GENERATOR],
    //     js_generator_proto_funcs.as_ptr(),
    //     std::mem::size_of_val(js_generator_proto_funcs) / std::mem::size_of::<JSFunction>(),
    // );

    // (*ctx).class_proto[JS_CLASS_GENERATOR_FUNCTION] = JS_NewObjectProto(ctx, (*ctx).function_proto);
    // obj1 = JS_NewCFunctionMagic(
    //     ctx,
    //     js_function_constructor,
    //     c"GeneratorFunction",
    //     1,
    //     JS_CFUNC_constructor_or_func_magic,
    //     JS_FUNC_GENERATOR,
    // );
    // JS_SetPropertyFunctionList(
    //     ctx,
    //     (*ctx).class_proto[JS_CLASS_GENERATOR_FUNCTION],
    //     js_generator_function_proto_funcs.as_ptr(),
    //     std::mem::size_of_val(js_generator_function_proto_funcs)
    //         / std::mem::size_of::<JSFunction>(),
    // );
    // JS_SetConstructor2(
    //     ctx,
    //     (*ctx).class_proto[JS_CLASS_GENERATOR_FUNCTION],
    //     (*ctx).class_proto[JS_CLASS_GENERATOR],
    //     JS_PROP_CONFIGURABLE,
    //     JS_PROP_CONFIGURABLE,
    // );
    // JS_SetConstructor2(
    //     ctx,
    //     obj1,
    //     (*ctx).class_proto[JS_CLASS_GENERATOR_FUNCTION],
    //     0,
    //     JS_PROP_CONFIGURABLE,
    // );
    // JS_FreeValue(ctx, obj1);

    //
    // global properties
    //

    (*ctx).eval_obj = JS_NewCFunction(ctx, js_global_eval, c"eval", 1);
    (*ctx)
        .global_obj
        .define_property_value(
            ctx.as_mut().unwrap(),
            JS_ATOM_eval,
            (*ctx).eval_obj.dup(),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )
        .unwrap();

    (*ctx)
        .global_obj
        .define_property_value(
            ctx.as_mut().unwrap(),
            JS_ATOM_globalThis,
            (*ctx).global_obj.dup(),
            JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
        )
        .unwrap();
}

unsafe fn js_throw_type_error(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    ctx.as_mut()
        .unwrap()
        .JS_ThrowTypeError(c"invalid property access")
}

// XXX: not 100% compatible, but mozilla seems to use a similar
// implementation to ensure that caller in non strict mode does not
// throw (ES5 compatibility)
unsafe fn js_function_proto_caller(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let b = JS_GetFunctionBytecode(this_val);
    if b.is_null() || (*b).js_mode & JS_MODE_STRICT as u8 != 0 || !(*b).has_prototype {
        return js_throw_type_error(ctx, this_val, 0, std::ptr::null_mut());
    }
    return JSValue::EXCEPTION;
}
