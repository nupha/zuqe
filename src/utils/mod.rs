mod helpers;

pub use helpers::*;

#[cfg(target_pointer_width = "64")]
pub const JS_PTR64: () = ();

#[inline(always)]
pub const fn likely(x: bool) -> bool {
    x
}

#[inline(always)]
pub const fn unlikely(x: bool) -> bool {
    x
}
