use crate::{
    bytecode::{OpCode, OpCodeTemp, opcodes},
    function::JSFunctionDef,
};

use super::{
    emit::get_prev_opcode,
    state::JSParseState,
    token::{CHAR_SQUARE_R, TOK_PRIVATE_NAME},
};

impl<'s> JSParseState<'s> {
    pub(super) fn parse_property(
        &mut self,
        fd: &JSFunctionDef, // TODO: remove this arg
        has_optional_chain: bool,
        optional_chaining_label: &mut i32,
    ) -> Result<(), i32> {
        if self.token.val == TOK_PRIVATE_NAME {
            // private class field
            if get_prev_opcode(fd) == opcodes::OP_get_super {
                self.throw_syntax_error("private class field forbidden after super");
                return Err(-1);
            }
            if has_optional_chain {
                self.optional_chain_test(optional_chaining_label, 1);
            }
            self.emit_op_temp(OpCodeTemp::SCOPE_GET_PRIVATE_FIELD);
            self.emit_atom_dup(self.token.atom().unwrap());
            self.emit_u16(self.cur_func().scope_level as _);
        } else {
            if !Self::token_maybe_ident(self.token.val) {
                self.throw_syntax_error("expecting field name");
                return Err(-1);
            }
            if get_prev_opcode(fd) == opcodes::OP_get_super {
                let val = self.token.atom().unwrap().string_or_symbol(&self.ctx)?;
                self.emit_push_const(val, true)?;
                self.emit_op(OpCode::GET_SUPER_VALUE);
            } else {
                if has_optional_chain {
                    self.optional_chain_test(optional_chaining_label, 1);
                }
                self.emit_op(OpCode::GET_FIELD);
                self.emit_atom_dup(self.token.atom().unwrap());
            }
        }

        self.next_token()
    }

    pub(super) fn parse_array_access(
        &mut self,
        fd: &JSFunctionDef, // TODO: remove this arg
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
            self.emit_op(OpCode::GET_SUPER_VALUE);
        } else {
            self.emit_op(OpCode::GET_ARRAY_EL);
        }

        Ok(())
    }
}
