use crate::{
    context::{JSContext, JSContextWeakref},
    function::{
        JSAsyncFunctionState, JSAsyncFunctionStateWeakref, JSFunctionBytecode,
        JSFunctionBytecodeWeakref, JSVarRef, JSVarRefWeakref,
    },
    value::{
        JSString, JSStringWeakref,
        object::{JSObjectWeakref, JSShapeWeakref},
    },
};

use super::{JSObject, JSShape};

#[repr(u8)]
#[derive(Clone)]
pub enum GCObject {
    Object(JSObject),
    FunctionBytecode(JSFunctionBytecode),
    String(JSString),
    Shape(JSShape),
    VarRef(JSVarRef),
    AsyncFunction(JSAsyncFunctionState),
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
}

#[repr(u8)]
#[derive(Debug, Clone)]
pub enum GCObjectWeakref {
    Object(JSObjectWeakref),
    FunctionBytecode(JSFunctionBytecodeWeakref),
    String(JSStringWeakref),
    Shape(JSShapeWeakref),
    VarRef(JSVarRefWeakref),
    AsyncFunction(JSAsyncFunctionStateWeakref),
    Context(JSContextWeakref),
}

impl GCObjectWeakref {
    pub fn upgrade(&self) -> Option<GCObject> {
        match self {
            Self::Object(o) => o.upgrade().map(GCObject::Object),
            Self::FunctionBytecode(o) => o.upgrade().map(GCObject::FunctionBytecode),
            Self::String(o) => o.upgrade().map(GCObject::String),
            Self::Shape(o) => o.upgrade().map(GCObject::Shape),
            Self::VarRef(o) => o.upgrade().map(GCObject::VarRef),
            Self::AsyncFunction(o) => o.upgrade().map(GCObject::AsyncFunction),
            Self::Context(o) => o.upgrade().map(GCObject::Context),
        }
    }
}
