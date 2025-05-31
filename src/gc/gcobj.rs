use std::{pin::Pin, ptr::NonNull};

use crate::{
    context::JSContext,
    function::{JSAsyncFunctionState, JSFunctionBytecode, JSVarRef},
    runtime::JSRuntime,
    utils::{list_head, Node},
    value::object::{JSObject, JSShape},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSGCObjectTypeEnum {
    JS_GC_OBJ_TYPE_JS_OBJECT,
    JS_GC_OBJ_TYPE_FUNCTION_BYTECODE,
    JS_GC_OBJ_TYPE_SHAPE,
    JS_GC_OBJ_TYPE_VAR_REF,
    JS_GC_OBJ_TYPE_ASYNC_FUNCTION,
    JS_GC_OBJ_TYPE_JS_CONTEXT,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GCObjectType {
    Object(NonNull<JSObject>),
    FunctionBytecode(NonNull<JSFunctionBytecode>),
    Shape(NonNull<JSShape>),
    VarRef(NonNull<Pin<JSVarRef>>),
    AsyncFunction(NonNull<JSAsyncFunctionState>),
    Context(NonNull<JSContext>),
}

pub type GCObjectNodeData = Box<dyn GCObject>;

pub trait RefCounted {
    /// get current ref count
    fn ref_count(&self) -> i32;

    /// increament ref count, then returns new ref count
    fn inc_ref(&mut self) -> i32;

    /// decreament ref count, then returns new ref count
    fn dec_ref(&mut self) -> i32;
}

pub trait GCObject: RefCounted {
    fn gc_type(&self) -> GCObjectType;

    /// get node link - the raw pointer of wrapper Node instance.
    fn gc_node_ptr(&self) -> NonNull<Node<GCObjectNodeData>>;

    fn gc_mark(&self) -> u8;
    fn set_gc_mark(&mut self, mark: u8);

    fn gc_dump(&self, rt: &JSRuntime);

    /// perform gcobject clean up before object is about to drop.
    /// when calling this, ref_count may not nessesarily be zero.
    fn finalize(&mut self, rt: &mut JSRuntime);
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GCObjectHeader {
    pub ref_count: i32,

    bitfields: u8,

    /// raw pointer of Node<T> wrapper object that embedding this.
    /// when not wrapped in Node<T>, should be null.
    pub list_node: *mut Node<GCObjectNodeData>,
}

impl GCObjectHeader {
    pub const fn new(ref_count: i32) -> Self {
        Self {
            ref_count,
            bitfields: 0,
            list_node: std::ptr::null_mut(), // no Node<T> wrapper
        }
    }

    #[inline(always)]
    pub const fn gc_mark(&self) -> u8 {
        self.bitfields
    }

    #[inline(always)]
    pub const fn set_gc_mark(&mut self, m: u8) {
        self.bitfields = m
    }
}
