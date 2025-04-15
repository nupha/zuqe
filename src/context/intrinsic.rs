use crate::{
    atom::{consts::*, JS_NewAtomString},
    class::{JS_NewGlobalCConstructor2, JS_SetConstructor2},
    context::{JSContext, JS_MODE_STRICT},
    error::{js_error_proto_funcs, JSErrorEnum, JS_NATIVE_ERROR_COUNT, NATIVE_ERROR_NAMES},
    eval::__JS_EvalInternal,
    function::{
        js_function_proto, JSCFunction, JSCFunctionEnum, JSCFunctionMagic, JSFunctionKindEnum,
        JS_GetFunctionBytecode, JS_NewCFunction, JS_NewCFunction2, JS_NewCFunction3,
        JS_NewCFunctionMagic, JS_SetPropertyFunctionList,
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
        promise::{
            js_async_from_sync_iterator_proto_funcs, js_async_function_proto_funcs,
            js_async_generator_function_proto_funcs, js_async_generator_proto_funcs,
            js_async_iterator_proto_funcs, js_promise_funcs, js_promise_proto_funcs,
        },
        string::{
            js_string_constructor, js_string_funcs, js_string_iterator_proto_funcs,
            js_string_proto_funcs,
        },
    },
    promise::js_promise_constructor,
    value::{
        object::{
            get_proto_obj, js_new_shape2, js_object_seal, JS_NewObject, JS_NewObjectProto,
            JS_NewObjectProtoClass, JS_SetObjectData, JS_PROP_CONFIGURABLE, JS_PROP_C_W_E,
            JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_INITIAL_HASH_SIZE,
            JS_PROP_LENGTH, JS_PROP_WRITABLE,
        },
        JSValue, JSValueConst, JS_FreeValue,
    },
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_ITERATOR, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR,
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BOOLEAN, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_ERROR, JS_CLASS_GENERATOR,
    JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_PROMISE,
    JS_CLASS_STRING, JS_CLASS_STRING_ITERATOR, JS_CLASS_SYMBOL, STD_CLASS_DEFS,
};

