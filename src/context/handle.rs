use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use super::JSContext;

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct JSContextHandle(NonNull<JSContext>);

impl JSContextHandle {
    #[inline(always)]
    pub fn as_raw(self) -> NonNull<JSContext> {
        self.0
    }

    #[inline(always)]
    pub fn as_ptr(self) -> &JSContext {
        self.0.as_ptr()
    }
}

impl From<&mut JSContext> for JSContextHandle {
    fn from(value: &mut JSContext) -> Self {
        value.handle()
    }
}

impl From<&JSContext> for JSContextHandle {
    #[inline(always)]
    fn from(ctx: &JSContext) -> Self {
        debug_assert!(!ctx.is_null());
        unsafe { Self(NonNull::new_unchecked(ctx)) }
    }
}

impl From<JSContextHandle> for &JSContext {
    #[inline(always)]
    fn from(value: JSContextHandle) -> Self {
        value.0.as_ptr()
    }
}

impl Deref for JSContextHandle {
    type Target = JSContext;
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl DerefMut for JSContextHandle {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl JSContext {
    /// Convert to context handle, a non-null pointer to JSContext
    #[inline(always)]
    pub fn handle(&mut self) -> JSContextHandle {
        JSContextHandle::from(self as &JSContext)
    }
}
