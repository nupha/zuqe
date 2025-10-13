use crate::{
    atom::{
        JS_ATOM_NULL,
        consts::{JS_ATOM_done, JS_ATOM_value},
    },
    bytecode::OpCode,
    error::JS_THROW_ERROR_ITERATOR_THROW,
    function::JSFunctionKindEnum,
    parse::token::{
        CHAR_BRACKET_R, CHAR_COLON, CHAR_COMMA, CHAR_CURLY_R, CHAR_SEMI, CHAR_SQUARE_R, CHAR_STAR,
        TOK_YIELD,
    },
};

use super::JSParseState;

impl<'s> JSParseState<'s> {
    pub(super) fn parse_yield(&mut self, parse_flags: i32) -> Result<(), i32> {
        debug_assert_eq!(self.token.val, TOK_YIELD);

        if !matches!(
            self.cur_func().attrs.func_kind,
            JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            self.throw_syntax_error("unexpected 'yield' keyword");
            return Err(-1);
        } else if !self.cur_func().in_function_body {
            self.throw_syntax_error("yield in default expression");
            return Err(-1);
        }

        self.next_token()?;

        let is_async =
            self.cur_func().attrs.func_kind == JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR;
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
            self.emit_op(OpCode::UNDEFINED);
        }

        if is_star {
            let (label_loop, label_yield) = {
                let fd = self.cur_func_mut();
                (fd.add_label()?, fd.add_label()?)
            };

            self.emit_op(if is_async {
                OpCode::FOR_AWAIT_OF_START
            } else {
                OpCode::FOR_OF_START
            });

            // remove the catch offset (XXX: could avoid pushing back undefined)
            self.emit_op(OpCode::DROP);
            self.emit_op(OpCode::UNDEFINED);

            // initial value
            self.emit_op(OpCode::UNDEFINED);

            self.emit_label(label_loop as _);
            self.emit_op(OpCode::ITERATOR_NEXT);
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_op(OpCode::ITERATOR_CHECK_OBJECT);
            self.emit_op(OpCode::GET_FIELD2);
            self.emit_atom_dup(JS_ATOM_done);
            let label_next = self.emit_goto(OpCode::IF_TRUE, -1); // end of loop
            self.emit_label(label_yield as _);

            if is_async {
                // OP_async_yield_star takes the value as parameter
                self.emit_op(OpCode::GET_FIELD);
                self.emit_atom_dup(JS_ATOM_value);
                self.emit_op(OpCode::ASYNC_YIELD_STAR);
            } else {
                // OP_yield_star takes (value, done) as parameter
                self.emit_op(OpCode::YIELD_STAR);
            }
            self.emit_op(OpCode::DUP);
            let label_return = self.emit_goto(OpCode::IF_TRUE, -1);
            self.emit_op(OpCode::DROP);
            self.emit_goto(OpCode::GOTO, label_loop as _);

            self.emit_label(label_return);
            self.emit_op(OpCode::PUSH_I32);
            self.emit_u32(2);
            self.emit_op(OpCode::STRICT_EQ);

            let label_throw = self.emit_goto(OpCode::IF_TRUE, -1);

            // return handling
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_op(OpCode::ITERATOR_CALL);
            self.emit_u8(0);
            let label_return1 = self.emit_goto(OpCode::IF_TRUE, -1);
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_op(OpCode::ITERATOR_CHECK_OBJECT);
            self.emit_op(OpCode::GET_FIELD2);
            self.emit_atom_dup(JS_ATOM_done);
            self.emit_goto(OpCode::IF_FALSE, label_yield as _);

            self.emit_op(OpCode::GET_FIELD);
            self.emit_atom_dup(JS_ATOM_value);

            self.emit_label(label_return1);
            self.emit_op(OpCode::NIP);
            self.emit_op(OpCode::NIP);
            self.emit_op(OpCode::NIP);
            self.emit_return(true);

            // throw handling
            self.emit_label(label_throw);
            self.emit_op(OpCode::ITERATOR_CALL);
            self.emit_u8(1);
            let label_throw1 = self.emit_goto(OpCode::IF_TRUE, -1);
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_op(OpCode::ITERATOR_CHECK_OBJECT);
            self.emit_op(OpCode::GET_FIELD2);
            self.emit_atom_dup(JS_ATOM_done);
            self.emit_goto(OpCode::IF_FALSE, label_yield as _);
            self.emit_goto(OpCode::GOTO, label_next);
            // close the iterator and throw a type error exception
            self.emit_label(label_throw1);
            self.emit_op(OpCode::ITERATOR_CALL);
            self.emit_u8(2);
            let label_throw2 = self.emit_goto(OpCode::IF_TRUE, -1);
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_label(label_throw2);

            self.emit_op(OpCode::THROW_ERROR);
            self.emit_atom_dup(JS_ATOM_NULL);
            self.emit_u8(JS_THROW_ERROR_ITERATOR_THROW);

            self.emit_label(label_next);
            self.emit_op(OpCode::GET_FIELD);
            self.emit_atom_dup(JS_ATOM_value);
            self.emit_op(OpCode::NIP); // keep the value associated with done = true
            self.emit_op(OpCode::NIP);
            self.emit_op(OpCode::NIP);
        } else {
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_op(OpCode::YIELD);
            let label_next = self.emit_goto(OpCode::IF_FALSE, -1);
            self.emit_return(true);
            self.emit_label(label_next);
        }

        Ok(())
    }
}
