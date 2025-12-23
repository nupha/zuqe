use crate::bytecode::OpCode;

use super::{
    state::{JSParseState, PF_ARROW_FUNC, PF_IN_ACCEPTED},
    tokenize::TokenKind,
};

impl<'s> JSParseState<'s> {
    /// cond_expr? a : b
    /// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
    pub(super) fn parse_cond_expr(&mut self, parse_flags: i32) -> Result<(), i32> {
        self.parse_coalesce_expr(parse_flags)?;

        if self.token.kind == TokenKind::Char('?') {
            self.next_token()?;

            self.with_cur_func_mut(|s, fd| -> Result<(), i32> {
                let label1 = fd.emit_goto(OpCode::IF_FALSE, -1, s.last_line_num);

                s.parse_assign_expr()?;
                s.parse_expect(TokenKind::Char(':'))?;

                let label2 = fd.emit_goto(OpCode::GOTO, -1, s.last_line_num);
                fd.emit_label(label1, s.last_line_num);

                s.parse_assign_expr2(parse_flags & PF_IN_ACCEPTED)?;

                fd.emit_label(label2, s.last_line_num);
                Ok(())
            })
            .unwrap()?;
        }

        Ok(())
    }

    fn parse_coalesce_expr(&mut self, parse_flags: i32) -> Result<(), i32> {
        self.parse_logical_and_or(TokenKind::LOR, parse_flags)?;

        if self.token.kind == TokenKind::DOUBLE_QUESTION_MARK {
            self.with_cur_func_mut(|s, fd| -> Result<(), i32> {
                let label1 = fd.add_label()? as i32;

                loop {
                    s.next_token()?;

                    let last_line_num = s.last_line_num;
                    fd.emit_op(OpCode::DUP, last_line_num);
                    fd.emit_op(OpCode::IS_UNDEFINED_OR_NULL, last_line_num);
                    fd.emit_goto(OpCode::IF_FALSE, label1, last_line_num);
                    fd.emit_op(OpCode::DROP, last_line_num);

                    s.parse_expr_binary(8, parse_flags & !PF_ARROW_FUNC)?;

                    if s.token.kind != TokenKind::DOUBLE_QUESTION_MARK {
                        break;
                    }
                }

                fd.emit_label(label1, s.last_line_num);
                Ok(())
            })
            .unwrap()?;
        }

        Ok(())
    }

    // allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
    fn parse_logical_and_or(&mut self, op: TokenKind, parse_flags: i32) -> Result<(), i32> {
        if op == TokenKind::LAND {
            self.parse_expr_binary(8, parse_flags)?;
        } else {
            self.parse_logical_and_or(TokenKind::LAND, parse_flags)?;
        }

        if self.token.kind == op {
            let label1 = self.cur_func_mut().add_label()? as i32;

            loop {
                self.next_token()?;

                self.emit_op(OpCode::DUP);
                self.emit_goto(
                    if op == TokenKind::LAND {
                        OpCode::IF_FALSE
                    } else {
                        OpCode::IF_TRUE
                    },
                    label1,
                );
                self.emit_op(OpCode::DROP);

                if op == TokenKind::LAND {
                    self.parse_expr_binary(8, parse_flags & !PF_ARROW_FUNC)?;
                } else {
                    self.parse_logical_and_or(TokenKind::LAND, parse_flags & !PF_ARROW_FUNC)?;
                }

                if self.token.kind != op {
                    if self.token.kind == TokenKind::DOUBLE_QUESTION_MARK {
                        self.throw_syntax_error("cannot mix ?? with && or ||");
                        return Err(-1);
                    }
                    break;
                }
            }

            self.emit_label(label1);
        }

        Ok(())
    }
}
