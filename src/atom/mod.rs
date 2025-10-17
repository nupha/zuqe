mod atom;
mod atom_rc;
mod registry;

pub use {atom::*, atom_rc::*};

/// the atom descriptor
pub type JSAtomStruct = crate::string::JSString;

pub mod consts {
    use super::AtomRef;

    include!(concat!(env!("OUT_DIR"), "/atoms.rs"));
}
