mod detail;
mod dump;
mod gcobj;
mod obj;
mod private;
mod property;
mod prototype;
mod shape;

pub use {detail::*, dump::*, gcobj::*, obj::*, private::*, property::*, shape::*};

#[macro_export]
macro_rules! object_cast {
    ($ty:ident, $class_id:expr) => {
        impl TryFrom<JSObject> for $ty {
            type Error = i32;

            fn try_from(obj: JSObject) -> Result<Self, Self::Error> {
                if obj.class_id() == $class_id {
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
    };
}
