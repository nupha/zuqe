use std::ops::Deref;

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_ARRAY_ITERATOR, JS_CLASS_BOOLEAN, JS_CLASS_BYTECODE_FUNCTION,
    JS_CLASS_ERROR, JS_CLASS_NUMBER, JS_CLASS_OBJECT,
    array::{
        ArrayObjPayload, js_array_constructor, js_array_funcs, js_array_iterator_proto_funcs,
        js_array_proto_funcs,
    },
    atom::consts::{
        JS_ATOM_Symbol_unscopables, JS_ATOM_arguments, JS_ATOM_caller, JS_ATOM_empty_string,
        JS_ATOM_length, JS_ATOM_message, JS_ATOM_name, JS_ATOM_values,
    },
    context::{JS_MODE_STRICT, JSContext},
    error::{ErrorKind, STD_ERROR_NAMES, js_error_proto_funcs},
    function::{BytecodeFunction, JSFunctionKindEnum, NativeFunctionType, js_function_proto},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_LENGTH, JS_PROP_WRITABLE, JSShape, ObjectPayload},
    value::JSValue,
};

impl JSContext {
    /// Initialize minimum amount of objects to be able to compile code and display error messages.
    /// No Atom should be allocated by this function.
    pub fn init_core_objects_ex(&self, with_array: bool, with_errors: bool) {
        // Object
        let object = self.create_base_object_with_proto(None).unwrap();
        self.0.borrow_mut().class_proto[JS_CLASS_OBJECT as usize] = object.clone().into();

        // Function
        {
            let func_obj = self
                .create_native_function_with_proto(
                    NativeFunctionType::Func(js_function_proto),
                    "",
                    0,
                    0,
                    object.clone().into(),
                )
                .unwrap();

            let mut cx = self.0.borrow_mut();
            cx.function_proto.replace(func_obj.clone());
            cx.class_proto[JS_CLASS_BYTECODE_FUNCTION as usize] = func_obj.into();
        }

        // Error
        if with_errors {
            let error = self.create_base_object().unwrap();
            self.0.borrow_mut().class_proto[JS_CLASS_ERROR as usize] = error.clone().into();
            error
                .install_field_defs(self, &js_error_proto_funcs)
                .unwrap();

            // standard error objects
            let msg = JS_ATOM_empty_string.force_string(self).unwrap();
            for (i, name) in STD_ERROR_NAMES.iter().enumerate() {
                let err = self
                    .create_object_with_proto(error.clone().into(), ObjectPayload::OBJECT)
                    .unwrap();

                let name = self.create_atom_string(name).unwrap();
                err.define_property_value(
                    self,
                    &JS_ATOM_name,
                    JSValue::String(name),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

                err.define_property_value(
                    self,
                    &JS_ATOM_message,
                    JSValue::String(msg.clone()),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

                self.0.borrow_mut().native_error_proto[i] = err.into();
            }
        }

        // Array
        if with_array {
            // 1. make Array prototype object
            let array_proto = self
                .create_object_with_proto(
                    object.clone().into(),
                    ObjectPayload::ARRAY(ArrayObjPayload::default()),
                )
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_ARRAY as usize] = array_proto.clone().into();

            // 2. make default shape for Array
            let array_shape = JSShape::new(self, Some(array_proto.clone()), 1).unwrap();

            // define length property, index=0
            array_shape
                .add_shape_property(
                    self,
                    JS_ATOM_length.deref().clone(),
                    JS_PROP_WRITABLE | JS_PROP_LENGTH,
                    None,
                )
                .unwrap();

            self.0.borrow_mut().array_shape.replace(array_shape);
        }

        /* XXX: could test it on first context creation to ensure that no
        new atoms are created in JS_AddIntrinsicBasicObjects(). It is
        necessary to avoid useless renumbering of atoms after
        JS_EvalBinary() if it is done just after JS_AddIntrinsicBasicObjects(). */
        // assert_eq!((*ctx.rt).atom_count as u32, JS_ATOM_INIT_COUNT);
    }

    /// Initialize minimum amount of objects to be able to compile code and display error messages.
    /// No AtomRef should be allocated by this function.
    #[inline(always)]
    pub fn init_core_objects(&self) {
        self.init_core_objects_ex(true, true);
    }
}
