use crate::{
    JS_CLASS_ARRAY, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_ERROR, JS_CLASS_OBJECT,
    array::JSObjectArray,
    atom::consts::{JS_ATOM_empty_string, JS_ATOM_length, JS_ATOM_message, JS_ATOM_name},
    context::JSContext,
    error::{STD_ERROR_NAMES, js_error_proto_funcs},
    function::{NativeFunctionType, js_function_proto},
    object::ObjectDetail,
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_INITIAL_HASH_SIZE, JS_PROP_LENGTH, JS_PROP_WRITABLE, JSShape,
    },
    value::JSValue,
};

impl JSContext {
    /// Initialize minimum amount of objects to be able to compile code and display error messages.
    /// No JSAtom should be allocated by this function.
    pub fn init_core_objects_ex(&self, with_function: bool, with_array: bool, with_errors: bool) {
        // Object
        let object = self
            .create_object_from_proto(JSValue::NULL, ObjectDetail::OBJECT)
            .unwrap();
        self.0.borrow_mut().class_proto[JS_CLASS_OBJECT as usize] = object.clone().into();

        if with_function {
            // Function
            let func = self
                .create_native_function_from_proto(
                    NativeFunctionType::Func(js_function_proto),
                    "",
                    0,
                    0,
                    object.clone().into(),
                )
                .unwrap();

            self.with_mut(|cx| {
                cx.function_proto = func.clone();
                cx.class_proto[JS_CLASS_BYTECODE_FUNCTION as usize] = func;
            });
        }

        if with_errors {
            // Error objects
            let error = self.create_base_object().unwrap();
            self.0.borrow_mut().class_proto[JS_CLASS_ERROR as usize] = error.clone().into();
            error
                .install_native_fields(self, &js_error_proto_funcs)
                .unwrap();

            // standard error objects
            let msg = JS_ATOM_empty_string.to_string(self).unwrap();
            for (i, name) in STD_ERROR_NAMES.iter().enumerate() {
                let err = self
                    .create_object_from_proto(error.clone().into(), ObjectDetail::OBJECT)
                    .unwrap();

                let name = self.create_atom_string(name).unwrap();
                err.define_property_value(
                    self,
                    JS_ATOM_name,
                    name,
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

                err.define_property_value(
                    self,
                    JS_ATOM_message,
                    msg.clone(),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )
                .unwrap();

                self.0.borrow_mut().native_error_proto[i] = err.into();
            }
        }

        if with_array {
            // Array: the array prototype is an array
            let array = self
                .create_object_from_proto(
                    object.clone().into(),
                    ObjectDetail::ARRAY(JSObjectArray::default()),
                )
                .unwrap();

            self.0.borrow_mut().class_proto[JS_CLASS_ARRAY as usize] = array.clone().into();

            let array_shape0 =
                JSShape::with_capacity(self, Some(array.clone()), JS_PROP_INITIAL_HASH_SIZE, 1)
                    .unwrap();
            // define length property at index 0
            array_shape0
                .add_shape_property(
                    self,
                    JS_ATOM_length,
                    JS_PROP_WRITABLE | JS_PROP_LENGTH,
                    None,
                )
                .unwrap();
            self.0.borrow_mut().array_shape.replace(array_shape0);
        }

        /* XXX: could test it on first context creation to ensure that no
        new atoms are created in JS_AddIntrinsicBasicObjects(). It is
        necessary to avoid useless renumbering of atoms after
        JS_EvalBinary() if it is done just after JS_AddIntrinsicBasicObjects(). */
        // assert_eq!((*ctx.rt).atom_count as u32, JS_ATOM_INIT_COUNT);
    }

    /// Initialize minimum amount of objects to be able to compile code and display error messages.
    /// No JSAtom should be allocated by this function.
    #[inline(always)]
    pub fn init_core_objects(&self) {
        self.init_core_objects_ex(true, true, true);
    }
}
