//! Value boxing, tag tests and the specialized numeric fast paths.

use super::*;

/// Box a 32-bit bit-operation result exactly like `JSValue::from(u32)`: an `Int`
/// when it fits in `i32`, otherwise a `Float64`. Used only where the engine
/// boxes through `u32` — `shr` and typed-array `u32` reads (the interpreter
/// really does return `4294967295` for `-1 >>> 0`; the baseline matches it
/// bit-for-bit).
pub(in crate::jit) fn box_bit_result(b: &mut FunctionBuilder, result: Value) -> Value {
    let nonneg = b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, result, 0);
    let as_int = b.ins().uextend(types::I64, result);
    let as_float = b.ins().fcvt_from_uint(types::F64, result);
    let bits = b.ins().bitcast(types::I64, MemFlags::new(), as_float);
    let addend = b.ins().iconst(types::I64, FLOAT64_ADDEND);
    let as_float = b.ins().isub(bits, addend);
    b.ins().select(nonneg, as_int, as_float)
}

/// The `tag == 0` (Int) test a binary fast path emits per unchecked operand.
pub(in crate::jit) fn is_int(b: &mut FunctionBuilder, value: Value) -> Value {
    let tag = b.ins().ushr_imm(value, 48);
    b.ins().icmp_imm(IntCC::Equal, tag, 0)
}

/// The `tag == 0xffff` (Object) test. The tag sits in the high 16 bits of the
/// encoded value and the object tag is -1, so this runs before any dereference.
pub(super) fn is_object(b: &mut FunctionBuilder, value: Value) -> Value {
    let tag = b.ins().ushr_imm(value, 48);
    b.ins().icmp_imm(IntCC::Equal, tag, 0xffff)
}

/// The `tag < Symbol(=-8) || tag > Exception(=6)` test that recognizes an
/// encoded `Float64` (`JSValueTag::is_float64`): the addend shift keeps every
/// real double's raw tag outside the non-float window.
pub(super) fn is_float64(b: &mut FunctionBuilder, value: Value) -> Value {
    let tag = b.ins().sshr_imm(value, 48);
    let below = b.ins().icmp_imm(
        IntCC::SignedLessThan,
        tag,
        i64::from(crate::JSValueTag::Symbol as i32),
    );
    let above = b.ins().icmp_imm(
        IntCC::SignedGreaterThan,
        tag,
        i64::from(crate::JSValueTag::Exception as i32),
    );
    b.ins().bor(below, above)
}

/// Whether a value is one of the two numeric tags (`Int` or `Float64`), i.e.
/// the set the inline float path can convert to `f64` without running
/// `ToNumber`.
fn is_number(b: &mut FunctionBuilder, value: Value) -> Value {
    let int = is_int(b, value);
    let float = is_float64(b, value);
    b.ins().bor(int, float)
}

/// Unbox a value already known to be a `Float64`: the tagged bits plus the
/// addend are the double's raw bits (`js_value_get_float64`).
pub(super) fn unbox_float64(b: &mut FunctionBuilder, value: Value) -> Value {
    let addend = b.ins().iconst(types::I64, FLOAT64_ADDEND);
    let bits = b.ins().iadd(value, addend);
    b.ins().bitcast(types::F64, MemFlags::new(), bits)
}

/// Convert an `Int`-tagged value to `f64`: its low 32 bits are the `i32`.
pub(super) fn int_to_float64(b: &mut FunctionBuilder, value: Value) -> Value {
    let v = b.ins().ireduce(types::I32, value);
    b.ins().fcvt_from_sint(types::F64, v)
}

/// Convert a proven number to `f64`: widen an `Int` (known-int operands need no
/// check) or unbox a `Float64`. `is_float` is the runtime `is_float64` test for
/// an operand whose tag is not statically known.
fn number_to_float64(
    b: &mut FunctionBuilder,
    value: Value,
    known_int: bool,
    is_float: Option<Value>,
) -> Value {
    if known_int {
        return int_to_float64(b, value);
    }
    let from_float = unbox_float64(b, value);
    match is_float {
        Some(is_float) => {
            let from_int = int_to_float64(b, value);
            b.ins().select(is_float, from_float, from_int)
        }
        None => from_float,
    }
}

