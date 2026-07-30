// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::stack_frame::ExecMode;

use super::{
    state::JSParseState,
    tokenize::{TokenData, TokenKind},
};

impl JSParseState<'_> {
    pub(super) fn parse_directives(&mut self) -> Result<(), i32> {
        if !matches!(self.token.data, TokenData::Str { .. }) {
            return Ok(());
        }

        // seeking sole strings within current code block to check for directives,
        // then restart from the saved position.
        self.with_lookahead(|s| {
            while let TokenData::Str { .. } = &s.token.data {
                debug_assert!(
                    matches!(s.token.kind, TokenKind::STRING | TokenKind::TEMPLATE),
                    "{:?}",
                    s.token
                );

                // Note: don't use parsed string content, it could have been escaped.
                // instead, we extract raw bytes as-is from input ranging
                // last token ptr through current buf ptr.
                // this range of bytes includes 2 quote chars.
                let str = str::from_utf8(unsafe {
                    debug_assert!(s.token.ptr <= s.buf_ptr.offset(-2));

                    std::slice::from_raw_parts(
                        s.token.ptr.add(1),                              // ingore leading quote
                        s.buf_ptr.offset_from(s.token.ptr) as usize - 2, // length exclude 2 quotes
                    )
                })
                .map(|s| s.to_string());

                s.next_token()?;

                let mut has_semi = false;
                match s.token.kind {
                    TokenKind::Char(';') => {
                        s.next_token()?;
                        has_semi = true;
                    }
                    TokenKind::Char('}') | TokenKind::EOF => {
                        has_semi = true;
                    }
                    TokenKind::NUMBER
                    | TokenKind::STRING
                    | TokenKind::TEMPLATE
                    | TokenKind::IDENT
                    | TokenKind::REGEXP
                    | TokenKind::DEC
                    | TokenKind::INC
                    | TokenKind::NULL
                    | TokenKind::FALSE
                    | TokenKind::TRUE
                    | TokenKind::IF
                    | TokenKind::RETURN
                    | TokenKind::VAR
                    | TokenKind::THIS
                    | TokenKind::DELETE
                    | TokenKind::TYPEOF
                    | TokenKind::NEW
                    | TokenKind::DO
                    | TokenKind::WHILE
                    | TokenKind::FOR
                    | TokenKind::SWITCH
                    | TokenKind::THROW
                    | TokenKind::TRY
                    | TokenKind::FUNCTION
                    | TokenKind::DEBUGGER
                    | TokenKind::WITH
                    | TokenKind::CLASS
                    | TokenKind::CONST
                    | TokenKind::ENUM
                    | TokenKind::EXPORT
                    | TokenKind::IMPORT
                    | TokenKind::SUPER
                    | TokenKind::INTERFACE
                    | TokenKind::LET
                    | TokenKind::PACKAGE
                    | TokenKind::PRIVATE
                    | TokenKind::PROTECTED
                    | TokenKind::PUBLIC
                    | TokenKind::STATIC
                        if s.got_lf =>
                    {
                        // automatic insertion of ';'
                        has_semi = true;
                    }
                    _ => {}
                }

                if !has_semi {
                    break;
                }

                if let Ok(str) = str {
                    s.handle_directive(&str);
                }
            }

            Ok(())
        })
    }

    fn handle_directive(&mut self, directive: &str) {
        match directive {
            "use strict" => {
                let cfn = self.cur_func_mut();
                cfn.has_use_strict = true;
                cfn.attrs.exec_mode |= ExecMode::STRICT;
            }
            "use strip" => {
                self.cur_func_mut().attrs.exec_mode |= ExecMode::STRIP;
            }

            #[cfg(all(debug_assertions, feature = "dump_bytecode"))]
            "debug dump_run" => {
                self.cur_func_mut().debug.dump_run = 1;
            }
            #[cfg(all(debug_assertions, feature = "dump_bytecode"))]
            "debug dump_run_stack" => {
                self.cur_func_mut().debug.dump_run = 2;
            }

            _ => {}
        }
    }
}
