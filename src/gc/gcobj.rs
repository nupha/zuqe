use std::ptr::NonNull;

use crate::runtime::JSRuntime;

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

#[repr(u8)]
pub enum GCObjectStatus {
    Zeroed = 0,
    Tracking = 1,
    Temp = 2,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct GCObjectHeader {
    pub ref_count: i32,
    bitfields: u8,

    /// the runtime ptr
    rt: NonNull<JSRuntime>,
}

impl GCObjectHeader {
    pub const fn new(rt: *mut JSRuntime, ref_count: i32) -> Self {
        Self {
            ref_count,
            bitfields: (GCObjectStatus::Tracking as u8) << 1,
            rt: unsafe { NonNull::new_unchecked(rt) },
        }
    }

    #[inline(always)]
    pub fn rt_ptr(&self) -> NonNull<JSRuntime> {
        self.rt
    }

    #[inline(always)]
    pub fn rt(&self) -> &JSRuntime {
        unsafe { self.rt.as_ref() }
    }

    #[inline(always)]
    pub fn rt_mut(&mut self) -> &mut JSRuntime {
        unsafe { self.rt.as_mut() }
    }

    #[inline(always)]
    pub const fn gc_has_mark(&self) -> bool {
        (self.bitfields & 0x01) != 0
    }

    #[inline(always)]
    pub const fn gc_set_mark(&mut self, m: bool) {
        self.bitfields = (self.bitfields & 0b1111_1110) | (m as u8)
    }

    #[inline(always)]
    pub const fn gc_status(&self) -> GCObjectStatus {
        unsafe { std::mem::transmute::<u8, _>((self.bitfields & 0b0000_0110) >> 1) }
    }
}
