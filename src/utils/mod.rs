mod helpers;

pub use helpers::*;

#[cfg(target_pointer_width = "64")]
pub const JS_PTR64: () = ();
