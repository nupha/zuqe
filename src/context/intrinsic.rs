// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_ITERATOR, JS_CLASS_BOOLEAN, JS_CLASS_BYTECODE_FUNCTION,
    JS_CLASS_ERROR, JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_STRING, JS_CLASS_STRING_ITERATOR,
    atom::Atom,
    builtins::{
        array::{js_array_iterator_proto_funcs, js_array_proto_funcs},
        boolean::js_boolean_proto_funcs,
        error::js_error_constructor,
        function::js_function_proto_funcs,
        global::js_global_eval,
        number::js_number_proto_funcs,
        object::js_object_proto_funcs,
        string::{js_string_iterator_proto_funcs, js_string_proto_funcs},
    },
    context::ScopedContext,
    error::{ErrorKind, STD_ERROR_NAMES},
    function::{BytecodeFunction, Callback, FunctionKind, NativeFunction},
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_GET,
        JS_PROP_HAS_SET, JS_PROP_WRITABLE, PayloadPtr,
    },
    value::JSValue,
    vm_stack::ExecMode,
};

impl<'c> ScopedContext<'c> {
    pub(super) fn intrinsic_base_object(&mut self) {
        let mut obj = self.class_proto(JS_CLASS_OBJECT).unwrap();
        obj.install_fields(self, js_object_proto_funcs).unwrap();

        self.use_class(JS_CLASS_OBJECT).unwrap();
        let ctor = self.class_constructor(JS_CLASS_OBJECT).unwrap();

        self.global_obj()
            .define_property_value(
                self,
                Atom::CONST_Object,
                ctor.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }

    pub(super) fn intrinsic_base_function(&mut self) {
        debug_assert!(
            self.class_slots[JS_CLASS_BYTECODE_FUNCTION as usize]
                .proto
                .is_some()
        );

        let mut proto = self.function_proto();
        proto.install_fields(self, js_function_proto_funcs).unwrap();

        // FOR LEGACY: make throw type error function
        let mut throw_type_error = self
            .create_native_function(
                Callback::Func(|ctx, cc| {
                    // throw only in strict mode
                    let throw = cc
                        .this()
                        .as_object()
                        .and_then(|o| {
                            o.payload_bc_function().map(|bc| {
                                let b = bc.bytecode();
                                b.mode().contains(ExecMode::STRICT) || !b.has_prototype()
                            })
                        })
                        .unwrap_or(true);

                    if throw {
                        Err(ctx.throw_type_error("invalid property access"))
                    } else {
                        Ok(JSValue::UNDEFINED)
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

        self.use_class(JS_CLASS_ERROR).unwrap();

        let ctor = self.class_constructor(JS_CLASS_ERROR).unwrap();

        self.global_obj()
            .define_property_value(
                self,
                Atom::CONST_Error,
                ctor.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        // standard error constructors
        for (i, name) in STD_ERROR_NAMES.iter().enumerate() {
            let atom = Atom::intern_static(self, name).unwrap();
            let func_obj = NativeFunction::create(
                self,
                Callback::ConstructorOrFunc(js_error_constructor),
                atom,
                if i == ErrorKind::Aggregate as usize {
                    2
                } else {
                    1
                },
                i as _, // magic
                ctor,
                None,
            )
            .unwrap();

            let et_proto = self.error_protos[i]; // <-- this was initialized in core
            self.add_global_constructor(name, func_obj.into(), et_proto.as_object().unwrap())
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

        self.use_class(JS_CLASS_ARRAY).unwrap();

        let array_ctor = self.class_constructor(JS_CLASS_ARRAY).unwrap();

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
        {
            const unscopables: &[&str] = &[
                "at",
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
                obj1.define_property_value_str(self, s, true.into(), JS_PROP_C_W_E)
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
            .create_instance_with_proto(
                self.class_proto(JS_CLASS_OBJECT),
                PayloadPtr::new(JS_CLASS_NUMBER, JSValue::ZERO),
            )
            .unwrap();
        proto.install_fields(self, js_number_proto_funcs).unwrap();
        self.class_slots[JS_CLASS_NUMBER as usize].proto = Some(proto);

        self.use_class(JS_CLASS_NUMBER).unwrap();

        let ctor = self.class_constructor(JS_CLASS_NUMBER).unwrap();
        self.global_obj()
            .define_property_value(
                self,
                Atom::CONST_Number,
                ctor.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }

    pub(super) fn intrinsic_base_boolean(&mut self) {
        let mut proto = self
            .create_instance_with_proto(
                self.class_proto(JS_CLASS_OBJECT),
                PayloadPtr::new(JS_CLASS_BOOLEAN, false),
            )
            .unwrap();
        proto.install_fields(self, js_boolean_proto_funcs).unwrap();
        self.class_slots[JS_CLASS_BOOLEAN as usize].proto = Some(proto);

        self.use_class(JS_CLASS_BOOLEAN).unwrap();

        let ctor = self.class_constructor(JS_CLASS_BOOLEAN).unwrap();

        self.global_obj()
            .define_property_value(
                self,
                Atom::CONST_Boolean,
                ctor.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }

    pub(super) fn intrinsic_base_string(&mut self) {
        let empty_str = Atom::CONST_empty_string.force_string(self).unwrap();
        let string_proto = {
            let mut o = self
                .create_instance_with_proto(
                    self.class_proto(JS_CLASS_OBJECT),
                    PayloadPtr::new(JS_CLASS_STRING, empty_str),
                )
                .unwrap();
            o.install_fields(self, js_string_proto_funcs).unwrap();
            o
        };
        self.class_slots[JS_CLASS_STRING as usize].proto = Some(string_proto);

        let iter_proto = self.iterator_proto().unwrap();
        let mut string_iter = self
            .create_base_object_with_proto(Some(iter_proto))
            .unwrap();
        string_iter
            .install_fields(self, js_string_iterator_proto_funcs)
            .unwrap();
        self.class_slots[JS_CLASS_STRING_ITERATOR as usize].proto = Some(string_iter);

        self.use_class(JS_CLASS_STRING).unwrap();

        let ctor = self.class_constructor(JS_CLASS_STRING).unwrap();
        self.global_obj()
            .define_property_value(
                self,
                Atom::CONST_String,
                ctor.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }

    pub(super) fn intrinsic_base_symbol(&mut self) {
        let mut proto = self.create_base_object().unwrap();
        proto
            .install_fields(self, crate::builtins::symbol::js_symbol_proto_funcs)
            .unwrap();
        self.class_slots[crate::JS_CLASS_SYMBOL as usize].proto = Some(proto);

        self.use_class(crate::JS_CLASS_SYMBOL).unwrap();

        let mut ctor = self.class_constructor(crate::JS_CLASS_SYMBOL).unwrap();
        self.global_obj()
            .define_property_value(
                self,
                Atom::CONST_Symbol,
                ctor.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        // pre-defined builtin Symbol_XXX
        {
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

    /// Mirror of `intrinsic_base_number` for the BigInt class (feature-gated).
    /// Per spec the BigInt prototype is a plain object — it has no
    /// `[[BigIntData]]` slot, so `thisBigIntValue(BigInt.prototype)` throws.
    #[cfg(feature = "big_int")]
    pub(super) fn intrinsic_base_bigint(&mut self) {
        let mut proto = self.create_base_object().unwrap();
        proto
            .install_fields(self, crate::builtins::bigint::js_bigint_proto_funcs)
            .unwrap();
        self.class_slots[crate::JS_CLASS_BIG_INT as usize].proto = Some(proto);

        self.use_class(crate::JS_CLASS_BIG_INT).unwrap();

        let ctor = self.class_constructor(crate::JS_CLASS_BIG_INT).unwrap();
        self.global_obj()
            .define_property_value(
                self,
                Atom::CONST_BigInt,
                ctor.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();
    }
}
