mod atom;
mod registry;

pub use atom::*;

pub(crate) use registry::*;

use std::{ptr::NonNull, rc::Rc};

use crate::{
    runtime::JSRuntime,
    string::{JSString, JSStringInner, StringContent},
};

pub type AtomDesc = JSString;

// /// Atom description. this struct hold one Rc reference.
// #[derive(Debug, PartialEq)]
// #[repr(transparent)]
// pub(super) struct AtomDesc(NonNull<JSStringInner>);

// impl Clone for AtomDesc {
//     fn clone(&self) -> Self {
//         debug_assert!(self.0.as_ptr() as usize != 0x01);

//         unsafe {
//             let d = Rc::from_raw(self.0.as_ref());
//             let s = d.clone();
//             let _ = Rc::into_raw(d);
//             let _ = Rc::into_raw(s);
//         }

//         Self(self.0)
//     }
// }

// impl From<AtomDesc> for Atom {
//     fn from(mut d: AtomDesc) -> Self {
//         let a = Atom(d.atom_info().id());
//         unsafe {
//             // set to an invalid pointer
//             d.0 = NonNull::new_unchecked(0x01 as *mut _);
//         }
//         a
//     }
// }

// impl Drop for AtomDesc {
//     fn drop(&mut self) {
//         unsafe {
//             if self.0.as_ptr() as usize != 0x01 {
//                 let d = Rc::from_raw(self.0.as_ref());

//                 drop(d)
//             }
//         }
//     }
// }

// impl AtomDesc {
//     pub(super) fn from(s: JSString) -> Self {
//         Self(unsafe { std::ptr::NonNull::new_unchecked(Rc::into_raw(s.0) as _) })
//     }

//     pub fn atom_info(&self) -> AtomHintAndId {
//         unsafe { self.0.as_ref().atom_info().unwrap() }
//     }

//     /// get atom description string
//     pub fn string(&self) -> &JSString {
//         unsafe { self.0.cast::<JSString>().as_ref() }
//     }

//     /// get atom description content
//     pub fn content(&self) -> &StringContent {
//         unsafe { self.0.as_ref().content() }
//     }

//     /// get atom description hash
//     #[inline(always)]
//     pub fn hash(&self) -> u32 {
//         unsafe { self.0.as_ref().hash() }
//     }

//     pub fn rt(&self) -> *mut JSRuntime {
//         unsafe { self.0.as_ref().rt_ptr() }
//     }

//     pub fn is_const(&self) -> bool {
//         is_const_atom(self.atom_info().id())
//     }

//     /// get reference count of description
//     pub fn ref_count(&self) -> usize {
//         self.string().ref_count()
//         // unsafe {
//         //     let d = Rc::from_raw(self.0.as_ref());
//         //     let n = Rc::strong_count(&d);
//         //     let _ = Rc::into_raw(d);
//         //     n
//         // }
//     }

//     /// increase reference count of description.
//     ///
//     /// Safty: this method will leak Rc reference count by 1.
//     pub(super) unsafe fn inc_ref(&self) {
//         let d = unsafe { Rc::from_raw(self.0.as_ref()) };
//         let s = d.clone();
//         let _ = Rc::into_raw(d);
//         let _ = Rc::into_raw(s);
//     }
// }

pub mod consts {
    use super::AtomRef;

    include!(concat!(env!("OUT_DIR"), "/atoms.rs"));
}
