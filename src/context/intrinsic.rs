use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_ITERATOR, JS_CLASS_BOOLEAN, JS_CLASS_ERROR, JS_CLASS_GENERATOR,
    JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_STRING,
    JS_CLASS_STRING_ITERATOR, JS_CLASS_SYMBOL,
    atom::{ATOM_CONST_SYMBOL_RANGE, Atom},
    builtins::{
        array::{
            js_array_constructor, js_array_funcs, js_array_iterator_proto_funcs,
            js_array_proto_funcs,
        },
        boolean::{js_boolean_constructor, js_boolean_proto_funcs},
        error::js_error_constructor,
        function::js_function_proto_funcs,
        generator::{js_generator_function_proto_funcs, js_generator_proto_funcs},
        global::js_global_eval,
        number::{js_number_constructor, js_number_funcs, js_number_proto_funcs},
        object::{js_object_constructor, js_object_funcs, js_object_proto_funcs},
        string::{
            js_string_constructor, js_string_funcs, js_string_iterator_proto_funcs,
            js_string_proto_funcs,
        },
        symbol::{js_symbol_constructor, js_symbol_funcs, js_symbol_proto_funcs},
    },
    context::ContextPtr,
    error::{ErrorKind, JSResult, STD_ERROR_NAMES},
    eval::{EvalMode, EvalType},
    function::{BytecodeFunction, Callback, JSFunctionKindEnum},
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_GET,
        JS_PROP_HAS_SET, JS_PROP_WRITABLE, ObjectPayload,
    },
    value::JSValue,
};

impl ContextPtr {
    pub(super) fn intrinsic_base_object(mut self) {
        let mut obj = self.class_proto(JS_CLASS_OBJECT).unwrap();
        obj.install_field_defs(&self, &js_object_proto_funcs)
            .unwrap();

        let mut ctor = self
            .create_global_constructor_or_func("Object", js_object_constructor, 1, obj.into(), 0)
            .unwrap();
        ctor.install_field_defs(&self, &js_object_funcs).unwrap();
    }

