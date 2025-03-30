mod atom;
mod registry;

pub use atom::*;

pub type JSAtomStruct = crate::value::JSString;

pub mod consts {
    use super::JSAtom;

    include!(concat!(env!("OUT_DIR"), "/atoms.rs"));
}
