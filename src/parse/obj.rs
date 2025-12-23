use crate::{
    atom::Atom,
    bytecode::{OpCode, opcodes},
    utils::{read_from_ptr, write_to_ptr},
};

use super::state::JSParseState;

impl<'s> JSParseState<'s> {
    /// `name` is not consumed.
    pub fn set_object_name(&mut self, name: Atom) {
        let fd = self.cur_func_mut();

        match fd.get_prev_opcode() {
            opcodes::OP_set_name => {
                // XXX: should free atom after OP_set_name?
                fd.byte_code.truncate(fd.last_opcode_pos as usize); //   (*fd).byte_code.size = (*fd).last_opcode_pos;
                fd.last_opcode_pos = -1;
                self.emit_op(OpCode::SET_NAME);
                self.emit_atom(name);
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

                // for consistency we free the previous atom, which is Atom::CONST_empty_string
                #[cfg(debug_assertions)]
                {
                    let old_atom = Atom::from_id(unsafe {
                        read_from_ptr::<u32>(
                            fd.byte_code.as_ptr().offset(define_class_pos as isize + 1),
                        )
                    });
                    debug_assert_eq!(old_atom, Atom::CONST_empty_string);
                }

                unsafe {
                    write_to_ptr::<u32>(
                        fd.byte_code
                            .as_mut_ptr()
                            .offset(define_class_pos as isize + 1),
                        name.id(),
                    );
                }
                fd.last_opcode_pos = -1;
            }
            _ => {}
        }
    }
}

pub fn set_object_name_computed(s: &mut JSParseState) {
    let fd = s.cur_func_mut();
    let opcode = fd.get_prev_opcode();

    if opcode == opcodes::OP_set_name {
        // XXX: should free atom after OP_set_name?
        fd.byte_code.truncate(fd.last_opcode_pos as usize); //  (*fd).byte_code.size = (*fd).last_opcode_pos;
        fd.last_opcode_pos = -1;
        (*s).emit_op(OpCode::SET_NAME_COMPUTED);
    } else if opcode == opcodes::op_set_class_name {
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

        unsafe {
            *fd.byte_code.as_mut_ptr().offset(define_class_pos as isize) =
                opcodes::OP_define_class_computed;
        }

        fd.last_opcode_pos = -1;
    }
}