    pub(super) fn intrinsic_base_function(mut self) {
        // FOR LEGACY: make throw type error function
        let mut throw_type_error = self
            .create_native_function(
                Callback::Func(|cc| {
                    // throw only in strict mode
                    let throw = cc
                        .this()
                        .as_object()
                        .and_then(|o| {
                            o.payload().bc_function().map(|bc| {
                                let b = bc.bytecode();
                                b.mode().contains(EvalMode::STRICT) || !b.has_prototype()
                            })
                        })
                        .unwrap_or(true);

                    if throw {
                        cc.ctx().throw_type_error("invalid property access");
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

        throw_type_error.seal(&self, true).unwrap();
        self.throw_type_error = throw_type_error.into();

        let mut proto = self.function_proto();

        // FOR LEGACY: add 'caller' and 'arguments' properties
        {
            proto
                .define_property(
                    &self,
                    Atom::CONST_caller,
                    JSValue::UNDEFINED,
                    throw_type_error.into(),
                    throw_type_error.into(),
                    JS_PROP_HAS_GET
                        | JS_PROP_HAS_SET
                        | JS_PROP_HAS_CONFIGURABLE
                        | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

            proto
                .define_property(
                    &self,
                    Atom::CONST_arguments,
                    JSValue::UNDEFINED,
                    throw_type_error.into(),
                    throw_type_error.into(),
                    JS_PROP_HAS_GET
                        | JS_PROP_HAS_SET
                        | JS_PROP_HAS_CONFIGURABLE
                        | JS_PROP_CONFIGURABLE,
                )
                .unwrap();
        }

        proto
            .install_field_defs(&self, &js_function_proto_funcs)
            .unwrap();

        let ctor: JSValue = self
            .create_native_function(
                Callback::ConstructorOrFunc(BytecodeFunction::class_constructor),
                "Function",
                1,
                JSFunctionKindEnum::JS_FUNC_NORMAL as _,
            )
            .unwrap()
            .into();

        self.add_global_constructor("Function", ctor, proto.into());
        self.function_ctor = ctor;
    }

    pub(super) fn intrinsic_base_errors(mut self) {
        // Error() constructor
        let ctor = self
            .create_native_function(
                Callback::ConstructorOrFunc(js_error_constructor),
                "Error",
                1,
                -1,
            )
            .unwrap();

        let err_proto = self.class_proto(JS_CLASS_ERROR).unwrap();
        self.add_global_constructor("Error", ctor.into(), err_proto.into());

        // standard error constructors
        for (i, name) in STD_ERROR_NAMES.iter().enumerate() {
            let func_obj = self
                .create_native_function_with_proto(
                    Callback::ConstructorOrFunc(js_error_constructor),
                    name,
                    if i == ErrorKind::Aggregate as usize {
                        2
                    } else {
                        1
                    },
                    i as _, // magic
                    Some(ctor),
                )
                .unwrap();

            let err_proto = self.native_error_proto[i]; // <-- this was initialized in core
            self.add_global_constructor(name, func_obj.into(), err_proto);
        }
    }

    /// Note: make sure iterator_proto is initialized
    pub(super) fn intrinsic_base_array(mut self) {
        self.class_proto(JS_CLASS_ARRAY)
            .unwrap()
            .install_field_defs(&self, &js_array_proto_funcs)
            .unwrap();

        let array_proto = self.class_proto(JS_CLASS_ARRAY).unwrap();
        let mut array_ctor = self
            .create_global_constructor_or_func(
                "Array",
                js_array_constructor,
                1,
                array_proto.into(),
                0,
            )
            .unwrap();
        array_ctor
            .install_field_defs(&self, &js_array_funcs)
            .unwrap();
        self.array_ctor = array_ctor;

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
            let mut obj1 = self.create_base_object_with_proto(None).unwrap();
            for s in unscopables {
                obj1.define_property_value_str(&self, s, JSValue::Bool(true), JS_PROP_C_W_E)
                    .unwrap();
            }
            self.class_proto(JS_CLASS_ARRAY)
                .unwrap()
                .define_property_value(
                    &self,
                    Atom::CONST_Symbol_unscopables,
                    obj1.into(),
                    JS_PROP_CONFIGURABLE,
                )
                .unwrap();
        }

        let iter_proto = self.iterator_proto().unwrap();
        let mut array_iter = self
            .create_base_object_with_proto(Some(iter_proto))
            .unwrap();
        array_iter
            .install_field_defs(&self, &js_array_iterator_proto_funcs)
            .unwrap();

        let array_values = self
            .class_proto(JS_CLASS_ARRAY)
            .unwrap()
            .get_property(&self, Atom::CONST_values)
            .unwrap();

        self.array_proto_values = array_values;
        self.class_proto[JS_CLASS_ARRAY_ITERATOR as usize] = array_iter.into();
    }

    pub(super) fn intrinsic_base_number(mut self) {
        let mut obj = self
            .create_object_with_proto(
                self.class_proto(JS_CLASS_OBJECT),
                ObjectPayload::NUMBER(JSValue::Int(0)),
            )
            .unwrap();
        obj.install_field_defs(&self, &js_number_proto_funcs)
            .unwrap();

        let proto: JSValue = obj.into();

        let mut ctor = self
            .create_global_constructor_or_func("Number", js_number_constructor, 1, proto, 0)
            .unwrap();
        ctor.install_field_defs(&self, &js_number_funcs).unwrap();

        self.class_proto[JS_CLASS_NUMBER as usize] = proto;
    }

    pub(super) fn intrinsic_base_boolean(mut self) {
        let mut bool_obj = self
            .create_object_with_proto(
                self.class_proto(JS_CLASS_OBJECT),
                ObjectPayload::BOOLEAN(JSValue::Bool(false)),
            )
            .unwrap();

        bool_obj
            .install_field_defs(&self, &js_boolean_proto_funcs)
            .unwrap();

        self.create_global_constructor_or_func(
            "Boolean",
            js_boolean_constructor,
            1,
            bool_obj.into(),
            0,
        )
        .unwrap();

        self.class_proto[JS_CLASS_BOOLEAN as usize] = bool_obj.into();
    }

    pub(super) fn intrinsic_base_string(mut self) {
        let empty_str = Atom::CONST_empty_string.force_string(&self).unwrap();

        let string_obj: JSValue = {
            let mut o = self
                .create_object_with_proto(
                    self.class_proto(JS_CLASS_OBJECT),
                    ObjectPayload::STRING(empty_str.into()),
                )
                .unwrap();

            o.install_field_defs(&self, &js_string_proto_funcs).unwrap();
            o.into()
        };

        let mut string_ctor = self
            .create_global_constructor_or_func("String", js_string_constructor, 1, string_obj, 0)
            .unwrap();
        string_ctor
            .install_field_defs(&self, &js_string_funcs)
            .unwrap();

        let iter_proto = self.iterator_proto().unwrap();
        let mut string_iter = self
            .create_base_object_with_proto(Some(iter_proto))
            .unwrap();
        string_iter
            .install_field_defs(&self, &js_string_iterator_proto_funcs)
            .unwrap();

        self.class_proto[JS_CLASS_STRING as usize] = string_obj;
        self.class_proto[JS_CLASS_STRING_ITERATOR as usize] = string_iter.into();
    }

    pub(super) fn intrinsic_base_symbol(mut self) {
        let mut obj = self.create_base_object().unwrap();
        obj.install_field_defs(&self, &js_symbol_proto_funcs)
            .unwrap();

        let mut ctor = self
            .create_global_constructor_or_func("Symbol", js_symbol_constructor, 0, obj.into(), 0)
            .unwrap();
        ctor.install_field_defs(&self, &js_symbol_funcs).unwrap();

        // pre-define builtin Symbol_XXX
        for i in ATOM_CONST_SYMBOL_RANGE {
            let a = Atom::from_id(i);
            a.with_utf8(|s| {
                let str = s.split_once('.').unwrap().1; // skip "Symbol."
                ctor.define_property_value_str(&self, str, a.string_or_symbol(&self).unwrap(), 0)
                    .unwrap();
            });
        }

        self.class_proto[JS_CLASS_SYMBOL as usize] = obj.into();
    }

    pub(super) fn intrinsic_base_generator(mut self) {
        // generator prototype
        let iter_proto = self.iterator_proto().unwrap();
        let mut obj = self
            .create_base_object_with_proto(Some(iter_proto))
            .unwrap();
        obj.install_field_defs(&self, &js_generator_proto_funcs)
            .unwrap();
        let proto: JSValue = obj.into();
        self.class_proto[JS_CLASS_GENERATOR as usize] = proto;

        // generator function prototype
        let mut generator_function = self
            .create_base_object_with_proto(Some(self.function_proto()))
            .unwrap();
        generator_function
            .install_field_defs(&self, &js_generator_function_proto_funcs)
            .unwrap();
        let generator_function_proto: JSValue = generator_function.into();
        self.class_proto[JS_CLASS_GENERATOR_FUNCTION as usize] = generator_function_proto;

        // generator constructor
        let ctor: JSValue = self
            .create_native_function(
                Callback::ConstructorOrFunc(BytecodeFunction::class_constructor),
                "GeneratorFunction",
                1,
                JSFunctionKindEnum::JS_FUNC_GENERATOR as i16,
            )
            .unwrap()
            .into();

        self.bind_ctor_proto_ex(
            generator_function_proto,
            proto,
            JS_PROP_CONFIGURABLE,
            JS_PROP_CONFIGURABLE,
        )
        .unwrap();

        self.bind_ctor_proto_ex(ctor, generator_function_proto, 0, JS_PROP_CONFIGURABLE)
            .unwrap();
    }

    // #[deprecated]
    // pub fn intrinsic_date(mut self) {
    //     let mut obj: JSValue = self.create_base_object().unwrap().into();
    //     obj.install_field_defs(&self, &js_date_proto_funcs).unwrap();

    //     let mut ctor = self
    //         .create_global_constructor_or_func("Date", js_date_constructor, 7, obj, 0)
    //         .unwrap();
    //     ctor.install_field_defs(&self, &js_date_funcs).unwrap();

    //     self.class_proto[JS_CLASS_DATE as usize] = obj;
    // }

    pub(super) fn intrinsic_eval(mut self) {
        self.eval_handle.replace(
            |mut ctx: ContextPtr,
             this_obj: JSValue,
             input: &[u8],
             filename: &str,
             eval_type: EvalType,
             eval_flags: u32,
             scope_idx: i32|
             -> JSResult {
                ctx.eval_internal(this_obj, input, filename, eval_type, eval_flags, scope_idx)
            },
        );

        let eval_func: JSValue = self
            .create_native_function(Callback::Func(js_global_eval), "eval", 1, 0)
            .unwrap()
            .into();
        self.eval_func = eval_func;

        self.global_obj()
            .define_property_value(
                &self,
                Atom::CONST_eval,
                eval_func,
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }
}
