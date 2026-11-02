//! Emitting a copied body: one `match` over the body's opcodes plus
//! the operand-stack helpers it shares.

use super::*;

/// Emit the callee's body in the current function.
///
/// `args` holds the callee's declared arguments (missing ones filled with
/// `undefined`). `store` is `Some((base, sp_after))` for the top-level copy —
/// the result is written to the call window and `merge` receives `sp_after` —
/// and `None` for a nested copy, whose result travels as `merge`'s block
/// parameter. `bail` is the general call path, entered whenever an operand
/// needs semantics this subset does not implement.
#[allow(clippy::too_many_arguments)]
pub(in crate::jit) fn lower(
    b: &mut FunctionBuilder,
    plan: &InlinePlan,
    args: &[Value],
    store: Option<(Value, Value)>,
    next_tag: &mut u32,
    entry: Block,
    bail: Block,
    merge: Block,
    frame: Value,
    helpers: Value,
    helper_sig: SigRef,
) {
    #[cfg(test)]
    crate::jit::test_harness::note_inline_emission();
    let tag = *next_tag;
    *next_tag += 1;
    let body = &plan.body;
    let snapshot = body.snapshot();
    let undefined = b.ins().iconst(types::I64, UNDEFINED);

    let mut arg_vars = Vec::with_capacity(snapshot.arg_count as usize);
    for index in 0..snapshot.arg_count {
        let var = Variable::from_u32(tag * 0x100 + 0x10 + u32::from(index));
        b.declare_var(var, types::I64);
        let value = args.get(index as usize).copied().unwrap_or(undefined);
        b.def_var(var, value);
        arg_vars.push(var);
    }

    let mut locals = Vec::with_capacity(snapshot.var_count as usize);
    for index in 0..snapshot.var_count {
        let var = Variable::from_u32(tag * 0x100 + 0x40 + u32::from(index));
        b.declare_var(var, types::I64);
        b.def_var(var, undefined);
        locals.push(var);
    }

    // The callee's operand stack is caller-frame state too: one variable per
    // depth. The verifier's heights keep every use/def below the live top.
    let stack_size = usize::from(snapshot.stack_size) + 1;
    let mut stack = Vec::with_capacity(stack_size);
    for index in 0..stack_size {
        let var = Variable::from_u32(tag * 0x100 + 0x80 + index as u32);
        b.declare_var(var, types::I64);
        stack.push(var);
    }

    // One flag collects every "this operand is outside the subset" answer, so
    // a straight-line body stays a single block and the branch happens once.
    let poison = Variable::from_u32(tag * 0x100 + 0x04);
    b.declare_var(poison, types::I8);
    let zero_flag = b.ins().iconst(types::I8, 0);
    b.def_var(poison, zero_flag);

    let mut blocks: HashMap<u32, Block> = HashMap::new();
    let mut pending: Vec<u32> = Vec::new();
    blocks.insert(0, entry);
    pending.push(0);

    while let Some(start_pc) = pending.pop() {
        let block = blocks[&start_pc];
        // The caller is already in the entry block; only the other blocks
        // need switching to.
        if block != entry {
            b.switch_to_block(block);
        }
        let range = body
            .control_flow_graph()
            .block(start_pc)
            .map(|block| block.instruction_range())
            .expect("every inline block start is a CFG block");
        let instructions = body.instructions();
        let mut height = usize::from(body.stack_heights()[range.start]);
        let mut terminated = false;

        for index in range.clone() {
            let instruction = &instructions[index];
            if terminated {
                break;
            }
            debug_assert_eq!(
                height,
                usize::from(body.stack_heights()[index]),
                "inline stack height diverged at pc {}",
                instruction.pc()
            );
            let name = instruction.name();
            match name {
                "nop" => {}
                "push_i32" => {
                    let value = b
                        .ins()
                        .iconst(types::I64, i64::from(instruction.operand_u32(1)));
                    push(b, &stack, &mut height, value);
                }
                "undefined" => push(b, &stack, &mut height, undefined),
                "null" => {
                    let value = b.ins().iconst(types::I64, NULL);
                    push(b, &stack, &mut height, value);
                }
                "push_false" | "push_true" => {
                    let bit = i64::from(name == "push_true");
                    let value = b.ins().iconst(types::I64, BOOL_TAG | bit);
                    push(b, &stack, &mut height, value);
                }
                "get_arg" => {
                    let value = b.use_var(arg_vars[usize::from(instruction.operand_u16(1))]);
                    push(b, &stack, &mut height, value);
                }
                "get_loc" => {
                    let value = b.use_var(locals[usize::from(instruction.operand_u16(1))]);
                    push(b, &stack, &mut height, value);
                }
                "put_arg" => {
                    let value = pop(b, &stack, &mut height);
                    b.def_var(arg_vars[usize::from(instruction.operand_u16(1))], value);
                }
                "put_loc" => {
                    let value = pop(b, &stack, &mut height);
                    b.def_var(locals[usize::from(instruction.operand_u16(1))], value);
                }
                "set_arg" => {
                    let value = use_top(b, &stack, height);
                    b.def_var(arg_vars[usize::from(instruction.operand_u16(1))], value);
                }
                "set_loc" => {
                    let value = use_top(b, &stack, height);
                    b.def_var(locals[usize::from(instruction.operand_u16(1))], value);
                }
                "drop" => {
                    height -= 1;
                }
                "dup" => {
                    let value = use_top(b, &stack, height);
                    push(b, &stack, &mut height, value);
                }
                "object" | "array_from" => {
                    // The helper cannot collect (allocation only requests GC),
                    // so the unrooted register result is safe here; a body that
                    // allocates and contains a safepoint is not eligible.
                    let id = if name == "object" {
                        HELPER_ALLOC_OBJECT
                    } else {
                        HELPER_ALLOC_ARRAY0
                    };
                    let helper =
                        b.ins()
                            .load(types::I64, MemFlags::new(), helpers, (id * 8) as i32);
                    let call = b.ins().call_indirect(helper_sig, helper, &[frame]);
                    let status = b.inst_results(call)[0];
                    let failed = b.ins().icmp_imm(IntCC::NotEqual, status, 0);
                    let ok = b.create_block();
                    b.ins().brif(failed, bail, &[], ok, &[]);
                    b.switch_to_block(ok);
                    let value = b.ins().load(types::I64, MemFlags::new(), frame, O_SCRATCH);
                    push(b, &stack, &mut height, value);
                }
                "add" | "sub" | "mul" => {
                    let (a, c) = pop_binary(b, &stack, &mut height, poison);
                    let ai = b.ins().ireduce(types::I32, a);
                    let ci = b.ins().ireduce(types::I32, c);
                    let (result, overflow) = match name {
                        "add" => b.ins().sadd_overflow(ai, ci),
                        "sub" => b.ins().ssub_overflow(ai, ci),
                        _ => b.ins().smul_overflow(ai, ci),
                    };
                    // `0 * -1` is -0, which an Int cannot carry.
                    let overflow = if name == "mul" {
                        let a_zero = b.ins().icmp_imm(IntCC::Equal, ai, 0);
                        let c_zero = b.ins().icmp_imm(IntCC::Equal, ci, 0);
                        let a_neg = b.ins().icmp_imm(IntCC::SignedLessThan, ai, 0);
                        let c_neg = b.ins().icmp_imm(IntCC::SignedLessThan, ci, 0);
                        let neg_zero = {
                            let ac = b.ins().band(a_zero, c_neg);
                            let ca = b.ins().band(c_zero, a_neg);
                            b.ins().bor(ac, ca)
                        };
                        b.ins().bor(overflow, neg_zero)
                    } else {
                        overflow
                    };
                    note_poison(b, poison, overflow);
                    let boxed = b.ins().uextend(types::I64, result);
                    push(b, &stack, &mut height, boxed);
                }
                "add_loc" => {
                    let rhs = pop(b, &stack, &mut height);
                    let lhs = b.use_var(locals[usize::from(instruction.operand_u8(1))]);
                    let lhs_int = is_int(b, lhs);
                    let rhs_int = is_int(b, rhs);
                    let both = b.ins().band(lhs_int, rhs_int);
                    let failed = not(b, both);
                    note_poison(b, poison, failed);
                    let ai = b.ins().ireduce(types::I32, lhs);
                    let ci = b.ins().ireduce(types::I32, rhs);
                    let (result, overflow) = b.ins().sadd_overflow(ai, ci);
                    note_poison(b, poison, overflow);
                    let boxed = b.ins().uextend(types::I64, result);
                    b.def_var(locals[usize::from(instruction.operand_u8(1))], boxed);
                }
                "inc_loc" | "dec_loc" => {
                    let idx = usize::from(instruction.operand_u8(1));
                    let value = b.use_var(locals[idx]);
                    let is_num = is_int(b, value);
                    let failed = not(b, is_num);
                    note_poison(b, poison, failed);
                    let value = b.ins().ireduce(types::I32, value);
                    let one = b.ins().iconst(types::I32, 1);
                    let (result, overflow) = if name == "inc_loc" {
                        b.ins().sadd_overflow(value, one)
                    } else {
                        b.ins().ssub_overflow(value, one)
                    };
                    note_poison(b, poison, overflow);
                    let boxed = b.ins().uextend(types::I64, result);
                    b.def_var(locals[idx], boxed);
                }
                "lt" | "lte" | "gt" | "gte" | "eq" | "neq" | "strict_eq" | "strict_neq" => {
                    let (a, c) = pop_binary(b, &stack, &mut height, poison);
                    let ai = b.ins().ireduce(types::I32, a);
                    let ci = b.ins().ireduce(types::I32, c);
                    let condition = match name {
                        "lt" => b.ins().icmp(IntCC::SignedLessThan, ai, ci),
                        "lte" => b.ins().icmp(IntCC::SignedLessThanOrEqual, ai, ci),
                        "gt" => b.ins().icmp(IntCC::SignedGreaterThan, ai, ci),
                        "gte" => b.ins().icmp(IntCC::SignedGreaterThanOrEqual, ai, ci),
                        "eq" | "strict_eq" => b.ins().icmp(IntCC::Equal, ai, ci),
                        _ => b.ins().icmp(IntCC::NotEqual, ai, ci),
                    };
                    let widened = b.ins().uextend(types::I64, condition);
                    let boxed = b.ins().bor_imm(widened, BOOL_TAG);
                    push(b, &stack, &mut height, boxed);
                }
                "and" | "or" | "xor" | "shl" | "sar" | "shr" => {
                    let (a, c) = pop_binary(b, &stack, &mut height, poison);
                    let ai = b.ins().ireduce(types::I32, a);
                    let ci = b.ins().ireduce(types::I32, c);
                    let count = b.ins().band_imm(ci, 31);
                    let result = match name {
                        "and" => b.ins().band(ai, ci),
                        "or" => b.ins().bor(ai, ci),
                        "xor" => b.ins().bxor(ai, ci),
                        "shl" => b.ins().ishl(ai, count),
                        "shr" => b.ins().ushr(ai, count),
                        _ => b.ins().sshr(ai, count),
                    };
                    let boxed = match name {
                        // `shl`/`sar`/`and`/`or`/`xor` are `From<i32>`; `shr`
                        // is `From<u32>` (a Float64 once the sign bit is set).
                        "shl" | "sar" | "and" | "or" | "xor" => b.ins().uextend(types::I64, result),
                        _ => box_bit_result(b, result),
                    };
                    push(b, &stack, &mut height, boxed);
                }
                "plus" => {
                    let value = pop(b, &stack, &mut height);
                    let is_num = is_int(b, value);
                    let failed = not(b, is_num);
                    note_poison(b, poison, failed);
                    push(b, &stack, &mut height, value);
                }
                "neg" => {
                    let value = pop(b, &stack, &mut height);
                    let is_num = is_int(b, value);
                    let failed = not(b, is_num);
                    note_poison(b, poison, failed);
                    let value = b.ins().ireduce(types::I32, value);
                    // `-0` is a Float64; an Int 0 cannot carry its sign.
                    let zero = b.ins().icmp_imm(IntCC::Equal, value, 0);
                    note_poison(b, poison, zero);
                    let zero = b.ins().iconst(types::I32, 0);
                    let (result, overflow) = b.ins().ssub_overflow(zero, value);
                    note_poison(b, poison, overflow);
                    let boxed = b.ins().uextend(types::I64, result);
                    push(b, &stack, &mut height, boxed);
                }
                "inc" | "dec" => {
                    let value = pop(b, &stack, &mut height);
                    let is_num = is_int(b, value);
                    let failed = not(b, is_num);
                    note_poison(b, poison, failed);
                    let value = b.ins().ireduce(types::I32, value);
                    let one = b.ins().iconst(types::I32, 1);
                    let (result, overflow) = if name == "inc" {
                        b.ins().sadd_overflow(value, one)
                    } else {
                        b.ins().ssub_overflow(value, one)
                    };
                    note_poison(b, poison, overflow);
                    let boxed = b.ins().uextend(types::I64, result);
                    push(b, &stack, &mut height, boxed);
                }
                "lnot" => {
                    let value = pop(b, &stack, &mut height);
                    let truthy = truthy(b, value, poison);
                    let inverted = b.ins().icmp_imm(IntCC::Equal, truthy, 0);
                    let widened = b.ins().uextend(types::I64, inverted);
                    let boxed = b.ins().bor_imm(widened, BOOL_TAG);
                    push(b, &stack, &mut height, boxed);
                }
                "not" => {
                    let value = pop(b, &stack, &mut height);
                    let is_num = is_int(b, value);
                    let failed = not(b, is_num);
                    note_poison(b, poison, failed);
                    let value = b.ins().ireduce(types::I32, value);
                    let result = b.ins().bnot(value);
                    // Unary `~` boxes as `From<i32>` (an `Int`, sign kept),
                    // like `and`/`or`/`xor` (`shr` is the `From<u32>` one).
                    let boxed = b.ins().uextend(types::I64, result);
                    push(b, &stack, &mut height, boxed);
                }
                "call" | "call_method" => {
                    let argc = usize::from(instruction.operand_u16(1));
                    let method = name == "call_method";
                    let site = plan
                        .sites
                        .get(&instruction.pc())
                        .expect("eligibility only accepts calls with a planned body");
                    let mut call_args = vec![undefined; argc];
                    for index in (0..argc).rev() {
                        call_args[index] = pop(b, &stack, &mut height);
                    }
                    let func = if method {
                        let _receiver = pop(b, &stack, &mut height);
                        pop(b, &stack, &mut height)
                    } else {
                        pop(b, &stack, &mut height)
                    };
                    // The nested site's monomorphic guard. A miss bails the
                    // whole call: nothing observable happened yet.
                    let hit = callee_guard(b, func, site.callee, frame, bail);
                    let nested_merge = b.create_block();
                    b.append_block_param(nested_merge, types::I64);
                    let declared = usize::from(site.body.snapshot().arg_count);
                    let nested_args: Vec<Value> = (0..declared)
                        .map(|index| call_args.get(index).copied().unwrap_or(undefined))
                        .collect();
                    lower(
                        b,
                        site,
                        &nested_args,
                        None,
                        next_tag,
                        hit,
                        bail,
                        nested_merge,
                        frame,
                        helpers,
                        helper_sig,
                    );
                    b.switch_to_block(nested_merge);
                    let result = b.block_params(nested_merge)[0];
                    push(b, &stack, &mut height, result);
                }
                "catch" => {
                    // The interpreter's catch scan reads this sentinel from the
                    // frame after a native exception exit. The inline path
                    // never exits, so it only has to keep the stack height
                    // right (a throw bails and re-runs the callee).
                    let handler = instruction.branch_target().expect("catch target") as u32;
                    let sentinel = b
                        .ins()
                        .iconst(types::I64, (5i64 << 48) | i64::from(handler));
                    push(b, &stack, &mut height, sentinel);
                }
                "nip_catch" => {
                    // `[catch_offset, ..., value, ^] -> [value, ^]`. The
                    // sentinel's depth is fixed by the verifier's height after
                    // this instruction (the stack must be balanced there), so
                    // no runtime scan is needed.
                    let value = pop(b, &stack, &mut height);
                    let after = usize::from(
                        *body
                            .stack_heights()
                            .get(index + 1)
                            .expect("nip_catch is never the last instruction"),
                    );
                    b.def_var(stack[after - 1], value);
                    height = after;
                }
                "throw" => {
                    let _value = pop(b, &stack, &mut height);
                    // Nothing observable happened before the throw: the general
                    // path re-runs the callee and owns the catch scan, so a
                    // handler inside this body still wins exactly as it would.
                    b.ins().jump(bail, &[]);
                    terminated = true;
                }
                "goto" => {
                    let target_pc = instruction.branch_target().expect("goto target") as u32;
                    let target = block_for(b, &mut blocks, &mut pending, target_pc);
                    if target_pc <= instruction.pc() {
                        backedge_safepoint(b, poison, frame, helpers, helper_sig, bail);
                    }
                    b.ins().jump(target, &[]);
                    terminated = true;
                }
                "if_true" | "if_false" => {
                    let value = pop(b, &stack, &mut height);
                    let cond = truthy(b, value, poison);
                    let target_pc = instruction.branch_target().expect("if target") as u32;
                    let target = block_for(b, &mut blocks, &mut pending, target_pc);
                    let fallthrough_pc = instruction.pc() + instruction.size() as u32;
                    let fallthrough = block_for(b, &mut blocks, &mut pending, fallthrough_pc);
                    if target_pc <= instruction.pc() {
                        // Only the taken edge loops: enter a side block so the
                        // poison check and the safepoint run on that edge only.
                        let back = b.create_block();
                        if name == "if_true" {
                            b.ins().brif(cond, back, &[], fallthrough, &[]);
                        } else {
                            b.ins().brif(cond, fallthrough, &[], back, &[]);
                        }
                        b.switch_to_block(back);
                        backedge_safepoint(b, poison, frame, helpers, helper_sig, bail);
                        b.ins().jump(target, &[]);
                    } else if name == "if_true" {
                        b.ins().brif(cond, target, &[], fallthrough, &[]);
                    } else {
                        b.ins().brif(cond, fallthrough, &[], target, &[]);
                    }
                    terminated = true;
                }
                "return" | "return_undef" => {
                    let value = if name == "return" {
                        pop(b, &stack, &mut height)
                    } else {
                        undefined
                    };
                    // One bailout check for the whole body: a poisoned value
                    // may have travelled through branches, but every path ends
                    // here, and nothing was stored.
                    let failed = b.use_var(poison);
                    let failed = b.ins().icmp_imm(IntCC::NotEqual, failed, 0);
                    let good = b.create_block();
                    b.ins().brif(failed, bail, &[], good, &[]);
                    b.switch_to_block(good);
                    let out = if let Some((base, sp_after)) = store {
                        b.ins().store(MemFlags::new(), value, base, 0);
                        sp_after
                    } else {
                        value
                    };
                    b.ins().jump(merge, &[out]);
                    terminated = true;
                }
                _ => unreachable!("inline subset checked before lowering"),
            }
        }

        if !terminated {
            let tail = &instructions[range.end - 1];
            let next = tail.pc() + tail.size() as u32;
            let next = block_for(b, &mut blocks, &mut pending, next);
            b.ins().jump(next, &[]);
        }
    }
}
fn block_for(
    b: &mut FunctionBuilder,
    blocks: &mut HashMap<u32, Block>,
    pending: &mut Vec<u32>,
    pc: u32,
) -> Block {
    if let Some(block) = blocks.get(&pc) {
        return *block;
    }
    let block = b.create_block();
    blocks.insert(pc, block);
    pending.push(pc);
    block
}

