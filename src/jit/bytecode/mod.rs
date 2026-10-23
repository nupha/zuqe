// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Frontend: turn a pointer-free [`CompileSnapshot`](crate::jit::snapshot::CompileSnapshot)
//! into a [`VerifiedFunction`] the compiler may consume, or reject it.

// The JIT compiler reads these accessors.
#![allow(dead_code)]

pub(crate) mod cfg;
pub(crate) mod decode;
pub(crate) mod stack;
pub(crate) mod verify;

pub(crate) use verify::{VerifiedFunction, VerifyLimits, verify};

#[cfg(test)]
mod tests {
    use super::{
        verify,
        verify::{VerifyErrorKind, VerifyLimits},
    };
    use crate::{
        bytecode::opcodes as op,
        jit::snapshot::{CompileSnapshot, ConstMeta},
    };

    fn limits() -> VerifyLimits {
        VerifyLimits::default()
    }

    /// `function f(a){ return; }` in final bytecode: `return_undef`.
    fn snapshot(bytes: Vec<u8>, args: u16, vars: u16, stack: u16) -> CompileSnapshot {
        CompileSnapshot::from_raw(bytes, args, vars, 0, stack)
    }

    #[test]
    fn accepts_a_minimal_function() {
        let function = verify(snapshot(vec![op::OP_return_undef], 0, 0, 4), limits()).unwrap();
        assert_eq!(function.instructions().len(), 1);
        assert!(!function.has_exception_regions());
    }

    #[test]
    fn accepts_a_numeric_loop_and_finds_its_header() {
        // i = 0 (push_i32, put_loc0); loop: get_loc0; push_i32; lt; if_false end;
        // ...; goto loop; end: return_undef
        let mut bytes = vec![op::OP_push_i32];
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.push(op::OP_put_loc);
        bytes.extend_from_slice(&0u16.to_le_bytes());

        let loop_pc = bytes.len() as i32;
        bytes.push(op::OP_get_loc);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.push(op::OP_push_i32);
        bytes.extend_from_slice(&10i32.to_le_bytes());
        bytes.push(op::OP_lt);

        let if_pc = bytes.len() as i32; // if_false target filled below
        bytes.push(op::OP_if_false);
        bytes.extend_from_slice(&0i32.to_le_bytes());
        let after_if = bytes.len() as i32;

        // body: i = i + 1
        bytes.push(op::OP_get_loc);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.push(op::OP_inc);
        bytes.push(op::OP_put_loc);
        bytes.extend_from_slice(&0u16.to_le_bytes());

        let goto_pc = bytes.len() as i32;
        bytes.push(op::OP_goto);
        bytes.extend_from_slice(&0i32.to_le_bytes());

        let end_pc = bytes.len() as i32;
        bytes.push(op::OP_return_undef);

        // if_false lands on `end_pc`: operand_pc = if_pc + 1.
        let if_off = end_pc - (if_pc + 1);
        bytes[(if_pc as usize + 1)..(if_pc as usize + 5)].copy_from_slice(&if_off.to_le_bytes());
        // goto lands on `loop_pc`.
        let goto_off = loop_pc - (goto_pc + 1);
        bytes[(goto_pc as usize + 1)..(goto_pc as usize + 5)]
            .copy_from_slice(&goto_off.to_le_bytes());
        let _ = after_if;

        let function = verify(snapshot(bytes, 0, 1, 4), limits()).unwrap();
        assert_eq!(function.osr_points().len(), 1);
        assert_eq!(function.osr_points()[0].pc(), loop_pc as u32);
    }

    #[test]
    fn rejects_stack_underflow() {
        // `add` pops two operands from an empty stack.
        let error = verify(
            snapshot(vec![op::OP_add, op::OP_return_undef], 0, 0, 4),
            limits(),
        )
        .unwrap_err();
        assert!(matches!(
            error.kind(),
            VerifyErrorKind::StackUnderflow { .. }
        ));
    }

    #[test]
    fn rejects_out_of_range_constant() {
        let mut bytes = vec![op::OP_push_const];
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.push(op::OP_return);
        let error = verify(snapshot(bytes, 0, 0, 4), limits()).unwrap_err();
        assert!(matches!(
            error.kind(),
            VerifyErrorKind::ConstantIndexOutOfRange { .. }
        ));
    }

    #[test]
    fn rejects_branch_into_the_middle_of_an_instruction() {
        let mut bytes = vec![op::OP_goto];
        bytes.extend_from_slice(&2i32.to_le_bytes()); // target 3: inside the next insn
        bytes.push(op::OP_push_i32);
        bytes.extend_from_slice(&0i32.to_le_bytes());
        bytes.push(op::OP_return);
        let error = verify(snapshot(bytes, 0, 0, 4), limits()).unwrap_err();
        assert!(matches!(
            error.kind(),
            VerifyErrorKind::BranchTargetInsideInstruction { .. }
        ));
    }

    #[test]
    fn rejects_rejected_opcode_fail_closed() {
        // `yield` never compiles.
        let error = verify(
            snapshot(vec![op::OP_yield, op::OP_return], 0, 0, 4),
            limits(),
        )
        .unwrap_err();
        assert!(matches!(
            error.kind(),
            VerifyErrorKind::RejectedOpcode { .. }
        ));
    }

    #[test]
    fn rejects_missing_terminator() {
        let error = verify(snapshot(vec![op::OP_nop], 0, 0, 4), limits()).unwrap_err();
        assert!(matches!(error.kind(), VerifyErrorKind::MissingTerminator));
    }

    #[test]
    fn push_const_carries_a_numeric_kind_only_for_numbers() {
        // A snapshot whose single constant is Float64: `push_const; return` is
        // well-typed, and the pushed slot must not be mistaken for Int32.
        let mut snap = snapshot(
            {
                let mut bytes = vec![op::OP_push_const];
                bytes.extend_from_slice(&0u32.to_le_bytes());
                bytes.push(op::OP_return);
                bytes
            },
            0,
            0,
            4,
        );
        snap.const_meta = Box::new([ConstMeta { tag: 8 }]);
        assert!(verify(snap, limits()).is_ok());
    }
}
