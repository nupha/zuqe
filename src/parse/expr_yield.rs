use crate::{
    atom::{
        consts::{JS_ATOM_done, JS_ATOM_value},
        JS_ATOM_NULL,
    },
    bytecode::OpCode,
    error::JS_THROW_ERROR_ITERATOR_THROW,
    function::JSFunctionKindEnum,
    parse::{
        emit::{emit_atom_dup, emit_goto, emit_label, emit_op, emit_return, emit_u32, emit_u8},
        token::{
            CHAR_BRACKET_R, CHAR_COLON, CHAR_COMMA, CHAR_CURLY_R, CHAR_SEMI, CHAR_SQUARE_R,
            CHAR_STAR, TOK_YIELD,
        },
    },
};

use super::JSParseState;

impl<'s> JSParseState<'s> {
    pub(super) unsafe fn parse_yield(&mut self, parse_flags: i32) -> Result<(), i32> {
        debug_assert_eq!(self.token.val, TOK_YIELD);

        if !matches!(
            self.cur_func().func_kind,
            JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            self.throw_syntax_error(c"unexpected 'yield' keyword");
            return Err(-1);
        } else if !self.cur_func().in_function_body {
            self.throw_syntax_error(c"yield in default expression");
            return Err(-1);
        }

        self.next_token()?;

        let is_async = self.cur_func().func_kind == JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR;
        let mut is_star = false;

        // XXX: is there a better method to detect 'yield' without parameters?
        if !self.got_lf
            && !matches!(
                self.token.val,
                CHAR_SEMI | CHAR_BRACKET_R | CHAR_SQUARE_R | CHAR_CURLY_R | CHAR_COMMA | CHAR_COLON
            )
        {
            if self.token.val == CHAR_STAR {
                is_star = true;
                self.next_token()?;
            }
            self.parse_assign_expr2(parse_flags)?;
        } else {
            emit_op(self, OpCode::UNDEFINED);
        }

        if is_star {
            let label_loop = self.cur_func().add_label()?;
            let label_yield = self.cur_func().add_label()?;

            emit_op(
                self,
                if is_async {
                    OpCode::FOR_AWAIT_OF_START
                } else {
                    OpCode::FOR_OF_START
                },
            );

            // remove the catch offset (XXX: could avoid pushing back undefined)
            emit_op(self, OpCode::DROP);
            emit_op(self, OpCode::UNDEFINED);

            // initial value
            emit_op(self, OpCode::UNDEFINED);

            emit_label(self, label_loop as _);
            emit_op(self, OpCode::ITERATOR_NEXT);
            if is_async {
                emit_op(self, OpCode::AWAIT);
            }
            emit_op(self, OpCode::ITERATOR_CHECK_OBJECT);
            emit_op(self, OpCode::GET_FIELD2);
            emit_atom_dup(self, JS_ATOM_done);
            let label_next = emit_goto(self, OpCode::IF_TRUE, -1); // end of loop
            emit_label(self, label_yield as _);

            if is_async {
                // OP_async_yield_star takes the value as parameter
                emit_op(self, OpCode::GET_FIELD);
                emit_atom_dup(self, JS_ATOM_value);
                emit_op(self, OpCode::ASYNC_YIELD_STAR);
            } else {
                // OP_yield_star takes (value, done) as parameter
                emit_op(self, OpCode::YIELD_STAR);
            }
            emit_op(self, OpCode::DUP);
            let label_return = emit_goto(self, OpCode::IF_TRUE, -1);
            emit_op(self, OpCode::DROP);
            emit_goto(self, OpCode::GOTO, label_loop as _);

            emit_label(self, label_return);
            emit_op(self, OpCode::PUSH_I32);
            emit_u32(self, 2);
            emit_op(self, OpCode::STRICT_EQ);

            let label_throw = emit_goto(self, OpCode::IF_TRUE, -1);

            // return handling
            if is_async {
                emit_op(self, OpCode::AWAIT);
            }
            emit_op(self, OpCode::ITERATOR_CALL);
            emit_u8(self, 0);
            let label_return1 = emit_goto(self, OpCode::IF_TRUE, -1);
            if is_async {
                emit_op(self, OpCode::AWAIT);
            }
            emit_op(self, OpCode::ITERATOR_CHECK_OBJECT);
            emit_op(self, OpCode::GET_FIELD2);
            emit_atom_dup(self, JS_ATOM_done);
            emit_goto(self, OpCode::IF_FALSE, label_yield as _);

            emit_op(self, OpCode::GET_FIELD);
            emit_atom_dup(self, JS_ATOM_value);

            emit_label(self, label_return1);
            emit_op(self, OpCode::NIP);
            emit_op(self, OpCode::NIP);
            emit_op(self, OpCode::NIP);
            emit_return(self, true);

            // throw handling
            emit_label(self, label_throw);
            emit_op(self, OpCode::ITERATOR_CALL);
            emit_u8(self, 1);
            let label_throw1 = emit_goto(self, OpCode::IF_TRUE, -1);
            if is_async {
                emit_op(self, OpCode::AWAIT);
            }
            emit_op(self, OpCode::ITERATOR_CHECK_OBJECT);
            emit_op(self, OpCode::GET_FIELD2);
            emit_atom_dup(self, JS_ATOM_done);
            emit_goto(self, OpCode::IF_FALSE, label_yield as _);
            emit_goto(self, OpCode::GOTO, label_next);
            // close the iterator and throw a type error exception
            emit_label(self, label_throw1);
            emit_op(self, OpCode::ITERATOR_CALL);
            emit_u8(self, 2);
            let label_throw2 = emit_goto(self, OpCode::IF_TRUE, -1);
            if is_async {
                emit_op(self, OpCode::AWAIT);
            }
            emit_label(self, label_throw2);

            emit_op(self, OpCode::THROW_ERROR);
            emit_atom_dup(self, JS_ATOM_NULL);
            emit_u8(self, JS_THROW_ERROR_ITERATOR_THROW);

            emit_label(self, label_next);
            emit_op(self, OpCode::GET_FIELD);
            emit_atom_dup(self, JS_ATOM_value);
            emit_op(self, OpCode::NIP); // keep the value associated with done = true
            emit_op(self, OpCode::NIP);
            emit_op(self, OpCode::NIP);
        } else {
            if is_async {
                emit_op(self, OpCode::AWAIT);
            }
            emit_op(self, OpCode::YIELD);
            let label_next = emit_goto(self, OpCode::IF_FALSE, -1);
            emit_return(self, true);
            emit_label(self, label_next);
        }

        Ok(())
    }
}
