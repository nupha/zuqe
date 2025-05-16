use std::{ffi::CStr, ops::DerefMut};

use crate::{
    error::{JSErrorEnum, JS_BACKTRACE_FLAG_SINGLE_LEVEL},
    value::JSValue,
};

use super::state::JSParseState;

impl JSParseState<'_> {
    /// given an `exc_obj` object, build traceback and throw it as current exception.
    /// old exception will be freed.
    pub fn throw(&mut self, exc_obj: JSValue) {
        let backtrace_flags =
            if !self.cur_func_ptr.is_null() && unsafe { self.cur_func().backtrace_barrier } {
                JS_BACKTRACE_FLAG_SINGLE_LEVEL
            } else {
                0
            };

        let ctx = self.ctx.deref_mut();

        ctx.build_backtrace_to(
            ctx.rt().current_exception,
            Some((self.filename, self.line_num)),
            backtrace_flags,
        );

        ctx.throw(exc_obj);
    }

    pub fn throw_syntax_error(&mut self, message: &CStr) {
        self.ctx.throw_error_ex(JSErrorEnum::Syntax, message, false);

        let backtrace_flags =
            if !self.cur_func_ptr.is_null() && unsafe { self.cur_func().backtrace_barrier } {
                JS_BACKTRACE_FLAG_SINGLE_LEVEL
            } else {
                0
            };

        let exc = self.ctx.rt().current_exception;
        self.ctx
            .build_backtrace_to(exc, Some((self.filename, self.line_num)), backtrace_flags);
    }
}

/// always returns -1
pub unsafe fn js_parse_error_reserved_identifier(s: *mut JSParseState) -> i32 {
    (*s).throw_syntax_error(CStr::from_ptr(
        format!(
            "'{}' is a reserved identifier\0",
            (*s).token.u.ident.atom.to_str((*(*s).ctx).rt()),
        )
        .as_ptr()
        .cast(),
    ));
    -1
}
