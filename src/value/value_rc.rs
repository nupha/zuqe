use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
    rc::Rc,
};

use crate::runtime::JSRuntime;

use super::{JSValue, JS_FreeValueRT};

/// A reference-counted wrapper for JSValue that provides automatic memory management
/// and safe access to the underlying JSValue.
pub struct JSValueRc {
    rt: NonNull<JSRuntime>,
    value: JSValue,
    on_drop: Option<Rc<dyn Fn(&mut JSRuntime, JSValue)>>,
}

impl JSValueRc {
    pub fn undefined(rt: &JSRuntime) -> Self {
        Self {
            rt: NonNull::from(rt),
            value: JSValue::UNDEFINED,
            on_drop: None,
        }
    }

    /// set a callback to run before value drop
    pub fn on_drop(&mut self, f: impl Fn(&mut JSRuntime, JSValue) + 'static) {
        self.on_drop = Some(Rc::new(f));
    }
}

impl Deref for JSValueRc {
    type Target = JSValue;

    /// Automatically dereferences to access the inner JSValue
    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl DerefMut for JSValueRc {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
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
            on_drop: self.on_drop.clone(),
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
            let rt = self.rt.as_mut();

            if let Some(f) = self.on_drop.take() {
                f(rt, self.value);
            }

            JS_FreeValueRT(rt, self.value);
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
            on_drop: None,
        }
    }
}
