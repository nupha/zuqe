// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The inlined call path: copy an inlinable callee's body into the caller.
//!
//! The callee's arguments, locals and operand stack live in caller-frame
//! variables (Cranelift SSA), so an inlined call site costs no
//! `alloc_frame`/`init_frame_args` and no second activation.
//!
//! # The subset
//!
//! The copied body is *pure*: no heap helper except the allocation entries
//! below, no closure/global/`this`. Every operation is lowered for the
//! `Int`/`Bool`/`Null`/`Undefined` tags. An operand outside them sets the poison
//! flag instead of splitting the block; at the `return` a single branch sends
//! the whole call to the general path, which re-runs the callee from scratch
//! with the untouched call window. That is why a bailout is safe without a
//! deopt map: the inline path has written nothing observable. The flush rule
//! still holds: no register-resident value crosses a helper or an exit.
//!
//! The extensions below are independent:
//!
//! * **Loops**: a backward edge polls the caller frame's safepoint first; the
//!   inline body carries only integer-domain values (or copies of the original
//!   arguments, still in the call window), so the caller frame is a complete GC
//!   root set without a frame of its own. A non-zero status — and a poisoned
//!   back edge — bails to the general path.
//! * **Calls**: a monomorphic nested site copies its callee too (the snapshot
//!   carries the body and its own sites, depth ≤2). The nested guard compares
//!   the live callee's bytecode identity with a compile-time constant; a miss
//!   bails.
//! * **Exceptions**: `catch`/`nip_catch` only keep the stack height right;
//!   every `throw` bails, so the interpreter's catch scan (including a handler
//!   inside the callee) owns the semantics.
//! * **Allocation**: only `{}` and `[]` are in the subset. The helper writes the
//!   new object to the frame scratch slot; that is safe because an allocation
//!   never collects (it only sets `gc_req`), so a copied subtree that allocates
//!   must contain no safepoint (`hazards`).

use std::collections::HashMap;

use cranelift_codegen::ir::{Block, InstBuilder, MemFlags, SigRef, Value, condcodes::IntCC, types};
use cranelift_frontend::{FunctionBuilder, Variable};

use super::{
    abi::{HELPER_ALLOC_ARRAY0, HELPER_ALLOC_OBJECT, HELPER_SAFEPOINT},
    bytecode::VerifiedFunction,
    compiler::{box_bit_result, is_int},
};

const UNDEFINED: i64 = 3 << 48;
const NULL: i64 = 2 << 48;
const BOOL_TAG: i64 = 1 << 48;
/// The nested guard's hit counter (nested-inline instrumentation).
const O_NESTED_INLINE_HITS: i32 =
    std::mem::offset_of!(crate::jit::abi::JitExecFrame, nested_inline_hits) as i32;
/// The allocation helper's result slot.
const O_SCRATCH: i32 = std::mem::offset_of!(crate::jit::abi::JitExecFrame, scratch) as i32;

/// Bounds on a copied body. They keep the caller's generated code (and the
/// inline plan) small; the same limits are already implied by the snapshot's
/// bytecode cap, but these are the numbers the subset was tested at.
const MAX_INSTRUCTIONS: usize = 96;
const MAX_BLOCKS: usize = 16;

mod guards;
mod lower;
mod plan;

use guards::*;
pub(super) use lower::lower;
pub(super) use plan::{InlinePlan, eligible};

#[cfg(test)]
mod tests;
