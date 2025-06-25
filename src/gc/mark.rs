use crate::{
    context::JSContext,
    error::JS_NATIVE_ERROR_COUNT,
    list_entry,
    module::{JSExportTypeEnum, JSModuleDef},
    runtime::JSRuntime,
    utils::list_foreach,
    value::{
        JSValue,
        object::{GCObject, JSProperty},
    },
};

/// Function type for marking objects during garbage collection
/// Parameters:
/// - rt: Pointer to the JavaScript runtime
/// - gp: Pointer to the garbage collected object header
pub type GCMarkFunc = unsafe fn(rt: *mut JSRuntime, ob: &GCObject);

impl JSRuntime {
    /// Marks a JavaScript value during garbage collection
    /// Only marks values that have reference counts (objects and function bytecode)
    pub(crate) fn gc_mark_value(&mut self, val: &JSValue, mark_func: GCMarkFunc) {
        match val {
            JSValue::Object(o) => unsafe {
                // mark_func(self, val);
            },
            JSValue::FunctionBytecode(_) => unsafe {
                // mark_func(self, val);
            },
            _ => {}
        }
    }

    pub unsafe fn gc_mark_module_def(&mut self, m: &mut JSModuleDef, mark_func: GCMarkFunc) {
        for me in m.0.borrow_mut().export_entries.iter_mut() {
            if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                var_ref: Some(vr), ..
            } = &me.export_type
            {
                // mark for var_ref
                // mark_func(self, vr);
            }
        }

        let m = m.0.borrow();
        self.gc_mark_value(&m.module_ns, mark_func);
        self.gc_mark_value(&m.func_obj, mark_func);
        self.gc_mark_value(&m.eval_exception, mark_func);
        self.gc_mark_value(&m.meta_obj, mark_func);
        self.gc_mark_value(&m.promise, mark_func);
        self.gc_mark_value(&m.resolving_funcs[0], mark_func);
        self.gc_mark_value(&m.resolving_funcs[1], mark_func);
    }

    /// Marks the realm associated with an auto-initialized property
    /// This ensures the realm is preserved during garbage collection
    pub unsafe fn gc_mark_autoinit(&mut self, pr: *mut JSProperty, mark_func: GCMarkFunc) {
        // Get the realm from the property and mark its header
        let realm = (*pr).realm().unwrap();
        //  mark_func(self, realm);
    }

    pub unsafe fn gc_mark_context(&mut self, ctx: &JSContext, mark_func: GCMarkFunc) {
        // Modules are not seen by the GC, so we directly mark the objects
        // referenced by each module

        // list_foreach(&mut ctx.loaded_modules, |el| {
        //     let m = list_entry!(el, JSModuleDef, link);
        //     self.gc_mark_module_def(m.as_mut().unwrap(), mark_func);
        // });

        // self.gc_mark_value(ctx.global_obj, mark_func);
        // self.gc_mark_value(ctx.global_var_obj, mark_func);

        // self.gc_mark_value(ctx.throw_type_error, mark_func);
        // self.gc_mark_value(ctx.eval_func, mark_func);

        // self.gc_mark_value(ctx.array_proto_values, mark_func);

        // for i in 0..JS_NATIVE_ERROR_COUNT {
        //     self.gc_mark_value(ctx.native_error_proto[i], mark_func);
        // }

        // for cproto in ctx.class_proto.iter() {
        //     self.gc_mark_value(*cproto, mark_func);
        // }

        // self.gc_mark_value(ctx.iterator_proto, mark_func);
        // self.gc_mark_value(ctx.async_iterator_proto, mark_func);
        // self.gc_mark_value(ctx.promise_ctor, mark_func);
        // self.gc_mark_value(ctx.array_ctor, mark_func);
        // self.gc_mark_value(ctx.regexp_ctor, mark_func);
        // self.gc_mark_value(ctx.function_ctor, mark_func);
        // self.gc_mark_value(ctx.function_proto, mark_func);

        // if !ctx.array_shape.is_null() {
        //     mark_func(self, ctx.array_shape);
        // }
    }
}
