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
    fn gc_list_node(&self) -> NonNull<Node<GCObjectNodeData>>;

    fn gc_mark(&self) -> u8;
    fn set_gc_mark(&mut self, mark: u8);

    fn gc_dump(&self, rt: &JSRuntime);

    /// perform gcobject clean ups before object is about to drop.
    /// when calling this, ref_count may not nessesarily be zero.
    fn finalize(&mut self, rt: &mut JSRuntime);
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JSGCObjectHeader {
    pub ref_count: i32, // must come first, 32-bit

    // gc_obj_type: 4 bits
    // mark:  4 bits used by the GC
    bitfields: u8,

    dummy2: u16, /* not used by the GC */

    pub list_node: *mut Node<GCObjectNodeData>,
    pub link: list_head,
}

impl JSGCObjectHeader {
    pub const fn new(ref_count: i32) -> Self {
        Self {
            ref_count,
            bitfields: 0,
            dummy2: 0,
            list_node: std::ptr::null_mut(),
            link: list_head {
                prev: std::ptr::null_mut(),
                next: std::ptr::null_mut(),
            },
        }
    }

    pub fn gc_obj_type(&self) -> JSGCObjectTypeEnum {
        unsafe { std::mem::transmute(self.bitfields >> 4) }
    }
    pub fn set_gc_obj_type(&mut self, t: JSGCObjectTypeEnum) {
        self.bitfields = (self.bitfields & 0x0f) | ((t as u8) << 4);
    }

    #[inline(always)]
    pub fn gc_mark(&self) -> u8 {
        self.bitfields & 0x0f
    }

    #[inline(always)]
    pub fn set_gc_mark(&mut self, m: u8) {
        self.bitfields = (self.bitfields & 0xf0) | (m & 0x0f)
    }
}
