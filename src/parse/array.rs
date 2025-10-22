use crate::{
    atom::{Atom, consts::JS_ATOM_length},
    bytecode::OpCode,
    parse::token::CHAR_COMMA,
};

use super::{state::JSParseState, token::TOK_ELLIPSIS};

// -1: error, 0: success
pub fn js_parse_array_literal(s: &mut JSParseState) -> i32 {
    macro_rules! done {
        () => {
            return s.parse_expect(']' as i32).map_or(-1, |_| 0);
        };
    }

    if s.next_token().is_err() {
        return -1;
    }

    let mut idx = 0_u32;

    // small regular arrays are created on the stack
    while s.token.val != ']' as i32 && idx < 32 {
        if matches!(s.token.val, CHAR_COMMA | TOK_ELLIPSIS) {
            break;
        }
        if s.parse_assign_expr().is_err() {
            return -1;
        }
        idx += 1;
        // accept trailing comma
        if s.token.val == ',' as i32 {
            if s.next_token().is_err() {
                return -1;
            }
        } else if s.token.val != ']' as i32 {
            done!();
        }
    }

    s.emit_op(OpCode::ARRAY_FROM);
    s.emit_u16(idx as _);

    // larger arrays and holes are handled with explicit indices
    let mut need_length = false;
    while s.token.val != ']' as i32 && idx < 0x7fffffff {
        if s.token.val == TOK_ELLIPSIS {
            break;
        }
        need_length = true;
        if s.token.val != ',' as i32 {
            if s.parse_assign_expr().is_err() {
                return -1;
            }
            s.emit_op(OpCode::DEFINE_FIELD);
            s.emit_atom(Atom::new_small_int(idx));
            need_length = false;
        }
        idx += 1;

        // accept trailing comma
        if s.token.val == ',' as i32 && s.next_token().is_err() {
            return -1;
        }
    }

    if s.token.val == ']' as i32 {
        if need_length {
            // Set the length: Cannot use OP_define_field because
            // length is not configurable
            s.emit_op(OpCode::DUP);
            s.emit_op(OpCode::PUSH_I32);
            s.emit_u32(idx);
            s.emit_op(OpCode::PUT_FIELD);
            s.emit_atom(Atom::new_const(JS_ATOM_length));
        }
        done!();
    }

    // huge arrays and spread elements require a dynamic index on the stack
    s.emit_op(OpCode::PUSH_I32);
    s.emit_u32(idx);

    // stack has array, index
    while s.token.val != ']' as i32 {
        if s.token.val == TOK_ELLIPSIS {
            if s.next_token().is_err() {
                return -1;
            }
            if s.parse_assign_expr().is_err() {
                return -1;
            }

            s.emit_op(OpCode::APPEND);

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
            if s.token.val != ',' as i32 {
                if s.parse_assign_expr().is_err() {
                    return -1;
                }
                // a idx val
                s.emit_op(OpCode::DEFINE_ARRAY_EL);
                need_length = false;
            }
            s.emit_op(OpCode::INC);
        }
        if s.token.val != ',' as i32 {
            break;
        }
        if s.next_token().is_err() {
            return -1;
        }
    }

    if need_length {
        // Set the length: cannot use OP_define_field because
        // length is not configurable
        s.emit_op(OpCode::DUP1); // array length - array array length
        s.emit_op(OpCode::PUT_FIELD);
        s.emit_atom(Atom::new_const(JS_ATOM_length));
    } else {
        s.emit_op(OpCode::DROP); // array length - array
    }

    done!();
}
