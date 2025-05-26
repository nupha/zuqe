use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use super::JSShape;

/// A wrapper upon raw pointer of Shape.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct JSShapeHandle(NonNull<JSShape>);

impl JSShapeHandle {
    /// increase shape ref count
    #[inline(always)]
    pub const fn dup(mut self) -> Self {
        unsafe {
            self.0.as_mut().dup();
        }
        self
    }

    #[inline(always)]
    pub const fn as_ptr(self) -> *mut JSShape {
        self.0.as_ptr()
    }
}

impl Deref for JSShapeHandle {
    type Target = JSShape;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl DerefMut for JSShapeHandle {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl PartialEq for JSShapeHandle {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl From<*mut JSShape> for JSShapeHandle {
    #[inline(always)]
    fn from(value: *mut JSShape) -> Self {
        Self(NonNull::new(value).unwrap())
    }
}

impl From<&mut JSShape> for JSShapeHandle {
    #[inline(always)]
    fn from(value: &mut JSShape) -> Self {
        Self(NonNull::from(value))
    }
}

impl From<JSShapeHandle> for *mut JSShape {
    #[inline(always)]
    fn from(value: JSShapeHandle) -> Self {
        value.0.as_ptr()
    }
}
