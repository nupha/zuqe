use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR, JS_CLASS_ASYNC_FUNCTION,
    JS_CLASS_ASYNC_GENERATOR, JS_CLASS_ASYNC_GENERATOR_FUNCTION, JS_CLASS_DATAVIEW,
    JS_CLASS_FLOAT64_ARRAY, JS_CLASS_PROMISE, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8C_ARRAY,
    atom::Atom,
    builtins::{global::js_global_props, math::js_math_obj, reflect::js_reflect_obj},
    class::ClassID,
    context::Context,
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object},
    runtime::RuntimeImpl,
};

pub type SetupGlobalFn = fn(ctx: Context, global: Object) -> Result<(), i32>;

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

    pub fn build<'a>(self, rt: &mut RuntimeImpl) -> Result<Context<'a>, i32> {
        rt.create_context_raw(|mut cx| {
            //
            // make global namespace - this must come first.
            //
            let mut gobj = cx.create_base_object()?;

            // define name `globalThis` and `global`
            gobj.define_property_value(
                &cx,
                Atom::CONST_globalThis,
                gobj.into(),
                JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
            )?;
            gobj.define_property_value(
                &cx,
                Atom::CONST_global,
                gobj.into(),
                JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE,
            )?;

            // make global var object
            let gvobj = cx.create_base_object_with_proto(None)?;
            // save to ctx
            cx.global_obj = gobj.into();
            cx.global_var_obj = gvobj.into();

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
            // setup global
            //
            if let Some(f) = self.setup_global {
                f(cx.into(), gobj)?
            }

            Ok(())
        })
    }
}

impl RuntimeImpl {
    /// Creates new context initialized with ECMA standard classes and global entries.
    ///
    /// This also provides a demostration on how to use ContextBuilder to customize a context.
    pub fn create_context_ecma<'a>(&mut self) -> Result<Context<'a>, i32> {
        let mut cb = ContextBuilder::new();

        cb = cb.use_classes(&[
            // async
            JS_CLASS_PROMISE,
            JS_CLASS_ASYNC_FUNCTION,
            JS_CLASS_ASYNC_GENERATOR,
            JS_CLASS_ASYNC_GENERATOR_FUNCTION,
            JS_CLASS_ASYNC_FROM_SYNC_ITERATOR,
            // array buffer & dataview
            JS_CLASS_ARRAY_BUFFER,
            JS_CLASS_SHARED_ARRAY_BUFFER,
            JS_CLASS_DATAVIEW,
        ]);

        // TypedArray
        cb = cb.use_classes(&(JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).collect::<Vec<_>>());

        // Proxy
        #[cfg(feature = "builtin_proxy")]
        {
            cb = cb.use_class(crate::JS_CLASS_PROXY);
        }

        // RegExp
        #[cfg(feature = "builtin_regexp")]
        {
            cb = cb.use_classes(&[
                crate::JS_CLASS_REGEXP,
                crate::JS_CLASS_REGEXP_STRING_ITERATOR,
            ]);
        }

        // Map/Set
        #[cfg(feature = "builtin_mapset")]
        {
            cb = cb.use_classes(&[
                crate::JS_CLASS_MAP,
                crate::JS_CLASS_SET,
                crate::JS_CLASS_WEAKMAP,
                crate::JS_CLASS_WEAKSET,
                crate::JS_CLASS_MAP_ITERATOR,
                crate::JS_CLASS_SET_ITERATOR,
            ]);
        }

        // Date
        #[cfg(feature = "builtin_date")]
        {
            cb = cb.use_class(crate::JS_CLASS_DATE);
        }

        // setup globals handler
        cb = cb.global(|mut cx, mut gobj| -> Result<(), i32> {
            gobj.install_fields(&cx, js_global_props)?;
            // Reflect: lazy init
            gobj.install_fields(&cx, js_reflect_obj)?;
            // JSON: lazy init
            #[cfg(feature = "builtin_json")]
            gobj.install_fields(&cx, crate::builtins::json::js_json_obj)?;
            // Math: lazy init
            cx.init_random_seed();
            gobj.install_fields(&cx, js_math_obj)?;

            Ok(())
        });

        cb.build(self)
    }
}
