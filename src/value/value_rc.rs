use std::{ops::Deref, ptr::NonNull};

use crate::runtime::JSRuntime;

use super::{JSValue, JS_FreeValueRT};

/// JSValue with ref count management
pub struct JSValueRc {
    rt: NonNull<JSRuntime>,
    value: JSValue,
}

impl Deref for JSValueRc {
    type Target = JSValue;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl Clone for JSValueRc {
    fn clone(&self) -> Self {
        let v = self.value.dup();
        Self {
            rt: self.rt,
            value: v,
        }
    }
}

impl From<JSValueRc> for JSValue {
    fn from(mut v: JSValueRc) -> Self {
        // reset inner value to undefined
        std::mem::replace(&mut v.value, JSValue::UNDEFINED)
    }
}

impl Drop for JSValueRc {
    fn drop(&mut self) {
        unsafe {
            JS_FreeValueRT(self.rt.as_mut(), self.value);
        }
    }
}

impl JSValue {
    #[inline]
    pub fn into_rc(self, rt: &JSRuntime) -> JSValueRc {
        JSValueRc {
            rt: NonNull::from(rt),
            value: self,
        }
    }
}
