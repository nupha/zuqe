use crate::{
    atom::{Atom, AtomTypeHint, JS_ATOM_NULL, JSAtom, consts::*},
    bytecode::{OpCode, OpCodeTemp, opcodes},
    class::ClassFieldsDef,
    function::{JSFunctionDef, JSFunctionKindEnum},
    parse::class::js_parse_function_class_fields_init,
    utils::{push_vec_u8, push_vec_u16, push_vec_u32},
    value::JSValue,
};

use super::state::JSParseState;

impl JSFunctionDef {
    #[inline(always)]
    pub fn emit_u8(&mut self, val: u8) {
        push_vec_u8(&mut self.byte_code, val);
    }

    #[inline(always)]
    pub fn emit_u16(&mut self, val: u16) {
        push_vec_u16(&mut self.byte_code, val);
    }

    #[inline(always)]
    pub fn emit_u32(&mut self, val: u32) {
        push_vec_u32(&mut self.byte_code, val);
    }

    /// emit an atom by given atom ref. the atom ref will be dup'ed inside.
    #[inline]
    pub fn emit_atom_ref<A: Into<JSAtom>>(&mut self, atom: A) {
        let a: JSAtom = atom.into();
        self.emit_u32(if a.is_const() {
            a.0
        } else {
            unsafe { a.dup(self.ctx.rt()).leak() }
        });
    }

    /// emit an atom.
    /// Note: `atom` will be duplicated inside
    #[inline]
    pub fn emit_atom(&mut self, name: Atom) {
        self.emit_u32(if name.is_const() {
            name.id()
        } else {
            unsafe { name.leak() }
        });
    }

    fn emit_op_u8(&mut self, op: u8, last_line_num: i32) {
        debug_assert!(last_line_num >= 0);

        let bc = &mut self.byte_code;

        // Use the line number of the last token used, not the next token,
        // nor the current offset in the source file.
        if self.last_opcode_line_num != last_line_num {
            push_vec_u8(bc, OpCodeTemp::LINE_NUM as u8);
            push_vec_u32(bc, last_line_num as _);
            self.last_opcode_line_num = last_line_num;
        }

        self.last_opcode_pos = bc.len() as i32;
        bc.push(op);
    }

    #[inline(always)]
    pub fn emit_op(&mut self, op: OpCode, last_line_num: i32) {
        self.emit_op_u8(op as u8, last_line_num);
    }
    #[inline(always)]
    pub fn emit_op_temp(&mut self, op: OpCodeTemp, last_line_num: i32) {
        self.emit_op_u8(op as u8, last_line_num);
    }

