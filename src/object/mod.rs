mod create;
mod dump;
mod obj;
mod payload;
mod property;
mod prototype;
mod shape;

#[cfg(feature = "builtin_symbol")]
mod private;

pub use {obj::*, payload::*, property::*, shape::*};

#[macro_export]
macro_rules! gcref_wrapper {
    ($ty:ident, $in_ty:ident) => {
        unsafe impl gc_lite::GcTracable for $ty {
            #[inline(always)]
            fn trace(&self, mut tr: gc_lite::GcTraceOp) {
                tr.add(self.0)
            }
        }

        impl std::ops::Deref for $ty {
            type Target = $in_ty;

            #[inline(always)]
            fn deref(&self) -> &Self::Target {
                std::ops::Deref::deref(&self.0)
            }
        }

        impl std::ops::DerefMut for $ty {
            #[inline(always)]
            fn deref_mut(&mut self) -> &mut Self::Target {
                std::ops::DerefMut::deref_mut(&mut self.0)
            }
        }

        impl $ty {
            #[inline(always)]
            pub(crate) const fn from(r: gc_lite::GcRef<$in_ty>) -> Self {
                Self(r)
            }

            #[inline(always)]
            pub(crate) unsafe fn from_ref_unchecked(o: &$in_ty) -> Self {
                Self(unsafe { gc_lite::GcRef::from_ref_unchecked(o) })
            }

            #[inline(always)]
            pub(crate) const fn gc_ref(&self) -> GcRef<$in_ty> {
                self.0
            }
        }
    };
}

#[macro_export]
macro_rules! object_cast {
    ($ty:ident, $($class_id:tt)|+) => {
        impl std::ops::Deref for $ty  {
            type Target = Object;

            #[inline(always)]
            fn deref(&self) -> &Self::Target {
                unsafe { &*(self as *const Self as *const Self::Target) }
            }
        }

        impl std::ops::DerefMut for $ty {
            #[inline(always)]
            fn deref_mut(&mut self) -> &mut Self::Target {
                unsafe { &mut *(self as *mut Self as *mut Self::Target) }
            }
        }

        impl TryFrom<Object> for $ty {
            type Error = i32;

            fn try_from(obj: Object) -> Result<Self, Self::Error> {
                if matches!(obj.class_id(), $($class_id)|+) {
                    Ok(Self(obj.0))
                } else {
                    Err(-1)
                }
            }
        }

        impl TryFrom<JSValue> for $ty  {
            type Error = i32;

            fn try_from(value: JSValue) -> Result<Self, Self::Error> {
                if let JSValue::Object(o) = value {
                    o.try_into()
                } else {
                    Err(-1)
                }
            }
        }

        impl From<$ty> for Object {
            #[inline(always)]
            fn from(value: $ty ) -> Self {
                Object(value.0)
            }
        }

        impl From<$ty> for JSValue {
            #[inline(always)]
            fn from(value: $ty ) -> Self {
                Self::Object(value.into())
            }
        }

        impl Object {
            paste::paste! {
                #[inline]
                pub fn [<as_ $ty>](&self) -> Option<&$ty>{
                    if matches!(self.class_id(), $($class_id)|+) {
                        Some(unsafe { &*(self as *const Self as *const _) })
                    } else {
                        None
                    }
                }

                #[inline]
                pub fn [<as_ $ty _mut>](&mut self) -> Option<&mut $ty >{
                    if matches!(self.class_id(), $($class_id)|+) {
                        Some(unsafe { &mut *(self as *mut Self as *mut _) })
                    } else {
                        None
                    }
                }

                #[inline(always)]
                pub unsafe fn [<as_ $ty _unchecked>](&self) -> &$ty  {
                    debug_assert!(matches!(self.class_id(), $($class_id)|+));
                    unsafe { &*(self as *const Self as *const _) }
                }

                #[inline(always)]
                pub unsafe fn [<as_ $ty _mut_unchecked>](&mut self) -> &mut $ty  {
                    debug_assert!(matches!(self.class_id(), $($class_id)|+));
                    unsafe { &mut *(self as *mut Self as *mut _) }
                }


                #[inline(always)]
                pub unsafe fn [<to_ $ty _unchecked>](self) -> $ty {
                    debug_assert!(matches!(self.class_id(), $($class_id)|+));
                    unsafe { std::mem::transmute::<Object, $ty>(self) }
                }

                #[inline]
                pub fn [<to_ $ty>](self) -> Option<$ty>{
                    if matches!(self.class_id(), $($class_id)|+) {
                        Some(unsafe { std::mem::transmute::<Object, $ty>(self) })
                    } else {
                        None
                    }
                }
            }
        }

        impl JSValue {
            paste::paste! {
                #[inline]
                pub fn [<as_object_ $ty>](&self) -> Option<&$ty >{
                    if let Some(o) = self.as_object(){
                        o.[<as_ $ty>]()
                    } else {
                        None
                    }
                }

                #[inline]
                pub fn [<as_object_ $ty _mut>](&mut self) -> Option<&mut $ty >{
                    if let Some(o) = self.as_object_mut(){
                        o.[<as_ $ty _mut>]()
                    } else {
                        None
                    }
                }
            }
        }
    };
}
