use std::ffi::CStr;

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_BUFFER, JS_CLASS_ARRAY_ITERATOR,
    JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR,
    JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_BOOLEAN, JS_CLASS_DATAVIEW, JS_CLASS_ERROR,
    JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_PROMISE, JS_CLASS_SHARED_ARRAY_BUFFER,
    JS_CLASS_STRING, JS_CLASS_STRING_ITERATOR, JS_CLASS_UINT8C_ARRAY,
    array::{
        js_array_constructor, js_array_funcs, js_array_iterator_proto_funcs, js_array_proto_funcs,
    },
    array_buffer::{
        js_array_buffer_constructor, js_array_buffer_funcs, js_array_buffer_proto_funcs,
        js_shared_array_buffer_constructor, js_shared_array_buffer_funcs,
        js_shared_array_buffer_proto_funcs,
    },
    atom::{JSAtom, consts::*},
    class::{JS_SetConstructor, JS_SetConstructor2, JSClassID},
    context::{JS_MODE_STRICT, JSContext},
    error::{JS_NATIVE_ERROR_COUNT, JSErrorEnum, JSResult, NATIVE_ERROR_NAMES},
    function::{
        BytecodeFunctionValue, CallContext, JSCFunctionEnum, JSCFunctionListEntry,
        JSFunctionKindEnum, js_async_function_proto_funcs,
    },
    generator::{
        js_async_generator_function_proto_funcs, js_async_generator_proto_funcs,
        js_async_iterator_proto_funcs,
    },
    iterator::js_async_from_sync_iterator_proto_funcs,
    js_core::{
        boolean::{js_boolean_constructor, js_boolean_proto_funcs},
        error::js_error_constructor,
        function::js_function_proto_funcs,
        global::{js_global_eval, js_global_props},
        object::{js_object_constructor, js_object_funcs, js_object_proto_funcs},
        string::{
            js_string_constructor, js_string_funcs, js_string_iterator_proto_funcs,
            js_string_proto_funcs,
        },
    },
    promise::{PromiseValue, js_promise_funcs, js_promise_proto_funcs},
    typed_array::{
        JS_TYPED_ARRAY_COUNT, js_dataview_constructor, js_dataview_proto_funcs,
        js_typed_array_base_constructor, js_typed_array_base_funcs,
        js_typed_array_base_proto_funcs, js_typed_array_constructor, typed_array_size_log2,
    },
    value::{
        JSValue, js_number_constructor, js_number_funcs, js_number_proto_funcs,
        object::{
            JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_GET,
            JS_PROP_HAS_SET, JS_PROP_WRITABLE, JSObjectIsa, js_object_seal,
        },
    },
};

