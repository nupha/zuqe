use crate::{
    bytecode::{opcode_info_intermediate, opcodes},
    context::{JSContext, JS_MODE_STRIP},
    function::{JSFunctionDef, PC2LINE_BASE, PC2LINE_DIFF_PC_MAX, PC2LINE_OP_FIRST, PC2LINE_RANGE},
    utils::{push_vec_leb128, push_vec_sleb128, push_vec_u8, read_from_ptr},
};

use super::optimize::CodeContext;

#[derive(Debug)]
pub struct RelocEntry {
    pub addr: usize, /* address to patch */
    pub size: i32,   /* address size: 1, 2 or 4 bytes */
}

#[derive(Debug)]
pub struct LabelInfo {
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

impl LabelInfo {
    pub fn add_reloc(&mut self, ctx: &mut JSContext, addr: usize, size: i32) -> Result<(), i32> {
        if self.pending_relocs.len() == self.pending_relocs.capacity() {
            if self.pending_relocs.try_reserve_exact(4).is_err() {
                ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        self.pending_relocs.push(RelocEntry { addr, size });
        Ok(())
    }
}

impl JSFunctionDef {
    /// Create new label in JSFunctionDef, returns new label id.
    pub fn add_label(&mut self) -> Result<u32, i32> {
        if self.labels.len() == self.labels.capacity() {
            if self.labels.try_reserve_exact(4).is_err() {
                self.ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        self.labels.push(LabelInfo {
            ref_count: 0,
            pos: -1,
            pos2: -1,
            addr: -1,
            pending_relocs: Vec::new(),
        });

        Ok(self.labels.len() as u32 - 1)
    }
}

pub unsafe fn code_has_label(s: *mut CodeContext, mut pos: usize, label: u32) -> bool {
    while pos < (*s).bc_len {
        let op = *(*s).bc_buf.offset(pos as isize);
        if op == opcodes::op_line_num {
            pos += 5;
            continue;
        }
        if op == opcodes::op_label {
            let lab = read_from_ptr::<u32>((*s).bc_buf.offset((pos + 1) as isize));
            if lab == label {
                return true;
            }
            pos += 5;
            continue;
        }
        if op == opcodes::OP_goto {
            let lab = read_from_ptr::<u32>((*s).bc_buf.offset((pos + 1) as isize));
            if lab == label {
                return true;
            }
        }
        break;
    }

    false
}

pub unsafe fn compute_pc2line_info(s: *mut JSFunctionDef) {
    if (*s).js_mode & JS_MODE_STRIP as u8 == 0 && !(*s).line_number_slots.is_empty() {
        let mut last_line_num = (*s).line_num;
        let mut last_pc: u32 = 0;

        //js_dbuf_init((*s).ctx, &mut (*s).pc2line);
        if (*s).pc2line.is_none() {
            (*s).pc2line = Some(Vec::with_capacity(64));
        }
        let lst = (*s).pc2line.as_mut().unwrap();

        for ln in (*s).line_number_slots.iter() {
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
                    ((diff_line - PC2LINE_BASE) + diff_pc * PC2LINE_RANGE + PC2LINE_OP_FIRST) as u8,
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

/// return the target label, following the OP_goto jumps
/// the first opcode at destination is stored in *pop
pub unsafe fn find_jump_target(
    s: *mut JSFunctionDef,
    mut label: u32,
    pop: *mut u8,
    pline: *mut i32,
) -> u32 {
    (*s).update_label_ref_count(label as _, -1);

    let mut op: u8 = 0;

    for _ in 0..10 {
        debug_assert!(label < (*s).labels.len() as u32);
        let mut pos = (*s).labels[label as usize].pos2;

        loop {
            op = (*s).byte_code[pos as usize];
            match op {
                opcodes::op_line_num | opcodes::op_label => {
                    if op == opcodes::op_line_num {
                        if !pline.is_null() {
                            *pline = read_from_ptr::<i32>(
                                (*s).byte_code.as_ptr().offset(pos as isize + 1),
                            );
                        }
                    }
                    pos += opcode_info_intermediate(op).size as i32;
                    continue;
                }
                opcodes::OP_goto => {
                    label =
                        read_from_ptr::<u32>((*s).byte_code.as_ptr().offset((pos + 1) as isize));
                }
                opcodes::OP_drop => {
                    // ignore drop opcodes if followed by OP_return_undef
                    while *(*s).byte_code.as_ptr().offset((pos + 1) as isize) == opcodes::OP_drop {
                        pos += 1;
                    }
                    if (*s).byte_code[pos as usize] == opcodes::OP_return_undef {
                        op = opcodes::OP_return_undef;
                    }
                }
                _ => {}
            }

            break;
        } // loop
    } // for

    // done:
    *pop = op;
    (*s).update_label_ref_count(label, 1);
    label
}
