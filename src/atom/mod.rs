mod atom;
mod atom_rc;
mod registry;

pub use {atom::*, atom_rc::*};

pub type JSAtomStruct = crate::value::JSString;

pub mod consts {
    use super::JSAtom;

    include!(concat!(env!("OUT_DIR"), "/atoms.rs"));
}
