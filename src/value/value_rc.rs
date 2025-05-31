use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
    rc::Rc,
};

use crate::{
    function::JSFunctionBytecode,
    gc::RefCounted,
    module::JSModuleDef,
    runtime::JSRuntime,
    value::{JS_TAG_BIG_DECIMAL, JS_TAG_BIG_FLOAT, JS_TAG_BIG_INT},
};

use super::{
    JSValue, JS_FreeValueRT, JS_TAG_FUNCTION_BYTECODE, JS_TAG_MODULE, JS_TAG_OBJECT, JS_TAG_STRING,
    JS_TAG_SYMBOL, JS_VALUE_GET_PTR,
};

impl RefCounted for JSValue {
    fn ref_count(&self) -> i32 {
        match self.tag() {
            JS_TAG_OBJECT => unsafe { self.object_unchecked().ref_count() },
            JS_TAG_STRING | JS_TAG_SYMBOL => unsafe {
                let p = Rc::from_raw(self.string());
                let r = Rc::strong_count(&p);
                let _ = Rc::into_raw(p);
                r as i32
            },
            JS_TAG_FUNCTION_BYTECODE => unsafe {
                (*JS_VALUE_GET_PTR(*self).cast::<JSFunctionBytecode>()).ref_count()
            },
            JS_TAG_MODULE => unsafe {
                (*JS_VALUE_GET_PTR(*self).cast::<JSModuleDef>()).ref_count()
            },
            JS_TAG_BIG_DECIMAL | JS_TAG_BIG_FLOAT | JS_TAG_BIG_INT => {
                unimplemented!()
            }
            _ => 1,
        }
    }

    fn inc_ref(&mut self) -> i32 {
        match self.tag() {
            JS_TAG_OBJECT => unsafe { self.object_unchecked().inc_ref() },
            JS_TAG_STRING | JS_TAG_SYMBOL => unsafe {
                let s = self.string();
                Rc::increment_strong_count(s);
                let p = Rc::from_raw(s);
                let r = Rc::strong_count(&p);
                let _ = Rc::into_raw(p);
                r as i32
            },
            JS_TAG_FUNCTION_BYTECODE => unsafe {
                (*JS_VALUE_GET_PTR(*self).cast::<JSFunctionBytecode>()).inc_ref()
            },
            JS_TAG_MODULE => unsafe { (*JS_VALUE_GET_PTR(*self).cast::<JSModuleDef>()).inc_ref() },
            JS_TAG_BIG_DECIMAL | JS_TAG_BIG_FLOAT | JS_TAG_BIG_INT => {
                unimplemented!()
            }
            _ => 1,
        }
    }

    fn dec_ref(&mut self) -> i32 {
        match self.tag() {
            JS_TAG_OBJECT => unsafe { self.object_unchecked().dec_ref() },
            JS_TAG_FUNCTION_BYTECODE => unsafe {
                (*JS_VALUE_GET_PTR(*self).cast::<JSFunctionBytecode>()).inc_ref()
            },
            JS_TAG_MODULE => unsafe { (*JS_VALUE_GET_PTR(*self).cast::<JSModuleDef>()).inc_ref() },
            JS_TAG_STRING | JS_TAG_SYMBOL => {
                unreachable!("don't call ::def_ref() on a string value");
            }
            JS_TAG_BIG_DECIMAL | JS_TAG_BIG_FLOAT | JS_TAG_BIG_INT => {
                unimplemented!()
            }
            _ => 1,
        }
    }
}

impl JSValue {
    /// increase value ref_count, and returns the value
    #[inline(always)]
    pub fn dup(&self) -> Self {
        let mut v = *self;

        // if JS_VALUE_HAS_REF_COUNT(v) {
        //     match v.tag() {
        //         JS_TAG_STRING | JS_TAG_SYMBOL => unsafe {
        //             Rc::increment_strong_count(v.string());
        //         },
        //         _ => {
        //             let p = JS_VALUE_GET_PTR(v).cast::<JSRefCountHeader>();
        //             unsafe {
        //                 p.as_mut().unwrap().ref_count += 1;
        //             }
        //         }
        //     }
        // }

        v.inc_ref();
        v
    }
}

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
