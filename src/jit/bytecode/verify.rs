// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Frontend gate: decode → policy → index bounds → CFG → abstract stack.
//!
//! Fail-closed. A function either produces a `VerifiedFunction` the compiler is
//! allowed to consume, or an error and it stays in the interpreter forever. The
//! policy step rejects every opcode the generator cannot yet translate, so the
//! structural passes only ever see the accepted subset.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    bytecode::OpcodeFormat,
    jit::{
        policy::{JitPolicy, policy_for},
        snapshot::CompileSnapshot,
    },
};

use super::{
    cfg::{self, ControlFlowGraph},
    decode::{BoundedDecodeError, DecodeError, Instruction, decode_bounded},
    stack::{self, ExceptionHandler, SlotKind},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Resource {
    SnapshotBytes,
    DecodedInstructions,
    BasicBlocks,
    MetadataBytes,
    WorkUnits,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VerifyLimits {
    pub(crate) max_snapshot_bytes: usize,
    pub(crate) max_instructions: usize,
    pub(crate) max_basic_blocks: usize,
    pub(crate) max_metadata_bytes: usize,
    pub(crate) max_work_units: usize,
}

impl Default for VerifyLimits {
    fn default() -> Self {
        Self {
            max_snapshot_bytes: 16 * 1024 * 1024,
            max_instructions: 1_000_000,
            max_basic_blocks: 65_536,
            max_metadata_bytes: 8 * 1024 * 1024,
            max_work_units: 10_000_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct VerifyError {
    pc: u32,
    kind: VerifyErrorKind,
}

impl VerifyError {
    pub(crate) const fn new(pc: u32, kind: VerifyErrorKind) -> Self {
        Self { pc, kind }
    }

    pub(crate) const fn pc(&self) -> u32 {
        self.pc
    }

    pub(crate) const fn kind(&self) -> &VerifyErrorKind {
        &self.kind
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VerifyErrorKind {
    Decode(DecodeError),
    EmptyBytecode,
    MissingTerminator,
    UnreachableInstruction,
    BranchTargetOutOfRange {
        target: i64,
    },
    BranchTargetInsideInstruction {
        target: u32,
    },
    StackUnderflow {
        needed: usize,
        available: usize,
    },
    StackSizeExceeded {
        declared: u16,
        actual: usize,
    },
    InconsistentMergeHeight {
        expected: usize,
        actual: usize,
    },
    IncompatibleMergeKind {
        slot: usize,
        expected: SlotKind,
        actual: SlotKind,
    },
    LocalIndexOutOfRange {
        index: u32,
        count: u32,
    },
    ArgumentIndexOutOfRange {
        index: u32,
        count: u32,
    },
    ClosureIndexOutOfRange {
        index: u32,
        count: u32,
    },
    GlobalVarIndexOutOfRange {
        index: u32,
        count: u32,
    },
    ConstantIndexOutOfRange {
        index: u32,
        count: u32,
    },
    UnsupportedExceptionRegion,
    /// The opcode's policy is `Reject`, or an operand form the generator does
    /// not support.
    RejectedOpcode {
        op: u8,
    },
    IncompleteOsrState,
    ResourceLimit {
        resource: Resource,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct OsrPoint {
    pc: u32,
    live_slots: Vec<SlotKind>,
}

impl OsrPoint {
    pub(crate) const fn pc(&self) -> u32 {
        self.pc
    }

    pub(crate) fn live_slots(&self) -> &[SlotKind] {
        &self.live_slots
    }
}

#[derive(Clone, Debug)]
pub(crate) struct VerifiedFunction {
    snapshot: CompileSnapshot,
    instructions: Vec<Instruction>,
    cfg: ControlFlowGraph,
    osr_points: Box<[OsrPoint]>,
    handlers: BTreeMap<u32, ExceptionHandler>,
    unreachable: BTreeSet<u32>,
    exception_regions: bool,
    /// Operand-stack height *before* each instruction, indexed like
    /// `instructions` (epic F4): the lowering keys its block-local stack
    /// cache by these static heights. Unreachable instructions carry 0 and
    /// are never lowered.
    stack_heights: Box<[u16]>,
    /// Proven local kinds before each `get_loc`-family instruction, keyed by
    /// pc (epic 1.4 item 3).
    local_kinds: BTreeMap<u32, Box<[SlotKind]>>,
}

impl VerifiedFunction {
    pub(crate) fn snapshot(&self) -> &CompileSnapshot {
        &self.snapshot
    }

    pub(crate) fn instructions(&self) -> &[Instruction] {
        &self.instructions
    }

    /// Operand-stack height before each instruction (see the field docs). Indexed by
    /// instruction index, same order as [`Self::instructions`].
    pub(crate) fn stack_heights(&self) -> &[u16] {
        &self.stack_heights
    }

    /// The proven local kinds before the `get_loc`-family instruction at `pc`,
    /// if it is reachable (epic 1.4 item 3).
    pub(crate) fn local_kinds_at(&self, pc: u32) -> Option<&[SlotKind]> {
        self.local_kinds.get(&pc).map(|kinds| &**kinds)
    }

    pub(crate) const fn control_flow_graph(&self) -> &ControlFlowGraph {
        &self.cfg
    }

    pub(crate) fn osr_points(&self) -> &[OsrPoint] {
        &self.osr_points
    }

    pub(crate) fn is_reachable(&self, pc: u32) -> bool {
        !self.unreachable.contains(&pc)
    }

    pub(crate) fn exception_handler(&self, pc: u32) -> Option<ExceptionHandler> {
        self.handlers.get(&pc).copied()
    }

    /// True when the function raises or catches in-frame, so consumers that
    /// cannot model exceptional control flow must decline it.
    pub(crate) const fn has_exception_regions(&self) -> bool {
        self.exception_regions
    }
}

/// `special_object` kinds whose frame semantics the baseline tier cannot model.
/// `ImportMeta` is module state (`import.meta` needs the module record, not the
/// frame). `MappedArguments` used to be rejected here too; the JIT helper now
/// calls the interpreter's own builder
/// ([`crate::function::argument::js_build_mapped_arguments`]), so the aliasing
/// `VarRef`s into the frame's argument slots are created exactly as the
/// interpreter creates them.
pub(crate) const SPECIAL_OBJECT_IMPORT_META: u8 = 6;

fn special_object_rejection(instruction: &Instruction) -> bool {
    instruction.name() == "special_object"
        && instruction.operand_u8(1) == SPECIAL_OBJECT_IMPORT_META
}

fn decode_error_pc(error: &DecodeError) -> u32 {
    match *error {
        DecodeError::UnknownOpcode { pc, .. }
        | DecodeError::InvalidOpcode { pc }
        | DecodeError::Truncated { pc, .. } => pc,
    }
}

fn short_index(instruction: &Instruction) -> Option<u32> {
    instruction
        .name()
        .as_bytes()
        .last()
        .and_then(|value| value.is_ascii_digit().then_some(u32::from(value - b'0')))
}

#[derive(Clone, Copy)]
enum IndexSpace {
    Local,
    Argument,
    Closure,
    GlobalVar,
    Constant,
}

fn indexed_operand(instruction: &Instruction) -> Option<(IndexSpace, u32)> {
    let name = instruction.name();
    let format = instruction.format();
    if format == OpcodeFormat::ATOM_U16 {
        let space = match name {
            "make_loc_ref" => IndexSpace::Local,
            "make_arg_ref" => IndexSpace::Argument,
            "make_var_ref_ref" => IndexSpace::Closure,
            _ => return None,
        };
        return Some((space, u32::from(instruction.operand_u16(5))));
    }
    let space = match format {
        OpcodeFormat::LOC | OpcodeFormat::LOC8 | OpcodeFormat::NONE_LOC => IndexSpace::Local,
        OpcodeFormat::ARG | OpcodeFormat::NONE_ARG => IndexSpace::Argument,
        OpcodeFormat::VAR_REF | OpcodeFormat::NONE_VAR_REF => match name {
            "get_var" | "get_var_undef" | "put_var" | "put_var_init" => IndexSpace::GlobalVar,
            _ => IndexSpace::Closure,
        },
        OpcodeFormat::CONST | OpcodeFormat::CONST8 => IndexSpace::Constant,
        _ => return None,
    };
    let index = match format {
        OpcodeFormat::LOC | OpcodeFormat::ARG | OpcodeFormat::VAR_REF => {
            u32::from(instruction.operand_u16(1))
        }
        OpcodeFormat::LOC8 | OpcodeFormat::CONST8 => u32::from(instruction.operand_u8(1)),
        OpcodeFormat::CONST => instruction.operand_u32(1),
        OpcodeFormat::NONE_LOC | OpcodeFormat::NONE_ARG | OpcodeFormat::NONE_VAR_REF => {
            short_index(instruction).expect("compact index ends in a digit")
        }
        _ => unreachable!(),
    };
    Some((space, index))
}

fn validate_indices(
    snapshot: &CompileSnapshot,
    instructions: &[Instruction],
) -> Result<(), VerifyError> {
    for instruction in instructions {
        let Some((space, index)) = indexed_operand(instruction) else {
            continue;
        };
        let count = match space {
            IndexSpace::Local => u32::from(snapshot.var_count),
            IndexSpace::Argument => u32::from(snapshot.arg_count),
            IndexSpace::Closure => u32::from(snapshot.closure_var_count),
            IndexSpace::GlobalVar => u32::from(snapshot.global_var_ref_count),
            IndexSpace::Constant => snapshot.const_meta.len() as u32,
        };
        if index >= count {
            let kind = match space {
                IndexSpace::Local => VerifyErrorKind::LocalIndexOutOfRange { index, count },
                IndexSpace::Argument => VerifyErrorKind::ArgumentIndexOutOfRange { index, count },
                IndexSpace::Closure => VerifyErrorKind::ClosureIndexOutOfRange { index, count },
                IndexSpace::GlobalVar => VerifyErrorKind::GlobalVarIndexOutOfRange { index, count },
                IndexSpace::Constant => VerifyErrorKind::ConstantIndexOutOfRange { index, count },
            };
            return Err(VerifyError::new(instruction.pc(), kind));
        }
    }
    Ok(())
}

pub(crate) fn verify(
    snapshot: CompileSnapshot,
    limits: VerifyLimits,
) -> Result<VerifiedFunction, VerifyError> {
    let snapshot_bytes = snapshot.bytecode.len().saturating_add(
        snapshot.const_meta.len() * std::mem::size_of::<crate::jit::snapshot::ConstMeta>(),
    );
    if snapshot_bytes > limits.max_snapshot_bytes {
        return Err(VerifyError::new(
            0,
            VerifyErrorKind::ResourceLimit {
                resource: Resource::SnapshotBytes,
            },
        ));
    }
    if snapshot.bytecode.len() > limits.max_metadata_bytes {
        return Err(VerifyError::new(
            0,
            VerifyErrorKind::ResourceLimit {
                resource: Resource::MetadataBytes,
            },
        ));
    }

    let instructions = decode_bounded(&snapshot.bytecode, limits.max_instructions).map_err(
        |error| match error {
            BoundedDecodeError::Decode(error) => {
                VerifyError::new(decode_error_pc(&error), VerifyErrorKind::Decode(error))
            }
            BoundedDecodeError::InstructionLimit { pc } => VerifyError::new(
                pc,
                VerifyErrorKind::ResourceLimit {
                    resource: Resource::DecodedInstructions,
                },
            ),
        },
    )?;

    // Fail-closed policy first, so the structural passes only see opcodes the
    // generator will actually translate.
    for instruction in &instructions {
        if policy_for(instruction.op()) == JitPolicy::Reject
            || special_object_rejection(instruction)
        {
            return Err(VerifyError::new(
                instruction.pc(),
                VerifyErrorKind::RejectedOpcode {
                    op: instruction.op(),
                },
            ));
        }
    }

    validate_indices(&snapshot, &instructions)?;
    let cfg = cfg::build(
        &instructions,
        snapshot.bytecode.len(),
        limits.max_basic_blocks,
    )?;
    let proof = stack::prove(&snapshot, &instructions, &cfg, limits.max_work_units)?;

    let has_subroutines = instructions
        .iter()
        .any(|instruction| matches!(instruction.name(), "gosub" | "ret"));
    let unreachable: BTreeSet<u32> = instructions
        .iter()
        .map(Instruction::pc)
        .filter(|pc| !proof.visited.contains(pc))
        .collect();
    if let Some(pc) = unreachable.first().copied().filter(|_| !has_subroutines) {
        return Err(VerifyError::new(
            pc,
            VerifyErrorKind::UnreachableInstruction,
        ));
    }
    let tail = instructions.last().expect("CFG rejected empty bytecode");
    if !cfg::has_valid_exit(tail) {
        return Err(VerifyError::new(
            tail.pc(),
            VerifyErrorKind::MissingTerminator,
        ));
    }

    let osr_points = cfg
        .blocks()
        .iter()
        .map(|block| block.start_pc())
        .filter(|pc| cfg.is_loop_header(*pc) && !unreachable.contains(pc))
        .map(|pc| {
            let state = proof
                .before
                .get(&pc)
                .ok_or_else(|| VerifyError::new(pc, VerifyErrorKind::IncompleteOsrState))?;
            Ok(OsrPoint {
                pc,
                live_slots: state.live_slots(&snapshot),
            })
        })
        .collect::<Result<Vec<_>, VerifyError>>()?
        .into_boxed_slice();

    let exception_regions = proof
        .handlers
        .values()
        .any(|handler| handler.resumes_in_frame())
        || instructions.iter().any(|instruction| {
            matches!(
                instruction.name(),
                "catch" | "nip_catch" | "gosub" | "ret" | "throw" | "throw_error"
            )
        });

    // Epic F4: keep the proven entry height of every instruction — the
    // lowering keys its block-local stack cache by them.
    let stack_heights = proof.heights;
    let local_kinds = proof.local_kinds;

    Ok(VerifiedFunction {
        snapshot,
        instructions,
        cfg,
        osr_points,
        handlers: proof.handlers,
        unreachable,
        exception_regions,
        stack_heights: stack_heights.into_boxed_slice(),
        local_kinds,
    })
}
