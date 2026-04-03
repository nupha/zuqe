// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_ITERATOR, JS_CLASS_BOOLEAN, JS_CLASS_BYTECODE_FUNCTION,
    JS_CLASS_ERROR, JS_CLASS_GENERATOR, JS_CLASS_GENERATOR_FUNCTION, JS_CLASS_NUMBER,
    JS_CLASS_OBJECT, JS_CLASS_STRING, JS_CLASS_STRING_ITERATOR,
    atom::Atom,
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
    },
    context::{ScopedContext, ctx::ProtoCtorPair},
    error::{ErrorKind, STD_ERROR_NAMES},
    function::{BytecodeFunction, Callback, FunctionKind, NativeFunction},
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_GET,
        JS_PROP_HAS_SET, JS_PROP_WRITABLE, ObjectPayload,
    },
    stack_frame::ExecMode,
    value::JSValue,
};

impl<'c> ScopedContext<'c> {
    pub(super) fn intrinsic_base_object(&mut self) {
        let mut obj = self.class_proto(JS_CLASS_OBJECT).unwrap();
        obj.install_fields(self, js_object_proto_funcs).unwrap();

        let mut ctor = self
            .create_constructor_internal(
                "Object",
                Callback::ConstructorOrFunc(js_object_constructor),
                1,
                obj,
                0,
                true,
            )
            .unwrap();

        ctor.install_fields(self, js_object_funcs).unwrap();

        self.class_slots[JS_CLASS_OBJECT as usize].ctor = Some(ctor);
    }