fn push(b: &mut FunctionBuilder, stack: &[Variable], height: &mut usize, value: Value) {
    b.def_var(stack[*height], value);
    *height += 1;
}

fn pop(b: &mut FunctionBuilder, stack: &[Variable], height: &mut usize) -> Value {
    *height -= 1;
    b.use_var(stack[*height])
}

fn use_top(b: &mut FunctionBuilder, stack: &[Variable], height: usize) -> Value {
    b.use_var(stack[height - 1])
}

fn not(b: &mut FunctionBuilder, value: Value) -> Value {
    let zero = b.ins().iconst(types::I8, 0);
    b.ins().icmp(IntCC::Equal, value, zero)
}

/// OR a failure condition into the body's poison flag.
fn note_poison(b: &mut FunctionBuilder, poison: Variable, condition: Value) {
    let previous = b.use_var(poison);
    let merged = b.ins().bor(previous, condition);
    b.def_var(poison, merged);
}

/// Pop two operands; a non-`Int` poisons the body instead of splitting it.
fn pop_binary(
    b: &mut FunctionBuilder,
    stack: &[Variable],
    height: &mut usize,
    poison: Variable,
) -> (Value, Value) {
    let c = pop(b, stack, height);
    let a = pop(b, stack, height);
    let a_int = is_int(b, a);
    let c_int = is_int(b, c);
    let both = b.ins().band(a_int, c_int);
    let failed = not(b, both);
    note_poison(b, poison, failed);
    (a, c)
}

/// `ToBoolean` for the tags this subset carries. Other tags poison the body,
/// so the general path evaluates them (strings, objects, BigInt, …).
fn truthy(b: &mut FunctionBuilder, value: Value, poison: Variable) -> Value {
    let tag = b.ins().ushr_imm(value, 48);
    let simple = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, tag, 3);
    let failed = not(b, simple);
    note_poison(b, poison, failed);
    let low = b.ins().ireduce(types::I32, value);
    let int_true = b.ins().icmp_imm(IntCC::NotEqual, low, 0);
    let low_bit = b.ins().band_imm(value, 1);
    let bool_true = b.ins().icmp_imm(IntCC::Equal, low_bit, 1);
    let is_int_tag = b.ins().icmp_imm(IntCC::Equal, tag, 0);
    let is_bool_tag = b.ins().icmp_imm(IntCC::Equal, tag, 1);
    let int_true = b.ins().band(is_int_tag, int_true);
    let bool_true = b.ins().band(is_bool_tag, bool_true);
    b.ins().bor(int_true, bool_true)
}
