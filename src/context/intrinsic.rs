use std::ops::Deref;

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_BUFFER, JS_CLASS_ARRAY_ITERATOR,
    JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR,
    JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_BOOLEAN, JS_CLASS_DATAVIEW, JS_CLASS_DATE,
    JS_CLASS_ERROR, JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_MAP,
    JS_CLASS_MAP_ITERATOR, JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_PROMISE, JS_CLASS_REGEXP,
    JS_CLASS_REGEXP_STRING_ITERATOR, JS_CLASS_SET, JS_CLASS_SET_ITERATOR,
    JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_STRING, JS_CLASS_STRING_ITERATOR, JS_CLASS_SYMBOL,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_WEAKMAP, JS_CLASS_WEAKSET,
    array::{
        js_array_constructor, js_array_funcs, js_array_iterator_proto_funcs, js_array_proto_funcs,
    },
    array_buffer::{
        js_array_buffer_constructor, js_array_buffer_funcs, js_array_buffer_proto_funcs,
        js_shared_array_buffer_constructor, js_shared_array_buffer_funcs,
        js_shared_array_buffer_proto_funcs,
    },
    atom::{AtomRef, consts::*},
    class::JSClassID,
    context::{JS_MODE_STRICT, JSContext},
    date::{js_date_constructor, js_date_funcs, js_date_proto_funcs},
    error::{ErrorKind, JSResult, STD_ERROR_NAMES},
    function::{
        BytecodeFunction, FieldDef, JSFunctionKindEnum, NativeFunctionType,
        js_async_function_proto_funcs,
    },
    generator::{
        js_async_generator_function_proto_funcs, js_async_generator_proto_funcs,
        js_async_iterator_proto_funcs, js_generator_function_proto_funcs, js_generator_proto_funcs,
    },
    iterator::{js_async_from_sync_iterator_proto_funcs, js_iterator_proto_funcs},
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
    json::js_json_obj,
    mapset::{
        js_map_constructor, js_map_funcs, js_map_iterator_proto_funcs, js_map_proto_funcs,
        js_set_funcs, js_set_iterator_proto_funcs, js_set_proto_funcs, js_weakmap_proto_funcs,
        js_weakset_proto_funcs,
    },
    number::{js_number_constructor, js_number_funcs, js_number_proto_funcs},
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_GET,
        JS_PROP_HAS_SET, JS_PROP_WRITABLE, JS_SetConstructorBit, ObjectPayload,
    },
    promise::{Promise, js_promise_funcs, js_promise_proto_funcs},
    proxy::{js_proxy_constructor, js_proxy_funcs},
    reflect::js_reflect_obj,
    regexp::{
        js_regexp_constructor, js_regexp_funcs, js_regexp_proto_funcs,
        js_regexp_string_iterator_proto_funcs,
    },
    symbol::{js_symbol_constructor, js_symbol_funcs, js_symbol_proto_funcs},
    typed_array::{
        JS_TYPED_ARRAY_COUNT, TypedArray, js_dataview_constructor, js_dataview_proto_funcs,
        js_typed_array_base_constructor, js_typed_array_base_funcs,
        js_typed_array_base_proto_funcs, js_typed_array_constructor,
    },
    value::JSValue,
};

impl JSContext {
    fn intrinsic_base_object(&self) {
        let proto = self.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone();
        proto
            .install_field_defs(self, &js_object_proto_funcs)
            .unwrap();

        let ctor = self
            .create_global_constructor_or_func("Object", js_object_constructor, 1, &proto, 0)
            .unwrap();
        ctor.install_field_defs(self, &js_object_funcs).unwrap();
    }

