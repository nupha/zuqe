// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{atom::Atom, bytecode::OpCode, parse::tokenize::TokenKind};

use super::state::JSParseState;

impl<'s> JSParseState<'s> {
    pub(super) fn js_parse_array_literal(&mut self) -> Result<(), i32> {
        macro_rules! done {
            () => {
                return self.parse_expect(TokenKind::Char(']')).map(|_| ());
            };
        }

        self.next_token()?;

        let mut idx = 0_u32;

        // small regular arrays are created on the stack
        while self.token.kind != TokenKind::Char(']') && idx < 32 {
            if matches!(self.token.kind, TokenKind::Char(',') | TokenKind::ELLIPSIS) {
                break;
            }
            self.parse_assign_expr()?;

            idx += 1;
            // accept trailing comma
            if self.token.kind == TokenKind::Char(',') {
                self.next_token()?;
            } else if self.token.kind != TokenKind::Char(']') {
                done!();
            }
        }

        self.emit_op(OpCode::ARRAY_FROM);
        self.emit_u16(idx as _);

        // larger arrays and holes are handled with explicit indices
        let mut need_length = false;
        while self.token.kind != TokenKind::Char(']') && idx < 0x7fffffff {
            if self.token.kind == TokenKind::ELLIPSIS {
                break;
            }
            need_length = true;
            if self.token.kind != TokenKind::Char(',') {
                self.parse_assign_expr()?;
                self.emit_op(OpCode::DEFINE_FIELD);
                self.emit_atom(Atom::new_small_int(idx));
                need_length = false;
            }
            idx += 1;

            // accept trailing comma
            // if self.token.kind == TokenKind::Char(',') && self.next_token().is_err() {
            //     return Err(-1);
            // }
            if self.token.kind == TokenKind::Char(',') {
                self.next_token()?;
            }
        }

        if self.token.kind == TokenKind::Char(']') {
            if need_length {
                // Set the length: Cannot use OP_define_field because
                // length is not configurable
                self.emit_op(OpCode::DUP);
                self.emit_op(OpCode::PUSH_I32);
                self.emit_u32(idx);
                self.emit_put_field(Atom::CONST_length);
            }
            done!();
        }

        // huge arrays and spread elements require a dynamic index on the stack
        self.emit_op(OpCode::PUSH_I32);
        self.emit_u32(idx);

        // stack has array, index
        while self.token.kind != TokenKind::Char(']') {
            if self.token.kind == TokenKind::ELLIPSIS {
                self.next_token()?;
                self.parse_assign_expr()?;
                self.emit_op(OpCode::APPEND);

                // debug?
                // {
                //     let label_next;
                //     let label_done;
                //     label_next = new_label(s);
                //     label_done = new_label(s);
                //     // enumerate object
                //     emit_op(s, OpCode::FOR_OF_START);
                //     emit_op(s, OP_rot5l);
                //     emit_op(s, OP_rot5l);
                //     emit_label(s, label_next);
                //     // on stack: enum_rec array idx
                //     emit_op(s, OP_for_of_next);
                //     emit_u8(s, 2);
                //     emit_goto(s, OP_if_true, label_done);
                //     // append element
                //     // enum_rec array idx val -> enum_rec array new_idx
                //     emit_op(s, OP_define_array_el);
                //     emit_op(s, OP_inc);
                //     emit_goto(s, OP_goto, label_next);
                //     emit_label(s, label_done);
                //     // close enumeration
                //     emit_op(s, OP_drop); // drop undef val
                //     emit_op(s, OP_nip1); // drop enum_rec
                //     emit_op(s, OP_nip1);
                //     emit_op(s, OP_nip1);
                // }
            } else {
                need_length = true;
                if self.token.kind != TokenKind::Char(',') {
                    self.parse_assign_expr()?;
                    // a idx val
                    self.emit_op(OpCode::DEFINE_ARRAY_EL);
                    need_length = false;
                }
                self.emit_op(OpCode::INC);
            }
            if self.token.kind != TokenKind::Char(',') {
                break;
            }

            self.next_token()?;
        }

        if need_length {
            // Set the length: cannot use OP_define_field because
            // length is not configurable
            self.emit_op(OpCode::DUP1); // array length - array array length
            self.emit_put_field(Atom::CONST_length);
        } else {
            self.emit_op(OpCode::DROP); // array length - array
        }

        done!();
    }
}