/// The engine's `f64_to_i32` (`number/convert.rs`): ECMAScript `ToInt32` —
/// truncate toward zero, then wrap modulo 2^32. Cranelift only has saturating
/// float→int conversions, so the wrap is the engine's exponent/mantissa
/// reduction, computed branch-free over its three exponent ranges.
pub(super) fn to_int32_from_float64(b: &mut FunctionBuilder, d: Value) -> Value {
    let bits = b.ins().bitcast(types::I64, MemFlags::new(), d);
    let exponent = b.ins().ushr_imm(bits, 52);
    let e = b.ins().band_imm(exponent, 0x7ff);
    // |d| < 2^31: the saturating conversion is exact (truncation).
    let small = b.ins().fcvt_to_sint_sat(types::I32, d);
    // 2^31 <= |d| < 2^63: the mantissa with its implicit bit, shifted so the
    // bit-32 boundary is the wrapped result's sign bit.
    let mantissa = b.ins().band_imm(bits, (1i64 << 52) - 1);
    let sig = b.ins().bor_imm(mantissa, 1i64 << 52);
    let shift = b.ins().iadd_imm(e, -(1023 + 52 - 32));
    let shifted = b.ins().ishl(sig, shift);
    let top = b.ins().ushr_imm(shifted, 32);
    let low = b.ins().ireduce(types::I32, top);
    let negated = b.ins().ineg(low);
    let sign = b.ins().ushr_imm(bits, 63);
    let is_negative = b.ins().icmp_imm(IntCC::NotEqual, sign, 0);
    let wrapped = b.ins().select(is_negative, negated, low);
    let in_mid = b
        .ins()
        .icmp_imm(IntCC::UnsignedLessThanOrEqual, e, 1023 + 30 + 53);
    let zero = b.ins().iconst(types::I32, 0);
    let wide = b.ins().select(in_mid, wrapped, zero);
    let is_small = b
        .ins()
        .icmp_imm(IntCC::UnsignedLessThanOrEqual, e, 1023 + 30);
    b.ins().select(is_small, small, wide)
}

/// Raw `js_new_float64` encoder: any NaN payload becomes the engine's
/// canonical quiet NaN, everything else is the raw bits minus the addend
/// (which turns `-0.0`/`±Inf` into their tagged form unchanged). Internal
/// helper of [`box_float64_from_f64`] — box through that one for engine
/// semantics.
fn box_float64(b: &mut FunctionBuilder, d: Value) -> Value {
    let bits = b.ins().bitcast(types::I64, MemFlags::new(), d);
    let abs = b.ins().band_imm(bits, 0x7FFF_FFFF_FFFF_FFFF);
    let nan = b
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, abs, 0x7FF0_0000_0000_0000);
    let addend = b.ins().iconst(types::I64, FLOAT64_ADDEND);
    let encoded = b.ins().isub(bits, addend);
    let canonical = b.ins().iconst(types::I64, JS_NAN_U64_BITS);
    b.ins().select(nan, canonical, encoded)
}

