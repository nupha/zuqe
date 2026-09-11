// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::path::Path;

use once_cell::sync::Lazy;

struct REOpcodeDef {
    name: &'static str,
    size: usize,
}

macro_rules! RE_OPCODE_DEF {
    ($name:expr, $size:expr) => {
        REOpcodeDef {
            name: $name,
            size: $size,
        }
    };
}

static RE_OPCODES: Lazy<Vec<REOpcodeDef>> = Lazy::new(|| {
    let ops = vec![
        RE_OPCODE_DEF!("invalid", 1), /* never used */
        RE_OPCODE_DEF!("char", 3),
        RE_OPCODE_DEF!("char32", 5),
        RE_OPCODE_DEF!("dot", 1),
        RE_OPCODE_DEF!("any", 1), /* same as dot but match any character including line terminator */
        RE_OPCODE_DEF!("line_start", 1),
        RE_OPCODE_DEF!("line_end", 1),
        RE_OPCODE_DEF!("goto", 5),
        RE_OPCODE_DEF!("split_goto_first", 5),
        RE_OPCODE_DEF!("split_next_first", 5),
        RE_OPCODE_DEF!("match", 1),
        RE_OPCODE_DEF!("save_start", 2), /* save start position */
        RE_OPCODE_DEF!("save_end", 2),   /* save end position, must come after saved_start */
        RE_OPCODE_DEF!("save_reset", 3), /* reset save positions */
        RE_OPCODE_DEF!("loop", 5),       /* decrement the top the stack and goto if != 0 */
        RE_OPCODE_DEF!("push_i32", 5),   /* push integer on the stack */
        RE_OPCODE_DEF!("drop", 1),
        RE_OPCODE_DEF!("word_boundary", 1),
        RE_OPCODE_DEF!("not_word_boundary", 1),
        RE_OPCODE_DEF!("back_reference", 2),
        RE_OPCODE_DEF!("backward_back_reference", 2), /* must come after back_reference */
        RE_OPCODE_DEF!("range", 3),                   /* variable length */
        RE_OPCODE_DEF!("range32", 3),                 /* variable length */
        RE_OPCODE_DEF!("lookahead", 5),
        RE_OPCODE_DEF!("negative_lookahead", 5),
        RE_OPCODE_DEF!("push_char_pos", 1), /* push the character position on the stack */
        RE_OPCODE_DEF!("check_advance", 1), /* pop one stack element and check that it is different from the character position */
        RE_OPCODE_DEF!("prev", 1),          /* go to the previous char */
        RE_OPCODE_DEF!("simple_greedy_quant", 17),
    ];
    ops
});

pub fn gen_re_opcodes(outfile: &Path) {
    let re_opcode_defs = Lazy::force(&RE_OPCODES);

    let mut enums = Vec::<String>::with_capacity(512);
    let mut info = Vec::<String>::with_capacity(512);

    enums.push("#[derive(Debug,Clone,Copy,PartialEq,Eq)]".into());
    enums.push("#[repr(u8)]".into());
    enums.push("pub enum REOpCode {".into());

    info.push(format!(
        "pub const RE_OPCODE_SIZE: [u8;{}]=[",
        re_opcode_defs.len()
    ));

    for (i, op) in re_opcode_defs.iter().enumerate() {
        enums.push(format!("{}={i},", op.name.to_uppercase()));
        info.push(format!("{},", op.size));
    }

    enums.push("}".into());
    info.push("];".into());

    std::fs::write(
        outfile,
        vec![enums.join("\n"), info.join("\n"), "".to_string()].join("\n"),
    )
    .unwrap();

    let output = std::process::Command::new("rustfmt")
        .arg("--")
        .arg(outfile)
        .output()
        .expect("Failed to execute rustfmt");
    if !output.status.success() {
        eprintln!(
            "rustfmt failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::process::exit(1);
    }
}

// impl REParseState {
//     pub fn re_emit_op_i32(&mut self, op: REOpCode, val: i32) {
//         self.byte_code.push(op as u8);
//         self.byte_code.extend_from_slice(&val.to_ne_bytes());
//     }

//     pub fn re_emit_op_u16(&mut self, op: REOpCode, val: u16) {
//         self.byte_code.push(op as u8);
//         self.byte_code.extend_from_slice(&val.to_ne_bytes());
//     }

//     pub fn re_emit_op_u8(&mut self, op: REOpCode, val: u8) {
//         self.byte_code.push(op as u8);
//         self.byte_code.push(val);
//     }

//     pub fn re_emit_op(&mut self, op: REOpCode) {
//         self.byte_code.push(op as u8);
//     }

//     pub fn re_emit_op_goto(&mut self, op: REOpCode, val: i32) {
//         self.byte_code.push(op as u8);
//         let pos = self.byte_code.len() as i32;
//         self.byte_code
//             .extend_from_slice(&(val - (pos + 4)).to_ne_bytes());
//     }
// }
