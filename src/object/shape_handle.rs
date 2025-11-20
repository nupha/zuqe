use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use super::ShapeHandle;

/// A wrapper upon raw pointer of Shape.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct JSShapeHandle(NonNull<ShapeHandle>);

impl JSShapeHandle {
    #[inline(always)]
    pub const fn as_ptr(self) -> *mut ShapeHandle {
        self.0.as_ptr()
    }
}

impl Deref for JSShapeHandle {
    type Target = ShapeHandle;

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

impl From<*mut ShapeHandle> for JSShapeHandle {
    #[inline(always)]
    fn from(value: *mut ShapeHandle) -> Self {
        Self(NonNull::new(value).unwrap())
    }
}

impl From<&mut ShapeHandle> for JSShapeHandle {
    #[inline(always)]
    fn from(value: &mut ShapeHandle) -> Self {
        Self(NonNull::from(value))
    }
}

impl From<JSShapeHandle> for *mut ShapeHandle {
    #[inline(always)]
    fn from(value: JSShapeHandle) -> Self {
        value.0.as_ptr()
    }
}
