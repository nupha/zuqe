use std::{ops::Deref, ptr::NonNull};

use crate::runtime::JSRuntime;

use super::{JSValue, JS_FreeValueRT};

/// A reference-counted wrapper for JSValue that provides automatic memory management
/// and safe access to the underlying JSValue.
pub struct JSValueRc {
    rt: NonNull<JSRuntime>,
    value: JSValue,
}

impl Deref for JSValueRc {
    type Target = JSValue;

    /// Automatically dereferences to access the inner JSValue
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl Clone for JSValueRc {
    /// Dup the inner JSValue
    #[inline(always)]
    fn clone(&self) -> Self {
        let v = self.value.dup();
        Self {
            rt: self.rt,
            value: v,
        }
    }
}

impl From<JSValueRc> for JSValue {
    /// Converts JSValueRc to JSValue while resetting the underlying JSValue to undefined
    #[inline(always)]
    fn from(mut v: JSValueRc) -> Self {
        std::mem::replace(&mut v.value, JSValue::UNDEFINED)
    }
}

impl Drop for JSValueRc {
    /// Automatically frees associated resources in the JavaScript runtime when JSValueRc is destroyed
    fn drop(&mut self) {
        unsafe {
            JS_FreeValueRT(self.rt.as_mut(), self.value);
        }
    }
}

impl JSValue {
    /// Wraps a JSValue into a reference-counted JSValueRc
    #[inline(always)]
    pub fn into_rc(self, rt: &JSRuntime) -> JSValueRc {
        JSValueRc {
            rt: NonNull::from(rt),
            value: self,
        }
    }
}