/// Box a computed `f64` the way `From<f64> for JSValue` (`number/num.rs`)
/// does — an integral value that fits an `i32`, and is not `-0.0`, is an
/// `Int`; everything else is a `Float64`. The interpreter boxes every
/// computed f64 through `JSValue::from` (float `+`/`-`/`*`/`/` results and
/// typed-array element reads), so the differential suite compares per tag and
/// the generated code must agree — a raw `js_new_float64` box here would
/// hand back a `Float64` where the interpreter hands back an `Int`.
/// The saturating i32 round trip (`fcvt` → `fcvt` back, compare) disqualifies
/// NaN, ±Inf, fractions and out-of-range magnitudes on its own; only the
/// zero's sign bit needs a separate test.
pub(super) fn box_float64_from_f64(b: &mut FunctionBuilder, d: Value) -> Value {
    let boxed = box_float64(b, d);
    let iv = b.ins().fcvt_to_sint_sat(types::I32, d);
    let back = b.ins().fcvt_from_sint(types::F64, iv);
    let exact = b.ins().fcmp(FloatCC::Equal, back, d);
    // `-0.0`: integral and in range, but `JSValue::from` keeps it a `Float64`
    // (`-0.0` and `0` compare equal, yet `1/-0.0 === -Infinity` tells them
    // apart). `Float64(-d).to_bits()` is exactly `i64::MIN`.
    let bits = b.ins().bitcast(types::I64, MemFlags::new(), d);
    let neg_zero = b
        .ins()
        .icmp_imm(IntCC::Equal, bits, (-0.0f64).to_bits() as i64);
    let as_int = b.ins().uextend(types::I64, iv);
    let integral = b.ins().select(exact, as_int, boxed);
    b.ins().select(neg_zero, boxed, integral)
}

/// The CSE pass: this instruction is a repeat — the same block computed the
/// same expression over the same proven-numeric values, and its result is still
/// in `reg`. Copying it into the result slot is the whole instruction: the tag
/// checks, the arithmetic and the helper fallback are all skipped, which makes
/// the reuse both smaller and cheaper than recomputing.
pub(super) fn lower_cse_reuse(
    state: &mut State,
    b: &mut FunctionBuilder,
    sp: Value,
    reg: Value,
) -> Step {
    state.stack.flush(b, sp);
    b.ins().store(MemFlags::new(), reg, sp, -16);
    state.stack.consumed_binary();
    Step::Continue(b.ins().iadd_imm(sp, -8))
}

