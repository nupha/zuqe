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

// impl GCObjectType {
//     pub fn gc_link(&mut self) -> NonNull<list_head> {
//         unsafe {
//             match self {
//                 GCObjectType::Object(o) => o.as_mut().gc_link(),
//                 GCObjectType::FunctionByteCode(g) => g.as_mut().gc_link(),
//                 GCObjectType::Shape(g) => g.as_mut().gc_link(),
//                 GCObjectType::VarRef(g) => g.as_mut().gc_link(),
//                 GCObjectType::AsyncFunction(g) => g.as_mut().gc_link(),
//                 GCObjectType::Context(g) => g.as_mut().gc_link(),
//             }
//         }
//     }

//     pub fn gc_add_to_list(&mut self, list: &mut list_head) {
//         unsafe {
//             list_add_tail(self.gc_link().as_ptr(), list);
//         }
//     }
// }

pub type GCObjectNodeData = Box<dyn GCObject>;

pub trait GCObject {
    fn ref_count(&self) -> i32;
    fn inc_ref(&mut self) -> i32;
    fn dec_ref(&mut self) -> i32;

    fn gc_type(&self) -> GCObjectType;
    fn gc_list_node(&self) -> NonNull<Node<GCObjectNodeData>>;

    fn gc_mark(&self) -> u8;
    fn set_gc_mark(&mut self, mark: u8);

    fn gc_dump(&self, rt: &JSRuntime);