    pub fn emit_return(&mut self, mut hasval: bool, last_line_num: i32) {
        // execute the finally blocks before return
        if self.attrs.func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL {
            if !hasval {
                // no value: direct return in case of async generator
                self.emit_op(OpCode::UNDEFINED, last_line_num);
                hasval = true;
            } else if self.attrs.func_kind == JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR {
                // the await must be done before handling the "finally" in
                // case it raises an exception
                self.emit_op(OpCode::AWAIT, last_line_num);
            }
        }

        let mut top = self.top_break;
        while !top.is_null() {
            if unsafe { (*top).has_iterator || (*top).label_finally != -1 } {
                if !hasval {
                    self.emit_op(OpCode::UNDEFINED, last_line_num);
                    hasval = true;
                }
                // Remove the stack elements up to and including the catch
                // offset. When 'yield' is used in an expression we have
                // no easy way to count them, so we use this specific
                // instruction instead.
                self.emit_op(OpCode::NIP_CATCH, last_line_num);

                // stack: iter_obj next ret_val
                if unsafe { (&*top).has_iterator } {
                    if self.attrs.func_kind == JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR {
                        self.emit_op(OpCode::NIP, last_line_num); // next
                        self.emit_op(OpCode::SWAP, last_line_num);
                        self.emit_op(OpCode::GET_FIELD2, last_line_num);
                        self.emit_atom_ref(JS_ATOM_return);

                        // stack: iter_obj return_func
                        self.emit_op(OpCode::DUP, last_line_num);
                        self.emit_op(OpCode::IS_UNDEFINED_OR_NULL, last_line_num);

                        let label_next = self.emit_goto(OpCode::IF_TRUE, -1, last_line_num);
                        self.emit_op(OpCode::CALL_METHOD, last_line_num);
                        self.emit_u16(0);
                        self.emit_op(OpCode::ITERATOR_CHECK_OBJECT, last_line_num);
                        self.emit_op(OpCode::AWAIT, last_line_num);

                        let label_next2 = self.emit_goto(OpCode::GOTO, -1, last_line_num);
                        self.emit_label(label_next, last_line_num);
                        self.emit_op(OpCode::DROP, last_line_num);
                        self.emit_label(label_next2, last_line_num);
                        self.emit_op(OpCode::DROP, last_line_num);
                    } else {
                        self.emit_op(OpCode::ROT3R, last_line_num);
                        self.emit_op(OpCode::UNDEFINED, last_line_num); // dummy catch offset
                        self.emit_op(OpCode::ITERATOR_CLOSE, last_line_num);
                    }
                } else {
                    // execute the "finally" block
                    self.emit_goto(
                        OpCode::GOSUB,
                        unsafe { (&*top).label_finally },
                        last_line_num,
                    );
                }
            }

            top = unsafe { (*top).prev };
        }

        if self.attrs.is_derived_class_constructor {
            // 'this' can be uninitialized, so it may be accessed only if
            // the derived class constructor does not return an object
            let label_return = if hasval {
                self.emit_op(OpCode::CHECK_CTOR_RETURN, last_line_num);
                let l = self.emit_goto(OpCode::IF_FALSE, -1, last_line_num);
                self.emit_op(OpCode::DROP, last_line_num);
                l
            } else {
                -1
            };

            // The error should be raised in the caller context, so we use
            // a specific opcode
            self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR_CHECKTHIS, last_line_num);
            self.emit_atom_ref(JS_ATOM_this);
            self.emit_u16(0);

            self.emit_label(label_return, last_line_num);
            self.emit_op(OpCode::RETURN, last_line_num);
        } else if self.attrs.func_kind != JSFunctionKindEnum::JS_FUNC_NORMAL {
            self.emit_op(OpCode::RETURN_ASYNC, last_line_num);
        } else {
            self.emit_op(
                if hasval {
                    OpCode::RETURN
                } else {
                    OpCode::RETURN_UNDEF
                },
                last_line_num,
            );
        }
    }

    /* don't update the last opcode and don't emit line number info */
    fn emit_label_raw(&mut self, label: i32) {
        debug_assert!(label >= 0);
        self.emit_u8(OpCodeTemp::LABEL as u8);
        self.emit_u32(label as _);
        self.label_slots[label as usize].pos = self.byte_code.len() as _;
    }

    /// returns the offset to `label`
    pub fn emit_label(&mut self, label: i32, last_line_num: i32) -> i32 {
        if label >= 0 {
            self.emit_op_temp(OpCodeTemp::LABEL, last_line_num);
            self.emit_u32(label as _);
            self.label_slots[label as usize].pos = self.byte_code.len() as _;
            self.byte_code.len() as i32 - 4
        } else {
            -1
        }
    }

    /// return label or -1 if dead code
    pub fn emit_goto(&mut self, opcode: OpCode, mut label: i32, last_line_num: i32) -> i32 {
        if self.is_live_code() {
            if label < 0 {
                label = self.add_label().unwrap() as i32;
            }

            self.emit_op(opcode, last_line_num);
            self.emit_u32(label as _);
            self.label_slots[label as usize].ref_count += 1;
            label
        } else {
            -1
        }
    }

    pub fn emit_push_const(
        &mut self,
        val: JSValue,
        as_atom: bool,
        last_line_num: i32,
    ) -> Result<(), i32> {
        if let JSValue::String(ss) = &val
            && as_atom
        {
            if let Ok(atom) = Atom::from_string(self.ctx.rt_mut(), ss.clone(), AtomTypeHint::String)
                && !atom.is_small_int()
            {
                self.emit_op(OpCode::PUSH_ATOM_VALUE, last_line_num);
                self.emit_atom(atom);
                return Ok(());
            }
        }

        let cidx = self.cpool_add(val)?;
        self.emit_op(OpCode::PUSH_CONST, last_line_num);
        self.emit_u32(cidx as u32);
        Ok(())
    }

    /* initialize the class fields, called by the constructor. Note:
    super() can be called in an arrow function, so <this> and
    <class_fields_init> can be variable references */
    pub fn emit_class_field_init(&mut self, last_line_num: i32) {
        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR, last_line_num);
        self.emit_atom_ref(JS_ATOM__class_fields_init_);
        self.emit_u16(self.scope_level as _);

        /* no need to call the class field initializer if not defined */
        self.emit_op(OpCode::DUP, last_line_num);
        let label_next = self.emit_goto(OpCode::IF_FALSE, -1, last_line_num);

        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR, last_line_num);
        self.emit_atom_ref(JS_ATOM_this);
        self.emit_u16(0);

        self.emit_op(OpCode::SWAP, last_line_num);

        self.emit_op(OpCode::CALL_METHOD, last_line_num);
        self.emit_u16(0);

        self.emit_label(label_next, last_line_num);
        self.emit_op(OpCode::DROP, last_line_num);
    }

    pub fn emit_spread_code(&mut self, depth: i32, last_line_num: i32) {
        // XXX: could check if enum object is an actual array and optimize
        //       slice extraction. enumeration record and target array are in a
        //       different order from OP_append case.
        // enum_rec xxx -- enum_rec xxx array 0
        self.emit_op(OpCode::ARRAY_FROM, last_line_num);
        self.emit_u16(0);
        self.emit_op(OpCode::PUSH_I32, last_line_num);
        self.emit_u32(0);

        let label_rest_next = self.add_label().map_or(-1, |x| x as i32);
        self.emit_label(label_rest_next, last_line_num);
        self.emit_op(OpCode::FOR_OF_NEXT, last_line_num);
        self.emit_u8((2 + depth) as u8);

        let label_rest_done = self.emit_goto(OpCode::IF_TRUE, -1, last_line_num);
        // array idx val -- array idx
        self.emit_op(OpCode::DEFINE_ARRAY_EL, last_line_num);
        self.emit_op(OpCode::INC, last_line_num);
        self.emit_goto(OpCode::GOTO, label_rest_next as _, last_line_num);
        self.emit_label(label_rest_done, last_line_num);
        // enum_rec xxx array idx undef -- enum_rec xxx array
        self.emit_op(OpCode::DROP, last_line_num);
        self.emit_op(OpCode::DROP, last_line_num);
    }

    // pub unsafe fn emit_break(
    //     &mut self,
    //     name: JSAtom,
    //     is_continue: bool,
    //     last_line_num: u32,
    // ) -> i32 {
    //     let mut scope_level = self.scope_level;
    //     let mut top = self.top_break;

    //     while !top.is_null() {
    //         (*s).close_scopes(scope_level, (*top).scope_level);

    //         scope_level = (*top).scope_level;
    //         if is_continue
    //             && (*top).label_cont != -1
    //             && (name == JS_ATOM_NULL || (*top).label_name == name)
    //         {
    //             // continue stays inside the same block
    //             self.emit_goto(OpCode::GOTO, (*top).label_cont, last_line_num);
    //             return 0;
    //         }
    //         if !is_continue
    //             && (*top).label_break != -1
    //             && (name == JS_ATOM_NULL || (*top).label_name == name)
    //         {
    //             self.emit_goto(OpCode::GOTO, (*top).label_break, last_line_num);
    //             return 0;
    //         }

    //         let i = if (*top).has_iterator {
    //             self.emit_op(OpCode::ITERATOR_CLOSE, last_line_num);
    //             3
    //         } else {
    //             0
    //         };
    //         for _ in i..(*top).drop_count {
    //             self.emit_op(OpCode::DROP, last_line_num);
    //         }

    //         if (*top).label_finally != -1 {
    //             // must push dummy value to keep same stack depth
    //             self.emit_op(OpCode::UNDEFINED, last_line_num);
    //             self.emit_goto(OpCode::GOSUB, (*top).label_finally, last_line_num);
    //             self.emit_op(OpCode::DROP, last_line_num);
    //         }
    //         top = (*top).prev;
    //     }

    //     if name == JS_ATOM_NULL {
    //         if is_continue {
    //             return js_parse_error(s, c"continue must be inside loop");
    //         } else {
    //             return js_parse_error(s, c"break must be inside loop or switch");
    //         }
    //     } else {
    //         return js_parse_error(s, c"break/continue label not found");
    //     }
    // }

    #[inline(always)]
    pub fn get_prev_opcode(&self) -> u8 {
        if self.last_opcode_pos < 0 {
            OpCode::INVALID as _
        } else {
            self.byte_code[self.last_opcode_pos as usize] as _
        }
    }

    pub fn is_live_code(&self) -> bool {
        match self.get_prev_opcode() {
            opcodes::OP_tail_call
            | opcodes::OP_tail_call_method
            | opcodes::OP_return
            | opcodes::OP_return_undef
            | opcodes::OP_return_async
            | opcodes::OP_throw
            | opcodes::OP_throw_error
            | opcodes::OP_goto
            | opcodes::OP_ret => false,

            #[cfg(feature = "short_opcode")]
            opcodes::OpCode::GOTO8 | opcodes::OpCode::GOTO16 => false,

            _ => true,
        }
    }
}