impl JSContext {
    /// Add minimum amount of objects to be able to compile code and display error messages.
    /// No JSAtom should be allocated by this function.
    pub(super) fn add_core_objects(&mut self) {
        let object_proto = unsafe { JS_NewObjectProto(self, JSValue::NULL) };

        // Object
        self.class_proto[JS_CLASS_OBJECT as usize] = object_proto;

        // Function
        {
            self.function_proto = self
                .create_native_function_with_proto(
                    js_function_proto,
                    "",
                    0,
                    JSCFunctionEnum::NativeFunc,
                    0,
                    object_proto,
                )
                .unwrap();

            self.class_proto[JS_CLASS_BYTECODE_FUNCTION as usize] = self.function_proto.dup();
        }

        // Error
        unsafe {
            let err_proto = JS_NewObject(self);
            JS_SetPropertyFunctionList(
                self,
                err_proto,
                js_error_proto_funcs.as_ptr(),
                js_error_proto_funcs.len(),
            );
            self.class_proto[JS_CLASS_ERROR as usize] = err_proto;

            // standard Errors
            for i in 0..JS_NATIVE_ERROR_COUNT {
                let proto = JS_NewObjectProto(self, err_proto);

                let name = JS_NewAtomString(self, NATIVE_ERROR_NAMES[i]);
                proto
                    .define_property_value(
                        self,
                        JS_ATOM_name,
                        name,
                        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                    )
                    .unwrap();

                let msg = JS_ATOM_empty_string.to_value(self, true);
                proto
                    .define_property_value(
                        self,
                        JS_ATOM_message,
                        msg,
                        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                    )
                    .unwrap();

                self.native_error_proto[i as usize] = proto;
            }
        }

        // Array
        unsafe {
            // the array prototype is an array
            let array_proto = JS_NewObjectProtoClass(self, object_proto, JS_CLASS_ARRAY);
            self.class_proto[JS_CLASS_ARRAY as usize] = array_proto;

            self.array_shape = js_new_shape2(
                self,
                get_proto_obj(array_proto),
                JS_PROP_INITIAL_HASH_SIZE,
                1,
            );

            // add length property as #0
            self.array_shape.as_mut().unwrap().add_shape_property(
                self,
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

    unsafe fn intrinsic_throw_error(&mut self) {
        // throw TypeError()
        self.throw_type_error = JS_NewCFunction(self, js_throw_type_error, c"", 0);

        // add caller and arguments properties to throw a TypeError
        let obj1 = JS_NewCFunction(self, js_function_proto_caller, c"", 0);
        self.function_proto
            .define_property(
                self,
                JS_ATOM_caller,
                JSValue::EXCEPTION,
                obj1,
                self.throw_type_error,
                JS_PROP_HAS_GET | JS_PROP_HAS_SET | JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        self.function_proto
            .define_property(
                self,
                JS_ATOM_arguments,
                JSValue::EXCEPTION,
                obj1,
                self.throw_type_error,
                JS_PROP_HAS_GET | JS_PROP_HAS_SET | JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        JS_FreeValue(self, obj1);
        JS_FreeValue(
            self,
            js_object_seal(self, JSValue::EXCEPTION, 1, &mut self.throw_type_error, 1),
        );
    }

    unsafe fn intrinsic_object(&mut self) {
        let obj_ctor = self.create_global_constructor(
            c"Object",
            js_object_constructor,
            1,
            self.class_proto[JS_CLASS_OBJECT as usize],
        );
        JS_SetPropertyFunctionList(
            self,
            obj_ctor,
            js_object_funcs.as_ptr(),
            js_object_funcs.len(),
        );

        JS_SetPropertyFunctionList(
            self,
            self.class_proto[JS_CLASS_OBJECT as usize],
            js_object_proto_funcs.as_ptr(),
            js_object_proto_funcs.len(),
        );
    }

    unsafe fn intrinsic_function(&mut self) {
        JS_SetPropertyFunctionList(
            self,
            self.function_proto,
            js_function_proto_funcs.as_ptr(),
            js_function_proto_funcs.len(),
        );
        self.function_ctor = JS_NewCFunctionMagic(
            self,
            js_function_constructor,
            c"Function",
            1,
            JSCFunctionEnum::ConstructorOrFuncMagic,
            JSFunctionKindEnum::JS_FUNC_NORMAL as _,
        );
        self.add_global_constructor("Function", self.function_ctor.dup(), self.function_proto);
    }

    unsafe fn intrinsic_error(&mut self) {
        let error_ctor = self
            .create_native_function(
                js_error_constructor,
                "Error",
                1,
                JSCFunctionEnum::NativeConstructorOrFunc,
                -1,
            )
            .unwrap();

        self.add_global_constructor(
            "Error",
            error_ctor,
            self.class_proto[JS_CLASS_ERROR as usize],
        );

        for i in 0..JS_NATIVE_ERROR_COUNT {
            let n_args = 1 + (i == JSErrorEnum::Aggregate as usize) as i32;

            let func_obj = self
                .create_native_function_with_proto(
                    js_error_constructor,
                    NATIVE_ERROR_NAMES[i as usize].to_string_lossy().as_ref(),
                    n_args as _,
                    JSCFunctionEnum::NativeConstructorOrFunc,
                    i as _,
                    error_ctor,
                )
                .unwrap();

            self.add_global_constructor(
                NATIVE_ERROR_NAMES[i as usize].to_string_lossy().as_ref(),
                func_obj,
                self.native_error_proto[i as usize],
            );
        }
    }

    unsafe fn intrinsic_array(&mut self) {
        JS_SetPropertyFunctionList(
            self,
            self.class_proto[JS_CLASS_ARRAY as usize],
            js_array_proto_funcs.as_ptr(),
            js_array_proto_funcs.len(),
        );

        let obj = self.create_global_constructor(
            c"Array",
            js_array_constructor,
            1,
            self.class_proto[JS_CLASS_ARRAY as usize],
        );
        self.array_ctor = obj.dup();
        JS_SetPropertyFunctionList(self, obj, js_array_funcs.as_ptr(), js_array_funcs.len());

        // XXX: create auto_initializer
        {
            // initialize Array.prototype[Symbol.unscopables]
            let unscopables = [
                "copyWithin",
                "entries",
                "fill",
                "find",
                "findIndex",
                "findLast",
                "findLastIndex",
                "flat",
                "flatMap",
                "includes",
                "keys",
                "toReversed",
                "toSorted",
                "toSpliced",
                "values",
            ];
            let obj1 = JS_NewObjectProto(self, JSValue::NULL);
            for s in unscopables {
                obj1.define_property_value_str(self, s, JSValue::TRUE, JS_PROP_C_W_E)
                    .unwrap();
            }

            self.class_proto[JS_CLASS_ARRAY as usize]
                .define_property_value(self, JS_ATOM_Symbol_unscopables, obj1, JS_PROP_CONFIGURABLE)
                .unwrap();
        }

        // needed to initialize arguments[Symbol.iterator]
        self.array_proto_values =
            self.class_proto[JS_CLASS_ARRAY as usize].get_property(self, JS_ATOM_values);

        self.class_proto[JS_CLASS_ARRAY_ITERATOR as usize] =
            JS_NewObjectProto(self, self.iterator_proto);

        JS_SetPropertyFunctionList(
            self,
            self.class_proto[JS_CLASS_ARRAY_ITERATOR as usize],
            js_array_iterator_proto_funcs.as_ptr(),
            js_array_iterator_proto_funcs.len(),
        );
    }

    unsafe fn intrinsic_number(&mut self) {
        let number_proto = JS_NewObjectProtoClass(
            self,
            self.class_proto[JS_CLASS_OBJECT as usize],
            JS_CLASS_NUMBER,
        );
        JS_SetObjectData(self, number_proto, JSValue::ZERO);
        self.class_proto[JS_CLASS_NUMBER as usize] = number_proto;
        JS_SetPropertyFunctionList(
            self,
            number_proto,
            js_number_proto_funcs.as_ptr(),
            js_number_proto_funcs.len(),
        );

        let number_ctor =
            self.create_global_constructor(c"Number", js_number_constructor, 1, number_proto);
        JS_SetPropertyFunctionList(
            self,
            number_ctor,
            js_number_funcs.as_ptr(),
            js_number_funcs.len(),
        );
    }

    unsafe fn intrinsic_boolean(&mut self) {
        let bool_proto = JS_NewObjectProtoClass(
            self,
            self.class_proto[JS_CLASS_OBJECT as usize],
            JS_CLASS_BOOLEAN,
        );
        self.class_proto[JS_CLASS_BOOLEAN as usize] = bool_proto;
        JS_SetObjectData(self, bool_proto, JSValue::FALSE);
        JS_SetPropertyFunctionList(
            self,
            bool_proto,
            js_boolean_proto_funcs.as_ptr(),
            js_boolean_proto_funcs.len(),
        );

        self.create_global_constructor(c"Boolean", js_boolean_constructor, 1, bool_proto);
    }

    unsafe fn intrinsic_string(&mut self) {
        let string_proto = JS_NewObjectProtoClass(
            self,
            self.class_proto[JS_CLASS_OBJECT as usize],
            JS_CLASS_STRING,
        );
        self.class_proto[JS_CLASS_STRING as usize] = string_proto;
        JS_SetObjectData(
            self,
            string_proto,
            JS_ATOM_empty_string.to_value(self, true),
        );
        JS_SetPropertyFunctionList(
            self,
            string_proto,
            js_string_proto_funcs.as_ptr(),
            js_string_proto_funcs.len(),
        );

        let string_ctor = self.create_global_constructor(
            c"String",
            js_string_constructor,
            1,
            self.class_proto[JS_CLASS_STRING as usize],
        );
        JS_SetPropertyFunctionList(
            self,
            string_ctor,
            js_string_funcs.as_ptr(),
            js_string_funcs.len(),
        );

        self.class_proto[JS_CLASS_STRING_ITERATOR as usize] =
            JS_NewObjectProto(self, self.iterator_proto);
        JS_SetPropertyFunctionList(
            self,
            self.class_proto[JS_CLASS_STRING_ITERATOR as usize],
            js_string_iterator_proto_funcs.as_ptr(),
            js_string_iterator_proto_funcs.len(),
        );
    }

    unsafe fn intrinsic_iterator(&mut self) {
        // self.iterator_proto = JS_NewObject(ctx);
        // JS_SetPropertyFunctionList(
        //     self,
        //     self.iterator_proto,
        //     js_iterator_proto_funcs.as_ptr(),
        //     std::mem::size_of_val(js_iterator_proto_funcs) / std::mem::size_of::<JSFunction>(),
        // );
    }

    unsafe fn intrinsic_eval(&mut self) {
        self.eval_internal.replace(__JS_EvalInternal);

        //JS_NewCFunction(self, js_global_eval, c"eval", 1);
        self.eval_func = self
            .create_native_function(js_global_eval, "eval", 1, JSCFunctionEnum::NativeFunc, 0)
            .unwrap();

        self.global_obj
            .define_property_value(
                self,
                JS_ATOM_eval,
                self.eval_func.dup(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }

    pub unsafe fn intrinsic_base_objects(&mut self) {
        // make global object - this must come first
        {
            self.global_obj = JS_NewObject(self);
            self.global_var_obj = JS_NewObjectProto(self, JSValue::NULL);
        }

        // throw TypeError()
        self.intrinsic_throw_error();

        self.intrinsic_object(); // Object
        self.intrinsic_function(); // Function
        self.intrinsic_error(); // Error
        self.intrinsic_iterator(); // Iterator prototype
        self.intrinsic_array(); // Array

        self.intrinsic_number(); // Number
        self.intrinsic_boolean(); // Boolean
        self.intrinsic_string(); // String

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

        self.intrinsic_eval(); // enable `eval()` function

        //
        // define global properties (isNan, parseInt, undefined, NaN etc)
        //
        JS_SetPropertyFunctionList(
            self,
            self.global_obj,
            js_global_props.as_ptr(),
            js_global_props.len(),
        );
        // globalThis
        self.global_obj
            .define_property_value(
                self,
                JS_ATOM_globalThis,
                self.global_obj.dup(),
                JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
            )
            .unwrap();
    }

    pub unsafe fn intrinsic_async(&mut self) {
        let rt = self.rt_mut();
        debug_assert!(rt.is_class_registered(JS_CLASS_PROMISE));

        // Promise
        self.class_proto[JS_CLASS_PROMISE as usize] = JS_NewObject(self);
        JS_SetPropertyFunctionList(
            self,
            self.class_proto[JS_CLASS_PROMISE as usize],
            js_promise_proto_funcs.as_ptr(),
            js_promise_proto_funcs.len(),
        );
        let obj1 = JS_NewCFunction2(
            self,
            js_promise_constructor,
            c"Promise",
            1,
            JSCFunctionEnum::Constructor,
            0,
        );
        self.promise_ctor = obj1.dup();
        JS_SetPropertyFunctionList(
            self,
            obj1,
            js_promise_funcs.as_ptr(),
            js_promise_funcs.len(),
        );
        JS_NewGlobalCConstructor2(
            self,
            obj1,
            c"Promise",
            self.class_proto[JS_CLASS_PROMISE as usize],
        );

        // AsyncFunction
        self.class_proto[JS_CLASS_ASYNC_FUNCTION as usize] =
            JS_NewObjectProto(self, self.function_proto);
        let obj1 = JS_NewCFunction3(
            self,
            std::mem::transmute::<JSCFunctionMagic, JSCFunction>(js_function_constructor),
            c"AsyncFunction",
            1,
            JSCFunctionEnum::ConstructorOrFuncMagic,
            JSFunctionKindEnum::JS_FUNC_ASYNC as i16,
            self.function_ctor,
        );
        JS_SetPropertyFunctionList(
            self,
            self.class_proto[JS_CLASS_ASYNC_FUNCTION as usize],
            js_async_function_proto_funcs.as_ptr(),
            js_async_function_proto_funcs.len(),
        );
        JS_SetConstructor2(
            self,
            obj1,
            self.class_proto[JS_CLASS_ASYNC_FUNCTION as usize],
            0,
            JS_PROP_CONFIGURABLE,
        );
        JS_FreeValue(self, obj1);

        // AsyncIteratorPrototype
        self.async_iterator_proto = JS_NewObject(self);
        JS_SetPropertyFunctionList(
            self,
            self.async_iterator_proto,
            js_async_iterator_proto_funcs.as_ptr(),
            js_async_iterator_proto_funcs.len(),
        );

        // AsyncFromSyncIteratorPrototype
        self.class_proto[JS_CLASS_ASYNC_FROM_SYNC_ITERATOR as usize] =
            JS_NewObjectProto(self, self.async_iterator_proto);
        JS_SetPropertyFunctionList(
            self,
            self.class_proto[JS_CLASS_ASYNC_FROM_SYNC_ITERATOR as usize],
            js_async_from_sync_iterator_proto_funcs.as_ptr(),
            js_async_from_sync_iterator_proto_funcs.len(),
        );

        // AsyncGeneratorPrototype
        self.class_proto[JS_CLASS_ASYNC_GENERATOR as usize] =
            JS_NewObjectProto(self, self.async_iterator_proto);
        JS_SetPropertyFunctionList(
            self,
            self.class_proto[JS_CLASS_ASYNC_GENERATOR as usize],
            js_async_generator_proto_funcs.as_ptr(),
            js_async_generator_proto_funcs.len(),
        );

        // AsyncGeneratorFunction
        self.class_proto[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize] =
            JS_NewObjectProto(self, self.function_proto);
        let obj1 = JS_NewCFunction3(
            self,
            std::mem::transmute::<JSCFunctionMagic, JSCFunction>(js_function_constructor),
            c"AsyncGeneratorFunction",
            1,
            JSCFunctionEnum::ConstructorOrFuncMagic,
            JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR as i16,
            self.function_ctor,
        );
        JS_SetPropertyFunctionList(
            self,
            self.class_proto[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize],
            js_async_generator_function_proto_funcs.as_ptr(),
            js_async_generator_function_proto_funcs.len(),
        );
        JS_SetConstructor2(
            self,
            self.class_proto[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize],
            self.class_proto[JS_CLASS_ASYNC_GENERATOR as usize],
            JS_PROP_CONFIGURABLE,
            JS_PROP_CONFIGURABLE,
        );
        JS_SetConstructor2(
            self,
            obj1,
            self.class_proto[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize],
            0,
            JS_PROP_CONFIGURABLE,
        );
        JS_FreeValue(self, obj1);
    }
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
