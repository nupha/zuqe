use crate::{
    bytecode::{opcode_info_intermediate, opcodes},
    context::{JS_MODE_STRIP, JSContext},
    function::{
        BlockEnv, JSFunctionDef, PC2LINE_BASE, PC2LINE_DIFF_PC_MAX, PC2LINE_OP_FIRST, PC2LINE_RANGE,
    },
    utils::{push_vec_leb128, push_vec_sleb128, push_vec_u8, read_from_ptr},
};

#[derive(Debug)]
pub struct RelocEntry {
    pub addr: usize, /* address to patch */
    pub size: i32,   /* address size: 1, 2 or 4 bytes */
}

#[derive(Debug)]
pub struct Label {
    pub ref_count: i32,

    /// phase 1 address, -1 means not resolved yet
    pub pos: i32,
    /// phase 2 address, -1 means not resolved yet
    pub pos2: i32,
    /// phase 3 address, -1 means not resolved yet
    pub addr: i32,

    /// pending relocations
    pub pending_relocs: Vec<RelocEntry>,
}

impl Label {
    pub fn add_reloc(&mut self, ctx: &JSContext, addr: usize, size: i32) -> Result<(), i32> {
        if self.pending_relocs.len() == self.pending_relocs.capacity() {
            if self.pending_relocs.try_reserve_exact(4).is_err() {
                ctx.throw_out_of_memory();
                return Err(-1);
            }
        }

        self.pending_relocs.push(RelocEntry { addr, size });
        Ok(())
    }
}

impl JSFunctionDef {
    /// Create a new label in function def. returns new label id.
    pub fn add_label(&mut self) -> Result<u32, i32> {
        if self.label_slots.len() == self.label_slots.capacity() {
            if self.label_slots.try_reserve_exact(4).is_err() {
                self.ctx.throw_out_of_memory();
                return Err(-1);
            }
        }

        self.label_slots.push(Label {
            ref_count: 0,
            pos: -1,
            pos2: -1,
            addr: -1,
            pending_relocs: Vec::new(),
        });

        Ok(self.label_slots.len() as u32 - 1)
    }

    /// check if a specified `label` exists in bytecode, since `pos`.
    pub(super) fn code_has_label(&self, mut pos: usize, label: u32) -> bool {
        let bc_buf = self.byte_code.as_ptr();
        let bc_len = self.byte_code.len();

        while pos < bc_len {
            match unsafe { *bc_buf.add(pos) } {
                opcodes::op_line_num => {
                    pos += 5;
                    continue;
                }
                opcodes::op_label => {
                    let lab = unsafe { read_from_ptr::<u32>(bc_buf.add(pos + 1)) };
                    if lab == label {
                        return true;
                    }
                    pos += 5;
                    continue;
                }
                opcodes::OP_goto => {
                    let lab = unsafe { read_from_ptr::<u32>(bc_buf.add(pos + 1)) };
                    if lab == label {
                        return true;
                    }
                    break;
                }
                _ => {
                    break;
                }
            }
        }

        false
    }

    /// push a break entry on break stack of the function def.
    pub fn push_break_entry(&mut self, be: &mut BlockEnv) {
        be.scope_level = self.scope_level;

        // chain up break entry stack
        be.prev = self.top_break;
        self.top_break = be as *mut _;
    }

    pub fn pop_break_entry(&mut self) {
        let top = self.top_break;
        self.top_break = unsafe { (*top).prev };
    }

    /// return the target label, following the OP_goto jumps.
    /// returns tuple:
    /// 1. label: the label jump to
    /// 1. opcode: the first opcode at destination
    /// 1. line: line number
    pub(crate) fn find_jump_target(&mut self, mut label: u32) -> (u32, u8, i32) {
        self.update_label_ref_count(label as _, -1);

        let mut op: u8 = 0;
        let mut line = -1;

        for _ in 0..10 {
            debug_assert!(label < self.label_slots.len() as u32);

            let mut pos = self.label_slots[label as usize].pos2;

            loop {
                op = self.byte_code[pos as usize];
                match op {
                    opcodes::op_line_num | opcodes::op_label => {
                        if op == opcodes::op_line_num {
                            line = unsafe {
                                read_from_ptr::<i32>(
                                    self.byte_code.as_ptr().offset(pos as isize + 1),
                                )
                            };
                        }
                        pos += opcode_info_intermediate(op).size as i32;
                        continue;
                    }
                    opcodes::OP_goto => {
                        label = unsafe {
                            read_from_ptr::<u32>(self.byte_code.as_ptr().offset((pos + 1) as isize))
                        };
                    }
                    opcodes::OP_drop => {
                        // ignore drop opcodes if followed by OP_return_undef
                        while self.byte_code[(pos + 1) as usize] == opcodes::OP_drop {
                            pos += 1;
                        }
                        if self.byte_code[pos as usize] == opcodes::OP_return_undef {
                            op = opcodes::OP_return_undef;
                        }
                    }
                    _ => {}
                }

                break;
            } // loop
        } // for

        self.update_label_ref_count(label, 1);

        (label, op, line)
    }

    pub fn compute_pc2line_info(&mut self) {
        if self.js_mode & JS_MODE_STRIP as u8 == 0 && !self.line_number_slots.is_empty() {
            let mut last_line_num = self.line_num;
            let mut last_pc: u32 = 0;

            if self.pc2line.is_none() {
                self.pc2line.replace(Vec::with_capacity(64));
            }

            let lst = self.pc2line.as_mut().unwrap();

            for ln in self.line_number_slots.iter() {
                let pc = ln.pc;
                let line_num = ln.line_num;
                if line_num < 0 {
                    continue;
                }

                let diff_pc = pc.wrapping_sub(last_pc) as i32;
                let diff_line = line_num - last_line_num;
                if diff_line == 0 || diff_pc < 0 {
                    continue;
                }

                if diff_line >= PC2LINE_BASE
                    && diff_line < PC2LINE_BASE + PC2LINE_RANGE
                    && diff_pc <= PC2LINE_DIFF_PC_MAX
                {
                    push_vec_u8(
                        lst,
                        ((diff_line - PC2LINE_BASE) + diff_pc * PC2LINE_RANGE + PC2LINE_OP_FIRST)
                            as u8,
                    );
                } else {
                    // longer encoding
                    push_vec_u8(lst, 0);
                    push_vec_leb128(lst, diff_pc as u32);
                    push_vec_sleb128(lst, diff_line);
                }

                last_pc = pc;
                last_line_num = line_num;
            }
        }
    }
}