impl<'s> JSParseState<'s> {
    /// emit on current function def
    #[inline(always)]
    pub fn emit_u8(&mut self, val: u8) {
        self.cur_func_mut().emit_u8(val);
    }

    /// emit on current function def
    #[inline(always)]
    pub fn emit_u16(&mut self, val: u16) {
        self.cur_func_mut().emit_u16(val);
    }

    /// emit on current function def
    #[inline(always)]
    pub fn emit_u32(&mut self, val: u32) {
        self.cur_func_mut().emit_u32(val);
    }

    /// emit atom by given atom ref. the atom ref will be dup'ed inside.
    #[inline(always)]
    pub fn emit_atom_ref<A: Into<JSAtom>>(&mut self, atom: A) {
        self.cur_func_mut().emit_atom_ref(atom);
    }

    /// emit an atom.
    #[inline(always)]
    pub fn emit_atom(&mut self, atom: Atom) {
        self.cur_func_mut().emit_atom(atom);
    }

    /// emit on current function def
    #[inline(always)]
    pub fn emit_op_u8(&mut self, op: u8) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_op_u8(op, ln);
    }

    /// emit on current function def
    #[inline(always)]
    pub fn emit_op(&mut self, op: OpCode) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_op(op, ln);
    }

    /// don't update the last opcode and don't emit line number info
    /// emit on current function def
    #[inline(always)]
    pub fn emit_label_raw(&mut self, label: i32) {
        self.cur_func_mut().emit_label_raw(label);
    }

    /* return the label ID offset */
    /// emit on current function def
    #[inline(always)]
    pub fn emit_label(&mut self, label: i32) -> i32 {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_label(label, ln)
    }

    /// emit on current function def.
    /// return label or -1 if dead code.
    #[inline(always)]
    pub fn emit_goto(&mut self, opcode: OpCode, label: i32) -> i32 {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_goto(opcode, label, ln)
    }

    /// emit on current function def
    #[inline(always)]
    pub fn emit_push_const(&mut self, val: JSValue, as_atom: bool) -> Result<(), i32> {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_push_const(val, as_atom, ln)
    }

    /* initialize the class fields, called by the constructor. Note:
    super() can be called in an arrow function, so <this> and
    <class_fields_init> can be variable references */
    /// emit on current function def
    #[inline(always)]
    pub fn emit_class_field_init(&mut self) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_class_field_init(ln);
    }

    /// emit on current function def
    #[inline(always)]
    pub fn emit_op_temp(&mut self, op: OpCodeTemp) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_op_temp(op, ln);
    }

    /// emit on current function def
    #[inline(always)]
    pub fn emit_return(&mut self, hasval: bool) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_return(hasval, ln);
    }

    /// emit on current function def. `name` is the label name, JS_ATOM_NULL for none.
    /// Note: `name` is not consumed.
    pub fn emit_break(&mut self, name: JSAtom, is_continue: bool) -> i32 {
        let (mut scope_level, mut top_break) = {
            let f = self.cur_func();
            (f.scope_level, f.top_break)
        };

        while !top_break.is_null() {
            let top = unsafe { top_break.as_ref().unwrap() };

            self.close_scopes(scope_level, top.scope_level);
            scope_level = top.scope_level;
            if is_continue
                && top.label_cont != -1
                && (name == JS_ATOM_NULL || top.label_name == name)
            {
                // continue stays inside the same block
                self.emit_goto(OpCode::GOTO, top.label_cont);
                return 0;
            }
            if !is_continue
                && top.label_break != -1
                && (name == JS_ATOM_NULL || top.label_name == name)
            {
                self.emit_goto(OpCode::GOTO, top.label_break);
                return 0;
            }

            let i = if top.has_iterator {
                self.emit_op(OpCode::ITERATOR_CLOSE);
                3
            } else {
                0
            };
            for _ in i..top.drop_count {
                self.emit_op(OpCode::DROP);
            }

            if top.label_finally != -1 {
                // must push dummy value to keep same stack depth
                self.emit_op(OpCode::UNDEFINED);
                self.emit_goto(OpCode::GOSUB, top.label_finally);
                self.emit_op(OpCode::DROP);
            }

            top_break = top.prev;
        }

        self.throw_syntax_error(if name == JS_ATOM_NULL {
            if is_continue {
                "continue must be inside loop"
            } else {
                "break must be inside loop or switch"
            }
        } else {
            "break/continue label not found"
        });

        -1
    }

    pub fn emit_spread_code(&mut self, depth: i32) {
        // XXX: could check if enum object is an actual array and optimize
        //       slice extraction. enumeration record and target array are in a
        //       different order from OP_append case.
        // enum_rec xxx -- enum_rec xxx array 0
        self.emit_op(OpCode::ARRAY_FROM);
        self.emit_u16(0);
        self.emit_op(OpCode::PUSH_I32);
        self.emit_u32(0);

        let label_rest_next = self.cur_func_mut().add_label().map_or(-1, |x| x as i32);
        self.emit_label(label_rest_next);
        self.emit_op(OpCode::FOR_OF_NEXT);
        self.emit_u8((2 + depth) as u8); // C2RS: explicit cast from i32 to u8

        let label_rest_done = self.emit_goto(OpCode::IF_TRUE, -1);
        // array idx val -- array idx
        self.emit_op(OpCode::DEFINE_ARRAY_EL);
        self.emit_op(OpCode::INC);
        self.emit_goto(OpCode::GOTO, label_rest_next as _);
        self.emit_label(label_rest_done);
        // enum_rec xxx array idx undef -- enum_rec xxx array
        self.emit_op(OpCode::DROP);
        self.emit_op(OpCode::DROP);
    }

    pub(super) fn emit_class_init_start(&mut self, clsdef: &mut ClassFieldsDef) -> Result<(), i32> {
        debug_assert!(clsdef.fields_init_fd.is_null());

        clsdef.fields_init_fd = Box::into_raw(js_parse_function_class_fields_init(self)?);
        self.cur_func_def = clsdef.fields_init_fd;

        if !clsdef.is_static {
            // add the brand to the newly created instance
            // XXX: would be better to add the code only if needed, maybe in a
            // later pass
            self.emit_op(OpCode::PUSH_FALSE); // will be patched later
            clsdef.brand_push_pos = unsafe { (*clsdef.fields_init_fd).last_opcode_pos };

            let label_add_brand = self.emit_goto(OpCode::IF_FALSE, -1);

            self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
            self.emit_atom_ref(JS_ATOM_this);
            self.emit_u16(0);

            self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
            self.emit_atom_ref(JS_ATOM__home_object_);
            self.emit_u16(0);

            self.emit_op(OpCode::ADD_BRAND);
            self.emit_label(label_add_brand);
        }
        self.cur_func_def = self.cur_func().parent;

        Ok(())
    }

    pub(super) fn emit_class_init_end(&mut self, clsdef: &ClassFieldsDef) {
        self.cur_func_def = clsdef.fields_init_fd;
        self.emit_op(OpCode::RETURN_UNDEF);
        self.cur_func_def = self.cur_func().parent;

        let cpool_idx = self
            .cur_func_mut()
            .cpool_add(JSValue::NULL)
            .map_or(-1, |x| x as i32);

        unsafe {
            (*clsdef.fields_init_fd).parent_cpool_idx = cpool_idx;
        }
        self.emit_op(OpCode::FCLOSURE);
        self.emit_u32(cpool_idx as _);

        self.emit_op(OpCode::SET_HOME_OBJECT);
    }
}

#[inline(always)]
pub fn get_prev_opcode(fd: &JSFunctionDef) -> u8 {
    if fd.last_opcode_pos < 0 {
        OpCode::INVALID as _
    } else {
        fd.byte_code[fd.last_opcode_pos as usize] as _
    }
}

pub fn js_is_live_code(s: &JSParseState) -> bool {
    match get_prev_opcode(s.cur_func()) {
        opcodes::OP_tail_call
        | opcodes::OP_tail_call_method
        | opcodes::OP_return
        | opcodes::OP_return_undef
        | opcodes::OP_return_async
        | opcodes::OP_throw
        | opcodes::OP_throw_error
        | opcodes::OP_goto
        | opcodes::OP_ret => false,

        #[cfg(feature = "short_opcode")]
        opcodes::OpCode::GOTO8 | opcodes::OpCode::GOTO16 => false,

        _ => true,
    }
}
