use crate::{
    error::{ErrorKind, JS_BACKTRACE_FLAG_SINGLE_LEVEL},
    value::JSValue,
};

use super::state::JSParseState;

impl JSParseState<'_> {
    /// given an `exc_obj` object, build traceback and throw it as current exception.
    /// old exception will be freed.
    pub fn throw(&self, exc_obj: JSValue) {
        let backtrace_flags = if !self.cur_func_def.is_null() && self.cur_func().backtrace_barrier {
            JS_BACKTRACE_FLAG_SINGLE_LEVEL
        } else {
            0
        };

        self.ctx.build_backtrace_to(
            &self.ctx.rt().cur_exception,
            Some((self.filename, self.line_num)),
            backtrace_flags,
        );

        self.ctx.throw(exc_obj);
    }

    pub fn throw_syntax_error(&self, message: &str) {
        self.ctx.throw_error_ex(ErrorKind::Syntax, message, false);

        let backtrace_flags = if !self.cur_func_def.is_null() && self.cur_func().backtrace_barrier {
            JS_BACKTRACE_FLAG_SINGLE_LEVEL
        } else {
            0
        };
        self.ctx.build_backtrace_to(
            &self.ctx.rt().cur_exception,
            Some((self.filename, self.line_num)),
            backtrace_flags,
        );
    }

    pub fn throw_reserved_identifier_error(&self) {
        self.throw_syntax_error(&format!(
            "'{}' is a reserved identifier",
            self.token.atom().unwrap().to_str(self.ctx.rt()),
        ));
    }
}
