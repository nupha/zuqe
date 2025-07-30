use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_BUFFER, JS_CLASS_ARRAY_ITERATOR,
    JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR,
    JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_BOOLEAN, JS_CLASS_DATAVIEW, JS_CLASS_ERROR,
    JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_NUMBER, JS_CLASS_OBJECT,
    JS_CLASS_PROMISE, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_STRING, JS_CLASS_STRING_ITERATOR,
    JS_CLASS_SYMBOL, JS_CLASS_UINT8C_ARRAY,
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
    error::{ErrorKind, JSResult, STD_ERROR_NAMES},
    function::{
        BytecodeFunction, CallContext, JSFunctionKindEnum, NativeCallback, NativeField,
        NativeFunctionType, js_async_function_proto_funcs,
    },
    generator::{
        js_async_generator_function_proto_funcs, js_async_generator_proto_funcs,
        js_async_iterator_proto_funcs, js_function_constructor, js_generator_function_proto_funcs,
        js_generator_proto_funcs,
    },
    iterator::js_async_from_sync_iterator_proto_funcs,
    js_core::{
        boolean::{js_boolean_constructor, js_boolean_proto_funcs},
        error::js_error_constructor,
        function::js_function_proto_funcs,
        global::{js_global_eval, js_global_props},
        math::js_math_obj,
        object::{js_object_constructor, js_object_funcs, js_object_proto_funcs},
        string::{
            js_string_constructor, js_string_funcs, js_string_iterator_proto_funcs,
            js_string_proto_funcs,
        },
    },
    number::{js_number_constructor, js_number_funcs, js_number_proto_funcs},
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_GET,
        JS_PROP_HAS_SET, JS_PROP_WRITABLE, ObjectDetail, js_object_seal,
    },
    promise::{Promise, js_promise_funcs, js_promise_proto_funcs},
    symbol::{js_symbol_constructor, js_symbol_funcs, js_symbol_proto_funcs},
    typed_array::{
        JS_TYPED_ARRAY_COUNT, js_dataview_constructor, js_dataview_proto_funcs,
        js_typed_array_base_constructor, js_typed_array_base_funcs,
        js_typed_array_base_proto_funcs, js_typed_array_constructor, typed_array_size_log2,
    },
    value::JSValue,
};

