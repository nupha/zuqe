// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_ERROR, JS_CLASS_OBJECT,
    array::ArrayPayload,
    atom::Atom,
    builtins::error::js_error_proto_funcs,
    class::ClassID,
    context::{ContextImpl, ScopedContext},
    error::STD_ERROR_NAMES,
    function::{Callback, NativeFunction, js_function_proto},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_LENGTH, JS_PROP_WRITABLE, Object, PayloadPtr, Shape},
    value::JSValue,
};

impl<'c> ScopedContext<'c> {
    /// Initialize minimum amount of objects to be able to compile code and display error messages.
    /// No Atom should be allocated by this function.
    pub(super) fn init_core_objects(&mut self) -> Result<(), i32> {
        debug_assert!(
            [
                JS_CLASS_OBJECT,
                JS_CLASS_BYTECODE_FUNCTION,
                JS_CLASS_ARRAY,
                JS_CLASS_ERROR,
            ]
            .iter()
            .max()
            .copied()
            .unwrap()
                < self.rt_ptr().class_registry.len() as ClassID
        );

        // Object
        let mut object = self.create_base_object_with_proto(None)?;
        object.set_flag_immutable_prototype(true);
        self.class_slots[JS_CLASS_OBJECT as usize].proto = Some(object);

        // Function
        {
            let function = NativeFunction::create(
                self,
                Callback::Func(js_function_proto),
                Atom::CONST_empty_string,
                0,
                0,
                object,
                None,
            )?;
            self.class_slots[JS_CLASS_BYTECODE_FUNCTION as usize].proto = Some(function.into());
        }

        // Error
        {
            let mut error_proto = self.create_base_object()?;
            error_proto
                .install_fields(self, js_error_proto_funcs)
                .unwrap();
            self.class_slots[JS_CLASS_ERROR as usize].proto = Some(error_proto);

            // standard error objects
            let msg = Atom::CONST_empty_string.force_string(self).unwrap();
            for (i, &name) in STD_ERROR_NAMES.iter().enumerate() {
                let mut err = self.create_base_object_with_proto(Some(error_proto))?;

                let name = self.create_string_interned(name)?;
                //let name = self.create_string_interned(name).unwrap();
                err.define_property_value(
                    self,
                    Atom::CONST_name,
                    JSValue::make_string(name),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )?;
                err.define_property_value(
                    self,
                    Atom::CONST_message,
                    JSValue::make_string(msg),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )?;

                self.error_protos[i] = err.into();
            }
        }

        // Array
        {
            // 1. make Array prototype object
            let array_proto = self.create_instance_with_proto(
                object.into(),
                PayloadPtr::new(JS_CLASS_ARRAY, ArrayPayload::default()),
            )?;
            self.class_slots[JS_CLASS_ARRAY as usize].proto = Some(array_proto);

            // 2. make default shape for Array
            let mut array_shape = Shape::create(self, Some(array_proto), 1).unwrap();
            self.rt_mut().unregister_shape(&array_shape);
            array_shape.add_shape_properties(
                self,
                &[(Atom::CONST_length, JS_PROP_WRITABLE | JS_PROP_LENGTH)],
            );

            // register with new hash
            self.rt_mut().register_shape(&array_shape).unwrap();
            self.array_shape.replace(array_shape);
        }

        Ok(())
    }
}

impl ContextImpl {
    #[inline]
    pub fn object_proto(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_OBJECT as usize);
        self.class_proto(JS_CLASS_OBJECT).unwrap()
    }

    #[inline]
    pub fn function_proto(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_BYTECODE_FUNCTION as usize);
        self.class_proto(JS_CLASS_BYTECODE_FUNCTION).unwrap()
    }

    #[inline]
    pub fn array_proto(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_ARRAY as usize);
        self.class_proto(JS_CLASS_ARRAY).unwrap()
    }

    #[inline]
    pub fn function_ctor(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_BYTECODE_FUNCTION as usize);
        self.class_constructor(JS_CLASS_BYTECODE_FUNCTION).unwrap()
    }

    #[inline]
    pub fn array_ctor(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_ARRAY as usize);
        self.class_constructor(JS_CLASS_ARRAY).unwrap()
    }
}
