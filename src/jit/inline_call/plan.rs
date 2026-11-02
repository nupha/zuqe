//! Which copied bodies are eligible: the caps, the purity scan and
//! the verified plan the lowering consumes.

use super::*;

/// One verified body plus its nested monomorphic call sites. The plan is
/// built once per top-level inline site and consumed by [`lower`].
pub(in crate::jit) struct InlinePlan {
    pub(in crate::jit) body: VerifiedFunction,
    /// The callee's bytecode identity, compared by the nested guard.
    pub(in crate::jit) callee: u64,
    /// Nested call site pc -> the body it may copy.
    pub(in crate::jit) sites: HashMap<u32, InlinePlan>,
}

/// May this callee's body be copied? This is the whole structural contract:
/// the lowering below only knows the opcodes [`in_subset`] accepts, plus a
/// `call`/`call_method` at a pc [`InlinePlan::sites`] has a body for.
pub(in crate::jit) fn eligible(body: &VerifiedFunction, sites: &HashMap<u32, InlinePlan>) -> bool {
    let snapshot = body.snapshot();
    if snapshot.arg_count > 16
        || snapshot.var_count > 16
        || snapshot.stack_size > 32
        || body.instructions().len() > MAX_INSTRUCTIONS
        || body.control_flow_graph().blocks().len() > MAX_BLOCKS
    {
        return false;
    }
    for instruction in body.instructions() {
        let name = instruction.name();
        if matches!(name, "call" | "call_method") {
            if !sites.contains_key(&instruction.pc()) {
                return false;
            }
            continue;
        }
        if name == "array_from" && instruction.operand_u16(1) != 0 {
            // Only the empty-array helper returns through `scratch`; a
            // non-empty literal would need its elements in memory.
            return false;
        }
        if !in_subset(name) {
            return false;
        }
    }
    // An allocation leaves the new object only in a register. That is safe
    // exactly while no GC point can run in this copied subtree: an allocation
    // never collects by itself (it only sets `gc_req`).
    let (allocates, has_safepoint) = hazards(body, sites);
    !(allocates && has_safepoint)
}

/// `(allocates, has a back edge)` for a copied subtree.
fn hazards(body: &VerifiedFunction, sites: &HashMap<u32, InlinePlan>) -> (bool, bool) {
    let mut allocates = false;
    let mut safepoint = false;
    for instruction in body.instructions() {
        match instruction.name() {
            "object" | "array_from" => allocates = true,
            "goto" | "if_true" | "if_false" => {
                if instruction
                    .branch_target()
                    .is_some_and(|target| target <= i64::from(instruction.pc()))
                {
                    safepoint = true;
                }
            }
            _ => {}
        }
    }
    for plan in sites.values() {
        let (nested_alloc, nested_safepoint) = hazards(&plan.body, &plan.sites);
        allocates |= nested_alloc;
        safepoint |= nested_safepoint;
    }
    (allocates, safepoint)
}

/// The opcodes the inline lowering implements. Everything the ordinary
/// lowering would send to a helper is either lowered here for `Int` operands
/// or left out of the subset entirely.
fn in_subset(name: &str) -> bool {
    matches!(
        name,
        "nop"
            | "get_arg"
            | "get_loc"
            | "put_arg"
            | "put_loc"
            | "set_arg"
            | "set_loc"
            | "push_i32"
            | "undefined"
            | "null"
            | "push_false"
            | "push_true"
            | "drop"
            | "dup"
            | "goto"
            | "if_true"
            | "if_false"
            | "catch"
            | "nip_catch"
            | "throw"
            | "object"
            | "array_from"
            | "return"
            | "return_undef"
            | "add"
            | "sub"
            | "mul"
            | "add_loc"
            | "inc_loc"
            | "dec_loc"
            | "lt"
            | "lte"
            | "gt"
            | "gte"
            | "eq"
            | "neq"
            | "strict_eq"
            | "strict_neq"
            | "and"
            | "or"
            | "xor"
            | "shl"
            | "sar"
            | "shr"
            | "plus"
            | "neg"
            | "inc"
            | "dec"
            | "lnot"
            | "not"
    )
}
