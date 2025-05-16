use crate::{
    atom::{JSAtom, JS_DupAtom, JS_FreeAtom},
    bytecode::{opcodes, OpCode},
    utils::{get_u32, put_u32},
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
            let define_class_pos = (*fd).last_opcode_pos + 1
                - get_u32(
                    fd.byte_code
                        .as_ptr()
                        .offset(fd.last_opcode_pos as isize + 1),
                ) as i32;

            assert_eq!(
                *fd.byte_code.as_ptr().offset(define_class_pos as isize),
                opcodes::OP_define_class as u8
            );

            // for consistency we free the previous atom which is JS_ATOM_empty_string
            let atom = JSAtom(get_u32(
                fd.byte_code.as_ptr().offset(define_class_pos as isize + 1),
            ));
            JS_FreeAtom((*s).ctx.as_ptr(), atom);
            put_u32(
                fd.byte_code
                    .as_mut_ptr()
                    .offset(define_class_pos as isize + 1),
                JS_DupAtom((*s).ctx.as_ptr(), name).0,
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
            - get_u32(
                fd.byte_code
                    .as_ptr()
                    .offset(fd.last_opcode_pos as isize + 1),
            ) as i32;

        assert_eq!(
            *fd.byte_code.as_ptr().offset(define_class_pos as isize),
            opcodes::OP_define_class as u8
        );

        *fd.byte_code.as_mut_ptr().offset(define_class_pos as isize) =
            opcodes::OP_define_class_computed as u8;

        fd.last_opcode_pos = -1;
    }
}