/// Two-int fast path for a binary operator, with the interpreter helper as the
/// slow path (mixed types, overflow, string `+`, …). Operands stay in the frame
/// buffer the whole time, so the helper sees exactly what the interpreter would.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_fast_binary(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    insn: &Instruction,
    op: FastOp,
    helper_id: usize,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    // Operand slots: `(top-1)` is the left operand, `top` the right one. Both
    // may live only in registers; the slow path flushes them before the helper.
    let top = state.stack.height;
    // The CSE pass: a later instruction reuses this one's result, so it must
    // leave that value in a register (`cont` carries it as a parameter).
    let keep_id = state.cse_keep(insn.pc());
    // The stack-slot register cache (guard elimination): an operand the compiler itself produced — a
    // `push_i32` literal or an `Int` constant — is statically an `Int`, so its
    // tag check is not emitted at all. Everything else (a local, a helper
    // result, `box_bit_result`, which may be a Float64) is still checked.
    let a_known = state.stack.known_int(top - 1);
    let c_known = state.stack.known_int(top);
    let a = state.stack.read(b, sp, top - 1);
    let c = state.stack.read(b, sp, top);
    let both_int = match (a_known, c_known) {
        (true, true) => None,
        (true, false) => Some(is_int(b, c)),
        (false, true) => Some(is_int(b, a)),
        (false, false) => {
            let int_a = is_int(b, a);
            let int_c = is_int(b, c);
            Some(b.ins().band(int_a, int_c))
        }
    };

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // A reused value has to leave this instruction in a register, so the
    // continuation carries it as a parameter (both paths deliver it).
    let kept = keep_id.is_some();
    if kept {
        b.append_block_param(cont, types::I64);
    }
    // Both paths must agree about memory. Everything except the two operands
    // materializes here, where the flush dominates the split; the operands are
    // dead on the fast path (the result overwrites one, the other lies above
    // the new `sp`) and are flushed by the helper's own preamble on the slow
    // path, which is the only path that still reads them.
    state.stack.flush_except(b, sp, &[top - 1, top]);
    // Float fast path: `add`/`sub`/`mul` over two numbers where at least one is a
    // `Float64` get an inline arm too. Bitwise/shift/comparison ops stay
    // Int-only, and two operands the compiler proved Int never need it.
    let float_ok = matches!(op, FastOp::Add | FastOp::Sub | FastOp::Mul);
    let float = match both_int {
        Some(_) if float_ok => Some(b.create_block()),
        _ => None,
    };
    let else_target = float.unwrap_or(slow);
    match both_int {
        Some(both_int) => {
            b.ins().brif(both_int, fast, &[], else_target, &[]);
        }
        // Both operands are already known: the check is gone, but the split
        // stays so the overflow bail below still has its target.
        None => {
            let yes = b.ins().iconst(types::I8, 1);
            b.ins().brif(yes, fast, &[], slow, &[]);
        }
    }

    if let Some(float) = float {
        b.switch_to_block(float);
        // Not both Int here, so "both numbers" means at least one `Float64`. A
        // known-Int operand needs no check; the others must pass `is_number`.
        let a_num = (!a_known).then(|| is_number(b, a));
        let c_num = (!c_known).then(|| is_number(b, c));
        let both_num = match (a_num, c_num) {
            (Some(x), Some(y)) => b.ins().band(x, y),
            (Some(x), None) => x,
            (None, Some(y)) => y,
            (None, None) => unreachable!("both known Int went to the Int arm"),
        };
        let float_fast = b.create_block();
        b.ins().brif(both_num, float_fast, &[], slow, &[]);

        b.switch_to_block(float_fast);
        let a_float = (!a_known).then(|| is_float64(b, a));
        let c_float = (!c_known).then(|| is_float64(b, c));
        let af = number_to_float64(b, a, a_known, a_float);
        let cf = number_to_float64(b, c, c_known, c_float);
        let f = match op {
            FastOp::Add => b.ins().fadd(af, cf),
            FastOp::Sub => b.ins().fsub(af, cf),
            _ => b.ins().fmul(af, cf),
        };
        // The interpreter boxes this through `From<f64>` (integral + i32
        // range folds to an Int), so the generated box must too.
        let boxed = box_float64_from_f64(b, f);
        b.ins().store(MemFlags::new(), boxed, sp, -16);
        let sp_next = b.ins().iadd_imm(sp, -8);
        b.def_var(sp_var, sp_next);
        let keep = [boxed];
        b.ins().jump(cont, if kept { &keep } else { &[] });
    }

    b.switch_to_block(fast);
    let ai = b.ins().ireduce(types::I32, a);
    let ci = b.ins().ireduce(types::I32, c);
    match op {
        FastOp::Add | FastOp::Sub | FastOp::Mul => {
            let (result, overflow) = match op {
                FastOp::Add => b.ins().sadd_overflow(ai, ci),
                FastOp::Sub => b.ins().ssub_overflow(ai, ci),
                _ => b.ins().smul_overflow(ai, ci),
            };
            // `0 * -1` / `-1 * 0` are -0, which an Int box cannot hold: bail to
            // the helper, which owns the Int-vs-Float64 sign rules.
            let bail = match op {
                FastOp::Mul => {
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
                }
                _ => overflow,
            };
            // Overflow escapes the i32 domain: the helper produces a Float64.
            let ok = b.create_block();
            b.ins().brif(bail, slow, &[], ok, &[]);
            b.switch_to_block(ok);
            let boxed = b.ins().uextend(types::I64, result);
            b.ins().store(MemFlags::new(), boxed, sp, -16);
            let sp_next = b.ins().iadd_imm(sp, -8);
            b.def_var(sp_var, sp_next);
            let keep = [boxed];
            b.ins().jump(cont, if kept { &keep } else { &[] });
        }
        FastOp::Lt
        | FastOp::Lte
        | FastOp::Gt
        | FastOp::Gte
        | FastOp::Eq
        | FastOp::Neq
        | FastOp::StrictEq
        | FastOp::StrictNeq => {
            let cond = match op {
                FastOp::Lt => b.ins().icmp(IntCC::SignedLessThan, ai, ci),
                FastOp::Lte => b.ins().icmp(IntCC::SignedLessThanOrEqual, ai, ci),
                FastOp::Gt => b.ins().icmp(IntCC::SignedGreaterThan, ai, ci),
                FastOp::Gte => b.ins().icmp(IntCC::SignedGreaterThanOrEqual, ai, ci),
                FastOp::Eq | FastOp::StrictEq => b.ins().icmp(IntCC::Equal, ai, ci),
                _ => b.ins().icmp(IntCC::NotEqual, ai, ci),
            };
            let widened = b.ins().uextend(types::I64, cond);
            let boxed = b.ins().bor_imm(widened, BOOL_TAG);
            b.ins().store(MemFlags::new(), boxed, sp, -16);
            let sp_next = b.ins().iadd_imm(sp, -8);
            b.def_var(sp_var, sp_next);
            let keep = [boxed];
            b.ins().jump(cont, if kept { &keep } else { &[] });
        }
        _ => {
            // ToInt32 of an int is itself; shifts use the low 5 bits of the
            // count, exactly like `WrappingOp::shl/shr/sar`.
            let count = b.ins().band_imm(ci, 31);
            let result = match op {
                FastOp::And => b.ins().band(ai, ci),
                FastOp::Or => b.ins().bor(ai, ci),
                FastOp::Xor => b.ins().bxor(ai, ci),
                FastOp::Shl => b.ins().ishl(ai, count),
                FastOp::Shr => b.ins().ushr(ai, count),
                _ => b.ins().sshr(ai, count),
            };
            // `shl`/`sar`/`and`/`or`/`xor` are `From<i32>` (always an Int);
            // `shr` is `From<u32>` — Int, or a Float64 once the sign bit is
            // set. Match the interpreter exactly, quirk included.
            let boxed = match op {
                FastOp::Shl | FastOp::Sar | FastOp::And | FastOp::Or | FastOp::Xor => {
                    b.ins().uextend(types::I64, result)
                }
                _ => box_bit_result(b, result),
            };
            b.ins().store(MemFlags::new(), boxed, sp, -16);
            let sp_next = b.ins().iadd_imm(sp, -8);
            b.def_var(sp_var, sp_next);
            let keep = [boxed];
            b.ins().jump(cont, if kept { &keep } else { &[] });
        }
    }

    b.switch_to_block(slow);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, helper_id, pc_after);
    b.def_var(sp_var, sp_slow);
    // The helper left the result in slot `top-1` (relative to `sp_slow`). The
    // placeholder is never read when nothing reuses this value (the jump passes
    // no argument then).
    let mut slow_result = [sp_slow];
    if kept {
        slow_result[0] = b.ins().load(types::I64, MemFlags::new(), sp_slow, -8);
    }
    b.ins().jump(cont, if kept { &slow_result } else { &[] });

    b.switch_to_block(cont);
    // Both paths store the result into slot `top-1`; the operand entries are
    // now stale (a flush would overwrite that result), so drop them.
    state.stack.consumed_binary();
    if let (Some(keep_id), Some(&value)) = (keep_id, b.block_params(cont).first()) {
        state.cse_regs.insert(keep_id, value);
    }
    // Local phi design: `shl`/`sar` box `From<i32>`, so *both* paths deliver an
    // `Int` — the merged slot is a known-Int fact with no phi needed.
    if matches!(op, FastOp::Shl | FastOp::Sar) {
        let sp_next = b.use_var(sp_var);
        let v = b.ins().load(types::I64, MemFlags::new(), sp_next, -8);
        return Step::Continue(state.stack.result_int(sp_next, v));
    }
    Step::Continue(b.use_var(sp_var))
}
/// Float fast path: inline `Float64` division. `div` always produces a `Float64`
/// (`Int/Int` divides as `f64` in the interpreter: `TwoNumbers::div`), so there
/// is no Int arm — any two proven numbers convert to `f64` and divide.
/// Everything else (BigInt, string, `ToPrimitive`) falls back to the helper.
/// The float-heavy kernels spend most of their arithmetic here.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_fast_div(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    insn: &Instruction,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let top = state.stack.height;
    let a_known = state.stack.known_int(top - 1);
    let c_known = state.stack.known_int(top);
    let a = state.stack.read(b, sp, top - 1);
    let c = state.stack.read(b, sp, top);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // Like `lower_fast_binary`: materialize everything but the operands; the
    // helper's own preamble flushes those on the slow path.
    state.stack.flush_except(b, sp, &[top - 1, top]);
    let both_num = match (a_known, c_known) {
        (true, true) => None,
        (true, false) => Some(is_number(b, c)),
        (false, true) => Some(is_number(b, a)),
        (false, false) => {
            let a_num = is_number(b, a);
            let c_num = is_number(b, c);
            Some(b.ins().band(a_num, c_num))
        }
    };
    match both_num {
        Some(both_num) => {
            b.ins().brif(both_num, fast, &[], slow, &[]);
        }
        // Both operands are already known Int: Int/Int still divides through
        // `f64`, so this arm is the same as the float one.
        None => {
            let yes = b.ins().iconst(types::I8, 1);
            b.ins().brif(yes, fast, &[], slow, &[]);
        }
    }

    b.switch_to_block(fast);
    let a_float = (!a_known).then(|| is_float64(b, a));
    let c_float = (!c_known).then(|| is_float64(b, c));
    let af = number_to_float64(b, a, a_known, a_float);
    let cf = number_to_float64(b, c, c_known, c_float);
    let quotient = b.ins().fdiv(af, cf);
    // Same boxing rule as `+`/`-`/`*`: the interpreter boxes the quotient
    // through `From<f64>` (integral + i32 range folds to an Int).
    let boxed = box_float64_from_f64(b, quotient);
    b.ins().store(MemFlags::new(), boxed, sp, -16);
    let sp_next = b.ins().iadd_imm(sp, -8);
    b.def_var(sp_var, sp_next);
    b.ins().jump(cont, &[]);

    b.switch_to_block(slow);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_DIV, pc_after);
    b.def_var(sp_var, sp_slow);
    b.ins().jump(cont, &[]);

    b.switch_to_block(cont);
    state.stack.consumed_binary();
    Step::Continue(b.use_var(sp_var))
}

