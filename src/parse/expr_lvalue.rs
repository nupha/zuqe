use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp, opcodes},
    context::EvalMode,
    utils::read_from_ptr,
};

use super::{
    emit::get_prev_opcode,
    state::{JSParseState, PF_POSTFIX_CALL},
    token::{TOK_DEC, TOK_FOR, TOK_INC},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PutLValueEnum {
    /// [depth] v ->
    PUT_LVALUE_NOKEEP,
    /// [depth] v -> , keep depth (currently just disable optimizations)
    PUT_LVALUE_NOKEEP_DEPTH,
    /// [depth] v -> v
    PUT_LVALUE_KEEP_TOP,
    /// [depth] v0 v -> v0
    PUT_LVALUE_KEEP_SECOND,
    /// v [depth] ->
    PUT_LVALUE_NOKEEP_BOTTOM,
}

impl<'s> JSParseState<'s> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn get_lvalue(
        &mut self,
        popcode: &mut i32,
        pscope: &mut i32,
        pname: &mut Atom,
        plabel: &mut i32,
        pdepth: *mut i32,
        keep: bool,
        tok: i32,
    ) -> Result<(), i32> {
        macro_rules! throw_error {
            () => {
                self.throw_syntax_error(match tok {
                    TOK_FOR => "invalid for in/of left hand-side",
                    TOK_INC | TOK_DEC => "invalid increment/decrement operand",
                    // 91='[', 123='{'
                    91 | 123 => "invalid destructuring target",
                    _ => "invalid assignment left-hand side",
                });
            };
        }

        let mut scope = 0;
        let mut label = -1;
        let mut name = Atom::NULL; // restore atom in bytecode, take ownership
        let depth: i32;

        // we check the last opcode to get the lvalue type
        let mut opcode = get_prev_opcode(self.cur_func());

        match opcode {
            opcodes::op_scope_get_var => {
                name = unsafe {
                    Atom::from_id(read_from_ptr::<u32>(
                        self.cur_func()
                            .byte_code
                            .as_ptr()
                            .offset(self.cur_func().last_opcode_pos as isize + 1),
                    ))
                };

                let last_opcode_pos = self.cur_func().last_opcode_pos;
                scope = unsafe {
                    read_from_ptr::<u16>(
                        self.cur_func()
                            .byte_code
                            .as_ptr()
                            .offset(last_opcode_pos as isize + 5),
                    )
                };

                match name {
                    Atom::CONST_arguments | Atom::CONST_eval
                        if self.cur_func().attrs.js_mode.contains(EvalMode::STRICT) =>
                    {
                        self.throw_syntax_error("invalid lvalue in strict mode");
                        return Err(-1);
                    }
                    Atom::CONST_this | Atom::CONST_new_target => {
                        throw_error!();
                        return Err(-1);
                    }
                    _ => {
                        depth = 2; // will generate OP_get_ref_value
                    }
                }
            }
            opcodes::OP_get_field => {
                let last_opcode_pos = self.cur_func().last_opcode_pos;
                name = unsafe {
                    Atom::from_id(read_from_ptr::<u32>(
                        self.cur_func()
                            .byte_code
                            .as_ptr()
                            .offset(last_opcode_pos as isize + 1),
                    ))
                };
                depth = 1;
            }
            opcodes::op_scope_get_private_field => {
                let last_opcode_pos = self.cur_func().last_opcode_pos;
                name = unsafe {
                    Atom::from_id(read_from_ptr::<u32>(
                        self.cur_func()
                            .byte_code
                            .as_ptr()
                            .offset(last_opcode_pos as isize + 1),
                    ))
                };
                scope = unsafe {
                    read_from_ptr::<u16>(
                        self.cur_func()
                            .byte_code
                            .as_ptr()
                            .offset(last_opcode_pos as isize + 5),
                    )
                };
                depth = 1;
            }
            opcodes::OP_get_array_el => {
                depth = 2;
            }
            opcodes::OP_get_super_value => {
                depth = 3;
            }
            _ => {
                throw_error!();
                return Err(-1);
            }
        }

        // remove the last opcode
        let last_opcode_pos = self.cur_func().last_opcode_pos;
        debug_assert!(last_opcode_pos > 0);
        self.cur_func_mut().byte_code.truncate(last_opcode_pos as _);
        self.cur_func_mut().last_opcode_pos = -1;

        if keep {
            // get the value but keep the object/fields on the stack
            match opcode {
                opcodes::op_scope_get_var => {
                    label = self
                        .cur_func_mut()
                        .add_label()
                        .expect("add_label() can't fail") as i32;

                    self.emit_op_temp(OpCodeTemp::SCOPE_MAKE_REF);
                    self.emit_atom(name);
                    self.emit_u32(label as _);
                    self.emit_u16(scope);
                    self.cur_func_mut().update_label_ref_count(label as _, 1);
                    self.emit_op(OpCode::GET_REF_VALUE);

                    opcode = OpCode::GET_REF_VALUE as _;
                }
                opcodes::OP_get_field => {
                    self.emit_op(OpCode::GET_FIELD2);
                    self.emit_atom(name);
                }
                opcodes::op_scope_get_private_field => {
                    self.emit_op_temp(OpCodeTemp::SCOPE_GET_PRIVATE_FIELD2);
                    self.emit_atom(name);
                    self.emit_u16(scope);
                }
                opcodes::OP_get_array_el => {
                    // XXX: replace by a single opcode ?
                    self.emit_op(OpCode::TO_PROPKEY2);
                    self.emit_op(OpCode::DUP2);
                    self.emit_op(OpCode::GET_ARRAY_EL);
                }
                opcodes::OP_get_super_value => {
                    self.emit_op(OpCode::TO_PROPKEY);
                    self.emit_op(OpCode::DUP3);
                    self.emit_op(OpCode::GET_SUPER_VALUE);
                }
                _ => {
                    unreachable!();
                }
            }
        } else {
            match opcode {
                opcodes::op_scope_get_var => {
                    label = self
                        .cur_func_mut()
                        .add_label()
                        .expect("add_label() can't fail") as i32;

                    self.emit_op_temp(OpCodeTemp::SCOPE_MAKE_REF);
                    self.emit_atom(name);
                    self.emit_u32(label as _);
                    self.emit_u16(scope);
                    self.cur_func_mut().update_label_ref_count(label as _, 1);

                    opcode = OpCode::GET_REF_VALUE as _;
                }
                opcodes::OP_get_array_el => {
                    self.emit_op(OpCode::TO_PROPKEY2);
                }
                opcodes::OP_get_super_value => {
                    self.emit_op(OpCode::TO_PROPKEY);
                }
                _ => {}
            }
        }

        *popcode = opcode as i32;
        *pscope = scope as _;
        *plabel = label;
        *pname = name;

        unsafe {
            if !pdepth.is_null() {
                *pdepth = depth;
            }
        }

        Ok(())
    }

    /// name has a live reference. 'is_let' is only used with opcode =
    /// op_scope_get_var which is never generated by get_lvalue().
    pub(super) fn put_lvalue(
        &mut self,
        opcode: i32,
        scope: i32,
        name: Atom,
        label: i32,
        special: PutLValueEnum,
        is_let: bool,
    ) {
        match opcode as u8 {
            opcodes::OP_get_field | opcodes::op_scope_get_private_field => {
                // depth = 1
                match special {
                    PutLValueEnum::PUT_LVALUE_NOKEEP | PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH => {}
                    PutLValueEnum::PUT_LVALUE_KEEP_TOP => {
                        self.emit_op(OpCode::INSERT2); // obj v -> v obj v
                    }
                    PutLValueEnum::PUT_LVALUE_KEEP_SECOND => {
                        self.emit_op(OpCode::PERM3); // obj v0 v -> v0 obj v
                    }
                    PutLValueEnum::PUT_LVALUE_NOKEEP_BOTTOM => {
                        self.emit_op(OpCode::SWAP);
                    }
                }
            }
            opcodes::OP_get_array_el | opcodes::OP_get_ref_value => {
                // depth = 2
                if opcode == opcodes::OP_get_ref_value as i32 {
                    self.emit_label(label);
                }
                match special {
                    PutLValueEnum::PUT_LVALUE_NOKEEP => {
                        self.emit_op(OpCode::NOP); // will trigger optimization
                    }
                    PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH => {}
                    PutLValueEnum::PUT_LVALUE_KEEP_TOP => {
                        self.emit_op(OpCode::INSERT3); // obj prop v -> v obj prop v
                    }
                    PutLValueEnum::PUT_LVALUE_KEEP_SECOND => {
                        self.emit_op(OpCode::PERM4); // obj prop v0 v -> v0 obj prop v
                    }
                    PutLValueEnum::PUT_LVALUE_NOKEEP_BOTTOM => {
                        self.emit_op(OpCode::ROT3L);
                    }
                }
            }
            opcodes::OP_get_super_value => {
                // depth = 3
                match special {
                    PutLValueEnum::PUT_LVALUE_NOKEEP | PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH => {}
                    PutLValueEnum::PUT_LVALUE_KEEP_TOP => {
                        self.emit_op(OpCode::INSERT4); // this obj prop v -> v this obj prop v
                    }
                    PutLValueEnum::PUT_LVALUE_KEEP_SECOND => {
                        self.emit_op(OpCode::PERM5); // this obj prop v0 v -> v0 this obj prop v
                    }
                    PutLValueEnum::PUT_LVALUE_NOKEEP_BOTTOM => {
                        self.emit_op(OpCode::ROT4L);
                    }
                }
            }
            _ => {}
        }

        match opcode as u8 {
            opcodes::op_scope_get_var => {
                // val --
                assert!(
                    special == PutLValueEnum::PUT_LVALUE_NOKEEP
                        || special == PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH
                );
                self.emit_op_temp(if is_let {
                    OpCodeTemp::SCOPE_PUT_VAR_INIT
                } else {
                    OpCodeTemp::SCOPE_PUT_VAR
                });
                self.emit_atom(name);
                self.emit_u16(scope as _);
            }
            opcodes::OP_get_field => {
                self.emit_op(OpCode::PUT_FIELD);
                self.emit_atom(name);
            }
            opcodes::op_scope_get_private_field => {
                self.emit_op_temp(OpCodeTemp::SCOPE_PUT_PRIVATE_FIELD);
                self.emit_atom(name);
                self.emit_u16(scope as _);
            }
            opcodes::OP_get_array_el => {
                self.emit_op(OpCode::PUT_ARRAY_EL);
            }
            opcodes::OP_get_ref_value => {
                self.emit_op(OpCode::PUT_REF_VALUE);
            }
            opcodes::OP_get_super_value => {
                self.emit_op(OpCode::PUT_SUPER_VALUE);
            }
            _ => {
                unreachable!();
            }
        }
    }

    pub(super) fn parse_left_hand_side_expr(&mut self) -> Result<(), i32> {
        self.parse_postfix_expr(PF_POSTFIX_CALL)
    }
}