    pub(super) fn intrinsic_base_function(&mut self) {
        debug_assert!(
            self.class_slots[JS_CLASS_BYTECODE_FUNCTION as usize]
                .proto
                .is_some()
        );

        let mut proto = self.function_proto();

        // FOR LEGACY: make throw type error function
        let mut throw_type_error = self
            .create_native_function(
                Callback::Func(|ctx, cc| {
                    // throw only in strict mode
                    let throw = cc
                        .this()
                        .as_object()
                        .and_then(|o| {
                            o.payload().bc_function().map(|bc| {
                                let b = bc.bytecode();
                                b.mode().contains(ExecMode::STRICT) || !b.has_prototype()
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
        self.throw_type_error = throw_type_error.into();

        // FOR LEGACY: add 'caller' and 'arguments' properties to proto
        {
            proto
                .define_property(
                    self,
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
                    self,
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

        proto.install_fields(self, js_function_proto_funcs).unwrap();

        let ctor = self
            .init_class_constructor_or_func(
                JS_CLASS_BYTECODE_FUNCTION,
                BytecodeFunction::class_constructor,
                1,
                FunctionKind::Normal as _,
                proto,
                &[],
            )
            .unwrap();

        self.global_obj()
            .define_property_value(
                self,
                Atom::CONST_Function,
                ctor.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }

    pub(super) fn intrinsic_base_errors(&mut self) {
        debug_assert!(self.class_slots[JS_CLASS_ERROR as usize].proto.is_some());

        let proto = self.class_proto(JS_CLASS_ERROR).unwrap();
        let ctor = self
            .create_constructor_internal(
                "Error",
                Callback::ConstructorOrFunc(js_error_constructor),
                1,
                proto,
                -1,
                true,
            )
            .unwrap();
        self.class_slots[JS_CLASS_ERROR as usize].ctor = Some(ctor);

        // standard error constructors
        for (i, name) in STD_ERROR_NAMES.iter().enumerate() {
            let func_obj = NativeFunction::create(
                self,
                Callback::ConstructorOrFunc(js_error_constructor),
                name,
                if i == ErrorKind::Aggregate as usize {
                    2
                } else {
                    1
                },
                i as _, // magic
                Some(ctor),
                None,
            )
            .unwrap();

            let et_proto = self.error_protos[i]; // <-- this was initialized in core
            self.add_global_constructor(
                name,
                func_obj.into(),
                et_proto.as_object().copied().unwrap(),
            )
            .unwrap();
        }
    }

    /// Note: make sure iterator_proto is initialized
    pub(super) fn intrinsic_base_array(&mut self) {
        let mut array_proto = self.class_proto(JS_CLASS_ARRAY).unwrap();
        array_proto
            .install_fields(self, js_array_proto_funcs)
            .unwrap();

        self.array_proto_values = array_proto.get_property(self, Atom::CONST_values).unwrap();

        let array_ctor = self
            .init_class_constructor_or_func(
                JS_CLASS_ARRAY,
                js_array_constructor,
                1,
                0,
                array_proto,
                js_array_funcs,
            )
            .unwrap();

        self.global_obj()
            .define_property_value(
                self,
                Atom::CONST_Array,
                array_ctor.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        // initialize Array.prototype[Symbol.unscopables]
        // XXX: create auto_initializer
        #[cfg(feature = "builtin_symbol")]
        {
            const unscopables: &[&str] = &[
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
                obj1.define_property_value_str(self, s, JSValue::Bool(true), JS_PROP_C_W_E)
                    .unwrap();
            }

            array_proto
                .define_property_value(
                    self,
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
            .install_fields(self, js_array_iterator_proto_funcs)
            .unwrap();
        self.class_slots[JS_CLASS_ARRAY_ITERATOR as usize].proto = Some(array_iter);
    }

    pub(super) fn intrinsic_base_number(&mut self) {
        let mut proto = self
            .create_object_with_proto(
                self.class_proto(JS_CLASS_OBJECT),
                ObjectPayload::NUMBER(JSValue::Int(0)),
            )
            .unwrap();
        proto.install_fields(self, js_number_proto_funcs).unwrap();

        let mut ctor = self
            .create_constructor_internal(
                "Number",
                Callback::ConstructorOrFunc(js_number_constructor),
                1,
                proto,
                0,
                true,
            )
            .unwrap();

        ctor.install_fields(self, js_number_funcs).unwrap();

        self.class_slots[JS_CLASS_NUMBER as usize] = ProtoCtorPair {
            proto: Some(proto),
            ctor: Some(ctor),
        };
    }

    pub(super) fn intrinsic_base_boolean(&mut self) {
        let mut proto = self
            .create_object_with_proto(
                self.class_proto(JS_CLASS_OBJECT),
                ObjectPayload::BOOLEAN(false),
            )
            .unwrap();

        proto.install_fields(self, js_boolean_proto_funcs).unwrap();

        let ctor = self
            .create_constructor_internal(
                "Boolean",
                Callback::ConstructorOrFunc(js_boolean_constructor),
                1,
                proto,
                0,
                true,
            )
            .unwrap();

        self.class_slots[JS_CLASS_BOOLEAN as usize] = ProtoCtorPair {
            proto: Some(proto),
            ctor: Some(ctor),
        };
    }

    pub(super) fn intrinsic_base_string(&mut self) {
        let empty_str = Atom::CONST_empty_string.force_string(self).unwrap();

        let string_proto = {
            let mut o = self
                .create_object_with_proto(
                    self.class_proto(JS_CLASS_OBJECT),
                    ObjectPayload::STRING(empty_str),
                )
                .unwrap();
            o.install_fields(self, js_string_proto_funcs).unwrap();
            o
        };

        let mut string_ctor = self
            .create_constructor_internal(
                "String",
                Callback::ConstructorOrFunc(js_string_constructor),
                1,
                string_proto,
                0,
                true,
            )
            .unwrap();

        string_ctor.install_fields(self, js_string_funcs).unwrap();

        let iter_proto = self.iterator_proto().unwrap();
        let mut string_iter = self
            .create_base_object_with_proto(Some(iter_proto))
            .unwrap();
        string_iter
            .install_fields(self, js_string_iterator_proto_funcs)
            .unwrap();

        self.class_slots[JS_CLASS_STRING as usize] = ProtoCtorPair {
            proto: Some(string_proto),
            ctor: Some(string_ctor),
        };
        self.class_slots[JS_CLASS_STRING_ITERATOR as usize] = ProtoCtorPair {
            proto: Some(string_iter),
            ctor: None,
        };
    }

    #[cfg(feature = "builtin_symbol")]
    pub(super) fn intrinsic_base_symbol(&mut self) {
        let mut proto = self.create_base_object().unwrap();
        proto
            .install_fields(self, crate::builtins::symbol::js_symbol_proto_funcs)
            .unwrap();

        let mut ctor = self
            .create_constructor_internal(
                "Symbol",
                Callback::ConstructorOrFunc(crate::builtins::symbol::js_symbol_constructor),
                0,
                proto,
                0,
                true,
            )
            .unwrap();

        ctor.install_fields(self, crate::builtins::symbol::js_symbol_funcs)
            .unwrap();

        // pre-define builtin Symbol_XXX
        {
            // let mut ctx_ptr = ContextPtr::from(self);
            let rt = self.rt_ptr();

            for i in crate::atom::ATOM_CONST_SYMBOL_RANGE {
                let a = Atom::from_id(i);
                let atom_val = a.to_value(self).unwrap();

                ctor.define_property_value_str(
                    self,
                    a.to_utf8(&rt).split_once('.').unwrap().1, // skip "Symbol."
                    atom_val,
                    0,
                )
                .unwrap();
            }
        }

        self.class_slots[crate::JS_CLASS_SYMBOL as usize] = ProtoCtorPair {
            proto: Some(proto),
            ctor: Some(ctor),
        };
    }

    pub(super) fn intrinsic_base_generator(&mut self) {
        // generator prototype
        let iter_proto = self.iterator_proto().unwrap();
        let function_proto = self.function_proto();

        // generator
        let mut gen_proto = self
            .create_base_object_with_proto(Some(iter_proto))
            .unwrap();
        gen_proto
            .install_fields(self, js_generator_proto_funcs)
            .unwrap();

        self.class_slots[JS_CLASS_GENERATOR as usize] = ProtoCtorPair {
            proto: Some(gen_proto),
            ctor: None,
        };

        // generator function
        let mut gen_func_proto = self
            .create_base_object_with_proto(Some(function_proto))
            .unwrap();
        gen_func_proto
            .install_fields(self, js_generator_function_proto_funcs)
            .unwrap();
        self.class_slots[JS_CLASS_GENERATOR_FUNCTION as usize] = ProtoCtorPair {
            proto: Some(gen_func_proto),
            ctor: None,
        };

        // generator constructor
        let ctor = self
            .create_native_function(
                Callback::ConstructorOrFunc(BytecodeFunction::class_constructor),
                "GeneratorFunction",
                1,
                FunctionKind::Generator as i16,
            )
            .unwrap();

        self.bind_ctor_proto_ex(
            gen_func_proto,
            gen_proto,
            JS_PROP_CONFIGURABLE,
            JS_PROP_CONFIGURABLE,
        )
        .unwrap();

        self.bind_ctor_proto_ex(ctor.into(), gen_func_proto, 0, JS_PROP_CONFIGURABLE)
            .unwrap();
    }

    pub(super) fn intrinsic_eval(&mut self) {
        let eval_func: JSValue = self
            .create_native_function(Callback::Func(js_global_eval), "eval", 1, 0)
            .unwrap()
            .into();

        self.eval_func = eval_func;

        self.global_obj()
            .define_property_value(
                self,
                Atom::CONST_eval,
                eval_func,
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }
}
