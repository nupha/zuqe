use crate::bytecode::OpCode;

use super::{
    emit::{emit_goto, emit_label, emit_op},
    error::js_parse_error,
    state::{JSParseState, PF_ARROW_FUNC, PF_IN_ACCEPTED},
    token::{CHAR_COLON, TOK_DOUBLE_QUESTION_MARK, TOK_LAND, TOK_LOR},
};

impl JSParseState {
    /// cond_expr? a : b
    /// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
    pub unsafe fn parse_cond_expr(&mut self, parse_flags: i32) -> Result<(), i32> {
        self.parse_coalesce_expr(parse_flags)?;

        if self.token.val == b'?' as i32 {
            self.next_token()?;

            let label1 = emit_goto(self, OpCode::IF_FALSE, -1);

            self.parse_assign_expr()?;
            self.parse_expect(CHAR_COLON)?;

            let label2 = emit_goto(self, OpCode::GOTO, -1);
            emit_label(self, label1);

            self.parse_assign_expr2(parse_flags & PF_IN_ACCEPTED)?;

            emit_label(self, label2);
        }

        Ok(())
    }

    pub unsafe fn parse_coalesce_expr(&mut self, parse_flags: i32) -> Result<(), i32> {
        self.parse_logical_and_or(TOK_LOR, parse_flags)?;

        if self.token.val == TOK_DOUBLE_QUESTION_MARK {
            let label1 = (*self.cur_func).add_label()? as i32;

            loop {
                self.next_token()?;

                emit_op(self, OpCode::DUP);
                emit_op(self, OpCode::IS_UNDEFINED_OR_NULL);
                emit_goto(self, OpCode::IF_FALSE, label1);
                emit_op(self, OpCode::DROP);

                self.parse_expr_binary(8, parse_flags & !PF_ARROW_FUNC)?;

                if self.token.val != TOK_DOUBLE_QUESTION_MARK {
                    break;
                }
            }

            emit_label(self, label1);
        }

        Ok(())
    }

    // allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
    pub unsafe fn parse_logical_and_or(&mut self, op: i32, parse_flags: i32) -> Result<(), i32> {
        if op == TOK_LAND {
            self.parse_expr_binary(8, parse_flags)?;
        } else {
            self.parse_logical_and_or(TOK_LAND, parse_flags)?;
        }

        if self.token.val == op {
            let label1 = (*self.cur_func).add_label()? as i32;

            loop {
                self.next_token()?;

                emit_op(self, OpCode::DUP);
                emit_goto(
                    self,
                    if op == TOK_LAND {
                        OpCode::IF_FALSE
                    } else {
                        OpCode::IF_TRUE
                    },
                    label1,
                );
                emit_op(self, OpCode::DROP);

                if op == TOK_LAND {
                    self.parse_expr_binary(8, parse_flags & !PF_ARROW_FUNC)?;
                } else {
                    self.parse_logical_and_or(TOK_LAND, parse_flags & !PF_ARROW_FUNC)?;
                }

                if self.token.val != op {
                    if self.token.val == TOK_DOUBLE_QUESTION_MARK {
                        js_parse_error(self, c"cannot mix ?? with && or ||");
                        return Err(-1);
                    }
                    break;
                }
            }

            emit_label(self, label1);
        }

        Ok(())
    }
}
