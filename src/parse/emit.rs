// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp},
    function::{FunctionDef, FunctionKind},
    value::JSValue,
};

use super::{JSParseFunctionEnum, class::ClassFieldsParse, state::JSParseState};

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

    /// emit an atom.
    #[inline(always)]
    pub fn emit_atom(&mut self, atom: Atom) {
        self.cur_func_mut().emit_atom(atom);
    }

    /// emit on current function def
    pub fn emit_op_u8(&mut self, op: u8) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_op_u8(op, ln);
    }

    /// don't update the last opcode and don't emit line number info
    /// emit on current function def
    #[inline(always)]
    pub(super) fn emit_label_raw(&mut self, label: i32) {
        self.cur_func_mut().emit_label_raw(label);
    }

    /* return the label ID offset */
    /// emit on current function def
    pub fn emit_label(&mut self, label: i32) -> i32 {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_label(label, ln)
    }

    /// emit on current function def
    pub fn emit_op(&mut self, op: OpCode) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_op(op, ln);
    }

    /// emit on current function def.
    /// return label or -1 if dead code.
    pub fn emit_goto(&mut self, opcode: OpCode, label: i32) -> i32 {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_goto(opcode, label, ln)
    }

    /// emit on current function def
    pub fn emit_push_const(&mut self, val: JSValue, as_atom: bool) -> Result<(), i32> {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_push_const(val, as_atom, ln)
    }

    /// emit on current function def
    pub fn emit_op_temp(&mut self, op: OpCodeTemp) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_op_temp(op, ln);
    }

    /// emit on current function def
    pub fn emit_return(&mut self, hasval: bool) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_return(hasval, ln);
    }

    /// emit on current function def. `label` is the label name, JS_ATOM_NULL for none.
    pub fn emit_break(&mut self, label: Atom, is_continue: bool) -> Result<(), i32> {
        let ln = self.last_line_num;

        self.cur_func_mut()
            .emit_break(label, is_continue, ln)
            .inspect_err(|_| {
                self.throw_syntax_error(if label.is_null() {
                    if is_continue {
                        "continue must be inside loop"
                    } else {
                        "break must be inside loop or switch"
                    }
                } else {
                    "break/continue label not found"
                });
            })
    }

    pub fn emit_spread_code(&mut self, depth: i32) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_spread_code(depth, ln);
    }

    /* initialize the class fields, called by the constructor. Note:
    super() can be called in an arrow function, so <this> and
    <class_fields_init> can be variable references */
    /// emit on current function def
    pub fn emit_class_field_init(&mut self) {
        let ln = self.last_line_num;
        self.cur_func_mut().emit_class_field_init(ln);
    }

    pub(super) fn emit_class_init_start(&mut self, cd: &mut ClassFieldsParse) -> Result<(), i32> {
        debug_assert!(cd.fields_init_fd.is_null());

        cd.fields_init_fd = Box::into_raw(self.make_class_fields_init_fd()?);
        self.cur_func_def = cd.fields_init_fd;

        if !cd.is_static {
            // add the brand to the newly created instance
            // XXX: would be better to add the code only if needed, maybe in a
            // later pass
            self.emit_op(OpCode::PUSH_FALSE); // will be patched later

            #[cfg(feature = "builtin_symbol")]
            {
                cd.brand_push_pos = unsafe { (*cd.fields_init_fd).last_opcode_pos };
            }

            let label_add_brand = self.emit_goto(OpCode::IF_FALSE, -1);

            self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
            self.emit_atom(Atom::CONST_this);
            self.emit_u16(0);

            self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
            self.emit_atom(Atom::CONST__home_object_);
            self.emit_u16(0);

            self.emit_op(OpCode::ADD_BRAND);
            self.emit_label(label_add_brand);
        }

        self.cur_func_def = self.cur_func().parent;

        Ok(())
    }

    pub(super) fn emit_class_init_end(&mut self, cd: &ClassFieldsParse) {
        self.cur_func_def = cd.fields_init_fd;
        self.emit_op(OpCode::RETURN_UNDEF);

        self.cur_func_def = self.cur_func().parent;

        let cpool_idx = self
            .cur_func_mut()
            .cpool_add(JSValue::NULL)
            .map_or(-1, |x| x as i32);

        unsafe {
            (*cd.fields_init_fd).parent_cpool_idx = cpool_idx;
        }
        self.emit_op(OpCode::FCLOSURE);
        self.emit_u32(cpool_idx as _);

        self.emit_op(OpCode::SET_HOME_OBJECT);
    }

    /// create a function def for class fields initialization
    fn make_class_fields_init_fd(&mut self) -> Result<Box<FunctionDef>, i32> {
        let mut fd = FunctionDef::new(
            self.ctx().ctx_ptr,
            Some(self.cur_func_mut()),
            false,
            false,
            self.filename,
            0,
        )?;

        fd.func_type = JSParseFunctionEnum::JS_PARSE_FUNC_METHOD;
        fd.has_home_object = true; // <-- this line must be prior to `fd.attrs.super_allowed = XXX`
        fd.has_arguments_binding = false;
        fd.has_this_binding = true;

        fd.attrs.func_kind = FunctionKind::Normal;
        fd.attrs.super_allowed = fd.has_home_object;
        fd.attrs.new_target_allowed = true;

        Ok(fd)
    }
}
