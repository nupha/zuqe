use crate::{
    atom::{JSAtom, JS_DupAtom, JS_FreeAtom, JS_ATOM_NULL},
    bytecode::{OpCode, OpCodeTemp},
    error::JS_ThrowSyntaxError,
    function::JSFunctionKindEnum,
    parse::{
        state::{PF_ARROW_FUNC, PF_IN_ACCEPTED, PF_POW_ALLOWED},
        token::{peek_token, TOK_IN},
    },
};

use super::{
    emit::{emit_atom_dup, emit_op, emit_op_temp, emit_u16, get_prev_opcode},
    error::js_parse_error,
    expr_lvalue::PutLValueEnum,
    parse::js_parse_delete,
    state::{JSParseState, PF_POSTFIX_CALL, PF_POW_FORBIDDEN},
    token::{
        CHAR_MINUS, CHAR_PERCENT, CHAR_PLUS, CHAR_SLASH, CHAR_STAR, TOK_AWAIT, TOK_DEC, TOK_DELETE,
        TOK_EQ, TOK_GTE, TOK_INC, TOK_INSTANCEOF, TOK_LTE, TOK_NEQ, TOK_POW, TOK_PRIVATE_NAME,
        TOK_SAR, TOK_SHL, TOK_SHR, TOK_STRICT_EQ, TOK_STRICT_NEQ, TOK_TYPEOF, TOK_VOID,
    },
};

impl JSParseState {
    /// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
    pub(super) unsafe fn parse_expr_binary(
        &mut self,
        level: i32,
        parse_flags: i32,
    ) -> Result<(), i32> {
        if level == 0 {
            return self.parse_unary((parse_flags & PF_ARROW_FUNC) | PF_POW_ALLOWED);
        } else if self.token.val == TOK_PRIVATE_NAME
            && (parse_flags & PF_IN_ACCEPTED) != 0
            && level == 4
            && peek_token(self, false) == TOK_IN
        {
            let atom = JS_DupAtom(self.ctx, self.token.u.ident.atom);

            if self.next_token().is_err() {
                JS_FreeAtom(self.ctx, atom);
                return Err(-1);
            }
            if self.token.val != TOK_IN {
                JS_FreeAtom(self.ctx, atom);
                return Err(-1);
            }
            if self.next_token().is_err() {
                JS_FreeAtom(self.ctx, atom);
                return Err(-1);
            }
            if self
                .parse_expr_binary(level - 1, parse_flags & !PF_ARROW_FUNC)
                .is_err()
            {
                JS_FreeAtom(self.ctx, atom);
                return Err(-1);
            }

            emit_op_temp(self, OpCodeTemp::SCOPE_IN_PRIVATE_FIELD);
            emit_atom_dup(self, atom);
            emit_u16(self, (*self.cur_func).scope_level as _);
            JS_FreeAtom(self.ctx, atom);

            return Ok(());
        } else {
            self.parse_expr_binary(level - 1, parse_flags)?;
        }

        loop {
            let op = self.token.val;

            let opcode = match level {
                1 => match op {
                    CHAR_STAR => OpCode::MUL,    // '*'
                    CHAR_SLASH => OpCode::DIV,   // '/'
                    CHAR_PERCENT => OpCode::MOD, // '%'
                    _ => {
                        return Ok(());
                    }
                },
                2 => match op {
                    CHAR_PLUS => OpCode::ADD,  // '+'
                    CHAR_MINUS => OpCode::SUB, // '-'
                    _ => {
                        return Ok(());
                    }
                },
                3 => match op {
                    TOK_SHL => OpCode::SHL,
                    TOK_SAR => OpCode::SAR,
                    TOK_SHR => OpCode::SHR,
                    _ => {
                        return Ok(());
                    }
                },
                4 => match op {
                    0x3c => OpCode::LT, // '<'
                    0x3e => OpCode::GT, // '>'
                    TOK_LTE => OpCode::LTE,
                    TOK_GTE => OpCode::GTE,
                    TOK_INSTANCEOF => OpCode::INSTANCEOF,
                    TOK_IN => {
                        if parse_flags & PF_IN_ACCEPTED != 0 {
                            OpCode::IN
                        } else {
                            return Ok(());
                        }
                    }
                    _ => {
                        return Ok(());
                    }
                },
                5 => match op {
                    TOK_EQ => OpCode::EQ,
                    TOK_NEQ => OpCode::NEG,
                    TOK_STRICT_EQ => OpCode::STRICT_EQ,
                    TOK_STRICT_NEQ => OpCode::STRICT_NEQ,
                    _ => {
                        return Ok(());
                    }
                },
                6 => match op {
                    0x26 => OpCode::AND, // '&'
                    _ => {
                        return Ok(());
                    }
                },
                7 => match op {
                    0x5e => OpCode::XOR, // '^'
                    _ => {
                        return Ok(());
                    }
                },
                8 => match op {
                    0x7c => OpCode::OR, // '|'
                    _ => {
                        return Ok(());
                    }
                },
                _ => unreachable!(),
            };

            self.next_token()?;
            self.parse_expr_binary(level - 1, parse_flags & !PF_ARROW_FUNC)?;
            emit_op(self, opcode);
        }
    }

