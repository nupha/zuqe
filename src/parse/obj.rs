use crate::{
    atom::{JS_FreeAtom, JSAtom},
    bytecode::{OpCode, opcodes},
    utils::{read_from_ptr, write_to_ptr},
};

use super::{emit::get_prev_opcode, state::JSParseState};

impl<'s> JSParseState<'s> {
    pub fn set_object_name(&mut self, name: JSAtom) {
        let ctx = self.ctx.clone();
        let fd = self.cur_func_mut();

        match get_prev_opcode(fd) {
            opcodes::OP_set_name => {
                // XXX: should free atom after OP_set_name?
                fd.byte_code.truncate(fd.last_opcode_pos as usize); //   (*fd).byte_code.size = (*fd).last_opcode_pos;
                fd.last_opcode_pos = -1;
                self.emit_op(OpCode::SET_NAME);
                self.emit_atom_dup(name);
            }
            opcodes::op_set_class_name => {
                let define_class_pos = fd.last_opcode_pos + 1
                    - unsafe {
                        read_from_ptr::<i32>(
                            fd.byte_code
                                .as_ptr()
                                .offset(fd.last_opcode_pos as isize + 1),
                        )
                    };

                debug_assert_eq!(
                    unsafe { *fd.byte_code.as_ptr().offset(define_class_pos as isize) },
                    opcodes::OP_define_class
                );

                // for consistency we free the previous atom which is JS_ATOM_empty_string
                let atom = JSAtom(unsafe {
                    read_from_ptr::<u32>(
                        fd.byte_code.as_ptr().offset(define_class_pos as isize + 1),
                    )
                });
                JS_FreeAtom(&ctx, atom);
                unsafe {
                    write_to_ptr::<u32>(
                        fd.byte_code
                            .as_mut_ptr()
                            .offset(define_class_pos as isize + 1),
                        name.dup(ctx.rt()).0,
                    );
                }
                fd.last_opcode_pos = -1;
            }
            _ => {}
        }
    }
}

pub unsafe fn set_object_name_computed(s: *mut JSParseState) {
    let fd = (*s).cur_func_mut();
    let opcode = get_prev_opcode(fd);

    if opcode == opcodes::OP_set_name {
        // XXX: should free atom after OP_set_name?
        fd.byte_code.truncate(fd.last_opcode_pos as usize); //  (*fd).byte_code.size = (*fd).last_opcode_pos;
        fd.last_opcode_pos = -1;
        (*s).emit_op(OpCode::SET_NAME_COMPUTED);
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
