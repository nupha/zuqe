// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    error::{ErrorKind, JS_BACKTRACE_FLAG_SINGLE_LEVEL},
    value::JSValue,
};

use super::state::JSParseState;

impl<'s> JSParseState<'s> {
    /// given an exception value, build traceback and throw it as current exception.
    /// old exception will be freed.
    #[cold]
    pub(super) fn throw(&mut self, err_val: JSValue) {
        let backtrace_flags =
            if !self.cur_func_def.is_null() && self.cur_func().attrs.backtrace_barrier {
                JS_BACKTRACE_FLAG_SINGLE_LEVEL
            } else {
                0
            };

        let fln = (self.filename, self.line_num);
        self.ctx()
            .build_backtrace_to(err_val, Some(fln), backtrace_flags);

        self.ctx().throw(err_val);
    }

    #[cold]
    pub(super) fn throw_syntax_error(&mut self, message: &str) {
        self.ctx()
            .throw_error_internal(ErrorKind::Syntax, message, false);

        let backtrace_flags =
            if !self.cur_func_def.is_null() && self.cur_func().attrs.backtrace_barrier {
                JS_BACKTRACE_FLAG_SINGLE_LEVEL
            } else {
                0
            };

        let exc = self.ctx().rt_mut().exception();
        let fln = (self.filename, self.line_num);
        self.ctx()
            .build_backtrace_to(exc, Some(fln), backtrace_flags);
    }

    #[cold]
    pub(super) fn throw_reserved_identifier_error(&mut self) {
        let msg = format!(
            "'{}' is a reserved identifier",
            self.token.atom().unwrap().to_utf8(self.ctx().rt()),
        );
        self.throw_syntax_error(&msg);
    }
}
