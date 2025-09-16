mod create;
mod dump;
mod gcobj;
mod obj;
mod payload;
mod private;
mod property;
mod prototype;
mod shape;

pub use {dump::*, gcobj::*, obj::*, payload::*, private::*, property::*, shape::*};

#[macro_export]
macro_rules! object_cast {
    ($ty:ident, $($class_id:tt)|+) => {
        impl TryFrom<JSObject> for $ty {
            type Error = i32;

            fn try_from(obj: JSObject) -> Result<Self, Self::Error> {
                if matches!(obj.class_id(), $($class_id)|+) {
                    Ok(Self(obj))
                } else {
                    Err(-1)
                }
            }
        }

        impl TryFrom<JSValue> for $ty {
            type Error = i32;

            fn try_from(value: JSValue) -> Result<Self, Self::Error> {
                if let JSValue::Object(o) = value {
                    o.try_into()
                } else {
                    Err(-1)
                }
            }
        }

        impl From<$ty> for JSObject {
            #[inline(always)]
            fn from(value: $ty) -> Self {
                value.0
            }
        }

        impl From<$ty> for JSValue {
            #[inline(always)]
            fn from(value: $ty) -> Self {
                Self::Object(value.into())
            }
        }

        impl std::ops::Deref for $ty {
            type Target = JSObject;

            #[inline(always)]
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl JSObject {
            paste::paste! {
                #[inline]
                pub fn [<as_ $ty>](&self) -> Option<&$ty>{
                    if matches!(self.class_id(), $($class_id)|+) {
                        Some(unsafe { &*(self as *const Self as *const $ty) })
                    } else {
                        None
                    }
                }

                #[inline(always)]
                pub unsafe fn [<as_ $ty _unchecked>](&self) -> &[<$ty>] {
                    debug_assert!(matches!(self.class_id(), $($class_id)|+));
                    unsafe { &*(self as *const Self as *const $ty) }
                }
            }
        }

        impl JSValue {
            paste::paste! {
                #[inline]
                pub fn [<as_object_ $ty>](&self) -> Option<&$ty>{
                    if let Some(o) = self.as_object(){
                        o.[<as_ $ty>]()
                    } else {
                        None
                    }
                }
            }
        }
    };
}
