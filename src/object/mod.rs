mod create;
mod dump;
mod gcobj;
mod obj;
mod payload;
mod private;
mod property;
mod prototype;
mod shape;

pub use {dump::*, gcobj::*, obj::*, payload::*, property::*, shape::*};

#[macro_export]
macro_rules! object_cast {
    ($ty:ident, $($class_id:tt)|+) => {
        impl<'a> TryFrom<JSObject<'a>> for $ty<'a> {
            type Error = i32;

            fn try_from(obj: JSObject<'a>) -> Result<Self, Self::Error> {
                if matches!(obj.class_id(), $($class_id)|+) {
                    Ok(Self(obj))
                } else {
                    Err(-1)
                }
            }
        }

        impl<'a> TryFrom<JSValue<'a>> for $ty<'a> {
            type Error = i32;

            fn try_from(value: JSValue<'a>) -> Result<Self, Self::Error> {
                if let JSValue::Object(o) = value {
                    o.try_into()
                } else {
                    Err(-1)
                }
            }
        }

        impl<'a> From<$ty<'a>> for JSObject<'a> {
            #[inline(always)]
            fn from(value: $ty<'a>) -> Self {
                value.0
            }
        }

        impl<'a> From<$ty<'a>> for JSValue<'a> {
            #[inline(always)]
            fn from(value: $ty<'a>) -> Self {
                Self::Object(value.into())
            }
        }

        impl<'a> std::ops::Deref for $ty<'a> {
            type Target = JSObject<'a>;

            #[inline(always)]
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl<'a> JSObject<'a> {
            paste::paste! {
                #[inline]
                pub fn [<as_ $ty>](&self) -> Option<&$ty<'a>>{
                    if matches!(self.class_id(), $($class_id)|+) {
                        Some(unsafe { &*(self as *const Self as *const _) })
                    } else {
                        None
                    }
                }

                #[inline(always)]
                pub unsafe fn [<as_ $ty _unchecked>](&self) -> &$ty<'a> {
                    debug_assert!(matches!(self.class_id(), $($class_id)|+));
                    unsafe { &*(self as *const Self as *const _) }
                }
            }
        }

        impl<'a> JSValue<'a> {
            paste::paste! {
                #[inline]
                pub fn [<as_object_ $ty>](&self) -> Option<&$ty<'a>>{
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
