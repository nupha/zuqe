use crate::{
    error::{JS_BACKTRACE_FLAG_SINGLE_LEVEL, JSErrorEnum},
    value::JSValue,
};

use super::state::JSParseState;

impl JSParseState<'_> {
    /// given an `exc_obj` object, build traceback and throw it as current exception.
    /// old exception will be freed.
    pub fn throw(&self, exc_obj: JSValue) {
        let backtrace_flags = if !self.cur_func_ptr.is_null() && self.cur_func().backtrace_barrier {
            JS_BACKTRACE_FLAG_SINGLE_LEVEL
        } else {
            0
        };

        self.ctx.build_backtrace_to(
            &self.ctx.rt().current_exception,
            Some((self.filename, self.line_num)),
            backtrace_flags,
        );

        self.ctx.throw(exc_obj);
    }

    pub fn throw_syntax_error(&self, message: &str) {
        self.ctx.throw_error_ex(JSErrorEnum::Syntax, message, false);

        let backtrace_flags = if !self.cur_func_ptr.is_null() && self.cur_func().backtrace_barrier {
            JS_BACKTRACE_FLAG_SINGLE_LEVEL
        } else {
            0
        };

        self.ctx.build_backtrace_to(
            &self.ctx.rt().current_exception,
            Some((self.filename, self.line_num)),
            backtrace_flags,
        );
    }
}

/// always returns -1
pub unsafe fn js_parse_error_reserved_identifier(s: *mut JSParseState) -> i32 {
    (*s).throw_syntax_error(&format!(
        "'{}' is a reserved identifier",
        (*s).token.data.ident_atom().unwrap().to_str(&(*s).ctx.rt()),
    ));
    -1
}
