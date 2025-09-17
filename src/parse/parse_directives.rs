use crate::context::{JS_MODE_STRICT, JS_MODE_STRIP};

use super::{
    state::JSParseState,
    token::{
        CHAR_CURLY_R, CHAR_SEMI, TOK_CLASS, TOK_CONST, TOK_DEBUGGER, TOK_DEC, TOK_DELETE, TOK_DO,
        TOK_ENUM, TOK_EOF, TOK_EXPORT, TOK_FALSE, TOK_FOR, TOK_FUNCTION, TOK_IDENT, TOK_IF,
        TOK_IMPORT, TOK_INC, TOK_INTERFACE, TOK_LET, TOK_NEW, TOK_NULL, TOK_NUMBER, TOK_PACKAGE,
        TOK_PRIVATE, TOK_PROTECTED, TOK_PUBLIC, TOK_REGEXP, TOK_RETURN, TOK_STATIC, TOK_STRING,
        TOK_SUPER, TOK_SWITCH, TOK_TEMPLATE, TOK_THIS, TOK_THROW, TOK_TRUE, TOK_TRY, TOK_TYPEOF,
        TOK_VAR, TOK_WHILE, TOK_WITH, TokenData,
    },
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
                matches!(self.token.val, TOK_STRING | TOK_TEMPLATE),
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
                // ';'
                CHAR_SEMI => {
                    self.next_token()?;
                    has_semi = true;
                }
                // '}'
                CHAR_CURLY_R | TOK_EOF => {
                    has_semi = true;
                }
                TOK_NUMBER | TOK_STRING | TOK_TEMPLATE | TOK_IDENT | TOK_REGEXP | TOK_DEC
                | TOK_INC | TOK_NULL | TOK_FALSE | TOK_TRUE | TOK_IF | TOK_RETURN | TOK_VAR
                | TOK_THIS | TOK_DELETE | TOK_TYPEOF | TOK_NEW | TOK_DO | TOK_WHILE | TOK_FOR
                | TOK_SWITCH | TOK_THROW | TOK_TRY | TOK_FUNCTION | TOK_DEBUGGER | TOK_WITH
                | TOK_CLASS | TOK_CONST | TOK_ENUM | TOK_EXPORT | TOK_IMPORT | TOK_SUPER
                | TOK_INTERFACE | TOK_LET | TOK_PACKAGE | TOK_PRIVATE | TOK_PROTECTED
                | TOK_PUBLIC | TOK_STATIC => {
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
        self.seek_token(&pos)
    }

    fn handle_directive(&mut self, directive: &str) {
        match directive {
            "use strict" => {
                let cfn = self.cur_func_mut();
                cfn.has_use_strict = true;
                cfn.js_mode |= JS_MODE_STRICT as u8;
            }
            "use strip" => {
                self.cur_func_mut().js_mode |= JS_MODE_STRIP as u8;
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