impl JSContext {
    unsafe fn intrinsic_throw_error(&self) {
        // throw TypeError()
        let throw_type_error = self
            .create_native_function(js_throw_type_error, "", 0, JSCFunctionEnum::NativeFunc, 0)
            .unwrap();
        self.0.borrow_mut().throw_type_error = throw_type_error.clone();

        // add `caller` and `arguments` properties to throw a TypeError
        let obj1 = self
            .create_native_function(
                js_function_proto_caller,
                "",
                0,
                JSCFunctionEnum::NativeFunc,
                0,
            )
            .unwrap();
        self.0
            .borrow()
            .function_proto
            .define_property(
                self,
                JS_ATOM_caller,
                &JSValue::EXCEPTION,
                &obj1,
                &self.0.borrow().throw_type_error,
                JS_PROP_HAS_GET | JS_PROP_HAS_SET | JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        self.0
            .borrow()
            .function_proto
            .define_property(
                self,
                JS_ATOM_arguments,
                &JSValue::EXCEPTION,
                &obj1,
                &self.0.borrow().throw_type_error,
                JS_PROP_HAS_GET | JS_PROP_HAS_SET | JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        js_object_seal(
            self,
            CallContext::new(
                self,
                JSValue::UNDEFINED,
                std::slice::from_ref(&throw_type_error),
                1,
                None,
            ),
        )
        .unwrap();
    }

    unsafe fn intrinsic_object(&self) {
        let obj_ctor = self
            .create_global_native_constructor(
                c"Object",
                js_object_constructor,
                1,
                &self.0.borrow().class_proto[JS_CLASS_OBJECT as usize],
            )
            .unwrap();
        obj_ctor
            .install_property_entries(self, &js_object_funcs)
            .unwrap();

        self.0.borrow().class_proto[JS_CLASS_OBJECT as usize]
            .install_property_entries(self, &js_object_proto_funcs)
            .unwrap();
    }

    unsafe fn intrinsic_function(&self) {
        self.0
            .borrow()
            .function_proto
            .install_property_entries(self, &js_function_proto_funcs)
            .unwrap();

        let function_ctor = self
            .create_native_function(
                BytecodeFunctionValue::hook_constructor,
                "Function",
                1,
                JSCFunctionEnum::NativeConstructorOrFunc,
                JSFunctionKindEnum::JS_FUNC_NORMAL as _,
            )
            .unwrap();

        self.add_global_constructor("Function", &function_ctor, &self.0.borrow().function_proto);
        self.0.borrow_mut().function_ctor = function_ctor;
    }

    unsafe fn intrinsic_error(&self) {
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
            &error_ctor,
            &self.0.borrow().class_proto[JS_CLASS_ERROR as usize],
        );

        for i in 0..JS_NATIVE_ERROR_COUNT {
            let n_args = 1 + (i == JSErrorEnum::Aggregate as usize) as i32;

            let func_obj = self
                .create_native_function_from_proto(
                    js_error_constructor,
                    NATIVE_ERROR_NAMES[i as usize].to_string_lossy().as_ref(),
                    n_args as _,
                    JSCFunctionEnum::NativeConstructorOrFunc,
                    i as _,
                    &error_ctor,
                )
                .unwrap();

            self.add_global_constructor(
                NATIVE_ERROR_NAMES[i as usize].to_string_lossy().as_ref(),
                &func_obj,
                &self.0.borrow().native_error_proto[i as usize],
            );
        }
    }

    unsafe fn intrinsic_array(&self) {
        self.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
            .install_property_entries(self, &js_array_proto_funcs)
            .unwrap();

        let array_ctor = self
            .create_global_native_constructor(
                c"Array",
                js_array_constructor,
                1,
                &self.0.borrow().class_proto[JS_CLASS_ARRAY as usize],
            )
            .unwrap();
        array_ctor
            .install_property_entries(self, &js_array_funcs)
            .unwrap();
        self.0.borrow_mut().array_ctor = array_ctor;

        // initialize Array.prototype[Symbol.unscopables]
        // XXX: create auto_initializer
        {
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
            let obj1 = self.create_base_object_with_proto(&JSValue::NULL).unwrap();
            for s in unscopables {
                obj1.define_property_value_str(self, s, JSValue::Bool(true), JS_PROP_C_W_E)
                    .unwrap();
            }
            self.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
                .define_property_value(self, JS_ATOM_Symbol_unscopables, obj1, JS_PROP_CONFIGURABLE)
                .unwrap();
        }

        let array_iter = self
            .create_base_object_with_proto(&self.0.borrow().iterator_proto)
            .unwrap();
        array_iter
            .install_property_entries(self, &js_array_iterator_proto_funcs)
            .unwrap();

        // needed to initialize arguments[Symbol.iterator]
        self.with_mut(|cx| {
            cx.array_proto_values =
                cx.class_proto[JS_CLASS_ARRAY as usize].get_property(self, JS_ATOM_values);

            cx.class_proto[JS_CLASS_ARRAY_ITERATOR as usize] = array_iter;
        });

        // self.0.borrow_mut().array_proto_values =
        //     self.0.borrow().class_proto[JS_CLASS_ARRAY as usize].get_property(self, JS_ATOM_values);

        // self.0.borrow_mut().class_proto[JS_CLASS_ARRAY_ITERATOR as usize] = self
        //     .create_base_object_with_proto(&self.0.borrow().iterator_proto)
        //     .unwrap();

        // self.0.borrow().class_proto[JS_CLASS_ARRAY_ITERATOR as usize]
        //     .install_property_entries(self, &js_array_iterator_proto_funcs)
        //     .unwrap();
    }

    unsafe fn intrinsic_number(&self) {
        let number_proto = self
            .create_instance_from_proto(
                &self.0.borrow().class_proto[JS_CLASS_OBJECT as usize],
                JSObjectIsa::NUMBER(JSValue::Int(0)),
            )
            .unwrap();
        number_proto
            .install_property_entries(self, &js_number_proto_funcs)
            .unwrap();

        let number_ctor = self
            .create_global_constructor("Number", js_number_constructor, 1, &number_proto, 0)
            .unwrap();
        number_ctor
            .install_property_entries(self, &js_number_funcs)
            .unwrap();

        self.0.borrow_mut().class_proto[JS_CLASS_NUMBER as usize] = number_proto;
    }

    unsafe fn intrinsic_boolean(&self) {
        let bool_proto = self
            .create_instance_from_proto(
                &self.0.borrow().class_proto[JS_CLASS_OBJECT as usize],
                JSObjectIsa::BOOLEAN(JSValue::Bool(false)),
            )
            .unwrap();

        bool_proto
            .install_property_entries(self, &js_boolean_proto_funcs)
            .unwrap();

        self.create_global_constructor("Boolean", js_boolean_constructor, 1, &bool_proto, 0)
            .unwrap();

        self.0.borrow_mut().class_proto[JS_CLASS_BOOLEAN as usize] = bool_proto;
    }

    unsafe fn intrinsic_string(&self) {
        let empty_str = JS_ATOM_empty_string
            .to_value(self, true)
            .check_exception()
            .unwrap();

        let string_proto = self
            .create_instance_from_proto(
                &self.0.borrow().class_proto[JS_CLASS_OBJECT as usize],
                JSObjectIsa::STRING(empty_str),
            )
            .unwrap();

        string_proto
            .install_property_entries(self, &js_string_proto_funcs)
            .unwrap();

        let string_ctor = self
            .create_global_native_constructor(c"String", js_string_constructor, 1, &string_proto)
            .unwrap();
        string_ctor
            .install_property_entries(self, &js_string_funcs)
            .unwrap();

        let string_iter = self
            .create_base_object_with_proto(&self.0.borrow().iterator_proto)
            .unwrap();
        string_iter
            .install_property_entries(self, &js_string_iterator_proto_funcs)
            .unwrap();

        self.0.borrow_mut().class_proto[JS_CLASS_STRING as usize] = string_proto;
        self.0.borrow_mut().class_proto[JS_CLASS_STRING_ITERATOR as usize] = string_iter;
    }

    unsafe fn intrinsic_iterator(&self) {
        // self.iterator_proto = JS_NewObject(ctx);
        // JS_SetPropertyFunctionList(
        //     self,
        //     self.iterator_proto,
        //     js_iterator_proto_funcs.as_ptr(),
        //     std::mem::size_of_val(js_iterator_proto_funcs) / std::mem::size_of::<JSFunction>(),
        // );
    }

    unsafe fn intrinsic_eval(&self) {
        self.0.borrow_mut().eval_handle.replace(
            |ctx: &JSContext,
             this_obj: JSValue,
             input: &CStr,
             filename: &str,
             flags: u32,
             scope_idx: i32|
             -> JSValue {
                unsafe { ctx.eval_internal(this_obj, input, filename, flags, scope_idx) }
            },
        );

        self.0.borrow_mut().eval_func = self
            .create_native_function(js_global_eval, "eval", 1, JSCFunctionEnum::NativeFunc, 0)
            .unwrap();

        self.global_obj()
            .define_property_value(
                self,
                JS_ATOM_eval,
                self.0.borrow().eval_func.clone(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }

    /// create global object
    pub fn intrinsic_global_object(&self, fields: &[JSCFunctionListEntry]) {
        println!("[O.o] create global object");
        let global = self.create_base_object().unwrap();

        // define global fields
        global.install_property_entries(self, fields).unwrap();

        // define prop `globalThis`
        // Note: global object is self referenced here.
        global
            .define_property_value(
                self,
                JS_ATOM_globalThis,
                global.clone(),
                JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
            )
            .unwrap();

        // global var object
        println!("[O.o] create global var object");
        let gvobj = self.create_base_object_with_proto(&JSValue::NULL).unwrap();

        {
            let mut cx = self.0.borrow_mut();
            cx.global_obj = global;
            cx.global_var_obj = gvobj;
        }
    }

    pub fn intrinsic_base_objects(&self) {
        // make global object - this must come first. (isNan, parseInt, undefined, NaN etc)
        self.intrinsic_global_object(&js_global_props);

        unsafe {
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
            //     ctx.global_obj,
            //     js_math_obj.as_ptr(),
            //     std::mem::size_of_val(js_math_obj) / std::mem::size_of::<JSFunction>(),
            // );

            // ES6 Reflect: create as autoinit object
            // JS_SetPropertyFunctionList(
            //     ctx,
            //     ctx.global_obj,
            //     js_reflect_obj.as_ptr(),
            //     std::mem::size_of_val(js_reflect_obj) / std::mem::size_of::<JSFunction>(),
            // );

            // ES6 Symbol
            // ctx.class_proto[JS_CLASS_SYMBOL] = JS_NewObject(ctx);
            // JS_SetPropertyFunctionList(
            //     ctx,
            //     ctx.class_proto[JS_CLASS_SYMBOL],
            //     js_symbol_proto_funcs.as_ptr(),
            //     std::mem::size_of_val(js_symbol_proto_funcs) / std::mem::size_of::<JSFunction>(),
            // );
            // obj = JS_NewGlobalCConstructor(
            //     ctx,
            //     c"Symbol",
            //     js_symbol_constructor,
            //     0,
            //     ctx.class_proto[JS_CLASS_SYMBOL],
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
            // ctx.class_proto[JS_CLASS_GENERATOR] = JS_NewObjectProto(ctx, ctx.iterator_proto);
            // JS_SetPropertyFunctionList(
            //     ctx,
            //     ctx.class_proto[JS_CLASS_GENERATOR],
            //     js_generator_proto_funcs.as_ptr(),
            //     std::mem::size_of_val(js_generator_proto_funcs) / std::mem::size_of::<JSFunction>(),
            // );

            // ctx.class_proto[JS_CLASS_GENERATOR_FUNCTION] = JS_NewObjectProto(ctx, ctx.function_proto);
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
            //     ctx.class_proto[JS_CLASS_GENERATOR_FUNCTION],
            //     js_generator_function_proto_funcs.as_ptr(),
            //     std::mem::size_of_val(js_generator_function_proto_funcs)
            //         / std::mem::size_of::<JSFunction>(),
            // );
            // JS_SetConstructor2(
            //     ctx,
            //     ctx.class_proto[JS_CLASS_GENERATOR_FUNCTION],
            //     ctx.class_proto[JS_CLASS_GENERATOR],
            //     JS_PROP_CONFIGURABLE,
            //     JS_PROP_CONFIGURABLE,
            // );
            // JS_SetConstructor2(
            //     ctx,
            //     obj1,
            //     ctx.class_proto[JS_CLASS_GENERATOR_FUNCTION],
            //     0,
            //     JS_PROP_CONFIGURABLE,
            // );
            // JS_FreeValue(ctx, obj1);

            self.intrinsic_eval(); // enable `eval()` function
        }
    }

    pub unsafe fn intrinsic_typed_arrays(&self) {
        // ArrayBuffer
        {
            let ab_proto = self.create_base_object().unwrap();
            ab_proto
                .install_property_entries(self, &js_array_buffer_proto_funcs)
                .unwrap();

            let ab_func = self
                .create_global_constructor_only(
                    "ArrayBuffer",
                    js_array_buffer_constructor,
                    1,
                    &ab_proto,
                    0,
                )
                .unwrap();
            ab_func
                .install_property_entries(self, &js_array_buffer_funcs)
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_ARRAY_BUFFER as usize] = ab_proto;
        }

        // SharedArrayBuffer
        {
            let sab_proto = self.create_base_object().unwrap();
            sab_proto
                .install_property_entries(self, &js_shared_array_buffer_proto_funcs)
                .unwrap();

            let sab_func = self
                .create_global_constructor_only(
                    "SharedArrayBuffer",
                    js_shared_array_buffer_constructor,
                    1,
                    &sab_proto,
                    0,
                )
                .unwrap();
            sab_func
                .install_property_entries(self, &js_shared_array_buffer_funcs)
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_SHARED_ARRAY_BUFFER as usize] = sab_proto;
        }

        // Typed arrays
        {
            let typed_array_base_proto = self.create_base_object().unwrap();

            typed_array_base_proto
                .install_property_entries(self, &js_typed_array_base_proto_funcs)
                .unwrap();

            // TypedArray.prototype.toString must be the same object as Array.prototype.toString
            let obj = self.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
                .get_property(self, JS_ATOM_toString);

            // XXX: should use alias method in JSCFunctionListEntry
            typed_array_base_proto
                .define_property_value(
                    self,
                    JS_ATOM_toString,
                    obj,
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

            let typed_array_base_func = self
                .create_native_function(
                    js_typed_array_base_constructor,
                    "TypedArray",
                    0,
                    JSCFunctionEnum::NativeFunc,
                    0,
                )
                .unwrap();
            typed_array_base_func
                .install_property_entries(self, &js_typed_array_base_funcs)
                .unwrap();
            JS_SetConstructor(self, &typed_array_base_func, &typed_array_base_proto);

            for i in
                JS_CLASS_UINT8C_ARRAY..(JS_CLASS_UINT8C_ARRAY + JS_TYPED_ARRAY_COUNT as JSClassID)
            {
                let proto = self
                    .create_base_object_with_proto(&typed_array_base_proto)
                    .unwrap();

                proto
                    .define_property_value_str(
                        self,
                        "BYTES_PER_ELEMENT",
                        (1 << typed_array_size_log2(i)).into(),
                        0,
                    )
                    .unwrap();

                let name = JSAtom(JS_ATOM_Uint8ClampedArray.0 + (i - JS_CLASS_UINT8C_ARRAY) as u32)
                    .to_utf8_string(self)
                    .unwrap();

                // let func_obj = JS_NewCFunction3(
                //     self,
                //     std::mem::transmute::<JSCFunctionMagic, JSCFunction>(
                //         js_typed_array_constructor,
                //     ),
                //     CStr::from_ptr(format!("{}\0", name.as_ref()).as_ptr().cast()),
                //     3,
                //     JSCFunctionEnum::ConstructorMagic,
                //     i as i16,
                //     typed_array_base_func,
                // );

                let func_obj = self
                    .create_native_function_from_proto(
                        js_typed_array_constructor,
                        name.as_ref(),
                        3,
                        JSCFunctionEnum::NativeFunc,
                        i as i16,
                        &typed_array_base_func,
                    )
                    .unwrap();
                self.add_global_constructor(name.as_ref(), &func_obj, &proto);
                func_obj
                    .define_property_value_str(
                        self,
                        "BYTES_PER_ELEMENT",
                        (1 << typed_array_size_log2(i)).into(),
                        0,
                    )
                    .unwrap();

                self.0.borrow_mut().class_proto[i as usize] = proto;
            }
        }

        // DataView
        {
            let dv_proto = self.create_base_object().unwrap();
            dv_proto
                .install_property_entries(self, &js_dataview_proto_funcs)
                .unwrap();

            let dv_ctor = self
                .create_native_function(
                    js_dataview_constructor,
                    "DataView",
                    1,
                    JSCFunctionEnum::NativeConstructor,
                    0,
                )
                .unwrap();
            self.add_global_constructor("DataView", &dv_ctor, &dv_proto);

            self.0.borrow_mut().class_proto[JS_CLASS_DATAVIEW as usize] = dv_proto;
        }
    }

    pub unsafe fn intrinsic_async(&self) {
        let rt = self.rt_mut();
        debug_assert!(rt.is_class_registered(JS_CLASS_PROMISE));

        // Promise
        {
            let promise_proto = self.create_base_object().unwrap();
            promise_proto
                .install_property_entries(self, &js_promise_proto_funcs)
                .unwrap();

            let promise_ctor = self
                .create_native_function(
                    PromiseValue::hook_constructor,
                    "Promise",
                    1,
                    JSCFunctionEnum::NativeConstructor,
                    0,
                )
                .unwrap();
            promise_ctor
                .install_property_entries(self, &js_promise_funcs)
                .unwrap();
            self.add_global_constructor("Promise", &promise_ctor, &promise_proto);

            self.with_mut(|cx| {
                cx.class_proto[JS_CLASS_PROMISE as usize] = promise_proto;
                cx.promise_ctor = promise_ctor;
            });
        }

        // AsyncFunction
        {
            let af_proto = self
                .create_base_object_with_proto(&self.0.borrow().function_proto)
                .unwrap();
            af_proto
                .install_property_entries(self, &js_async_function_proto_funcs)
                .unwrap();

            let af_ctor = self
                .create_native_function_from_proto(
                    BytecodeFunctionValue::hook_constructor,
                    "AsyncFunction",
                    1,
                    JSCFunctionEnum::NativeConstructorOrFunc,
                    JSFunctionKindEnum::JS_FUNC_ASYNC as i16,
                    &self.0.borrow().function_ctor,
                )
                .unwrap();
            JS_SetConstructor2(self, &af_ctor, &af_proto, 0, JS_PROP_CONFIGURABLE);

            self.0.borrow_mut().class_proto[JS_CLASS_ASYNC_FUNCTION as usize] = af_proto;
        }

        // AsyncIteratorPrototype
        {
            let aiter_proto = self.create_base_object().unwrap();
            aiter_proto
                .install_property_entries(self, &js_async_iterator_proto_funcs)
                .unwrap();
            self.0.borrow_mut().async_iterator_proto = aiter_proto;
        }

        // AsyncFromSyncIteratorPrototype
        {
            let asiter_proto = self
                .create_base_object_with_proto(&self.0.borrow().async_iterator_proto)
                .unwrap();
            asiter_proto
                .install_property_entries(self, &js_async_from_sync_iterator_proto_funcs)
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_ASYNC_FROM_SYNC_ITERATOR as usize] =
                asiter_proto;
        }

        // AsyncGenerator
        {
            // AsyncGeneratorPrototype
            let agen_proto = self
                .create_base_object_with_proto(&self.0.borrow().async_iterator_proto)
                .unwrap();
            agen_proto
                .install_property_entries(self, &js_async_generator_proto_funcs)
                .unwrap();

            // AsyncGeneratorFunction
            let agf_proto = self
                .create_base_object_with_proto(&self.0.borrow().function_proto)
                .unwrap();
            agf_proto
                .install_property_entries(self, &js_async_generator_function_proto_funcs)
                .unwrap();

            // let agf_ctor = JS_NewCFunction3(
            //     self,
            //     std::mem::transmute::<JSCFunctionMagic, JSCFunction>(
            //         BytecodeFunctionValue::hook_constructor,
            //     ),
            //     c"AsyncGeneratorFunction",
            //     1,
            //     JSCFunctionEnum::ConstructorOrFuncMagic,
            //     JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR as i16,
            //     self.0.borrow().function_ctor,
            // );

            let agf_ctor = self
                .create_native_function_from_proto(
                    BytecodeFunctionValue::hook_constructor,
                    "AsyncGeneratorFunction",
                    1,
                    JSCFunctionEnum::NativeConstructorOrFunc,
                    JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR as i16,
                    &self.0.borrow().function_ctor,
                )
                .unwrap();
            JS_SetConstructor2(
                self,
                &agf_proto,
                &agen_proto,
                JS_PROP_CONFIGURABLE,
                JS_PROP_CONFIGURABLE,
            );
            JS_SetConstructor2(self, &agf_ctor, &agf_proto, 0, JS_PROP_CONFIGURABLE);

            self.with_mut(|cx| {
                cx.class_proto[JS_CLASS_ASYNC_GENERATOR as usize] = agen_proto;
                cx.class_proto[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize] = agf_proto;
            });
        }

        // // AsyncGeneratorFunction
        // {
        //     let agf_proto = self
        //         .create_base_object_with_proto(&self.0.borrow().function_proto)
        //         .unwrap();

        //     agf_proto
        //         .install_property_entries(self, &js_async_generator_function_proto_funcs)
        //         .unwrap();

        //     self.class_proto[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize] = agf_proto;

        //     let agf_func = JS_NewCFunction3(
        //         self,
        //         std::mem::transmute::<JSCFunctionMagic, JSCFunction>(
        //             BytecodeFunctionValue::hook_constructor,
        //         ),
        //         c"AsyncGeneratorFunction",
        //         1,
        //         JSCFunctionEnum::ConstructorOrFuncMagic,
        //         JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR as i16,
        //         self.0.borrow().function_ctor,
        //     );

        //     JS_SetConstructor2(
        //         self,
        //         self.class_proto[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize],
        //         self.class_proto[JS_CLASS_ASYNC_GENERATOR as usize],
        //         JS_PROP_CONFIGURABLE,
        //         JS_PROP_CONFIGURABLE,
        //     );
        //     JS_SetConstructor2(
        //         self,
        //         agf_func,
        //         self.class_proto[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize],
        //         0,
        //         JS_PROP_CONFIGURABLE,
        //     );
        //     JS_FreeValue(self, &agf_func);
        // }
    }
}

fn js_throw_type_error(ctx: &JSContext, cc: CallContext) -> JSResult {
    ctx.JS_ThrowTypeError("invalid property access");
    Err(-1)
}

// XXX: not 100% compatible, but mozilla seems to use a similar
// implementation to ensure that caller in non strict mode does not
// throw (ES5 compatibility)
fn js_function_proto_caller(ctx: &JSContext, cc: CallContext) -> JSResult {
    let throw_type_error = cc
        .this()
        .object()
        .and_then(|o| {
            o.0.isa.borrow().func().and_then(|f| {
                f.function_bytecode.as_ref().map(|bc| {
                    let b = bc.0.borrow();
                    b.js_mode & JS_MODE_STRICT as u8 != 0 || !b.has_prototype
                })
            })
        })
        .unwrap_or(true);

    if throw_type_error {
        js_throw_type_error(ctx, cc)
    } else {
        Err(-1)
    }
}