    fn intrinsic_base_function(&self) {
        // FOR LEGACY: make throw type error function
        let throw_type_error = self
            .create_native_function(
                NativeFunctionType::Func(|ctx, cc| {
                    // throw only in strict mode
                    let throw = cc
                        .this()
                        .as_object()
                        .and_then(|o| {
                            o.payload().bc_function().map(|bc| {
                                let b = bc.bytecode();
                                b.mode() & JS_MODE_STRICT as u8 != 0 || !b.has_prototype()
                            })
                        })
                        .unwrap_or(true);

                    if throw {
                        ctx.throw_type_error("invalid property access");
                        Err(-1)
                    } else {
                        Ok(if cc.length() == 0 {
                            JSValue::NULL // getter
                        } else {
                            JSValue::UNDEFINED // setter
                        })
                    }
                }),
                "",
                0,
                0,
            )
            .unwrap();

        throw_type_error.seal(self, true).unwrap();
        self.0.borrow_mut().throw_type_error = throw_type_error.clone();

        // FOR LEGACY: add 'caller' and 'arguments' properties
        let proto = self.0.borrow().function_proto.clone();

        proto
            .define_property(
                self,
                JS_ATOM_caller.deref(),
                JSValue::UNDEFINED,
                throw_type_error.clone(),
                throw_type_error.clone(),
                JS_PROP_HAS_GET | JS_PROP_HAS_SET | JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        proto
            .define_property(
                self,
                JS_ATOM_arguments.deref(),
                JSValue::UNDEFINED,
                throw_type_error.clone(),
                throw_type_error.clone(),
                JS_PROP_HAS_GET | JS_PROP_HAS_SET | JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        proto
            .install_field_defs(self, &js_function_proto_funcs)
            .unwrap();

        let ctor = self
            .create_native_function(
                NativeFunctionType::ConstructorOrFunc(BytecodeFunction::class_constructor),
                "Function",
                1,
                JSFunctionKindEnum::JS_FUNC_NORMAL as _,
            )
            .unwrap();

        self.add_global_constructor("Function", &ctor, &proto);
        self.0.borrow_mut().function_ctor = ctor;
    }

    fn intrinsic_base_errors(&self) {
        // Error() constructor
        let ctor = self
            .create_native_function(
                NativeFunctionType::ConstructorOrFunc(js_error_constructor),
                "Error",
                1,
                -1,
            )
            .unwrap();

        self.add_global_constructor(
            "Error",
            &ctor,
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
                    ctor.clone(),
                )
                .unwrap();

            self.add_global_constructor(
                name,
                &func_obj,
                &self.0.borrow().native_error_proto[i], // <-- this was initialized in core
            );
        }
    }

    fn intrinsic_base_array(&self) {
        self.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
            .install_field_defs(self, &js_array_proto_funcs)
            .unwrap();

        let array_ctor = self
            .create_global_constructor_or_func(
                "Array",
                js_array_constructor,
                1,
                &self.0.borrow().class_proto[JS_CLASS_ARRAY as usize],
                0,
            )
            .unwrap();
        array_ctor
            .install_field_defs(self, &js_array_funcs)
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
                    JS_ATOM_Symbol_unscopables.deref(),
                    obj1.into(),
                    JS_PROP_CONFIGURABLE,
                )
                .unwrap();
        }

        let array_iter = self
            .create_base_object_from_proto(self.0.borrow().iterator_proto.clone())
            .unwrap();
        array_iter
            .install_field_defs(self, &js_array_iterator_proto_funcs)
            .unwrap();

        let array_values = self.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
            .get_property(self, JS_ATOM_values.deref())
            .unwrap();