/// Speculative `int` fast path for `local += operand`, emitted
/// only when type feedback proved the site int-monomorphic.
///
/// A non-int operand or an `i32` overflow falls back to the `add_loc` helper —
/// which is the interpreter's own `arith_add` operating on the same frame — so
/// the result stays bit-identical **without leaving native code**: zhuque's
/// helpers *are* the interpreter, so the fallback is a helper call, not a
/// deopt to the interpreter.
pub(super) fn lower_add_loc_int(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    insn: &Instruction,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let idx = insn.operand_u8(1) as i32;
    let byte_off = idx * 8;

    let local = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.var_base, byte_off);
    let top = state.stack.height;
    let operand = state.stack.read_top(b, sp);
    let tag_local = b.ins().ushr_imm(local, 48);
    let tag_operand = b.ins().ushr_imm(operand, 48);
    let int_local = b.ins().icmp_imm(IntCC::Equal, tag_local, 0);
    let int_operand = b.ins().icmp_imm(IntCC::Equal, tag_operand, 0);
    let both_int = b.ins().band(int_local, int_operand);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // The new local value travels to `cont` so a later read in the block reuses
    // it (the local phi design). Both paths write the same slot, so a phi is exact.
    b.append_block_param(cont, types::I64);
    // Like `lower_fast_binary`: materialize everything but the operand, which
    // only the slow path's helper still needs in memory.
    state.stack.flush_except(b, sp, &[top]);
    b.ins().brif(both_int, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    let li = b.ins().ireduce(types::I32, local);
    let oi = b.ins().ireduce(types::I32, operand);
    let (sum, overflow) = b.ins().sadd_overflow(li, oi);
    // Overflow escapes the i32 domain; the helper boxes it as a Float64.
    let ok = b.create_block();
    b.ins().brif(overflow, slow, &[], ok, &[]);
    b.switch_to_block(ok);
    let boxed = b.ins().uextend(types::I64, sum);
    b.ins().store(MemFlags::new(), boxed, fv.var_base, byte_off);
    let sp_next = b.ins().iadd_imm(sp, -8);
    b.def_var(sp_var, sp_next);
    b.ins().jump(cont, &[boxed]);

    b.switch_to_block(slow);
    // Leave the operand on the stack and set `imm`; the helper pops it and
    // re-derives the full `arith_add` semantics (string concat, ToPrimitive, …).
    let imm = b.ins().iconst(types::I32, i64::from(idx));
    b.ins().store(MemFlags::new(), imm, fv.frame, O_IMM);
    // Tier-down instrumentation: mark this helper call as the bail of the int fast
    // path, so the helper tallies it as a guard miss.
    let miss = b.ins().iconst(types::I32, i64::from(SPEC_MISS_IMM2 as i32));
    b.ins().store(MemFlags::new(), miss, fv.frame, O_IMM2);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_ADD_LOC, pc_after);
    b.def_var(sp_var, sp_slow);
    let slow_local = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.var_base, byte_off);
    b.ins().jump(cont, &[slow_local]);

    b.switch_to_block(cont);
    // Pop2/push1 would be wrong here: this op only pops the operand — the
    // result went into the *local* (fast path stores it, helper writes it).
    state.stack.drop_top();
    state
        .locals
        .fill(false, idx as u32, b.block_params(cont)[0]);
    Step::Continue(b.use_var(sp_var))
}

