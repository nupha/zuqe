use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, JS_CLASS_ASYNC_FUNCTION,
    JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_DATAVIEW, JS_CLASS_DATE,
    JS_CLASS_FLOAT64_ARRAY, JS_CLASS_MAP, JS_CLASS_MAP_ITERATOR, JS_CLASS_PROMISE, JS_CLASS_PROXY,
    JS_CLASS_REGEXP, JS_CLASS_REGEXP_STRING_ITERATOR, JS_CLASS_SET, JS_CLASS_SET_ITERATOR,
    JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8C_ARRAY, JS_CLASS_WEAKMAP, JS_CLASS_WEAKSET,
    atom::consts::{JS_ATOM_global, JS_ATOM_globalThis},
    class::ClassID,
    context::JSContext,
    js_core::{global::js_global_props, math::js_math_obj},
    json::js_json_obj,
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, JSObject},
    reflect::js_reflect_obj,
    runtime::JSRuntime,
};

pub type SetupGlobalFn = fn(ctx: &JSContext, global: &JSObject) -> Result<(), i32>;

#[derive(Debug, Default)]
pub struct ContextBuilder {
    use_classes: Vec<ClassID>,
    setup_global: Option<SetupGlobalFn>,
}

impl ContextBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// initialize order are same as use order. duplicated insertion will be ignored.
    pub fn use_classes(mut self, class_ids: &[ClassID]) -> Self {
        for id in class_ids {
            if !self.use_classes.contains(id) {
                self.use_classes.push(*id);
            }
        }
        self
    }

    pub fn use_class(self, class_id: ClassID) -> Self {
        self.use_classes(&[class_id])
    }

    pub fn global(mut self, f: SetupGlobalFn) -> Self {
        self.setup_global.replace(f);
        self
    }

    pub fn build(self, rt: &mut JSRuntime) -> Result<JSContext, i32> {
        rt.create_context_raw(|cx| {
            //
            // make global namespace - this must come first.
            //
            let gobj = cx.create_base_object()?;

            // define name `globalThis` and `global`
            gobj.define_property_value(
                cx,
                &JS_ATOM_globalThis,
                gobj.clone().into(),
                JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
            )?;
            gobj.define_property_value(
                cx,
                &JS_ATOM_global,
                gobj.clone().into(),
                JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
            )?;

            {
                // make global var object
                let gvobj = cx.create_base_object_with_proto(None)?;

                // save to ctx
                let mut cx = cx.0.borrow_mut();
                cx.global_obj = gobj.clone().into();
                cx.global_var_obj = gvobj.into();
            }

            //
            // init intrinsic base classes
            //
            cx.intrinsic_base_object(); // Object
            cx.intrinsic_base_function(); // Function
            cx.intrinsic_base_errors(); // Error
            cx.intrinsic_base_array(); // Array
            cx.intrinsic_base_number(); // Number
            cx.intrinsic_base_boolean(); // Boolean
            cx.intrinsic_base_string(); // String
            cx.intrinsic_base_symbol(); // Symbol

            cx.intrinsic_base_generator(); // Generator

            cx.intrinsic_eval(); // `eval()` function

            //
            // init user classes
            //
            cx.use_classes(&self.use_classes)?;

            //
            // setup global entries
            //
            if let Some(f) = self.setup_global {
                f(cx, &gobj)?
            }

            Ok(())
        })
    }
}

impl JSRuntime {
    /// Creates new context initialized with ECMA standard classes and global entries.
    ///
    /// This also provides a demostration on how to use ContextBuilder to customize a context.
    pub fn create_context_ecma(&mut self) -> Result<JSContext, i32> {
        ContextBuilder::new()
            .use_classes(&[
                //proxy
                JS_CLASS_PROXY,
                // regexp
                JS_CLASS_REGEXP,
                JS_CLASS_REGEXP_STRING_ITERATOR,
                // async
                JS_CLASS_PROMISE,
                JS_CLASS_ASYNC_FUNCTION,
                JS_CLASS_ASYNC_GENERATOR,
                JS_CLASS_ASYNC_GENERATOR_FUNCTION,
                JS_CLASS_ASYNC_FROM_SYNC_ITERATOR,
                // mapset
                JS_CLASS_MAP,
                JS_CLASS_SET,
                JS_CLASS_WEAKMAP,
                JS_CLASS_WEAKSET,
                JS_CLASS_MAP_ITERATOR,
                JS_CLASS_SET_ITERATOR,
                // date
                JS_CLASS_DATE,
                // array buffer & dataview
                JS_CLASS_ARRAY_BUFFER,
                JS_CLASS_SHARED_ARRAY_BUFFER,
                JS_CLASS_DATAVIEW,
            ])
            .use_classes(
                // typed arries
                &(JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).collect::<Vec<_>>(),
            )
            .global(|cx, gobj| -> Result<(), i32> {
                gobj.install_field_defs(cx, &js_global_props)?;

                // Reflect: lazy init
                gobj.install_field_defs(cx, &js_reflect_obj)?;

                // JSON: lazy init
                gobj.install_field_defs(cx, &js_json_obj)?;

                // Math: lazy init
                cx.0.borrow_mut().init_random_seed();
                gobj.install_field_defs(cx, &js_math_obj)?;

                Ok(())
            })
            .build(self)
    }
}
