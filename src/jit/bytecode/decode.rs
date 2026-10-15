// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Byte stream → instruction array. Bounded and total: every input either
//! decodes completely or returns an error, never panics and never loops.
//!
//! Instruction length comes from `opcode_info_final(op).size`, the same table
//! the interpreter dispatches on, so the two can never disagree.

use crate::bytecode::{OP_COUNT_FINAL, Opcode as OpcodeInfo, OpcodeFormat, opcode_info_final};

/// Largest final opcode in zhuque (`with_get_var` is 10 bytes). A little slack
/// keeps this a power of two; a longer future opcode trips the static assert.
pub(crate) const MAX_INSTRUCTION: usize = 16;

/// Name of a final opcode. The generated table mirrors the interpreter's
/// `Opcode.name`, which is only compiled under `dump_bytecode`.
pub(crate) fn opcode_name(op: u8) -> &'static str {
    crate::jit::policy::JS_JIT_OPCODE_NAMES
        .get(op as usize)
        .copied()
        .unwrap_or("invalid")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DecodeError {
    UnknownOpcode { pc: u32, opcode: u8 },
    InvalidOpcode { pc: u32 },
    Truncated { pc: u32, size: usize },
}

/// Decoding failure, distinguishing "malformed" from "too many instructions".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BoundedDecodeError {
    Decode(DecodeError),
    InstructionLimit { pc: u32 },
}

/// One decoded instruction: its PC, id, and raw bytes (opcode + operands).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Instruction {
    pc: u32,
    op: u8,
    len: u8,
    bytes: [u8; MAX_INSTRUCTION],
}

impl Instruction {
    pub(crate) const fn pc(&self) -> u32 {
        self.pc
    }

    pub(crate) const fn op(&self) -> u8 {
        self.op
    }

    pub(crate) const fn size(&self) -> usize {
        self.len as usize
    }

    pub(crate) fn name(&self) -> &'static str {
        opcode_name(self.op)
    }

    pub(crate) const fn info(&self) -> &'static OpcodeInfo {
        opcode_info_final(self.op)
    }

    pub(crate) fn format(&self) -> OpcodeFormat {
        self.info().fmt
    }

    pub(crate) const fn operand_u8(&self, offset: usize) -> u8 {
        self.bytes[offset]
    }

    pub(crate) const fn operand_u16(&self, offset: usize) -> u16 {
        u16::from_le_bytes([self.bytes[offset], self.bytes[offset + 1]])
    }

    pub(crate) const fn operand_i16(&self, offset: usize) -> i16 {
        i16::from_le_bytes([self.bytes[offset], self.bytes[offset + 1]])
    }

    pub(crate) const fn operand_u32(&self, offset: usize) -> u32 {
        u32::from_le_bytes([
            self.bytes[offset],
            self.bytes[offset + 1],
            self.bytes[offset + 2],
            self.bytes[offset + 3],
        ])
    }

    pub(crate) const fn operand_i32(&self, offset: usize) -> i32 {
        self.operand_u32(offset) as i32
    }

    /// Static branch destination, if this instruction branches. Label offsets
    /// are relative to the byte after the opcode, matching the interpreter's
    /// `PC_GO!`/`PC_FETCH!` arithmetic.
    pub(crate) fn branch_target(&self) -> Option<i64> {
        let operand_pc = i64::from(self.pc) + 1;
        match self.format() {
            OpcodeFormat::LABEL8 => Some(operand_pc + i64::from(self.operand_u8(1) as i8)),
            OpcodeFormat::LABEL16 => Some(operand_pc + i64::from(self.operand_i16(1))),
            OpcodeFormat::LABEL | OpcodeFormat::LABEL_U16 => {
                Some(operand_pc + i64::from(self.operand_i32(1)))
            }
            _ => None,
        }
    }
}

pub(crate) fn decode_bounded(
    bytes: &[u8],
    max_instructions: usize,
) -> Result<Vec<Instruction>, BoundedDecodeError> {
    let mut result = Vec::new();
    let mut pc = 0usize;
    while pc < bytes.len() {
        if result.len() >= max_instructions {
            return Err(BoundedDecodeError::InstructionLimit { pc: pc as u32 });
        }
        let op = bytes[pc];
        if op == 0 {
            // `invalid` is only ever a placeholder; real bytecode never emits it.
            return Err(BoundedDecodeError::Decode(DecodeError::InvalidOpcode {
                pc: pc as u32,
            }));
        }
        if op >= OP_COUNT_FINAL {
            return Err(BoundedDecodeError::Decode(DecodeError::UnknownOpcode {
                pc: pc as u32,
                opcode: op,
            }));
        }
        let size = opcode_info_final(op).size as usize;
        let end = pc.saturating_add(size);
        if size > MAX_INSTRUCTION || end > bytes.len() {
            return Err(BoundedDecodeError::Decode(DecodeError::Truncated {
                pc: pc as u32,
                size,
            }));
        }
        let mut insn = Instruction {
            pc: pc as u32,
            op,
            len: size as u8,
            bytes: [0; MAX_INSTRUCTION],
        };
        insn.bytes[..size].copy_from_slice(&bytes[pc..end]);
        result.push(insn);
        pc = end;
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::opcodes as op;

    #[test]
    fn decodes_totally_or_errors_without_panicking() {
        // Every opcode with zeroed operands decodes; every truncation errors.
        for value in 1..OP_COUNT_FINAL {
            let size = opcode_info_final(value).size as usize;
            let mut complete = vec![0u8; size];
            complete[0] = value;
            let decoded = decode_bounded(&complete, usize::MAX).expect("complete opcode");
            assert_eq!(decoded.len(), 1);
            assert_eq!(decoded[0].size(), size);
            for prefix in 1..size {
                let error = decode_bounded(&complete[..prefix], usize::MAX);
                assert!(error.is_err(), "prefix {prefix} of {value} must not decode");
            }
        }
    }

    #[test]
    fn rejects_invalid_and_unknown() {
        assert_eq!(
            decode_bounded(&[0], usize::MAX).unwrap_err(),
            BoundedDecodeError::Decode(DecodeError::InvalidOpcode { pc: 0 })
        );
        assert_eq!(
            decode_bounded(&[255], usize::MAX).unwrap_err(),
            BoundedDecodeError::Decode(DecodeError::UnknownOpcode { pc: 0, opcode: 255 })
        );
    }

    #[test]
    fn instruction_limit_is_reported() {
        // Two `nop`s (1 byte each) but a one-instruction cap.
        let bytes = [op::OP_nop, op::OP_nop];
        assert_eq!(
            decode_bounded(&bytes, 1).unwrap_err(),
            BoundedDecodeError::InstructionLimit { pc: 1 }
        );
    }

    #[test]
    fn goto_target_is_relative_to_the_next_byte() {
        // goto at pc 0, offset 4: target = 0 + 1 + 4 = 5.
        let mut bytes = vec![op::OP_goto];
        bytes.extend_from_slice(&4i32.to_le_bytes());
        let decoded = decode_bounded(&bytes, usize::MAX).unwrap();
        assert_eq!(decoded[0].branch_target(), Some(5));
        assert_eq!(decoded[0].name(), "goto");
    }
}