    /// do clean ups before object is about to drop.
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

// pub struct GCObjectNode {
//     prev: *mut Self,
//     next: *mut Self,
//     data: Box<dyn GCObject>,
// }

// impl Deref for GCObjectNode {
//     type Target = dyn GCObject;

//     fn deref(&self) -> &Self::Target {
//         self.data.as_ref()
//     }
// }

// impl DerefMut for GCObjectNode {
//     fn deref_mut(&mut self) -> &mut Self::Target {
//         self.data.as_mut()
//     }
// }

// impl Drop for GCObjectNode {
//     fn drop(&mut self) {
//         unsafe {
//             if !self.prev.is_null() {
//                 drop(Box::from_raw(self.prev));
//             }
//             if !self.next.is_null() {
//                 drop(Box::from_raw(self.next));
//             }
//         }
//     }
// }

// impl GCObjectNode {
//     pub fn from(data: Box<dyn GCObject>) -> Box<Self> {
//         unsafe {
//             let ptr = {
//                 let ly = core::alloc::Layout::from_size_align_unchecked(
//                     std::mem::size_of::<Self>(),
//                     std::mem::align_of::<Self>(),
//                 );
//                 std::alloc::alloc(ly).cast::<Self>()
//             };
//             debug_assert!(!ptr.is_null(), "fail to alloc"); // TODO

//             // set list_node to addr of GCObjectNode
//             match data.gc_type() {
//                 GCObjectType::Object(mut o) => {
//                     o.as_mut().header.list_node = ptr;
//                 }
//                 GCObjectType::FunctionByteCode(mut o) => {
//                     o.as_mut().header.list_node = ptr;
//                 }
//                 GCObjectType::Shape(mut o) => {
//                     o.as_mut().header.list_node = ptr;
//                 }
//                 GCObjectType::VarRef(mut o) => {
//                     o.as_mut().u0.header.list_node = ptr;
//                 }
//                 GCObjectType::AsyncFunction(mut o) => {
//                     o.as_mut().header.list_node = ptr;
//                 }
//                 GCObjectType::Context(mut o) => {
//                     o.as_mut().header.list_node = ptr;
//                 }
//             }

//             ptr.write(Self {
//                 prev: std::ptr::null_mut(),
//                 next: std::ptr::null_mut(),
//                 data,
//             });

//             Box::from_raw(ptr)
//         }
//     }

//     pub fn into_inner(self) -> Box<dyn GCObject> {
//         unsafe {
//             let r = std::ptr::read(&self.data);

//             if !self.prev.is_null() {
//                 drop(Box::from_raw(self.prev));
//             }
//             if !self.next.is_null() {
//                 drop(Box::from_raw(self.next));
//             }

//             std::mem::forget(self);
//             r
//         }
//     }

//     pub fn remove_from_list(&mut self) {
//         let before = self.prev;
//         let after = self.next;
//         self.prev = std::ptr::null_mut();
//         self.next = std::ptr::null_mut();

//         unsafe {
//             if !before.is_null() {
//                 (*before).next = after;
//             }
//             if !after.is_null() {
//                 (*after).prev = before;
//             }
//         }
//     }
// }

// pub struct GCObjectNodeIter<'a> {
//     cursor: *const GCObjectNode,
//     _mark: PhantomData<&'a ()>,
// }

// impl<'a> Iterator for GCObjectNodeIter<'a> {
//     type Item = &'a GCObjectNode;

//     fn next(&mut self) -> Option<Self::Item> {
//         unsafe {
//             if let Some(c) = self.cursor.as_ref() {
//                 self.cursor = c.next;
//                 Some(c)
//             } else {
//                 None
//             }
//         }
//     }
// }

// pub struct GCObjectNodeIterMut<'a> {
//     cursor: *mut GCObjectNode,
//     _mark: PhantomData<&'a ()>,
// }

// impl<'a> Iterator for GCObjectNodeIterMut<'a> {
//     type Item = &'a mut GCObjectNode;

//     fn next(&mut self) -> Option<Self::Item> {
//         unsafe {
//             if let Some(c) = self.cursor.as_mut() {
//                 self.cursor = c.next;
//                 Some(c)
//             } else {
//                 None
//             }
//         }
//     }
// }

// pub struct GCObjectList {
//     head: *mut GCObjectNode,
//     tail: *mut GCObjectNode,
// }

// impl Drop for GCObjectList {
//     fn drop(&mut self) {
//         unsafe {
//             if !self.head.is_null() {
//                 drop(Box::from_raw(self.head));
//             }
//             if !self.tail.is_null() {
//                 drop(Box::from_raw(self.tail));
//             }
//         }
//     }
// }

// impl GCObjectList {
//     pub fn new() -> Self {
//         Self {
//             head: std::ptr::null_mut(),
//             tail: std::ptr::null_mut(),
//         }
//     }

//     pub(crate) unsafe fn push_ptr(&mut self, mut ptr: NonNull<GCObjectNode>) {
//         if self.head.is_null() {
//             debug_assert!(self.tail.is_null());
//             self.head = ptr.as_ptr();
//         }
//         ptr.as_mut().prev = self.tail;
//         ptr.as_mut().next = std::ptr::null_mut();
//         self.tail = ptr.as_ptr();
//     }

//     pub fn push(&mut self, node: Box<GCObjectNode>) -> NonNull<GCObjectNode> {
//         unsafe {
//             let p = NonNull::new_unchecked(Box::into_raw(node));
//             self.push_ptr(p);
//             p
//         }
//     }

//     pub fn push_data(&mut self, data: Box<dyn GCObject>) {
//         let node = GCObjectNode::from(data);
//         unsafe {
//             self.push_ptr(NonNull::new_unchecked(Box::into_raw(node)));
//         }
//     }

//     pub fn pop_front(&mut self) -> Option<Box<GCObjectNode>> {
//         unsafe {
//             if let Some(c) = self.head.as_mut() {
//                 self.head = c.next;
//                 Some(Box::from_raw(c))
//             } else {
//                 self.tail = std::ptr::null_mut();
//                 debug_assert!(self.is_empty());
//                 None
//             }
//         }
//     }

//     pub fn pop_back(&mut self) -> Option<Box<GCObjectNode>> {
//         unsafe {
//             if let Some(c) = self.tail.as_mut() {
//                 self.tail = c.prev;
//                 Some(Box::from_raw(c))
//             } else {
//                 self.head = std::ptr::null_mut();
//                 debug_assert!(self.is_empty());
//                 None
//             }
//         }
//     }

//     pub fn iter(&self) -> GCObjectNodeIter {
//         GCObjectNodeIter {
//             cursor: self.head,
//             _mark: PhantomData,
//         }
//     }

//     pub fn iter_mut(&mut self) -> GCObjectNodeIterMut {
//         GCObjectNodeIterMut {
//             cursor: self.head,
//             _mark: PhantomData,
//         }
//     }

//     pub fn len(&self) -> usize {
//         self.iter().count()
//     }

//     pub fn is_empty(&self) -> bool {
//         self.head.is_null()
//     }
// }