    // allowed parse_flags: PF_ARROW_FUNC, PF_POW_ALLOWED, PF_POW_FORBIDDEN
    pub(super) unsafe fn parse_unary(&mut self, mut parse_flags: i32) -> Result<(), i32> {
        let op: i32;

        match self.token.val {
            // '+', '-', '!', '~'
            op @ (CHAR_PLUS | CHAR_MINUS | 0x21 | 0x7e | TOK_VOID) => {
                self.next_token()?;
                self.parse_unary(PF_POW_FORBIDDEN)?;

                match op {
                    0x2d => emit_op(self, OpCode::NEG),  // '-'
                    0x2b => emit_op(self, OpCode::PLUS), // '+'
                    0x21 => emit_op(self, OpCode::LNOT), // '!'
                    0x7e => emit_op(self, OpCode::NOT),  // '~'
                    TOK_VOID => {
                        emit_op(self, OpCode::DROP);
                        emit_op(self, OpCode::UNDEFINED);
                    }
                    _ => unreachable!(),
                }
                parse_flags = 0;
            }
            op @ (TOK_DEC | TOK_INC) => {
                self.next_token()?;
                self.parse_unary(0)?;

                let mut opcode: i32 = 0;
                let mut scope: i32 = 0;
                let mut label: i32 = 0;
                let mut name = JS_ATOM_NULL;

                self.get_lvalue(
                    &mut opcode,
                    &mut scope,
                    &mut name,
                    &mut label,
                    std::ptr::null_mut(),
                    true,
                    op,
                )?;

                emit_op(
                    self,
                    if op == TOK_DEC {
                        OpCode::DEC
                    } else {
                        OpCode::INC
                    },
                ); //emit_op_u8(s, (OpCode::DEC as i32 + op - TOK_DEC) as u8);

                self.put_lvalue(
                    opcode,
                    scope,
                    name,
                    label,
                    PutLValueEnum::PUT_LVALUE_KEEP_TOP,
                    false,
                );
            }
            TOK_TYPEOF => {
                self.next_token()?;
                self.parse_unary(PF_POW_FORBIDDEN)?;

                /* reference access should not return an exception, so we
                patch the get_var */
                let fd = self.cur_func;
                if get_prev_opcode(fd) == OpCodeTemp::SCOPE_GET_VAR as u8 {
                    (*fd).byte_code[(*fd).last_opcode_pos as usize] =
                        OpCodeTemp::SCOPE_GET_VAR_UNDEF as u8;
                }
                emit_op(self, OpCode::TYPEOF);
                parse_flags = 0;
            }
            TOK_DELETE => {
                if js_parse_delete(self) != 0 {
                    return Err(-1);
                }
                parse_flags = 0;
            }
            TOK_AWAIT => {
                if matches!(
                    (*self.cur_func).func_kind,
                    JSFunctionKindEnum::JS_FUNC_ASYNC | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
                ) {
                    js_parse_error(self, c"unexpected 'await' keyword");
                    return Err(-1);
                }
                if !(*self.cur_func).in_function_body {
                    js_parse_error(self, c"await in default expression");
                    return Err(-1);
                }

                self.next_token()?;
                self.parse_unary(PF_POW_FORBIDDEN)?;

                (*self.cur_func).has_await = true;
                emit_op(self, OpCode::AWAIT);
                parse_flags = 0;
            }
            _ => {
                self.parse_postfix_expr((parse_flags & PF_ARROW_FUNC) | PF_POSTFIX_CALL)?;

                if !self.got_lf && (self.token.val == TOK_DEC || self.token.val == TOK_INC) {
                    let mut opcode: i32 = 0;
                    let mut scope: i32 = 0;
                    let mut label: i32 = 0;
                    let mut name = JS_ATOM_NULL;
                    op = self.token.val;

                    self.get_lvalue(
                        &mut opcode,
                        &mut scope,
                        &mut name,
                        &mut label,
                        std::ptr::null_mut(),
                        true,
                        op,
                    )?;

                    emit_op(
                        self,
                        if self.token.val == TOK_DEC {
                            OpCode::POST_DEC
                        } else {
                            OpCode::POST_INC
                        },
                    ); //  emit_op_u8(s, (OpCode::POST_DEC as i32 + op - TOK_DEC) as u8);

                    self.put_lvalue(
                        opcode,
                        scope,
                        name,
                        label,
                        PutLValueEnum::PUT_LVALUE_KEEP_SECOND,
                        false,
                    );

                    self.next_token()?;
                }
            }
        }

        if parse_flags & (PF_POW_ALLOWED | PF_POW_FORBIDDEN) != 0 {
            if self.token.val == TOK_POW {
                /* Strict ES7 exponentiation syntax rules: To solve
                conficting semantics between different implementations
                regarding the precedence of prefix operators and the
                postfix exponential, ES7 specifies that -2**2 is a
                syntax error. */
                if parse_flags & PF_POW_FORBIDDEN != 0 {
                    JS_ThrowSyntaxError(
                    self.ctx,
                    c"unparenthesized unary expression can't appear on the left-hand side of '**'",
                );
                    return Err(-1);
                }

                self.next_token()?;
                self.parse_unary(PF_POW_ALLOWED)?;
                emit_op(self, OpCode::POW);
            }
        }

        Ok(())
    }
}
