use std::ffi::{CStr, c_char};
use std::ptr;

use crate::{ModuleDef, ScopedContext};
use zuqe_ffi::{ZqContextRef, ZqModuleRef};

unsafe fn ctx_as_scoped<'a>(ctx: ZqContextRef) -> &'a mut ScopedContext<'a> {
    unsafe { &mut *(ctx as *mut ScopedContext) }
}

fn module_from(m: ModuleDef) -> ZqModuleRef {
    Box::into_raw(Box::new(m)) as ZqModuleRef
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn zq_create_module_demo(
    ctx: ZqContextRef,
    name: *const c_char,
) -> ZqModuleRef {
    if ctx.is_null() || name.is_null() {
        return ptr::null_mut();
    }

    let name = match unsafe { CStr::from_ptr(name) }.to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };

    let ctx = unsafe { ctx_as_scoped(ctx) };

    match ctx.create_module(name, &[]) {
        Ok(m) => module_from(m),
        Err(_) => ptr::null_mut(),
    }
}