impl JSContext {
    fn intrinsic_throw_error(&self) {
        // throw TypeError()
        let throw_type_error = self
            .create_native_function(NativeFunctionType::Func(js_throw_type_error), "", 0, 0)
            .unwrap();
        self.0.borrow_mut().throw_type_error = throw_type_error.clone();

        // add `caller` and `arguments` properties to throw a TypeError
        let obj1 = self
            .create_native_function(NativeFunctionType::Func(js_function_proto_caller), "", 0, 0)
            .unwrap();
        self.0
            .borrow()
            .function_proto
            .define_property(
                self,
                JS_ATOM_caller,
                JSValue::UNDEFINED,
                obj1.clone(),
                self.0.borrow().throw_type_error.clone(),
                JS_PROP_HAS_GET | JS_PROP_HAS_SET | JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        self.0
            .borrow()
            .function_proto
            .define_property(
                self,
                JS_ATOM_arguments,
                JSValue::UNDEFINED,
                obj1.clone(),
                self.0.borrow().throw_type_error.clone(),
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

    fn intrinsic_object(&self) {
        let obj_ctor = self
            .create_global_constructor(
                "Object",
                js_object_constructor,
                1,
                &self.0.borrow().class_proto[JS_CLASS_OBJECT as usize],
                0,
            )
            .unwrap();
        obj_ctor
            .install_native_fields(self, &js_object_funcs)
            .unwrap();

        self.0.borrow().class_proto[JS_CLASS_OBJECT as usize]
            .install_native_fields(self, &js_object_proto_funcs)
            .unwrap();
    }

    fn intrinsic_function(&self) {
        self.0
            .borrow()
            .function_proto
            .install_native_fields(self, &js_function_proto_funcs)
            .unwrap();

        let function_ctor = self
            .create_native_function(
                NativeFunctionType::ConstructorOrFunc(BytecodeFunction::hook_constructor),
                "Function",
                1,
                JSFunctionKindEnum::JS_FUNC_NORMAL as _,
            )
            .unwrap();

        self.add_global_constructor("Function", &function_ctor, &self.0.borrow().function_proto);
        self.0.borrow_mut().function_ctor = function_ctor;
    }

    fn intrinsic_errors(&self) {
        // Error() constructor
        let error_ctor = self
            .create_native_function(
                NativeFunctionType::ConstructorOrFunc(js_error_constructor),
                "Error",
                1,
                -1,
            )
            .unwrap();

        self.add_global_constructor(
            "Error",
            &error_ctor,
            &self.0.borrow().class_proto[JS_CLASS_ERROR as usize],
        );

        // standard error constructors
        for (i, name) in STD_ERROR_NAMES.iter().enumerate() {
            let func_obj = self
                .create_native_function_from_proto(
                    NativeFunctionType::ConstructorOrFunc(js_error_constructor),
                    name,
                    if i == ErrorKind::Aggregate as usize {
                        2
                    } else {
                        1
                    },
                    i as _, // magic
                    error_ctor.clone(),
                )
                .unwrap();

            self.add_global_constructor(
                name,
                &func_obj,
                &self.0.borrow().native_error_proto[i as usize], // <-- this was initialized in core
            );
        }
    }

    fn intrinsic_array(&self) {
        self.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
            .install_native_fields(self, &js_array_proto_funcs)
            .unwrap();

        let array_ctor = self
            .create_global_constructor(
                "Array",
                js_array_constructor,
                1,
                &self.0.borrow().class_proto[JS_CLASS_ARRAY as usize],
                0,
            )
            .unwrap();
        array_ctor
            .install_native_fields(self, &js_array_funcs)
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
            let obj1 = self.create_base_object_from_proto(JSValue::NULL).unwrap();
            for s in unscopables {
                obj1.define_property_value_str(self, s, JSValue::Bool(true), JS_PROP_C_W_E)
                    .unwrap();
            }
            self.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
                .define_property_value(
                    self,
                    JS_ATOM_Symbol_unscopables,
                    obj1.into(),
                    JS_PROP_CONFIGURABLE,
                )
                .unwrap();
        }

        let array_iter = self
            .create_base_object_from_proto(self.0.borrow().iterator_proto.clone())
            .unwrap();
        array_iter
            .install_native_fields(self, &js_array_iterator_proto_funcs)
            .unwrap();

        // needed to initialize arguments[Symbol.iterator]
        let mut cx = self.0.borrow_mut();
        cx.array_proto_values =
            cx.class_proto[JS_CLASS_ARRAY as usize].get_property(self, JS_ATOM_values);
        cx.class_proto[JS_CLASS_ARRAY_ITERATOR as usize] = array_iter.into();
    }

    fn intrinsic_number(&self) {
        let number_obj = self
            .create_object_from_proto(
                self.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone(),
                ObjectDetail::NUMBER(JSValue::Int(0)),
            )
            .unwrap();
        number_obj
            .install_native_fields(self, &js_number_proto_funcs)
            .unwrap();
        let number_proto: JSValue = number_obj.into();

        let number_ctor = self
            .create_global_constructor("Number", js_number_constructor, 1, &number_proto, 0)
            .unwrap();
        number_ctor
            .install_native_fields(self, &js_number_funcs)
            .unwrap();

        self.0.borrow_mut().class_proto[JS_CLASS_NUMBER as usize] = number_proto;
    }

    fn intrinsic_boolean(&self) {
        let bool_obj = self
            .create_object_from_proto(
                self.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone(),
                ObjectDetail::BOOLEAN(JSValue::Bool(false)),
            )
            .unwrap();

        bool_obj
            .install_native_fields(self, &js_boolean_proto_funcs)
            .unwrap();

        self.create_global_constructor(
            "Boolean",
            js_boolean_constructor,
            1,
            &bool_obj.clone().into(),
            0,
        )
        .unwrap();

        self.0.borrow_mut().class_proto[JS_CLASS_BOOLEAN as usize] = bool_obj.into();
    }

    fn intrinsic_string(&self) {
        let empty_str = JS_ATOM_empty_string
            .to_value(self, true)
            .check_exception()
            .unwrap();

        let string_proto: JSValue = self
            .create_object_from_proto(
                self.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone(),
                ObjectDetail::STRING(empty_str),
            )
            .unwrap()
            .into();
        string_proto
            .install_native_fields(self, &js_string_proto_funcs)
            .unwrap();

        let string_ctor = self
            .create_global_constructor("String", js_string_constructor, 1, &string_proto, 0)
            .unwrap();
        string_ctor
            .install_native_fields(self, &js_string_funcs)
            .unwrap();

        let string_iter = self
            .create_base_object_from_proto(self.0.borrow().iterator_proto.clone())
            .unwrap();
        string_iter
            .install_native_fields(self, &js_string_iterator_proto_funcs)
            .unwrap();

        self.0.borrow_mut().class_proto[JS_CLASS_STRING as usize] = string_proto;
        self.0.borrow_mut().class_proto[JS_CLASS_STRING_ITERATOR as usize] = string_iter.into();
    }

    fn intrinsic_symbol(&self) {
        let symbol_obj = self.create_base_object().unwrap();
        symbol_obj
            .install_native_fields(self, &js_symbol_proto_funcs)
            .unwrap();

        let symbol_ctor = self
            .create_global_constructor(
                "Symbol",
                js_symbol_constructor,
                0,
                &symbol_obj.clone().into(),
                0,
            )
            .unwrap();
        symbol_ctor
            .install_native_fields(self, &js_symbol_funcs)
            .unwrap();

        // pre-define builtin Symbol_XXX
        for i in JS_ATOM_FIRST_SYMBOL.0..=JS_ATOM_LAST_SYMBOL.0 {
            let a = JSAtom(i);
            a.with_utf8(self.rt(), |s| {
                let str = s.as_ref().split_once('.').unwrap().1; // skip "Symbol."
                symbol_ctor
                    .define_property_value_str(self, str, a.to_value(self, false), 0)
                    .unwrap();
            });
        }

        self.0.borrow_mut().class_proto[JS_CLASS_SYMBOL as usize] = symbol_obj.into();
    }

    fn intrinsic_iterator(&self) {
        // self.iterator_proto = JS_NewObject(ctx);
        // JS_SetPropertyFunctionList(
        //     self,
        //     self.iterator_proto,
        //     js_iterator_proto_funcs.as_ptr(),
        //     std::mem::size_of_val(js_iterator_proto_funcs) / std::mem::size_of::<JSFunction>(),
        // );
    }

    fn intrinsic_generator(&self) {
        // generator prototype
        let generator = self
            .create_base_object_from_proto(self.0.borrow().iterator_proto.clone())
            .unwrap();
        generator
            .install_native_fields(self, &js_generator_proto_funcs)
            .unwrap();
        let generator_proto: JSValue = generator.into();
        self.0.borrow_mut().class_proto[JS_CLASS_GENERATOR as usize] = generator_proto.clone();

        // generator function prototype
        let generator_function = self
            .create_base_object_from_proto(self.0.borrow().function_proto.clone())
            .unwrap();
        generator_function
            .install_native_fields(self, &js_generator_function_proto_funcs)
            .unwrap();
        let generator_function_proto: JSValue = generator_function.into();
        self.0.borrow_mut().class_proto[JS_CLASS_GENERATOR_FUNCTION as usize] =
            generator_function_proto.clone();

        // generator constructor
        let generator_ctor = self
            .create_native_function(
                NativeFunctionType::ConstructorOrFunc(js_function_constructor),
                "GeneratorFunction",
                1,
                JSFunctionKindEnum::JS_FUNC_GENERATOR as i16,
            )
            .unwrap();
        JS_SetConstructor2(
            self,
            &generator_function_proto,
            &generator_proto,
            JS_PROP_CONFIGURABLE,
            JS_PROP_CONFIGURABLE,
        );
        JS_SetConstructor2(
            self,
            &generator_ctor,
            &generator_function_proto,
            0,
            JS_PROP_CONFIGURABLE,
        );
    }

    fn intrinsic_eval(&self) {
        self.0.borrow_mut().eval_handle.replace(
            |ctx: &JSContext,
             this_obj: &JSValue,
             input: &[u8],
             filename: &str,
             flags: u32,
             scope_idx: i32|
             -> JSValue {
                ctx.eval_source(this_obj, input, filename, flags, scope_idx)
            },
        );

        let eval_func = self
            .create_native_function(NativeFunctionType::Func(js_global_eval), "eval", 1, 0)
            .unwrap();
        self.0.borrow_mut().eval_func = eval_func.clone();

        self.global_obj()
            .define_property_value(
                self,
                JS_ATOM_eval,
                eval_func,
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }

    /// the `Math` name space.
    pub fn intrinsic_math(&self) {
        self.0.borrow_mut().init_random_seed();
        self.0
            .borrow()
            .global_obj
            .install_native_fields(self, &js_math_obj)
            .unwrap();
    }

    /// create global object
    pub fn intrinsic_global_object(&self, fields: &[NativeField]) {
        let global = self.create_base_object().unwrap();
        global.install_native_fields(self, fields).unwrap();
        let global: JSValue = global.into();

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
        let gvobj = self.create_base_object_from_proto(JSValue::NULL).unwrap();

        {
            let mut cx = self.0.borrow_mut();
            cx.global_obj = global;
            cx.global_var_obj = gvobj.into();
        }
    }

    pub fn intrinsic_base_objects(&self) {
        // make global object - this must come first. (isNan, parseInt, undefined, NaN etc)
        self.intrinsic_global_object(&js_global_props);

        {
            self.intrinsic_throw_error(); // throw TypeError()

            self.intrinsic_object(); // Object
            self.intrinsic_function(); // Function
            self.intrinsic_errors(); // Error
            self.intrinsic_iterator(); // Iterator prototype
            self.intrinsic_array(); // Array

            self.intrinsic_number(); // Number
            self.intrinsic_boolean(); // Boolean
            self.intrinsic_string(); // String

            // ES6 Reflect: create as autoinit object
            // JS_SetPropertyFunctionList(
            //     ctx,
            //     ctx.global_obj,
            //     js_reflect_obj.as_ptr(),
            //     std::mem::size_of_val(js_reflect_obj) / std::mem::size_of::<JSFunction>(),
            // );

            self.intrinsic_symbol(); // Symbol
            self.intrinsic_generator(); // Generator

            self.intrinsic_eval(); // `eval()` function
        }
    }

    pub fn intrinsic_typed_arrays(&self) {
        // ArrayBuffer
        {
            let ab_obj = self.create_base_object().unwrap();
            ab_obj
                .install_native_fields(self, &js_array_buffer_proto_funcs)
                .unwrap();
            let ab: JSValue = ab_obj.into();

            let ab_func = self
                .create_global_constructor_only(
                    "ArrayBuffer",
                    js_array_buffer_constructor,
                    1,
                    &ab,
                    0,
                )
                .unwrap();
            ab_func
                .install_native_fields(self, &js_array_buffer_funcs)
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_ARRAY_BUFFER as usize] = ab;
        }

        // SharedArrayBuffer
        {
            let sab_obj = self.create_base_object().unwrap();
            sab_obj
                .install_native_fields(self, &js_shared_array_buffer_proto_funcs)
                .unwrap();
            let sab: JSValue = sab_obj.into();

            let sab_func = self
                .create_global_constructor_only(
                    "SharedArrayBuffer",
                    js_shared_array_buffer_constructor,
                    1,
                    &sab,
                    0,
                )
                .unwrap();
            sab_func
                .install_native_fields(self, &js_shared_array_buffer_funcs)
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_SHARED_ARRAY_BUFFER as usize] = sab;
        }

        // Typed arrays
        {
            let ta_obj = self.create_base_object().unwrap();
            ta_obj
                .install_native_fields(self, &js_typed_array_base_proto_funcs)
                .unwrap();
            let typed_array_base_proto: JSValue = ta_obj.into();

            // TypedArray.prototype.toString must be the same object as Array.prototype.toString
            let to_string = self.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
                .get_property(self, JS_ATOM_toString);

            // XXX: should use alias method in JSCFunctionListEntry
            typed_array_base_proto
                .define_property_value(
                    self,
                    JS_ATOM_toString,
                    to_string,
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

            let typed_array_base_func = self
                .create_native_function(
                    NativeFunctionType::Func(js_typed_array_base_constructor),
                    "TypedArray",
                    0,
                    0,
                )
                .unwrap();
            typed_array_base_func
                .install_native_fields(self, &js_typed_array_base_funcs)
                .unwrap();
            JS_SetConstructor(self, &typed_array_base_func, &typed_array_base_proto);

            for cid in
                JS_CLASS_UINT8C_ARRAY..(JS_CLASS_UINT8C_ARRAY + JS_TYPED_ARRAY_COUNT as JSClassID)
            {
                let ta_obj = self
                    .create_base_object_from_proto(typed_array_base_proto.clone())
                    .unwrap();
                ta_obj
                    .define_property_value_str(
                        self,
                        "BYTES_PER_ELEMENT",
                        (1 << typed_array_size_log2(cid)).into(),
                        0,
                    )
                    .unwrap();
                let proto: JSValue = ta_obj.into();

                let name =
                    JSAtom(JS_ATOM_Uint8ClampedArray.0 + (cid - JS_CLASS_UINT8C_ARRAY) as u32)
                        .to_utf8_string(self)
                        .unwrap();

                let func_obj = self
                    .create_native_function_from_proto(
                        NativeFunctionType::Constructor(js_typed_array_constructor),
                        &name,
                        3,
                        cid as i16, // magic
                        typed_array_base_func.clone(),
                    )
                    .unwrap();
                self.add_global_constructor(name.as_ref(), &func_obj, &proto);
                func_obj
                    .define_property_value_str(
                        self,
                        "BYTES_PER_ELEMENT",
                        (1 << typed_array_size_log2(cid)).into(),
                        0,
                    )
                    .unwrap();

                self.0.borrow_mut().class_proto[cid as usize] = proto;
            }
        }

        // DataView
        {
            let dv_obj = self.create_base_object().unwrap();
            dv_obj
                .install_native_fields(self, &js_dataview_proto_funcs)
                .unwrap();
            let dv_proto: JSValue = dv_obj.into();

            let dv_ctor = self
                .create_native_function(
                    NativeFunctionType::Constructor(js_dataview_constructor),
                    "DataView",
                    1,
                    0,
                )
                .unwrap();
            self.add_global_constructor("DataView", &dv_ctor, &dv_proto);

            self.0.borrow_mut().class_proto[JS_CLASS_DATAVIEW as usize] = dv_proto;
        }
    }

    pub fn intrinsic_async(&self) {
        let rt = self.rt_mut();
        debug_assert!(rt.is_class_registered(JS_CLASS_PROMISE));

        // Promise
        {
            let promise_obj = self.create_base_object().unwrap();
            promise_obj
                .install_native_fields(self, &js_promise_proto_funcs)
                .unwrap();
            let promise_proto: JSValue = promise_obj.into();

            let promise_ctor = self
                .create_native_function(
                    NativeFunctionType::Constructor(Promise::hook_constructor),
                    "Promise",
                    1,
                    0,
                )
                .unwrap();
            promise_ctor
                .install_native_fields(self, &js_promise_funcs)
                .unwrap();
            self.add_global_constructor("Promise", &promise_ctor, &promise_proto);

            self.with_mut(|cx| {
                cx.class_proto[JS_CLASS_PROMISE as usize] = promise_proto;
                cx.promise_ctor = promise_ctor;
            });
        }

        // AsyncFunction
        {
            let af_obj = self
                .create_base_object_from_proto(self.0.borrow().function_proto.clone())
                .unwrap();
            af_obj
                .install_native_fields(self, &js_async_function_proto_funcs)
                .unwrap();
            let af_proto = af_obj.into();

            let af_ctor = self
                .create_native_function_from_proto(
                    NativeFunctionType::ConstructorOrFunc(BytecodeFunction::hook_constructor),
                    "AsyncFunction",
                    1,
                    JSFunctionKindEnum::JS_FUNC_ASYNC as i16,
                    self.0.borrow().function_ctor.clone(),
                )
                .unwrap();
            JS_SetConstructor2(self, &af_ctor, &af_proto, 0, JS_PROP_CONFIGURABLE);

            self.0.borrow_mut().class_proto[JS_CLASS_ASYNC_FUNCTION as usize] = af_proto;
        }

        // AsyncIteratorPrototype
        {
            let a_iter_obj = self.create_base_object().unwrap();
            a_iter_obj
                .install_native_fields(self, &js_async_iterator_proto_funcs)
                .unwrap();
            self.0.borrow_mut().async_iterator_proto = a_iter_obj.into();
        }

        // AsyncFromSyncIteratorPrototype
        {
            let iter_obj = self
                .create_base_object_from_proto(self.0.borrow().async_iterator_proto.clone())
                .unwrap();
            iter_obj
                .install_native_fields(self, &js_async_from_sync_iterator_proto_funcs)
                .unwrap();
            self.0.borrow_mut().class_proto[JS_CLASS_ASYNC_FROM_SYNC_ITERATOR as usize] =
                iter_obj.into();
        }

        // AsyncGenerator
        {
            // AsyncGeneratorPrototype
            let a_gen_obj = self
                .create_base_object_from_proto(self.0.borrow().async_iterator_proto.clone())
                .unwrap();
            a_gen_obj
                .install_native_fields(self, &js_async_generator_proto_funcs)
                .unwrap();
            let agen_proto: JSValue = a_gen_obj.into();

            // AsyncGeneratorFunction
            let agf_obj = self
                .create_base_object_from_proto(self.0.borrow().function_proto.clone())
                .unwrap();
            agf_obj
                .install_native_fields(self, &js_async_generator_function_proto_funcs)
                .unwrap();
            let agf_proto: JSValue = agf_obj.into();

            let agf_ctor = self
                .create_native_function_from_proto(
                    NativeFunctionType::ConstructorOrFunc(BytecodeFunction::hook_constructor),
                    "AsyncGeneratorFunction",
                    1,
                    JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR as i16,
                    self.0.borrow().function_ctor.clone(),
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
    ctx.throw_type_error("invalid property access");
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
            o.0.payload.borrow().func().and_then(|f| {
                f.bytecode.as_ref().map(|bc| {
                    let b = bc.0.borrow();
                    b.js_mode & JS_MODE_STRICT as u8 != 0 || !b.has_prototype
                })
            })
        })
        .unwrap_or(true);

    if throw_type_error {
        js_throw_type_error(ctx, cc)
    } else {
        Ok(JSValue::UNDEFINED)
    }
}
