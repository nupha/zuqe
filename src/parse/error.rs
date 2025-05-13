use std::ffi::CStr;

use crate::error::{build_backtrace, JSErrorEnum, JS_BACKTRACE_FLAG_SINGLE_LEVEL};

use super::state::JSParseState;

/// always returns -1
pub unsafe fn js_parse_error(s: *mut JSParseState, message: &CStr) -> i32 {
    let ctx = (*s).ctx;

    ctx.as_mut()
        .unwrap()
        .JS_ThrowError2(JSErrorEnum::Syntax, message, false);

    let mut backtrace_flags = 0;
    if !(*s).cur_func.is_null() && (*(*s).cur_func).backtrace_barrier {
        backtrace_flags = JS_BACKTRACE_FLAG_SINGLE_LEVEL;
    }

    build_backtrace(
        ctx,
        (*ctx).rt().current_exception,
        Some((*s).filename),
        Some((*s).line_num),
        backtrace_flags,
    );

    -1
}

/// always returns -1
pub unsafe fn js_parse_error_reserved_identifier(s: *mut JSParseState) -> i32 {
    js_parse_error(
        s,
        CStr::from_ptr(
            format!(
                "'{}' is a reserved identifier\0",
                (*s).token.u.ident.atom.to_str((*(*s).ctx).rt()),
            )
            .as_ptr()
            .cast(),
        ),
    )
}
