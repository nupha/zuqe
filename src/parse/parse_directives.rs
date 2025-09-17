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

        let pos = self.get_pos();

        while let TokenData::Str { str, .. } = &self.token.data {
            debug_assert!(
                matches!(self.token.val, TOK_STRING | TOK_TEMPLATE),
                "{:?}",
                self.token
            );

            let txt: String = str.to_utf8().into_owned();
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

            self.handle_directives(&txt);
        }

        self.seek_token(&pos)
    }

    fn handle_directives(&mut self, directive: &str) {
        match directive {
            "use strict" => {
                let cfn = self.cur_func_mut();
                cfn.has_use_strict = true;
                cfn.js_mode |= JS_MODE_STRICT as u8;
            }
            "use strip" => {
                self.cur_func_mut().js_mode |= JS_MODE_STRIP as u8;
            }

            _ => {}
        }
    }
}
