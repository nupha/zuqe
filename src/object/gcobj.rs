use crate::{
    context::{JSContext, JSContextWeakref},
    function::{
        AsyncStateWeakref, Bytecode, BytecodeWeakref, JSVarRef, JSVarRefWeakref, AsyncObjPayload,
    },
    object::{JSObjectWeakref, JSShapeWeakref},
    string::{JSString, JSStringWeakref},
};

use super::{JSObject, JSShape};

#[repr(u8)]
#[derive(Debug, Clone)]
pub enum GCObject {
    Object(JSObject),
    FunctionBytecode(Bytecode),
    String(JSString),
    Shape(JSShape),
    VarRef(JSVarRef),
    AsyncFunction(AsyncObjPayload),
    Context(JSContext),
}

impl PartialEq for GCObject {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Object(l0), Self::Object(r0)) => l0.id() == r0.id(),
            (Self::String(l0), Self::String(r0)) => l0.content() == r0.content(),
            (Self::FunctionBytecode(l0), Self::FunctionBytecode(r0)) => l0.id() == r0.id(),
            (Self::Shape(l0), Self::Shape(r0)) => l0.id() == r0.id(),
            (Self::VarRef(l0), Self::VarRef(r0)) => l0.id() == r0.id(),
            (Self::AsyncFunction(l0), Self::AsyncFunction(r0)) => l0.id() == r0.id(),
            (Self::Context(l0), Self::Context(r0)) => l0.id() == r0.id(),
            _ => false,
        }
    }
}

impl GCObject {
    #[inline]
    pub fn id(&self) -> usize {
        match self {
            Self::Object(o) => o.id(),
            Self::FunctionBytecode(o) => o.id(),
            Self::String(o) => o.id(),
            Self::Shape(o) => o.id(),
            Self::VarRef(o) => o.id(),
            Self::AsyncFunction(o) => o.id(),
            Self::Context(o) => o.id(),
        }
    }

    #[cfg(debug_assertions)]
    pub fn ref_count(&self) -> usize {
        use std::rc::Rc;

        match self {
            Self::Object(o) => o.ref_count(),
            Self::FunctionBytecode(b) => Rc::strong_count(&b.0),
            Self::String(s) => Rc::strong_count(&s.0),
            Self::Shape(s) => s.strong_count(),
            Self::VarRef(v) => Rc::strong_count(&v.0),
            Self::AsyncFunction(a) => Rc::strong_count(&a.0),
            Self::Context(c) => Rc::strong_count(&c.0),
        }
    }

    pub fn gc_has_mark(&self) -> bool {
        match self {
            Self::Object(o) => o.gc_has_mark(),
            Self::FunctionBytecode(o) => o.gc_has_mark(),
            Self::String(o) => o.gc_has_mark(),
            Self::Shape(o) => o.gc_has_mark(),
            Self::VarRef(o) => o.gc_has_mark(),
            Self::AsyncFunction(o) => o.gc_has_mark(),
            Self::Context(o) => o.gc_has_mark(),
        }
    }

    pub fn gc_set_mark(&self, b: bool) {
        match self {
            Self::Object(o) => o.gc_set_mark(b),
            Self::FunctionBytecode(o) => o.gc_set_mark(b),
            Self::String(o) => o.gc_set_mark(b),
            Self::Shape(o) => o.gc_set_mark(b),
            Self::VarRef(o) => o.gc_set_mark(b),
            Self::AsyncFunction(o) => o.gc_set_mark(b),
            Self::Context(o) => o.gc_set_mark(b),
        }
    }

    pub fn gc_sweep(&self) {
        match self {
            Self::Object(o) => o.gc_sweep(),
            Self::FunctionBytecode(o) => o.gc_sweep(),
            Self::String(o) => o.gc_sweep(),
            Self::Shape(o) => o.gc_sweep(),
            Self::VarRef(o) => o.gc_sweep(),
            Self::AsyncFunction(o) => o.gc_sweep(),
            Self::Context(o) => o.gc_sweep(),
        }
    }
}

#[repr(u8)]
#[derive(Debug, Clone)]
pub enum GCObjectWeakref {
    Object(JSObjectWeakref),
    Bytecode(BytecodeWeakref),
    String(JSStringWeakref),
    Shape(JSShapeWeakref),
    VarRef(JSVarRefWeakref),
    AsyncFunction(AsyncStateWeakref),
    Context(JSContextWeakref),
}

impl GCObjectWeakref {
    pub fn upgrade(&self) -> Option<GCObject> {
        match self {
            Self::Object(o) => o.upgrade().map(GCObject::Object),
            Self::Bytecode(o) => o.upgrade().map(GCObject::FunctionBytecode),
            Self::String(o) => o.upgrade().map(GCObject::String),
            Self::Shape(o) => o.upgrade().map(GCObject::Shape),
            Self::VarRef(o) => o.upgrade().map(GCObject::VarRef),
            Self::AsyncFunction(o) => o.upgrade().map(GCObject::AsyncFunction),
            Self::Context(o) => o.upgrade().map(GCObject::Context),
        }
    }
}
