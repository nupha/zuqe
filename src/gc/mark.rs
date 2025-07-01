use crate::{
    JS_CLASS_OBJECT,
    context::JSContext,
    function::{JSAsyncFunctionState, JSFunctionBytecode, JSVarRef, VarRefState},
    module::{JSExportTypeEnum, JSModuleDef},
    value::{
        JSString, JSValue,
        object::{JSObject, JSProperty, JSShape, LazyInit},
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

    pub fn gc_sweep(&self) {
        // unref all fields
        self.0.shape.borrow_mut().take();
        for pr in self.0.prop_values.borrow_mut().iter_mut() {
            *pr = JSProperty::default();
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
            JSProperty::AutoInit { init_type, .. } => match init_type {
                LazyInit::ModuleNS(m) => {
                    m.gc_mark();
                }
                LazyInit::Prototype | LazyInit::Prop(_) => {}
            },
        }
    }

    pub fn gc_sweep(&mut self) {
        match self {
            JSProperty::Normal(val) => {
                *val = JSValue::UNDEFINED;
            }
            JSProperty::GetSet { getter, setter } => {
                getter.take();
                setter.take();
            }
            JSProperty::VarRef(vref) => {
                let state = &mut vref.0.borrow_mut().state;
                match state {
                    VarRefState::Detached { value } => {
                        *value = JSValue::UNDEFINED;
                    }
                    VarRefState::Bind {
                        pvalue, async_func, ..
                    } => unsafe {
                        *(pvalue.as_mut()) = JSValue::UNDEFINED;
                        async_func.take();
                    },
                }
            }
            JSProperty::AutoInit { init_type, .. } => match init_type {
                LazyInit::ModuleNS(m) => {
                    m.gc_sweep();
                }
                LazyInit::Prototype | LazyInit::Prop(_) => {}
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

    pub fn gc_sweep(&self) {
        self.0.borrow_mut().proto.take();
    }
}

impl JSString {
    pub fn gc_mark(&self) {
        if !self.gc_has_mark() {
            self.gc_set_mark(true);
        }
    }

    pub fn gc_sweep(&self) {}
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

    pub fn gc_sweep(&self) {
        let mut m = self.0.borrow_mut();

        m.module_ns = JSValue::UNDEFINED;
        m.func_obj = JSValue::UNDEFINED;
        m.eval_exception = JSValue::UNDEFINED;
        m.meta_obj = JSValue::UNDEFINED;
        m.promise = JSValue::UNDEFINED;
        m.resolving_funcs[0] = JSValue::UNDEFINED;
        m.resolving_funcs[1] = JSValue::UNDEFINED;

        for me in m.export_entries.iter_mut() {
            if let JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                var_ref: Some(vr), ..
            } = &mut me.export_type
            {
                vr.gc_sweep();
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

    pub fn gc_sweep(&self) {
        self.cpools_slice().iter().for_each(|v| {
            v.gc_mark();
        });
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

    pub fn gc_sweep(&self) {
        match &mut self.0.borrow_mut().state {
            VarRefState::Detached { value } => *value = JSValue::UNDEFINED,
            VarRefState::Bind { async_func, .. } => {
                async_func.take();
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

    pub fn gc_sweep(&self) {
        let mut b = self.0.borrow_mut();
        b.this_val = JSValue::UNDEFINED;
        b.resolving_funcs[0] = JSValue::UNDEFINED;
        b.resolving_funcs[1] = JSValue::UNDEFINED;
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

            for mw in self.0.borrow().loaded_modules.iter() {
                if let Some(m) = mw.upgrade() {
                    m.gc_mark();
                }
            }
        }
    }

    pub fn gc_sweep(&self) {
        let mut cx = self.0.borrow_mut();

        if let Some(o) = &mut cx.array_shape {
            o.gc_sweep();
        }

        cx.class_proto.clear();

        cx.function_proto = JSValue::UNDEFINED;
        cx.function_ctor = JSValue::UNDEFINED;
        cx.array_ctor = JSValue::UNDEFINED;
        cx.regexp_ctor = JSValue::UNDEFINED;
        cx.promise_ctor = JSValue::UNDEFINED;
        for v in cx.native_error_proto.iter_mut() {
            *v = JSValue::UNDEFINED;
        }
        cx.iterator_proto = JSValue::UNDEFINED;
        cx.async_iterator_proto = JSValue::UNDEFINED;
        cx.array_proto_values = JSValue::UNDEFINED;
        cx.throw_type_error = JSValue::UNDEFINED;
        cx.eval_func = JSValue::UNDEFINED;
        cx.global_obj = JSValue::UNDEFINED;
        cx.global_var_obj = JSValue::UNDEFINED;
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

    pub fn gc_sweep(&self) {
        match self {
            JSValue::Object(o) => {
                o.gc_sweep();
            }
            JSValue::FunctionBytecode(b) => {
                b.gc_sweep();
            }
            JSValue::String(s) | JSValue::Symbol(s) => {
                s.gc_sweep();
            }
            _ => {}
        }
    }
}
