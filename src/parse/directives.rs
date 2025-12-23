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

        // save current pos
        let pos = self.get_pos();

        // seeking sole strings within current code block to check for directives.
        while let TokenData::Str { .. } = &self.token.data {
            debug_assert!(
                matches!(self.token.val, TokenKind::STRING | TokenKind::TEMPLATE),
                "{:?}",
                self.token
            );

            // Note: don't use parsed string content, it could have been escaped.
            // instead, we extract raw bytes as-is from input ranging
            // last token ptr through current buf ptr.
            // this range of bytes includes 2 quote chars.
            let str = str::from_utf8(unsafe {
                debug_assert!(self.token.ptr <= self.buf_ptr.offset(-2));

                std::slice::from_raw_parts(
                    self.token.ptr.add(1),                                 // ingore leading quote
                    self.buf_ptr.offset_from(self.token.ptr) as usize - 2, // length exclude 2 quotes
                )
            })
            .map(|s| s.to_string());

            self.next_token()?;

            let mut has_semi = false;
            match self.token.val {
                TokenKind::Char(';') => {
                    self.next_token()?;
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
                | TokenKind::STATIC => {
                    // automatic insertion of ';'
                    if self.got_lf {
                        has_semi = true;
                    }
                }
                _ => {}
            }

            if !has_semi {
                break;
            }

            if let Ok(s) = str {
                self.handle_directive(&s);
            }
        }

        // restart from saved position
        self.seek_token(&pos)?;
        Ok(())
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
