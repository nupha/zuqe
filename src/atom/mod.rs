mod atom;
mod registry;

pub use atom::*;

pub(crate) use registry::*;

pub mod consts {
    use super::AtomRef;

    include!(concat!(env!("OUT_DIR"), "/atoms.rs"));
}

/// atom description
pub type AtomDesc = crate::string::JSString<'static>;
