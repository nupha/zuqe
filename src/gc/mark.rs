use crate::{
    JS_CLASS_OBJECT,
    context::JSContext,
    function::{AsyncFuncState, Bytecode, JSVarRef, VarRefState},
    module::{ExportType, ModuleDef},
    object::{JSObject, JSProperty, JSShape, LazyInit},
    stack_frame::StackFrame,
    string::JSString,
    value::JSValue,
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
                if let Some(f) = self.rt().class_defs[class_id as usize].gc_mark {
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
            JSProperty::LazyInit { init_type, .. } => match init_type {
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
            JSProperty::LazyInit { init_type, .. } => match init_type {
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

impl ModuleDef {
    pub fn gc_mark(&self) {
        if !self.gc_has_mark() {
            self.gc_set_mark(true);

            unsafe {
                if let Some(o) = (*self.0.module_ns.as_ptr()).as_ref() {
                    o.gc_mark();
                }

                (*self.0.bytecode.as_ptr()).gc_mark();
                if let Some(exc) = self.eval_exception() {
                    exc.gc_mark();
                }
                if let Some(o) = (*self.0.meta_obj.as_ptr()).as_ref() {
                    o.gc_mark();
                }
                self.promise().map(|p| p.gc_mark());
                (*self.0.resolving_funcs.as_ptr())[0].gc_mark();
                (*self.0.resolving_funcs.as_ptr())[1].gc_mark();
            }

            for me in self.0.export_entries.borrow().iter() {
                if let ExportType::JS_EXPORT_TYPE_LOCAL {
                    var_ref: Some(vr), ..
                } = &me.export_type
                {
                    vr.gc_mark();
                }
            }
        }
    }

    pub fn gc_sweep(&self) {
        self.0.module_ns.take();
        self.0.bytecode.set(JSValue::UNDEFINED);
        self.set_eval_exception(None);
        self.0.meta_obj.take();
        self.0.promise.take();
        self.0
            .resolving_funcs
            .set([JSValue::UNDEFINED, JSValue::UNDEFINED]);

        for me in self.0.export_entries.borrow().iter() {
            if let ExportType::JS_EXPORT_TYPE_LOCAL {
                var_ref: Some(vr), ..
            } = &me.export_type
            {
                vr.gc_sweep();
            }
        }
    }
}

impl Bytecode {
    pub fn gc_mark(&self) {
        if !self.gc_has_mark() {
            self.gc_set_mark(true);

            // The template objects can be part of a cycle
            self.consts().iter().for_each(|v| {
                v.gc_mark();
            });
        }
    }

    pub fn gc_sweep(&self) {
        self.consts().iter().for_each(|v| {
            v.gc_sweep();
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

impl AsyncFuncState {
    pub fn gc_mark(&self) {
        let s = self.0.borrow();
        s.this_val.gc_mark();
        s.resolving_funcs[0].gc_mark();
        s.resolving_funcs[1].gc_mark();
    }

    pub fn gc_sweep(&self) {
        let mut s = self.0.borrow_mut();
        s.this_val = JSValue::UNDEFINED;
        s.resolving_funcs[0] = JSValue::UNDEFINED;
        s.resolving_funcs[1] = JSValue::UNDEFINED;
    }
}

impl JSContext {
    pub fn gc_mark(&self) {
        if !self.gc_has_mark() {
            self.gc_set_mark(true);

            let cx = self.0.borrow();

            for p in cx.class_proto.iter() {
                p.gc_mark();
            }

            // cx.function_proto.gc_mark(); // done in JS_CLASS_BYTECODE_FUNCTION
            cx.iterator_proto.gc_mark();
            cx.async_iterator_proto.gc_mark();
            cx.array_proto_values.gc_mark();
            cx.throw_type_error.gc_mark();
            for ev in cx.native_error_proto.iter() {
                ev.gc_mark();
            }

            if let Some(o) = &cx.array_shape {
                o.gc_mark();
            }

            cx.function_ctor.gc_mark();
            cx.array_ctor.gc_mark();
            cx.regexp_ctor.gc_mark();
            cx.promise_ctor.gc_mark();

            cx.throw_type_error.gc_mark();
            cx.eval_func.gc_mark();
            cx.global_obj.gc_mark();
            cx.global_var_obj.gc_mark();

            if let Some(sf) = unsafe { cx.stack_frame.cast::<StackFrame>().as_ref() } {
                sf.gc_mark();
            }

            for m in self.0.borrow().loaded_modules.iter() {
                m.gc_mark();
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
            Self::Object(o) => {
                o.gc_mark();
            }
            Self::Bytecode(b) => {
                b.gc_mark();
            }
            _ => {}
        }
    }

    pub fn gc_sweep(&self) {
        match self {
            Self::Object(o) => {
                o.gc_sweep();
            }
            Self::Bytecode(b) => {
                b.gc_sweep();
            }
            Self::String(s) => {
                s.gc_sweep();
            }
            Self::Symbol(d) => {
                d.gc_sweep();
            }
            _ => {}
        }
    }
}

impl StackFrame {
    pub fn gc_mark(&self) {
        self.cur_func.gc_mark();
        for a in self.args_slice() {
            a.gc_mark();
        }
        for v in self.local_buf.iter() {
            v.gc_mark();
        }
        for vr in self.var_ref_list.iter() {
            vr.gc_mark();
        }

        if let Some(sf) = unsafe { self.prev_frame.cast::<StackFrame>().as_ref() } {
            sf.gc_mark();
        }
    }
}
