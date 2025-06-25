use std::ops::DerefMut;

use crate::{
    bytecode::{OpCode, OpCodeTemp, opcodes},
    function::JSFunctionDef,
    value::JS_FreeValue,
};

use super::{
    emit::{emit_atom_dup, emit_op, emit_op_temp, emit_push_const, emit_u16, get_prev_opcode},
    state::JSParseState,
    token::{CHAR_SQUARE_R, TOK_PRIVATE_NAME, token_is_ident},
};

impl<'s> JSParseState<'s> {
    pub(super) unsafe fn parse_property(
        &mut self,
        fd: *mut JSFunctionDef,
        has_optional_chain: bool,
        optional_chaining_label: &mut i32,
    ) -> Result<(), i32> {
        if self.token.val == TOK_PRIVATE_NAME {
            /* private class field */
            if get_prev_opcode(fd) == opcodes::OP_get_super {
                self.throw_syntax_error("private class field forbidden after super");
                return Err(-1);
            }
            if has_optional_chain {
                self.optional_chain_test(optional_chaining_label, 1);
            }
            emit_op_temp(self, OpCodeTemp::SCOPE_GET_PRIVATE_FIELD);
            emit_atom_dup(self, self.token.data.ident_atom().unwrap());
            emit_u16(self, self.cur_func().scope_level as _);
        } else {
            if !token_is_ident(self.token.val) {
                self.throw_syntax_error("expecting field name");
                return Err(-1);
            }
            if get_prev_opcode(fd) == opcodes::OP_get_super {
                let val = self
                    .token
                    .data
                    .ident_atom()
                    .unwrap()
                    .to_value(&self.ctx, false);
                let ret = emit_push_const(self, val, true);
                if ret != 0 {
                    return Err(-1);
                }
                emit_op(self, OpCode::GET_SUPER_VALUE);
            } else {
                if has_optional_chain {
                    self.optional_chain_test(optional_chaining_label, 1);
                }
                emit_op(self, OpCode::GET_FIELD);
                emit_atom_dup(self, self.token.data.ident_atom().unwrap());
            }
        }

        self.next_token()
    }

    pub(super) unsafe fn parse_array_access(
        &mut self,
        fd: *mut JSFunctionDef,
        has_optional_chain: bool,
        optional_chaining_label: &mut i32,
    ) -> Result<(), i32> {
        let prev_op = get_prev_opcode(fd);

        if has_optional_chain {
            self.optional_chain_test(optional_chaining_label, 1);
        }

        // NOTE: current token is '['; current buffer starts at next char of '['
        self.next_token()?;
        self.parse_expr()?;
        self.parse_expect(CHAR_SQUARE_R)?;

        if prev_op == opcodes::OP_get_super {
            emit_op(self, OpCode::GET_SUPER_VALUE);
        } else {
            emit_op(self, OpCode::GET_ARRAY_EL);
        }

        Ok(())
    }
}