/// Speculative `int` fast path for `local++`/`local--`, emitted
/// only when type feedback proved the site int-monomorphic. The stack is
/// untouched; a non-int local or an `i32` overflow falls back to the
/// `inc_loc`/`dec_loc` helper (see [`lower_add_loc_int`] for why that is a
/// helper call rather than an interpreter exit).
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_incdec_loc_int(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    insn: &Instruction,
    is_inc: bool,
    helper_id: usize,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let idx = insn.operand_u8(1) as i32;
    let byte_off = idx * 8;

    let local = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.var_base, byte_off);
    let local_is_int = is_int(b, local);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // Carry the new local value across the merge for a later read.
    b.append_block_param(cont, types::I64);
    // No stack operand here, but the model still has to match both paths:
    // materialize before the split, where the stores dominate it.
    state.stack.flush(b, sp);
    b.ins().brif(local_is_int, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    let li = b.ins().ireduce(types::I32, local);
    let one = b.ins().iconst(types::I32, 1);
    let (res, overflow) = if is_inc {
        b.ins().sadd_overflow(li, one)
    } else {
        b.ins().ssub_overflow(li, one)
    };
    // `i32::MAX`/`MIN` promote to a Float64 in the interpreter; the helper
    // owns that boundary, so hand the op back rather than duplicating it.
    let ok = b.create_block();
    b.ins().brif(overflow, slow, &[], ok, &[]);
    b.switch_to_block(ok);
    let boxed = b.ins().uextend(types::I64, res);
    b.ins().store(MemFlags::new(), boxed, fv.var_base, byte_off);
    b.def_var(sp_var, sp);
    b.ins().jump(cont, &[boxed.into()]);

    b.switch_to_block(slow);
    let imm = b.ins().iconst(types::I32, i64::from(idx));
    b.ins().store(MemFlags::new(), imm, fv.frame, O_IMM);
    // Tier-down instrumentation: mark this helper call as the bail of the int fast
    // path, so the helper tallies it as a guard miss.
    let miss = b.ins().iconst(types::I32, i64::from(SPEC_MISS_IMM2 as i32));
    b.ins().store(MemFlags::new(), miss, fv.frame, O_IMM2);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, helper_id, pc_after);
    b.def_var(sp_var, sp_slow);
    let slow_local = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.var_base, byte_off);
    b.ins().jump(cont, &[slow_local.into()]);

    b.switch_to_block(cont);
    state
        .locals
        .fill(false, idx as u32, b.block_params(cont)[0]);
    Step::Continue(b.use_var(sp_var))
}
