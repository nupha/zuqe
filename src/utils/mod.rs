mod helpers;
mod list;
mod node_list;

pub use {helpers::*, list::*, node_list::*};

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