        {
            let mut cx = self.0.borrow_mut();
            cx.array_proto_values = array_values;
            cx.class_proto[JS_CLASS_ARRAY_ITERATOR as usize] = array_iter.into();
        }
    }

    fn intrinsic_base_number(&self) {
        let number_obj = self
            .create_object_from_proto(
                self.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone(),
                ObjectPayload::NUMBER(JSValue::Int(0)),
            )
            .unwrap();
        number_obj
            .install_field_defs(self, &js_number_proto_funcs)
            .unwrap();
        let number_proto: JSValue = number_obj.into();

        let number_ctor = self
            .create_global_constructor_or_func("Number", js_number_constructor, 1, &number_proto, 0)
            .unwrap();
        number_ctor
            .install_field_defs(self, &js_number_funcs)
            .unwrap();

        self.0.borrow_mut().class_proto[JS_CLASS_NUMBER as usize] = number_proto;
    }

    fn intrinsic_base_boolean(&self) {
        let bool_obj = self
            .create_object_from_proto(
                self.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone(),
                ObjectPayload::BOOLEAN(JSValue::Bool(false)),
            )
            .unwrap();

        bool_obj
            .install_field_defs(self, &js_boolean_proto_funcs)
            .unwrap();

        self.create_global_constructor_or_func(
            "Boolean",
            js_boolean_constructor,
            1,
            &bool_obj.clone().into(),
            0,
        )
        .unwrap();

        self.0.borrow_mut().class_proto[JS_CLASS_BOOLEAN as usize] = bool_obj.into();
    }

    fn intrinsic_base_string(&self) {
        let empty_str = JS_ATOM_empty_string.force_string(self).unwrap();

        let string_obj: JSValue = {
            let o = self
                .create_object_from_proto(
                    self.0.borrow().class_proto[JS_CLASS_OBJECT as usize].clone(),
                    ObjectPayload::STRING(empty_str.into()),
                )
                .unwrap();

            o.install_field_defs(self, &js_string_proto_funcs).unwrap();
            o.into()
        };

        let string_ctor = self
            .create_global_constructor_or_func("String", js_string_constructor, 1, &string_obj, 0)
            .unwrap();
        string_ctor
            .install_field_defs(self, &js_string_funcs)
            .unwrap();

        let string_iter = self
            .create_base_object_from_proto(self.0.borrow().iterator_proto.clone())
            .unwrap();
        string_iter
            .install_field_defs(self, &js_string_iterator_proto_funcs)
            .unwrap();

        self.0.borrow_mut().class_proto[JS_CLASS_STRING as usize] = string_obj;
        self.0.borrow_mut().class_proto[JS_CLASS_STRING_ITERATOR as usize] = string_iter.into();
    }

    fn intrinsic_base_symbol(&self) {
        let obj = self.create_base_object().unwrap();
        obj.install_field_defs(self, &js_symbol_proto_funcs)
            .unwrap();

        let ctor = self
            .create_global_constructor_or_func(
                "Symbol",
                js_symbol_constructor,
                0,
                &obj.clone().into(),
                0,
            )
            .unwrap();
        ctor.install_field_defs(self, &js_symbol_funcs).unwrap();

        // pre-define builtin Symbol_XXX
        for i in JS_ATOM_FIRST_SYMBOL.0..=JS_ATOM_LAST_SYMBOL.0 {
            let a = AtomRef(i);
            a.with_utf8(|s| {
                let str = s.as_ref().split_once('.').unwrap().1; // skip "Symbol."
                ctor.define_property_value_str(self, str, a.string_or_symbol(self).unwrap(), 0)
                    .unwrap();
            });
        }

        self.0.borrow_mut().class_proto[JS_CLASS_SYMBOL as usize] = obj.into();
    }

    fn intrinsic_base_iterator(&self) {
        let obj = self.create_base_object().unwrap();
        obj.install_field_defs(self, &js_iterator_proto_funcs)
            .unwrap();
        self.0.borrow_mut().iterator_proto = obj.into();
    }

    fn intrinsic_base_generator(&self) {
        // generator prototype
        let obj = self
            .create_base_object_from_proto(self.0.borrow().iterator_proto.clone())
            .unwrap();
        obj.install_field_defs(self, &js_generator_proto_funcs)
            .unwrap();
        let proto: JSValue = obj.into();
        self.0.borrow_mut().class_proto[JS_CLASS_GENERATOR as usize] = proto.clone();

        // generator function prototype
        let generator_function = self
            .create_base_object_from_proto(self.0.borrow().function_proto.clone())
            .unwrap();
        generator_function
            .install_field_defs(self, &js_generator_function_proto_funcs)
            .unwrap();
        let generator_function_proto: JSValue = generator_function.into();
        self.0.borrow_mut().class_proto[JS_CLASS_GENERATOR_FUNCTION as usize] =
            generator_function_proto.clone();

        // generator constructor
        let ctor = self
            .create_native_function(
                NativeFunctionType::ConstructorOrFunc(BytecodeFunction::class_constructor),
                "GeneratorFunction",
                1,
                JSFunctionKindEnum::JS_FUNC_GENERATOR as i16,
            )
            .unwrap();

        self.bind_ctor_proto_ex(
            &generator_function_proto,
            &proto,
            JS_PROP_CONFIGURABLE,
            JS_PROP_CONFIGURABLE,
        )
        .unwrap();

        self.bind_ctor_proto_ex(&ctor, &generator_function_proto, 0, JS_PROP_CONFIGURABLE)
            .unwrap();
    }

    pub fn intrinsic_date(&self) {
        let obj: JSValue = self.create_base_object().unwrap().into();
        obj.install_field_defs(self, &js_date_proto_funcs).unwrap();

        let ctor = self
            .create_global_constructor_or_func("Date", js_date_constructor, 7, &obj, 0)
            .unwrap();
        ctor.install_field_defs(self, &js_date_funcs).unwrap();

        self.0.borrow_mut().class_proto[JS_CLASS_DATE as usize] = obj;
    }

    pub fn intrinsic_mapset(&self) {
        // Map
        {
            let obj: JSValue = self.create_base_object().unwrap().into();
            obj.install_field_defs(self, &js_map_proto_funcs).unwrap();

            let ctor = self
                .create_global_constructor_only("Map", js_map_constructor, 0, &obj, 0)
                .unwrap();
            ctor.install_field_defs(self, &js_map_funcs).unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_MAP as usize] = obj;
        }

        // Set
        {
            let obj: JSValue = self.create_base_object().unwrap().into();
            obj.install_field_defs(self, &js_set_proto_funcs).unwrap();

            let ctor = self
                .create_global_constructor_only("Set", js_map_constructor, 0, &obj, 1)
                .unwrap();
            ctor.install_field_defs(self, &js_set_funcs).unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_SET as usize] = obj;
        }

        // WeakMap
        {
            let obj: JSValue = self.create_base_object().unwrap().into();
            obj.install_field_defs(self, &js_weakmap_proto_funcs)
                .unwrap();

            let _ = self
                .create_global_constructor_only("WeakMap", js_map_constructor, 0, &obj, 2)
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_WEAKMAP as usize] = obj;
        }

        // WeakSet
        {
            let obj: JSValue = self.create_base_object().unwrap().into();
            obj.install_field_defs(self, &js_weakset_proto_funcs)
                .unwrap();

            let _ = self
                .create_global_constructor_only("WeakSet", js_map_constructor, 0, &obj, 3)
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_WEAKSET as usize] = obj;
        }

        // MapIterator
        {
            let proto: JSValue = self
                .create_base_object_from_proto(self.0.borrow().iterator_proto.clone())
                .unwrap()
                .into();
            proto
                .install_field_defs(self, &js_map_iterator_proto_funcs)
                .unwrap();
            self.0.borrow_mut().class_proto[JS_CLASS_MAP_ITERATOR as usize] = proto;
        }

        // SetIterator
        {
            let proto: JSValue = self
                .create_base_object_from_proto(self.0.borrow().iterator_proto.clone())
                .unwrap()
                .into();
            proto
                .install_field_defs(self, &js_set_iterator_proto_funcs)
                .unwrap();
            self.0.borrow_mut().class_proto[JS_CLASS_SET_ITERATOR as usize] = proto;
        }
    }

    fn intrinsic_eval(&self) {
        self.0.borrow_mut().eval_handle.replace(
            |ctx: &JSContext,
             this_obj: &JSValue,
             input: &[u8],
             filename: &str,
             flags: u32,
             scope_idx: i32|
             -> JSResult {
                ctx.eval_internal(this_obj, input, filename, flags, scope_idx)
            },
        );

        let eval_func = self
            .create_native_function(NativeFunctionType::Func(js_global_eval), "eval", 1, 0)
            .unwrap();
        self.0.borrow_mut().eval_func = eval_func.clone();

        self.global_obj()
            .define_property_value(
                self,
                JS_ATOM_eval.deref(),
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
            .install_field_defs(self, &js_math_obj)
            .unwrap();
    }

    /// create global object
    pub fn intrinsic_global(&self, fields: &[FieldDef]) {
        let gobj = self.create_base_object().unwrap();
        gobj.install_field_defs(self, fields).unwrap();

        let global: JSValue = gobj.into();

        // define name `globalThis` and `global`
        // Note: global object is self referenced here.
        global
            .define_property_value(
                self,
                JS_ATOM_globalThis.deref(),
                global.clone(),
                JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
            )
            .unwrap();
        global
            .define_property_value(
                self,
                JS_ATOM_global.deref(),
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
        // global object - this must come first.
        {
            // isNan, parseInt, undefined, NaN etc
            self.intrinsic_global(&js_global_props);

            // Reflect: create as autoinit object in global
            self.global_obj()
                .install_field_defs(self, &js_reflect_obj)
                .unwrap();
        }

        self.intrinsic_base_object(); // Object
        self.intrinsic_base_function(); // Function
        self.intrinsic_base_errors(); // Error
        self.intrinsic_base_iterator(); // Iterator prototype
        self.intrinsic_base_array(); // Array
        self.intrinsic_base_number(); // Number
        self.intrinsic_base_boolean(); // Boolean
        self.intrinsic_base_string(); // String
        self.intrinsic_base_symbol(); // Symbol
        self.intrinsic_base_generator(); // Generator

        self.intrinsic_eval(); // `eval()` function
    }

    pub fn intrinsic_typed_arrays(&self) {
        // ArrayBuffer
        {
            let ab_obj = self.create_base_object().unwrap();
            ab_obj
                .install_field_defs(self, &js_array_buffer_proto_funcs)
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
                .install_field_defs(self, &js_array_buffer_funcs)
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_ARRAY_BUFFER as usize] = ab;
        }

        // SharedArrayBuffer
        {
            let sab_obj = self.create_base_object().unwrap();
            sab_obj
                .install_field_defs(self, &js_shared_array_buffer_proto_funcs)
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
                .install_field_defs(self, &js_shared_array_buffer_funcs)
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_SHARED_ARRAY_BUFFER as usize] = sab;
        }

        // Typed arrays
        {
            let ta_obj = self.create_base_object().unwrap();
            ta_obj
                .install_field_defs(self, &js_typed_array_base_proto_funcs)
                .unwrap();
            let ta_proto: JSValue = ta_obj.into();

            // TypedArray.prototype.toString must be the same object as Array.prototype.toString
            let to_string = self.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
                .get_property(self, JS_ATOM_toString.deref())
                .unwrap();

            // XXX: should use alias method in JSCFunctionListEntry
            ta_proto
                .define_property_value(
                    self,
                    JS_ATOM_toString.deref(),
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
                .install_field_defs(self, &js_typed_array_base_funcs)
                .unwrap();

            self.bind_ctor_proto(&typed_array_base_func, &ta_proto)
                .unwrap();

            for cid in
                JS_CLASS_UINT8C_ARRAY..(JS_CLASS_UINT8C_ARRAY + JS_TYPED_ARRAY_COUNT as JSClassID)
            {
                let nby = 1 << TypedArray::SIZE_LOG2(cid);

                let ta_obj = self
                    .create_base_object_from_proto(ta_proto.clone())
                    .unwrap();
                ta_obj
                    .define_property_value_str(self, "BYTES_PER_ELEMENT", JSValue::Int(nby), 0)
                    .unwrap();
                let proto: JSValue = ta_obj.into();

                let name =
                    AtomRef(JS_ATOM_Uint8ClampedArray.id() + (cid - JS_CLASS_UINT8C_ARRAY) as u32)
                        .to_utf8();

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
                    .define_property_value_str(self, "BYTES_PER_ELEMENT", JSValue::Int(nby), 0)
                    .unwrap();

                self.0.borrow_mut().class_proto[cid as usize] = proto;
            }
        }

        // DataView
        {
            let dv_obj = self.create_base_object().unwrap();
            dv_obj
                .install_field_defs(self, &js_dataview_proto_funcs)
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

    pub fn intrinsic_regexp(&self) {
        let obj = self.create_base_object().unwrap();
        obj.install_field_defs(self, &js_regexp_proto_funcs)
            .unwrap();

        let ctor = self
            .create_global_constructor_or_func(
                "RegExp",
                js_regexp_constructor,
                2,
                &obj.clone().into(),
                0,
            )
            .unwrap();
        ctor.install_field_defs(self, &js_regexp_funcs).unwrap();

        let iter_proto = self.0.borrow().iterator_proto.clone();
        let iter = self.create_base_object_from_proto(iter_proto).unwrap();
        iter.install_field_defs(self, &js_regexp_string_iterator_proto_funcs)
            .unwrap();

        let mut cx = self.0.borrow_mut();
        cx.regexp_ctor = ctor;
        cx.class_proto[JS_CLASS_REGEXP as usize] = obj.into();
        cx.class_proto[JS_CLASS_REGEXP_STRING_ITERATOR as usize] = iter.into();
    }

    pub fn intrinsic_proxy(&self) {
        let func_obj = self
            .create_native_function(
                NativeFunctionType::Constructor(js_proxy_constructor),
                "Proxy",
                2,
                0,
            )
            .unwrap();
        JS_SetConstructorBit(self, &func_obj, true);

        func_obj.install_field_defs(self, &js_proxy_funcs).unwrap();

        self.global_obj()
            .define_property_value_str(
                self,
                "Proxy",
                func_obj,
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }

    pub fn intrinsic_async(&self) {
        let rt = self.rt();
        debug_assert!(rt.is_class_registered(JS_CLASS_PROMISE));

        // Promise
        {
            let promise_obj = self.create_base_object().unwrap();
            promise_obj
                .install_field_defs(self, &js_promise_proto_funcs)
                .unwrap();
            let promise_proto: JSValue = promise_obj.into();

            let promise_ctor = self
                .create_native_function(
                    NativeFunctionType::Constructor(Promise::class_constructor),
                    "Promise",
                    1,
                    0,
                )
                .unwrap();
            promise_ctor
                .install_field_defs(self, &js_promise_funcs)
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
                .install_field_defs(self, &js_async_function_proto_funcs)
                .unwrap();
            let af_proto = af_obj.into();

            let af_ctor = self
                .create_native_function_from_proto(
                    NativeFunctionType::ConstructorOrFunc(BytecodeFunction::class_constructor),
                    "AsyncFunction",
                    1,
                    JSFunctionKindEnum::JS_FUNC_ASYNC as i16,
                    self.0.borrow().function_ctor.clone(),
                )
                .unwrap();

            self.bind_ctor_proto_ex(&af_ctor, &af_proto, 0, JS_PROP_CONFIGURABLE)
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_ASYNC_FUNCTION as usize] = af_proto;
        }

        // AsyncIteratorPrototype
        {
            let a_iter_obj = self.create_base_object().unwrap();
            a_iter_obj
                .install_field_defs(self, &js_async_iterator_proto_funcs)
                .unwrap();
            self.0.borrow_mut().async_iterator_proto = a_iter_obj.into();
        }

        // AsyncFromSyncIteratorPrototype
        {
            let iter_obj = self
                .create_base_object_from_proto(self.0.borrow().async_iterator_proto.clone())
                .unwrap();
            iter_obj
                .install_field_defs(self, &js_async_from_sync_iterator_proto_funcs)
                .unwrap();
            self.0.borrow_mut().class_proto[JS_CLASS_ASYNC_FROM_SYNC_ITERATOR as usize] =
                iter_obj.into();
        }

        // AsyncGenerator
        {
            // AsyncGeneratorPrototype
            let proto: JSValue = {
                let o = self
                    .create_base_object_from_proto(self.0.borrow().async_iterator_proto.clone())
                    .unwrap();
                o.install_field_defs(self, &js_async_generator_proto_funcs)
                    .unwrap();
                o.into()
            };

            let func_proto = self.0.borrow().function_proto.clone();

            // AsyncGeneratorFunction
            let agen_func: JSValue = {
                let o = self
                    .create_base_object_from_proto(func_proto.clone())
                    .unwrap();
                o.install_field_defs(self, &js_async_generator_function_proto_funcs)
                    .unwrap();
                o.into()
            };

            self.bind_ctor_proto_ex(
                &agen_func,
                &proto,
                JS_PROP_CONFIGURABLE,
                JS_PROP_CONFIGURABLE,
            )
            .unwrap();

            let ctor = self
                .create_native_function_from_proto(
                    NativeFunctionType::ConstructorOrFunc(BytecodeFunction::class_constructor),
                    "AsyncGeneratorFunction",
                    1,
                    JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR as i16,
                    func_proto,
                )
                .unwrap();

            self.bind_ctor_proto_ex(&ctor, &agen_func, 0, JS_PROP_CONFIGURABLE)
                .unwrap();

            {
                let slot = &mut self.0.borrow_mut().class_proto;
                slot[JS_CLASS_ASYNC_GENERATOR as usize] = proto;
                slot[JS_CLASS_ASYNC_GENERATOR_FUNCTION as usize] = agen_func;
            }
        }
    }

    pub fn intrinsic_json(&self) {
        self.0
            .borrow()
            .global_obj
            .install_field_defs(self, &js_json_obj)
            .unwrap();
    }
}
