use std::ops::Deref;

use crate::{
    atom::{JS_FreeAtom, JSAtom},
    bytecode::{OpCode, opcodes},
    utils::{read_from_ptr, write_to_ptr},
};

use super::{
    emit::{emit_atom_dup, emit_op, get_prev_opcode},
    state::JSParseState,
};

pub unsafe fn set_object_name(s: *mut JSParseState, name: JSAtom) {
    let fd = (*s).cur_func_mut();

    match get_prev_opcode(fd) {
        opcodes::OP_set_name => {
            // XXX: should free atom after OP_set_name?
            fd.byte_code.truncate(fd.last_opcode_pos as usize); //   (*fd).byte_code.size = (*fd).last_opcode_pos;
            fd.last_opcode_pos = -1;
            emit_op(s, OpCode::SET_NAME);
            emit_atom_dup(s, name);
        }
        opcodes::op_set_class_name => {
            let define_class_pos = fd.last_opcode_pos + 1
                - read_from_ptr::<i32>(
                    fd.byte_code
                        .as_ptr()
                        .offset(fd.last_opcode_pos as isize + 1),
                );

            assert_eq!(
                *fd.byte_code.as_ptr().offset(define_class_pos as isize),
                opcodes::OP_define_class
            );

            // for consistency we free the previous atom which is JS_ATOM_empty_string
            let atom = JSAtom(read_from_ptr::<u32>(
                fd.byte_code.as_ptr().offset(define_class_pos as isize + 1),
            ));
            JS_FreeAtom(&(*s).ctx, atom);
            write_to_ptr::<u32>(
                fd.byte_code
                    .as_mut_ptr()
                    .offset(define_class_pos as isize + 1),
                name.dup((*s).ctx.rt().deref()).0,
            );
            fd.last_opcode_pos = -1;
        }
        _ => {}
    }
}

pub unsafe fn set_object_name_computed(s: *mut JSParseState) {
    let fd = (*s).cur_func_mut();
    let opcode = get_prev_opcode(fd);

    if opcode == opcodes::OP_set_name {
        // XXX: should free atom after OP_set_name?
        fd.byte_code.truncate(fd.last_opcode_pos as usize); //  (*fd).byte_code.size = (*fd).last_opcode_pos;
        fd.last_opcode_pos = -1;
        emit_op(s, OpCode::SET_NAME_COMPUTED);
    } else if opcode == opcodes::op_set_class_name {
        let define_class_pos = fd.last_opcode_pos + 1
            - read_from_ptr::<i32>(
                fd.byte_code
                    .as_ptr()
                    .offset(fd.last_opcode_pos as isize + 1),
            );

        assert_eq!(
            *fd.byte_code.as_ptr().offset(define_class_pos as isize),
            opcodes::OP_define_class
        );

        *fd.byte_code.as_mut_ptr().offset(define_class_pos as isize) =
            opcodes::OP_define_class_computed;

        fd.last_opcode_pos = -1;
    }
}
