use crate::{
    JS_CLASS_OBJECT,
    context::JSContext,
    function::{JSAsyncFunctionState, JSFunctionBytecode, JSVarRef, VarRefState},
    module::{JSExportTypeEnum, JSModuleDef},
    value::{
        JSValue,
        object::{AutoInitType, JSObject, JSProperty, JSShape},
    },
};

impl JSObject {
    pub fn gc_mark(&self) {
        if !self.gc_has_mark() {
            self.gc_set_mark(true);

            // Mark shape
            self.shape().unwrap().gc_mark();

            // Mark all fields
            let len = self.0.prop_values.borrow().len();
            for i in 0..len {
                let pr = self.0.prop_values.borrow()[i].clone();
                pr.gc_mark();
            }

            let class_id = self.class_id();
            if class_id != JS_CLASS_OBJECT {
                if let Some(f) = self.rt().class_array[class_id as usize].gc_mark {
                    f(JSValue::Object(self.clone()));
                }
            }
        }
    }
}

impl JSProperty {
    pub fn gc_mark(&self) {
        match self {
            JSProperty::Normal(val) => {
                val.gc_mark();
            }
            JSProperty::GetSet { getter, setter } => {
                if let Some(o) = getter {
                    o.gc_mark();
                }
                if let Some(o) = setter {
                    o.gc_mark();
                }
            }
            JSProperty::VarRef(vref) => {
                let state = vref.0.borrow().state.clone();
                match state {
                    VarRefState::Detached { value } => {
                        value.gc_mark();
                    }
                    VarRefState::Bind {
                        pvalue, async_func, ..
                    } => unsafe {
                        pvalue.as_ref().gc_mark();
                        if let Some(a) = &async_func {
                            a.gc_mark();
                        }
                    },
                }
            }
            JSProperty::AutoInit {
                realm,
                init_type,
                opaque,
            } => match init_type {
                AutoInitType::ModuleNS(m) => {
                    m.gc_mark();
                }
                AutoInitType::Prototype | AutoInitType::Prop(_) => {}
            },
        }
    }
}

impl JSShape {
    pub fn gc_mark(&self) {
        if !self.gc_has_mark() {
            self.gc_set_mark(true);

            let proto = self.0.borrow().proto.clone();
            if let Some(p) = proto {
                p.gc_mark();
            }
        }
    }
}

impl JSModuleDef {
    pub fn gc_mark(&self) {
        if !self.gc_has_mark() {
            self.gc_set_mark(true);

            {
                let m = self.0.borrow();
                m.module_ns.gc_mark();
                m.func_obj.gc_mark();
                m.eval_exception.gc_mark();
                m.meta_obj.gc_mark();
                m.promise.gc_mark();
                m.resolving_funcs[0].gc_mark();
                m.resolving_funcs[1].gc_mark();
            }

            for me in self.0.borrow_mut().export_entries.iter_mut() {
                if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                    var_ref: Some(vr), ..
                } = &me.export_type
                {
                    vr.gc_mark();
                }
            }
        }
    }
}

impl JSFunctionBytecode {
    pub fn gc_mark(&self) {
        if !self.gc_has_mark() {
            self.gc_set_mark(true);

            // The template objects can be part of a cycle
            self.cpools_slice().iter().for_each(|v| {
                v.gc_mark();
            });
        }
    }
}

impl JSVarRef {
    pub fn gc_mark(&self) {
        let state = self.0.borrow().state.clone();
        match state {
            VarRefState::Detached { value } => {
                value.gc_mark();
            }
            VarRefState::Bind { async_func, .. } => {
                if let Some(af) = async_func {
                    af.gc_mark();
                }
            }
        }
    }
}

impl JSAsyncFunctionState {
    pub fn gc_mark(&self) {
        self.0.borrow().this_val.gc_mark();
        self.0.borrow().resolving_funcs[0].gc_mark();
        self.0.borrow().resolving_funcs[1].gc_mark();
    }
}

impl JSContext {
    pub fn gc_mark(&self) {
        if !self.gc_has_mark() {
            self.gc_set_mark(true);

            let cx = self.0.borrow();

            if let Some(o) = &cx.array_shape {
                o.gc_mark();
            }
            for p in cx.class_proto.iter() {
                p.gc_mark();
            }
            cx.function_proto.gc_mark();
            cx.function_ctor.gc_mark();
            cx.array_ctor.gc_mark();
            cx.regexp_ctor.gc_mark();
            cx.promise_ctor.gc_mark();
            for p in cx.native_error_proto.iter() {
                p.gc_mark();
            }
            cx.iterator_proto.gc_mark();
            cx.async_iterator_proto.gc_mark();
            cx.array_proto_values.gc_mark();
            cx.throw_type_error.gc_mark();
            cx.eval_func.gc_mark();
            cx.global_obj.gc_mark();
            cx.global_var_obj.gc_mark();
        }
    }
}

impl JSValue {
    pub fn gc_mark(&self) {
        match self {
            JSValue::Object(o) => {
                o.gc_mark();
            }
            JSValue::FunctionBytecode(b) => {
                b.gc_mark();
            }
            _ => {}
        }
    }
}
