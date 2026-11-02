use super::*;
use crate::{
    bytecode::opcodes as op,
    jit::{
        bytecode::{VerifyLimits, verify},
        feedback::CLASS_FLOAT,
        snapshot::{CompileSnapshot, ConstMeta},
    },
};

/// Hand-built snapshot: `stack_size` is generous; index checks use `args`/
/// `vars`; no global var refs or constants unless a test adds them.
fn snapshot(bytes: Vec<u8>, args: u16, vars: u16) -> CompileSnapshot {
    CompileSnapshot::from_raw(bytes, args, vars, 0, 16)
}

/// `Some(Ok)` = verified and compiled; panics if verification rejects (a
/// test case being malformed is a bug in the test, not the compiler).
fn compile_bytes(bytes: Vec<u8>, args: u16, vars: u16) -> Result<(), JitError> {
    let isa = crate::jit::build_host_isa().unwrap();
    let verified = verify(snapshot(bytes, args, vars), VerifyLimits::default())
        .unwrap_or_else(|e| panic!("verify rejected: {e:?}"));
    compile(&verified, isa.as_ref(), &Cell::new(0), Tier::Tier1).map(|_| ())
}

/// Compiled code size for `bytes`, with one constant pool entry of tag
/// `const_tag` (0 = Int, 8 = Float64).
fn compile_len(bytes: Vec<u8>, vars: u16, const_tag: i16) -> usize {
    let mut snap = snapshot(bytes, 0, vars);
    snap.const_meta = Box::new([ConstMeta { tag: const_tag }]);
    let isa = crate::jit::build_host_isa().unwrap();
    let verified =
        verify(snap, VerifyLimits::default()).unwrap_or_else(|e| panic!("verify rejected: {e:?}"));
    compile(&verified, isa.as_ref(), &Cell::new(0), Tier::Tier1)
        .expect("compile")
        .len()
}

/// The CSE pass: a repeated numeric expression is computed once — the
/// second occurrence copies the register the first one left, so its tag
/// checks, arithmetic and helper fallback disappear. The same shape over a
/// *different* local cannot reuse anything and keeps all of that.
#[test]
fn cse_shrinks_the_code() {
    // The store gives the local a numeric fact (`push_i32`), which is what
    // the pass needs before it may claim an operand pair is pure.
    let literal = |idx: u16, value: i32| seq(&[&push_i32(value), &loc(op::OP_put_loc, idx)]);
    let shape = |second: u16| {
        seq(&[
            &literal(0, 1),
            &literal(1, 2),
            &loc(op::OP_get_loc, 0),
            &loc(op::OP_get_loc, 0),
            &[op::OP_add],
            &loc(op::OP_get_loc, second),
            &loc(op::OP_get_loc, second),
            &[op::OP_add],
            &[op::OP_add],
            &loc(op::OP_put_loc, 2),
            &[op::OP_return_undef],
        ])
    };
    // `(x+x) + (x+x)` reuses; `(x+x) + (y+y)` cannot.
    let reused = compile_len(shape(0), 3, 0);
    let distinct = compile_len(shape(1), 3, 0);
    assert!(
        reused < distinct,
        "the reused expression should be a copy: {reused} vs {distinct} bytes"
    );
}

/// The stack-slot register cache (guard elimination), measured deterministically: the same op
/// sequence needs a tag check for an operand that is *not* statically known.
/// An `Int` constant from the pool is known, a `Float64` one is not, so the
/// two compilations differ exactly by the check the second one emits.
#[test]
fn guard_elimination_shrinks_the_code() {
    let bytes = seq(&[
        &loc(op::OP_get_loc, 0),
        &[op::OP_push_const],
        &u32b(0),
        &[op::OP_mul],
        &loc(op::OP_put_loc, 0),
        &[op::OP_return_undef],
    ]);
    let known = compile_len(bytes.clone(), 1, 0);
    let checked = compile_len(bytes, 1, 8);
    assert!(
        known < checked,
        "the known-Int operand skips its check: {known} vs {checked} bytes"
    );
}

/// Local phi design: `shl`/`sar` box `From<i32>` on *both* paths, so their
/// merged result is a known `Int` and the consumer skips its tag check.
/// `shr` boxes `From<u32>` (possibly a Float64), so the identical shape is
/// bigger by exactly that check.
#[test]
fn shift_results_are_known_int() {
    let shape = |op: u8| {
        seq(&[
            &push_i32(3),
            &push_i32(1),
            &[op],
            &[op::OP_push_const],
            &u32b(0),
            &[op::OP_add],
            &loc(op::OP_put_loc, 0),
            &[op::OP_return_undef],
        ])
    };
    let shl = compile_len(shape(op::OP_shl), 1, 0);
    let shr = compile_len(shape(op::OP_shr), 1, 0);
    assert!(
        shl < shr,
        "a shift result skips the consumer's check: {shl} vs {shr} bytes"
    );
}

/// The merged literal: the atoms ride in `JitExecFrame::lit_atoms`,
/// addressed by generated code through `O_LIT_ATOMS`. Pin the offset the
/// way the other frame-layout contracts are pinned — a field that moves
/// without the constant moving would make the stores land elsewhere in the
/// frame (silently, since the helper reads the array by field, not offset).
#[test]
fn the_literal_atom_offset_is_stable() {
    assert_eq!(
        O_LIT_ATOMS as usize,
        offset_of!(JitExecFrame, lit_atoms),
        "O_LIT_ATOMS must track the frame field"
    );
    // The generated code stores one `u32` per field, up to LIT_ATOM_MAX;
    // those stores must stay inside the array and off the neighbouring
    // immediates the same call sequence writes (`imm` carries the count).
    let start = O_LIT_ATOMS as usize;
    let end = start + LIT_ATOM_MAX * 4;
    for neighbour in [
        offset_of!(JitExecFrame, imm),
        offset_of!(JitExecFrame, imm2),
    ] {
        assert!(
            end <= neighbour || neighbour + 4 <= start,
            "the atom stores overlap another immediate"
        );
    }
}

/// The window scan: `object` plus a run of (pure push, `define_field`)
/// pairs merges; a field value the window cannot defer (here `add`, which
/// may call a helper) or a duplicate atom (`{a:1, a:2}`, an update rather
/// than the batch append) truncates it, and the compile still succeeds.
#[test]
fn literal_window_scan_and_fallbacks() {
    let verified = |parts: &[&[u8]]| {
        let snapshot = snapshot(seq(parts), 0, 2);
        verify(snapshot, VerifyLimits::default()).expect("verify hand-written body")
    };
    let define = |atom: u32| seq(&[&[op::OP_define_field], &u32b(atom)]);

    // `{a: <push>, b: <push>}`: both fields merge into one window.
    let merged = verified(&[
        &[op::OP_object],
        &push_i32(1),
        &define(1),
        &push_i32(2),
        &define(2),
        &[op::OP_return],
    ]);
    let windows = literal_windows(&merged, &[]);
    assert_eq!(windows.len(), 1, "one merged literal window");
    let window = &windows[&0];
    assert_eq!(window.atoms, vec![1, 2], "both atoms in source order");
    assert_eq!(window.value_indices.len(), 2, "both value pushes");
    assert!(
        window.end_pc > merged.instructions()[4].pc(),
        "the window ends past the last define_field"
    );

    // `{a: <push>, b: x + y}`: `add` can call a helper, so the run stops
    // after the first field and the rest keeps the per-op path.
    let truncated = verified(&[
        &[op::OP_object],
        &push_i32(1),
        &define(1),
        &loc(op::OP_get_loc, 0),
        &loc(op::OP_get_loc, 1),
        &[op::OP_add],
        &define(2),
        &[op::OP_return],
    ]);
    let windows = literal_windows(&truncated, &[]);
    assert_eq!(windows.len(), 1, "the safe prefix still merges");
    assert_eq!(windows[&0].atoms, vec![1], "only the pure field merges");

    // `{a: <push>, a: <push>}`: the duplicate atom is an update, not the
    // batch's append, so the window stops before it.
    let duplicate = verified(&[
        &[op::OP_object],
        &push_i32(1),
        &define(7),
        &push_i32(2),
        &define(7),
        &[op::OP_return],
    ]);
    let windows = literal_windows(&duplicate, &[]);
    assert_eq!(windows.len(), 1);
    assert_eq!(
        windows[&0].atoms,
        vec![7],
        "the duplicate truncates the run"
    );

    // An OSR resume point inside the window would re-enter with the
    // un-merged stack shape, so it stops the merge there entirely.
    let osr_pc = merged.instructions()[1].pc();
    let windows = literal_windows(&merged, &[osr_pc]);
    assert!(
        windows.is_empty(),
        "an OSR point inside the run disables the merge"
    );
}

/// Cross-block facts: a local's `Int32` proof survives a **loop header** when
/// every edge into it agrees, so a loop-invariant bound is known-Int on every
/// iteration and the comparison in the header needs no tag check.
///
/// The two compilations are the *same bytes* and differ only in the tag the
/// pool constant carries (0 = Int, 8 = Float64), so the byte gap is exactly
/// the check the cross-block fact removes. Before this fact the header was widened
/// on the way in, and both shapes compiled identically.
#[test]
fn a_loop_invariant_int_bound_keeps_its_fact() {
    // i = 0; limit = <const>;                        (preheader)
    // L: get_loc i; get_loc limit; lt; if_false E;   (header)
    //    get_loc limit; push_i32 1; add; put_loc 3;  (body: sum up)
    //    inc_loc i; goto L;
    // E: return_undef
    let shape = |preheader: Vec<u8>| {
        let mut bytes = preheader;
        let header = bytes.len() as i32;
        bytes.extend_from_slice(&loc(op::OP_get_loc, 0));
        bytes.extend_from_slice(&loc(op::OP_get_loc, 1));
        bytes.extend_from_slice(&[op::OP_lt]);
        let if_pc = bytes.len() as i32;
        bytes.push(op::OP_if_false);
        bytes.extend_from_slice(&0i32.to_le_bytes());
        let body = bytes.len() as i32;
        bytes.extend_from_slice(&loc(op::OP_get_loc, 1));
        bytes.extend_from_slice(&push_i32(1));
        bytes.push(op::OP_add);
        bytes.extend_from_slice(&loc(op::OP_put_loc, 3));
        bytes.extend_from_slice(&loc8(op::OP_inc_loc, 0));
        let goto_pc = bytes.len() as i32;
        bytes.push(op::OP_goto);
        bytes.extend_from_slice(&0i32.to_le_bytes());
        let end = bytes.len() as i32;
        bytes.push(op::OP_return_undef);
        // Branch offsets are relative to the byte after the opcode.
        bytes[(if_pc as usize + 1)..(if_pc as usize + 5)]
            .copy_from_slice(&(end - (if_pc + 1)).to_le_bytes());
        bytes[(goto_pc as usize + 1)..(goto_pc as usize + 5)]
            .copy_from_slice(&(header - (goto_pc + 1)).to_le_bytes());
        assert!(body < end);
        bytes
    };
    // Both shapes are the *same* bytes: `limit` is stored from a pool constant,
    // and only the constant's tag differs (0 = Int, 8 = Float64). The gap is
    // exactly the checks the cross-block fact removes from the header.
    let body = shape(seq(&[
        &push_i32(0),
        &loc(op::OP_put_loc, 0),
        &[op::OP_push_const],
        &u32b(0),
        &loc(op::OP_put_loc, 1),
    ]));
    let proven = compile_len(body.clone(), 4, 0);
    let unproven = compile_len(body, 4, 8);
    assert!(
        proven < unproven,
        "a loop-invariant Int bound keeps its fact: {proven} vs {unproven} bytes"
    );
}

/// Local liveness: a local the verifier proves `Int32` carries the fact
/// through `get_loc`, so its consumer skips a check. The two shapes are the
/// same bytecode; only the initializing producer differs, so the byte-size
/// gap is the checks the `Int32` proof removes.
#[test]
fn proven_int_local_skips_the_check() {
    let tail = seq(&[
        &loc(op::OP_get_loc, 0),
        &[op::OP_push_const],
        &u32b(0),
        &[op::OP_mul],
        &loc(op::OP_put_loc, 1),
        &[op::OP_return_undef],
    ]);
    // `push_i32` proves the local `Int32`; a non-numeric `push_const` (tag
    // 2) leaves it `Tagged` and its constant operand unknown too.
    let int_local = seq(&[&push_i32(5), &loc(op::OP_put_loc, 0), &tail]);
    let tagged_local = seq(&[
        &[op::OP_push_const],
        &u32b(0),
        &loc(op::OP_put_loc, 0),
        &tail,
    ]);
    let proven = compile_len(int_local, 2, 0);
    let unproven = compile_len(tagged_local, 2, 2);
    assert!(
        proven < unproven,
        "a proven-Int local feeds the guard fact: {proven} vs {unproven} bytes"
    );
}

/// Like `compile_bytes`, but with `gv` global-variable references so
/// `get_var`/`put_var` index checks pass.
fn compile_bytes_gv(bytes: Vec<u8>, args: u16, vars: u16, gv: u16) -> Result<(), JitError> {
    let isa = crate::jit::build_host_isa().unwrap();
    let snap = CompileSnapshot::from_raw(bytes, args, vars, gv, 16);
    let verified =
        verify(snap, VerifyLimits::default()).unwrap_or_else(|e| panic!("verify rejected: {e:?}"));
    compile(&verified, isa.as_ref(), &Cell::new(0), Tier::Tier1).map(|_| ())
}

fn seq(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}
fn u16b(x: u16) -> [u8; 2] {
    x.to_le_bytes()
}
fn i32b(x: i32) -> [u8; 4] {
    x.to_le_bytes()
}
fn u32b(x: u32) -> [u8; 4] {
    x.to_le_bytes()
}
fn push_i32(x: i32) -> [u8; 5] {
    let mut out = [0u8; 5];
    out[0] = op::OP_push_i32;
    out[1..].copy_from_slice(&i32b(x));
    out
}

/// Local-indexed opcode: `[op, idx:u16]`.
fn loc(opcode: u8, idx: u16) -> [u8; 3] {
    let mut out = [0u8; 3];
    out[0] = opcode;
    out[1..].copy_from_slice(&u16b(idx));
    out
}
/// `[op, idx:u8]` (LOC8).
fn loc8(opcode: u8, idx: u8) -> [u8; 2] {
    [opcode, idx]
}
/// `[op, off:i32]`.
fn branch(opcode: u8, off: i32) -> [u8; 5] {
    let mut out = [0u8; 5];
    out[0] = opcode;
    out[1..].copy_from_slice(&i32b(off));
    out
}

/// `get_field`/`put_field` encoding: `[op, atom:u32]`, plus `[ic_idx:u16]`
/// when the inline cache is compiled in.
fn field(opcode: u8, ic: u16) -> Vec<u8> {
    let mut out = vec![opcode];
    out.extend_from_slice(&u32b(0));
    #[cfg(feature = "inline_cache")]
    out.extend_from_slice(&u16b(ic));
    let _ = ic;
    out
}

/// `get_array_el*`/`put_array_el` encoding: `[op]`, plus `[ic_idx:u16]` when
/// the inline cache is compiled in.
fn array_el(opcode: u8) -> Vec<u8> {
    let mut out = vec![opcode];
    #[cfg(feature = "inline_cache")]
    out.extend_from_slice(&u16b(0));
    out
}

/// Every opcode the baseline claims to support must lower without a
/// Cranelift error. This is the coverage net for the lowering `match`.
#[test]
fn every_supported_opcode_lowers() {
    let ret = [op::OP_return];
    let mut cases: Vec<(&str, Vec<u8>, u16, u16)> = Vec::new();

    // Pushes / constants.
    cases.push(("push_i32", seq(&[&push_i32(7), &ret]), 0, 0));
    cases.push((
        "push_atom_value",
        seq(&[&[op::OP_push_atom_value], &u32b(0), &ret]),
        0,
        0,
    ));
    for name in ["undefined", "null", "push_false", "push_true", "push_this"] {
        let code = match name {
            "undefined" => op::OP_undefined,
            "null" => op::OP_null,
            "push_false" => op::OP_push_false,
            "push_true" => op::OP_push_true,
            _ => op::OP_push_this,
        };
        cases.push((name, seq(&[&[code], &ret]), 0, 0));
    }

    // Stack shuffles: five operands is more than any shuffle pops.
    let shuffles: &[(&str, u8)] = &[
        ("drop", op::OP_drop),
        ("nip", op::OP_nip),
        ("nip1", op::OP_nip1),
        ("dup", op::OP_dup),
        ("dup1", op::OP_dup1),
        ("dup2", op::OP_dup2),
        ("dup3", op::OP_dup3),
        ("insert2", op::OP_insert2),
        ("insert3", op::OP_insert3),
        ("insert4", op::OP_insert4),
        ("perm3", op::OP_perm3),
        ("perm4", op::OP_perm4),
        ("perm5", op::OP_perm5),
        ("swap", op::OP_swap),
        ("swap2", op::OP_swap2),
        ("rot3l", op::OP_rot3l),
        ("rot3r", op::OP_rot3r),
        ("rot4l", op::OP_rot4l),
        ("rot5l", op::OP_rot5l),
    ];
    for (name, code) in shuffles {
        let mut bytes = Vec::new();
        for i in 0..5 {
            bytes.extend_from_slice(&push_i32(i));
        }
        bytes.extend_from_slice(&[*code]);
        bytes.extend_from_slice(&ret);
        cases.push((name, bytes, 0, 0));
    }

    // Locals and arguments.
    cases.push(("get_loc", seq(&[&loc(op::OP_get_loc, 0), &ret]), 0, 1));
    cases.push((
        "put_loc",
        seq(&[&push_i32(0), &loc(op::OP_put_loc, 0), &push_i32(1), &ret]),
        0,
        1,
    ));
    cases.push((
        "set_loc",
        seq(&[&push_i32(0), &loc(op::OP_set_loc, 0), &ret]),
        0,
        1,
    ));
    // Frame-entry TDZ prologue and its guarded read.
    cases.push((
        "set_loc_uninitialized",
        seq(&[
            &loc(op::OP_set_loc_uninitialized, 0),
            &[op::OP_return_undef],
        ]),
        0,
        1,
    ));
    cases.push((
        "get_loc_check",
        seq(&[&loc(op::OP_get_loc_check, 0), &ret]),
        0,
        1,
    ));
    cases.push(("get_arg", seq(&[&loc(op::OP_get_arg, 0), &ret]), 1, 0));
    cases.push((
        "put_arg",
        seq(&[&push_i32(0), &loc(op::OP_put_arg, 0), &push_i32(1), &ret]),
        1,
        0,
    ));
    cases.push((
        "set_arg",
        seq(&[&push_i32(0), &loc(op::OP_set_arg, 0), &ret]),
        1,
        0,
    ));
    cases.push((
        "inc_loc",
        seq(&[
            &push_i32(0),
            &loc(op::OP_put_loc, 0),
            &loc8(op::OP_inc_loc, 0),
            &push_i32(1),
            &ret,
        ]),
        0,
        1,
    ));
    cases.push((
        "dec_loc",
        seq(&[
            &push_i32(0),
            &loc(op::OP_put_loc, 0),
            &loc8(op::OP_dec_loc, 0),
            &push_i32(1),
            &ret,
        ]),
        0,
        1,
    ));

    // Terminators / no-ops.
    cases.push(("return_undef", vec![op::OP_return_undef], 0, 0));
    cases.push(("nop", seq(&[&[op::OP_nop], &[op::OP_return_undef]]), 0, 0));
    cases.push((
        "goto",
        seq(&[&branch(op::OP_goto, 4), &[op::OP_return_undef]]),
        0,
        0,
    ));
    // push 1; if_false L(16); push 2; return; L: return_undef
    let if_body = seq(&[
        &push_i32(1),
        &branch(op::OP_if_false, 10),
        &push_i32(2),
        &[op::OP_return, op::OP_return_undef],
    ]);
    cases.push(("if_false", if_body.clone(), 0, 0));
    let if_true_body = {
        let mut b = if_body.clone();
        b[5] = op::OP_if_true;
        b
    };
    cases.push(("if_true", if_true_body, 0, 0));

    // Helpers: binary then unary.
    let binary: &[(&str, u8)] = &[
        ("add", op::OP_add),
        ("sub", op::OP_sub),
        ("mul", op::OP_mul),
        ("div", op::OP_div),
        ("mod", op::OP_mod),
        ("lt", op::OP_lt),
        ("lte", op::OP_lte),
        ("gt", op::OP_gt),
        ("gte", op::OP_gte),
        ("eq", op::OP_eq),
        ("neq", op::OP_neq),
        ("strict_eq", op::OP_strict_eq),
        ("strict_neq", op::OP_strict_neq),
        ("and", op::OP_and),
        ("or", op::OP_or),
        ("xor", op::OP_xor),
        ("shl", op::OP_shl),
        ("sar", op::OP_sar),
        ("shr", op::OP_shr),
    ];
    for (name, code) in binary {
        cases.push((
            name,
            seq(&[&push_i32(1), &push_i32(2), &[*code], &ret]),
            0,
            0,
        ));
    }
    let unary: &[(&str, u8)] = &[
        ("plus", op::OP_plus),
        ("neg", op::OP_neg),
        ("inc", op::OP_inc),
        ("dec", op::OP_dec),
        ("lnot", op::OP_lnot),
        ("not", op::OP_not),
        ("typeof", op::OP_typeof),
    ];
    for (name, code) in unary {
        cases.push((name, seq(&[&push_i32(1), &[*code], &ret]), 0, 0));
    }

    // Array-element helpers. Stack effects differ per opcode.
    cases.push((
        "get_array_el",
        seq(&[
            &push_i32(0),
            &push_i32(0),
            &array_el(op::OP_get_array_el),
            &ret,
        ]),
        0,
        0,
    ));
    cases.push((
        "get_array_el2",
        seq(&[
            &push_i32(0),
            &push_i32(0),
            &array_el(op::OP_get_array_el2),
            &ret,
        ]),
        0,
        0,
    ));
    cases.push((
        "get_array_el3",
        seq(&[
            &push_i32(0),
            &push_i32(0),
            &array_el(op::OP_get_array_el3),
            &ret,
        ]),
        0,
        0,
    ));
    cases.push((
        "put_array_el",
        seq(&[
            &push_i32(0),
            &push_i32(0),
            &push_i32(0),
            &array_el(op::OP_put_array_el),
            &[op::OP_return_undef],
        ]),
        0,
        0,
    ));

    // Property helpers (`get_field`/`get_field2`/`put_field`).
    // Stack effects differ; the operand is atom + (inline_cache) ic index.
    let get_field = field(op::OP_get_field, 0);
    let get_field2 = field(op::OP_get_field2, 0);
    let put_field = field(op::OP_put_field, 0);
    cases.push(("get_field", seq(&[&push_i32(0), &get_field, &ret]), 0, 0));
    cases.push(("get_field2", seq(&[&push_i32(0), &get_field2, &ret]), 0, 0));
    cases.push((
        "put_field",
        seq(&[
            &push_i32(0),
            &push_i32(0),
            &put_field,
            &[op::OP_return_undef],
        ]),
        0,
        0,
    ));

    // The remaining stack ops.
    cases.push((
        "pow",
        seq(&[&push_i32(2), &push_i32(3), &[op::OP_pow], &ret]),
        0,
        0,
    ));
    for (name, code) in [
        ("to_object", op::OP_to_object),
        ("to_propkey", op::OP_to_propkey),
    ] {
        cases.push((name, seq(&[&push_i32(1), &[code], &ret]), 0, 0));
    }
    // `rest` spends its u16 operand to index the helper's argument window.
    cases.push(("rest", seq(&[&[op::OP_rest, 1, 0], &ret]), 2, 0));
    // Literal seeds: `object` pushes an empty base object; `array_from`
    // pops its NPOP operand and pushes one array.
    cases.push(("object", seq(&[&[op::OP_object, op::OP_return]]), 0, 0));
    cases.push((
        "array_from",
        seq(&[
            &push_i32(1),
            &push_i32(2),
            &[op::OP_array_from],
            &u16b(2),
            &ret,
        ]),
        0,
        0,
    ));
    // `??`'s guard: lowered inline (the tag of the top slot), not resumed.
    cases.push((
        "is_undefined_or_null",
        seq(&[&push_i32(1), &[op::OP_is_undefined_or_null], &ret]),
        0,
        0,
    ));
    // Frame-entry special objects (kind 3 = `new.target`) and sloppy `this`.
    cases.push((
        "special_object",
        seq(&[&[op::OP_special_object, 3], &ret]),
        0,
        0,
    ));
    // `set_name` peeks the function it names (atom operand).
    cases.push((
        "set_name",
        seq(&[&push_i32(0), &[op::OP_set_name], &u32b(0), &ret]),
        0,
        0,
    ));
    for (name, code) in [
        ("in", op::OP_in),
        ("instanceof", op::OP_instanceof),
        ("delete", op::OP_delete),
    ] {
        cases.push((
            name,
            seq(&[&push_i32(0), &push_i32(0), &[code], &ret]),
            0,
            0,
        ));
    }
    cases.push((
        "post_inc",
        seq(&[&push_i32(1), &[op::OP_post_inc], &ret]),
        0,
        0,
    ));
    cases.push((
        "post_dec",
        seq(&[&push_i32(1), &[op::OP_post_dec], &ret]),
        0,
        0,
    ));
    cases.push((
        "add_loc",
        seq(&[
            &push_i32(0),
            &loc(op::OP_put_loc, 0),
            &push_i32(0),
            &loc8(op::OP_add_loc, 0),
            &[op::OP_return_undef],
        ]),
        0,
        1,
    ));

    // Sync iteration: real `for-of`/`for-in` shapes. The
    // iterator window is [iter_obj, next, catch_offset]; `for_of_next`
    // appends value/done and `iterator_close` drops the window.
    cases.push((
        "for_of_start",
        seq(&[
            &push_i32(0),
            &[op::OP_for_of_start],
            &[op::OP_iterator_close, op::OP_return_undef],
        ]),
        0,
        0,
    ));
    cases.push((
        "for_of_next",
        seq(&[
            &push_i32(0),
            &[op::OP_for_of_start],
            &[op::OP_for_of_next, 0],
            &[op::OP_drop, op::OP_drop],
            &[op::OP_iterator_close, op::OP_return_undef],
        ]),
        0,
        0,
    ));
    cases.push((
        "for_in",
        seq(&[
            &push_i32(0),
            &[op::OP_for_in_start, op::OP_for_in_next],
            &[op::OP_drop, op::OP_drop, op::OP_drop],
            &[op::OP_return_undef],
        ]),
        0,
        0,
    ));
    cases.push((
        "iterator_get_value_done",
        seq(&[
            &push_i32(0),
            &[op::OP_iterator_get_value_done],
            &[op::OP_drop, op::OP_drop, op::OP_return_undef],
        ]),
        0,
        0,
    ));
    cases.push((
        "iterator_check_object",
        seq(&[
            &push_i32(0),
            &[
                op::OP_iterator_check_object,
                op::OP_drop,
                op::OP_return_undef,
            ],
        ]),
        0,
        0,
    ));
    cases.push((
        "iterator_next",
        seq(&[
            &push_i32(0),
            &[op::OP_for_of_start],
            &push_i32(0),
            &[op::OP_iterator_next, op::OP_drop],
            &[op::OP_iterator_close, op::OP_return_undef],
        ]),
        0,
        0,
    ));
    cases.push((
        "iterator_call",
        seq(&[
            &push_i32(0),
            &[op::OP_for_of_start],
            &push_i32(0),
            &[op::OP_iterator_call, 0, op::OP_drop, op::OP_drop],
            &[op::OP_iterator_close, op::OP_return_undef],
        ]),
        0,
        0,
    ));

    // Class / object-literal / private-field machinery. Each
    // entry pushes as many operands as the opcode pops, applies the opcode,
    // then drops whatever it left (so a `pops`/`pushes` mismatch fails to
    // even verify).
    let class_cases: Vec<(&str, Vec<u8>, usize, usize)> = vec![
        ("check_ctor", seq(&[&[op::OP_check_ctor]]), 0, 0),
        ("init_ctor", seq(&[&[op::OP_init_ctor]]), 0, 1),
        (
            "private_symbol",
            seq(&[&[op::OP_private_symbol], &u32b(0)]),
            0,
            1,
        ),
        ("get_super", seq(&[&[op::OP_get_super]]), 1, 1),
        ("private_in", seq(&[&[op::OP_private_in]]), 2, 1),
        ("check_brand", seq(&[&[op::OP_check_brand]]), 2, 2),
        ("add_brand", seq(&[&[op::OP_add_brand]]), 2, 0),
        (
            "get_private_field",
            seq(&[&[op::OP_get_private_field]]),
            2,
            1,
        ),
        (
            "define_field",
            seq(&[&[op::OP_define_field], &u32b(0)]),
            2,
            1,
        ),
        (
            "set_name_computed",
            seq(&[&[op::OP_set_name_computed]]),
            2,
            2,
        ),
        ("set_proto", seq(&[&[op::OP_set_proto]]), 2, 1),
        ("set_home_object", seq(&[&[op::OP_set_home_object]]), 2, 2),
        (
            "define_method",
            seq(&[&[op::OP_define_method], &u32b(0), &[0]]),
            2,
            1,
        ),
        (
            "define_class",
            seq(&[&[op::OP_define_class], &u32b(0), &[0]]),
            2,
            2,
        ),
        (
            "copy_data_properties",
            seq(&[&[op::OP_copy_data_properties, 0]]),
            3,
            3,
        ),
        ("define_array_el", seq(&[&[op::OP_define_array_el]]), 3, 2),
        ("append", seq(&[&[op::OP_append]]), 3, 2),
        (
            "put_private_field",
            seq(&[&[op::OP_put_private_field]]),
            3,
            0,
        ),
        (
            "define_private_field",
            seq(&[&[op::OP_define_private_field]]),
            3,
            1,
        ),
        (
            "define_method_computed",
            seq(&[&[op::OP_define_method_computed, 0]]),
            3,
            1,
        ),
        (
            "define_class_computed",
            seq(&[&[op::OP_define_class_computed], &u32b(0), &[0]]),
            3,
            3,
        ),
        ("get_super_value", seq(&[&[op::OP_get_super_value]]), 3, 1),
        ("put_super_value", seq(&[&[op::OP_put_super_value]]), 4, 0),
        ("close_loc", seq(&[&loc(op::OP_close_loc, 0)]), 0, 0),
    ];
    for (name, op_bytes, pops, pushes) in class_cases {
        let mut bytes = Vec::new();
        for i in 0..pops {
            bytes.extend_from_slice(&push_i32(i as i32));
        }
        bytes.extend_from_slice(&op_bytes);
        for _ in 0..pushes {
            bytes.push(op::OP_drop);
        }
        bytes.push(op::OP_return_undef);
        cases.push((name, bytes, 0, 1));
    }

    // Ref-cell family: `make_*_ref` push the ref object and its
    // property name; `get`/`put_ref_value` read/write through them. The
    // indices must fit their space (local/argument/closure).
    cases.push((
        "make_loc_ref",
        seq(&[
            &[op::OP_make_loc_ref],
            &u32b(0),
            &u16b(0),
            &[op::OP_drop, op::OP_drop, op::OP_return_undef],
        ]),
        0,
        1,
    ));
    cases.push((
        "make_arg_ref",
        seq(&[
            &[op::OP_make_arg_ref],
            &u32b(0),
            &u16b(0),
            &[op::OP_drop, op::OP_drop, op::OP_return_undef],
        ]),
        1,
        0,
    ));
    cases.push((
        "make_var_ref",
        seq(&[
            &[op::OP_make_var_ref],
            &u32b(0),
            &[op::OP_drop, op::OP_drop, op::OP_return_undef],
        ]),
        0,
        0,
    ));
    cases.push((
        "get_ref_value",
        seq(&[
            &push_i32(0),
            &push_i32(0),
            &[
                op::OP_get_ref_value,
                op::OP_drop,
                op::OP_drop,
                op::OP_return_undef,
            ],
        ]),
        0,
        0,
    ));
    cases.push((
        "put_ref_value",
        seq(&[
            &push_i32(0),
            &push_i32(0),
            &push_i32(0),
            &[op::OP_put_ref_value, op::OP_return_undef],
        ]),
        0,
        0,
    ));

    // `with_*`: the environment object(s) are the entry window, the
    // label is the hit edge, and the fall-through runs the ordinary variable
    // access the opcode guards. That access is sized so both edges reach the
    // label at the same height.
    for &(name, code, pops, pushed) in &[
        ("with_get_var", op::OP_with_get_var, 1, 1),
        // Its fall-through leaves the value `put_loc`/`set_loc` would take;
        // drop it, since the hit edge consumed the value too.
        ("with_put_var", op::OP_with_put_var, 2, 0),
        ("with_delete_var", op::OP_with_delete_var, 1, 1),
        ("with_make_ref", op::OP_with_make_ref, 1, 2),
        ("with_get_ref", op::OP_with_get_ref, 1, 2),
    ] {
        let access: Vec<u8> = if name == "with_put_var" {
            vec![op::OP_drop]
        } else {
            (0..pushed).flat_map(push_i32).collect()
        };
        let mut bytes = Vec::new();
        for i in 0..pops {
            bytes.extend_from_slice(&push_i32(i));
        }
        bytes.push(code);
        bytes.extend_from_slice(&u32b(0));
        // The label sits `5 + access.len()` bytes after the operand base.
        bytes.extend_from_slice(&i32b(5 + access.len() as i32));
        bytes.push(1);
        let label = bytes.len() + access.len();
        bytes.extend_from_slice(&access);
        bytes.push(op::OP_return_undef);
        assert_eq!(bytes.len(), label + 1);
        cases.push((name, bytes, 0, 0));
    }

    // Exception regions. `catch` pushes the handler sentinel;
    // `gosub`/`ret` are the `finally` subroutine.
    let gosub_ret = seq(&[
        &branch(op::OP_gosub, 5),
        &[op::OP_return_undef],
        &[op::OP_ret],
    ]);
    cases.push(("gosub_ret", gosub_ret, 0, 0));
    let catch_body = seq(&[
        &branch(op::OP_catch, 5),
        &[op::OP_return_undef],
        &[op::OP_return_undef],
    ]);
    cases.push(("catch", catch_body, 0, 0));

    // The engine-generated opcodes. `throw` pops its operand and
    // ends the block; `throw_error` moves nothing. `regexp` replaces the
    // slot below its id, `push_bigint_i32` pushes one value.
    cases.push(("throw", seq(&[&push_i32(1), &[op::OP_throw]]), 0, 0));
    let mut throw_error = vec![op::OP_throw_error];
    throw_error.extend_from_slice(&u32b(0));
    throw_error.push(0);
    cases.push(("throw_error", seq(&[&throw_error]), 0, 0));
    cases.push((
        "regexp",
        seq(&[&push_i32(0), &push_i32(0), &[op::OP_regexp], &ret]),
        0,
        0,
    ));
    let mut push_bigint = vec![op::OP_push_bigint_i32];
    push_bigint.extend_from_slice(&i32b(-7));
    cases.push(("push_bigint_i32", seq(&[&push_bigint, &ret]), 0, 0));

    // For the call opcodes the helper takes the whole call window off the
    // operand stack, so just enough operands are pushed for the verifier.
    // `call`/`call_method`/`call_constructor` carry an NPOP argc (0 here);
    // `apply` a `U16` magic.
    cases.push((
        "call",
        seq(&[&push_i32(0), &[op::OP_call], &u16b(0), &ret]),
        0,
        0,
    ));
    cases.push((
        "call_method",
        seq(&[
            &push_i32(0),
            &push_i32(0),
            &[op::OP_call_method],
            &u16b(0),
            &ret,
        ]),
        0,
        0,
    ));
    cases.push((
        "call_constructor",
        seq(&[
            &push_i32(0),
            &push_i32(0),
            &[op::OP_call_constructor],
            &u16b(0),
            &ret,
        ]),
        0,
        0,
    ));
    cases.push((
        "apply",
        seq(&[
            &push_i32(0),
            &push_i32(0),
            &push_i32(0),
            &[op::OP_apply],
            &u16b(0),
            &ret,
        ]),
        0,
        0,
    ));

    for (name, bytes, args, vars) in cases {
        compile_bytes(bytes, args, vars)
            .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
    }

    // A long straight-line block full of calls must not get one post-call OSR
    // entry per call.
    //
    // `block_range` hands a mid-block entry the whole tail of its block, and
    // `block_for` gives that tail a Cranelift block of its own, so every copy
    // stays in the compiled code: a block with N calls costs O(N x length).
    // Unbounded, that pushed a real test262 function (658-instruction block,
    // 116 `call_method`s) to 10.5M IR instructions, 31 s of Cranelift, and a
    // `CodeTooLarge` that cost it native code entirely. Entries are a
    // performance affordance — a dropped call site resumes in the
    // interpreter — so they are what gives way.
    const CALLS: usize = 40;
    let mut heavy = Vec::new();
    for _ in 0..CALLS {
        heavy.extend_from_slice(&push_i32(0)); // func
        heavy.extend_from_slice(&push_i32(0)); // this
        heavy.push(op::OP_call_method);
        heavy.extend_from_slice(&u16b(0)); // argc
    }
    heavy.push(op::OP_return_undef);

    let verified = verify(
        CompileSnapshot::from_raw(heavy, 0, 0, 0, CALLS as u16 + 4),
        VerifyLimits::default(),
    )
    .expect("a straight-line block of calls verifies");
    let pcs = collect_osr_pcs(&verified);
    assert!(
        !pcs.is_empty(),
        "the first call still hands its tail back to native code"
    );
    assert!(
        pcs.len() <= 8,
        "{CALLS} calls in one block yielded {} OSR entries",
        pcs.len()
    );
    let isa = crate::jit::build_host_isa().unwrap();
    compile(&verified, isa.as_ref(), &Cell::new(0), Tier::Tier1)
        .expect("a call-heavy straight-line block compiles");

    // Global-variable helpers need a non-zero global-var-ref count,
    // so they use their own snapshot shape.
    let globals: &[(&str, u8)] = &[
        ("get_var", op::OP_get_var),
        ("get_var_undef", op::OP_get_var_undef),
    ];
    for (name, code) in globals {
        compile_bytes_gv(seq(&[&loc(*code, 0), &ret]), 0, 0, 1)
            .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
    }
    for (name, code) in [
        ("put_var", op::OP_put_var),
        ("put_var_init", op::OP_put_var_init),
    ] {
        compile_bytes_gv(
            seq(&[&push_i32(0), &loc(code, 0), &[op::OP_return_undef]]),
            0,
            0,
            1,
        )
        .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
    }

    // `delete name` on the global object: the fall-through of `with_delete_var`,
    // so a `with` body's `delete` runs natively too.
    compile_bytes(
        seq(&[
            &[op::OP_delete_var],
            &u32b(0),
            &[op::OP_drop, op::OP_return_undef],
        ]),
        0,
        0,
    )
    .unwrap_or_else(|e| panic!("opcode delete_var failed to lower: {e:?}"));

    // Closure variable refs need a non-zero closure-var count.
    for (name, code) in [
        ("get_var_ref", op::OP_get_var_ref),
        ("get_var_ref_check", op::OP_get_var_ref_check),
    ] {
        compile_bytes_gv(seq(&[&loc(code, 0), &ret]), 0, 0, 1)
            .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
    }
    for (name, code) in [
        ("put_var_ref", op::OP_put_var_ref),
        ("put_var_ref_check", op::OP_put_var_ref_check),
        ("put_var_ref_check_init", op::OP_put_var_ref_check_init),
    ] {
        compile_bytes_gv(
            seq(&[&push_i32(0), &loc(code, 0), &[op::OP_return_undef]]),
            0,
            0,
            1,
        )
        .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
    }
    compile_bytes_gv(
        seq(&[&push_i32(0), &loc(op::OP_set_var_ref, 0), &ret]),
        0,
        0,
        1,
    )
    .unwrap_or_else(|e| panic!("opcode set_var_ref failed to lower: {e:?}"));

    // `make_var_ref_ref` needs the closure-var space to be non-empty.
    compile_bytes_gv(
        seq(&[
            &[op::OP_make_var_ref_ref],
            &u32b(0),
            &u16b(0),
            &[op::OP_drop, op::OP_drop, op::OP_return_undef],
        ]),
        0,
        0,
        1,
    )
    .unwrap_or_else(|e| panic!("opcode make_var_ref_ref failed to lower: {e:?}"));

    // TDZ-checked local ops.
    for (name, code) in [
        ("put_loc_check", op::OP_put_loc_check),
        ("put_loc_check_init", op::OP_put_loc_check_init),
    ] {
        compile_bytes(
            seq(&[
                &loc(op::OP_set_loc_uninitialized, 0),
                &push_i32(0),
                &loc(code, 0),
                &[op::OP_return_undef],
            ]),
            0,
            1,
        )
        .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
    }
    compile_bytes(
        seq(&[
            &loc(op::OP_set_loc_uninitialized, 0),
            &push_i32(0),
            &loc(op::OP_set_loc_check, 0),
            &ret,
        ]),
        0,
        1,
    )
    .unwrap_or_else(|e| panic!("opcode set_loc_check failed to lower: {e:?}"));
    compile_bytes(seq(&[&loc(op::OP_get_loc_checkthis, 0), &ret]), 0, 1)
        .unwrap_or_else(|e| panic!("opcode get_loc_checkthis failed to lower: {e:?}"));
}

/// TDZ-checked locals: policy promises `Native` must mean "the baseline lowers it".
/// Every opcode the generated table calls `Native` must pass [`lowers`];
/// otherwise it silently degrades to an instruction-level `Resume` (the
/// function compiles, but re-enters the interpreter at that op). This is
/// the guard that stops a new `Native` opcode from being added without a
/// lowering.
#[test]
fn every_native_policy_opcode_is_lowered() {
    use crate::jit::{JitPolicy, opcode_names, opcode_policies};
    let missing: Vec<&str> = opcode_names()
        .iter()
        .zip(opcode_policies())
        .filter(|(name, policy)| **policy == JitPolicy::Native && !lowers(name))
        .map(|(name, _)| *name)
        .collect();
    assert!(
        missing.is_empty(),
        "Native opcodes with no lowering: {missing:?}"
    );
}

/// `Helper` opcodes the baseline deliberately does **not** lower. Empty:
/// every `Helper` opcode has a lowering. This test exists because the
/// `Native`-only test cannot catch a `Helper` without one.
#[test]
fn every_helper_policy_opcode_is_lowered_or_pinned() {
    use crate::jit::{JitPolicy, opcode_names, opcode_policies};
    /// `Helper` policy, no lowering, intentional. A new entry must carry the
    /// reason it still exits the native frame.
    const PINNED: &[&str] = &[];
    let mut missing: Vec<&str> = opcode_names()
        .iter()
        .zip(opcode_policies())
        .filter(|(name, policy)| **policy == JitPolicy::Helper && !lowers(name))
        .map(|(name, _)| *name)
        .collect();
    missing.sort_unstable();
    assert_eq!(
        missing, PINNED,
        "Helper opcodes without a lowering changed: add the lowering, or pin the \
             name here with the reason it still exits the native frame"
    );
}

/// Every opcode the verifier accepts is lowered, so the `Resume` fallback in
/// `lower_insn` is unreachable for verified bytecode. The only unsupported
/// names are policy `Reject`, which `verify` refuses before lowering (this
/// test pins that equality so a new `Reject` that verifies — or a new
/// opcode that verifies but has no lowering — is caught).
#[test]
fn every_opcode_the_verifier_accepts_is_lowered() {
    use crate::jit::{JitPolicy, opcode_names, opcode_policies};
    let unsupported: Vec<&str> = opcode_names()
        .iter()
        .zip(opcode_policies())
        .filter(|(name, _)| !lowers(name))
        .map(|(name, _)| *name)
        .collect();
    let rejected: Vec<&str> = opcode_names()
        .iter()
        .zip(opcode_policies())
        .filter(|(_, policy)| **policy == JitPolicy::Reject)
        .map(|(name, _)| *name)
        .collect();
    assert_eq!(
        unsupported, rejected,
        "the unsupported set must be exactly the `Reject` policy set"
    );
}

/// Every shuffle's JIT permutation must equal the frontend's model in
/// `jit/bytecode/stack.rs::copied_stack_values`, which is written against
/// the QuickJS interpreter arms (`quickjs-opcode.h`: `perm4: obj prop a b
/// -> a obj prop b`). No JS case reaches some of these shuffles on its own,
/// so this table is what keeps them honest.
#[test]
fn shuffle_table_matches_the_verifier() {
    // Five distinct kinds stand in for the slots, so a permutation that
    // reorders them is visible.
    let kinds = [
        SlotKind::Tagged,
        SlotKind::Int32,
        SlotKind::Float64,
        SlotKind::CatchOffset,
        SlotKind::Uninitialized,
    ];
    // `shuffle_opcodes` is the single source of truth: it must name exactly
    // the opcodes the dispatch below handles, so neither can drift.
    for name in crate::jit::opcode_names() {
        assert_eq!(
            shuffle_opcodes().contains(name),
            shuffle_outputs(name).is_some(),
            "{name}: `shuffle_opcodes` and the table disagree"
        );
    }
    for name in shuffle_opcodes() {
        let (pop, outputs) = shuffle_outputs(name).expect("known shuffle");
        let popped = &kinds[..pop];
        let modeled = crate::jit::bytecode::stack::copied_stack_values(name, popped)
            .unwrap_or_else(|| panic!("{name} has no verifier model"));
        assert_eq!(outputs.len(), modeled.len(), "{name}: output count");
        for (slot, &source) in outputs.iter().enumerate() {
            assert_eq!(
                modeled[slot], popped[source],
                "{name}: output {slot} takes the wrong operand"
            );
        }
    }
}

/// A small pre-tested/loop/body/exit function: `i = 0`; `while (i < 3) i++`.
/// The header reads `i` before writing it, so the carried-local pass carries it; the body
/// writes it, so the back edge re-carries the written value.
fn loop_bytes() -> Vec<u8> {
    seq(&[
        &push_i32(0),                 // 0..5
        &loc(op::OP_put_loc, 0),      // 5..8
        &branch(op::OP_goto, 4),      // 8..13 -> header at 13
        &loc(op::OP_get_loc, 0),      // 13..16
        &push_i32(3),                 // 16..21
        &[op::OP_lt],                 // 21..22
        &branch(op::OP_if_false, 11), // 22..27 -> end at 34
        &[op::OP_inc_loc, 0],         // 27..29
        &branch(op::OP_goto, -17),    // 29..34 -> header
        &[op::OP_return_undef],       // 34
    ])
}

fn verified_local(bytes: Vec<u8>, args: u16, vars: u16) -> VerifiedFunction {
    verify(snapshot(bytes, args, vars), VerifyLimits::default())
        .unwrap_or_else(|e| panic!("verify rejected: {e:?}"))
}

/// Carried locals: the loop header reads `i` before writing it, so the block
/// declares it carried; a block that writes before reading does not.
#[test]
fn the_loop_header_carries_its_induction_local() {
    let verified = verified_local(loop_bytes(), 0, 1);
    let analysis = Analysis::build(&verified, true);
    let carried = carried_local_sets(&analysis, &verified, true, true);
    // Header at pc 13.
    assert_eq!(
        carried.get(&13).cloned(),
        Some(vec![LocalRef::local(0)]),
        "the header reads `i` before writing it"
    );
    // The entry block has no incoming edge, so it carries nothing.
    assert!(!carried.contains_key(&0));
}

/// Dead-store elimination: an unread local store is dead; a read one is not; and the
/// gate (no resume points / exact liveness) turns the pass off entirely.
#[test]
fn dead_local_stores_are_elided_only_when_read_free() {
    // push_i32 1; put_loc 0; push_i32 2; put_loc 1; return_undef
    let dead = seq(&[
        &push_i32(1),
        &loc(op::OP_put_loc, 0),
        &push_i32(2),
        &loc(op::OP_put_loc, 1),
        &[op::OP_return_undef],
    ]);
    let verified = verified_local(dead, 0, 2);
    let analysis = Analysis::build(&verified, true);
    let hints = dead_local_stores(&analysis, verified.instructions(), true);
    assert!(hints.contains(&5), "put_loc 0 at pc 5 is never read");
    assert!(hints.contains(&13), "put_loc 1 at pc 13 is never read");
    // The gate being closed yields no hints at all.
    assert!(dead_local_stores(&analysis, verified.instructions(), false).is_empty());

    // push_i32 1; put_loc 0; get_loc 0; return -> the store is live.
    let live = seq(&[
        &push_i32(1),
        &loc(op::OP_put_loc, 0),
        &loc(op::OP_get_loc, 0),
        &[op::OP_return],
    ]);
    let verified = verified_local(live, 0, 1);
    let analysis = Analysis::build(&verified, true);
    let hints = dead_local_stores(&analysis, verified.instructions(), true);
    assert!(!hints.contains(&5), "put_loc 0 at pc 5 is read back");
}

/// Result classes: a site whose *result* was observed as `Float64` (an
/// always-overflowing accumulator) is not `int_only`, so the speculative path never pays its
/// overflow bail every trip.
#[test]
fn an_overflowing_site_is_not_int_only() {
    let site = |result: u16| {
        (
            0u32,
            SiteFeedback {
                lhs: CLASS_INT,
                rhs: CLASS_INT,
                result,
                samples: 8,
            },
        )
    };
    assert!(int_only(&[site(CLASS_INT)], 0, true));
    assert!(!int_only(&[site(CLASS_INT | CLASS_FLOAT)], 0, true));
    assert!(!int_only(&[site(CLASS_FLOAT)], 0, true));
}

/// Dead-store elimination: a read in a *later block* keeps the store alive through
/// the analysis' liveness (not just the block-local scan).
#[test]
fn a_store_read_in_a_successor_block_stays_live() {
    let bytes = seq(&[
        &push_i32(1),
        &loc(op::OP_put_loc, 0),
        &[op::OP_push_false],
        &branch(op::OP_if_false, 5), // -> L
        &[op::OP_return_undef],
        &loc(op::OP_get_loc, 0), // L
        &[op::OP_return],
    ]);
    let verified = verified_local(bytes, 0, 1);
    let analysis = Analysis::build(&verified, true);
    let hints = dead_local_stores(&analysis, verified.instructions(), true);
    assert!(!hints.contains(&5), "the successor reads local 0");
}

#[test]
fn push_const_lowers_against_the_constant_pool() {
    let mut snap = snapshot(
        seq(&[&[op::OP_push_const], &u32b(0), &[op::OP_return]]),
        0,
        0,
    );
    snap.const_meta = Box::new([ConstMeta { tag: 0 }]);
    let isa = crate::jit::build_host_isa().unwrap();
    let verified = verify(snap, VerifyLimits::default()).unwrap();
    assert!(compile(&verified, isa.as_ref(), &Cell::new(0), Tier::Tier1).is_ok());
}

/// `fclosure` (the bytecode contract) reads a nested bytecode out of the constant
/// pool, so it needs the same non-empty pool `push_const` does.
#[test]
fn fclosure_lowers_against_the_constant_pool() {
    let mut snap = snapshot(seq(&[&[op::OP_fclosure], &u32b(0), &[op::OP_return]]), 0, 0);
    snap.const_meta = Box::new([ConstMeta { tag: 0 }]);
    let isa = crate::jit::build_host_isa().unwrap();
    let verified = verify(snap, VerifyLimits::default()).unwrap();
    assert!(compile(&verified, isa.as_ref(), &Cell::new(0), Tier::Tier1).is_ok());
}

/// Why [`entry_is_translatable`] cannot fire today: the only opcodes the
/// baseline could refuse are policy `Reject`, and a `Reject` opcode at pc 0
/// fails verification before the entry check is reached. The check stays as
/// the fail-closed net for a future operand-free unsupported opcode.
#[test]
fn the_call_opcodes_cannot_be_a_function_entry() {
    use crate::bytecode::opcode_info_final;
    use crate::jit::opcode_names;
    // `Reject` opcodes that pop no operand are the only ones that could start
    // a verified function; none of them lower, so `entry_is_translatable`
    // stays unreachable. `apply`/`call`/`call_constructor`/`call_method` pop
    // at least one operand each.
    const PINNED: &[&str] = &["apply", "call", "call_constructor", "call_method"];
    for name in PINNED {
        let id = opcode_names()
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("{name} is not in the policy table")) as u8;
        assert!(
            opcode_info_final(id).n_pop >= 1,
            "{name} pops nothing, so it *can* start a verified function — and then \
                 `entry_is_translatable` would refuse the whole function"
        );
    }
}

// ── The stack-slot register cache (unit level) ──

/// Emit one block through `f` and hand back the printed IR, so a test can
/// assert on what the cache emitted — or deliberately did not emit.
fn ir_of(f: impl FnOnce(&mut FunctionBuilder)) -> String {
    let mut ctx = Context::new();
    let mut fbctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
    let entry = b.create_block();
    b.switch_to_block(entry);
    f(&mut b);
    // Terminate the block so `finalize` accepts it; the terminator is
    // irrelevant to what these tests count.
    b.ins().return_(&[]);
    b.seal_all_blocks();
    b.finalize(); // consumes the builder, ending its borrow of `ctx.func`
    format!("{}", ctx.func.display())
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// The `store` lines of printed IR, in emission order.
fn stores(ir: &str) -> Vec<&str> {
    ir.lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("store"))
        .collect()
}

/// A value pushed and popped again never reaches memory: both sides hold
/// the same SSA value, so the store/load pair is never emitted (DCE), and
/// a later flush has nothing left to write.
#[test]
fn stack_cache_pop_dces_the_push_store() {
    let ir = ir_of(|b| {
        let sp = b.ins().iconst(types::I64, 4096);
        let v = b.ins().iconst(types::I64, 42);
        let mut cache = StackCache::default();
        cache.anchor(0);
        let sp = cache.push(b, sp, v);
        let (sp, popped) = cache.pop(b, sp);
        assert_eq!(
            popped, v,
            "the register-resident value comes back unchanged"
        );
        assert_eq!(cache.height, 0, "pop accounts for the slot");
        assert!(cache.is_empty(), "the popped slot died with its value");
        cache.flush(b, sp);
    });
    assert_eq!(
        stores(&ir).len(),
        0,
        "push→pop must not touch memory:\n{ir}"
    );
}

/// The guard fact: only values the compiler produced as `Int` (a `push_i32`
/// literal, an `Int` constant) are known, and the fact dies with the entry —
/// a flush (helper call, exit, branch) drops both.
#[test]
fn stack_cache_tracks_known_int_operands() {
    let ir = ir_of(|b| {
        let sp = b.ins().iconst(types::I64, 4096);
        let mut cache = StackCache::default();
        cache.anchor(0);
        let literal = b.ins().iconst(types::I64, 7);
        let sp = cache.push_int(b, sp, literal);
        assert!(cache.known_int(1), "a literal is a known Int");
        let loaded = b.ins().load(types::I64, MemFlags::new(), sp, 0);
        let sp = cache.push(b, sp, loaded);
        assert!(!cache.known_int(2), "a loaded value is not");
        // Storing materializes the value without changing the fact.
        cache.flush(b, sp);
        assert!(!cache.known_int(1), "the flush dropped the entry");
        assert!(cache.is_empty());
    });
    assert_eq!(count(&ir, "load"), 1, "only the loaded operand:\n{ir}");
}

/// The stack-slot register cache: a slot read twice in one block is loaded once — the second read
/// reuses the register the first one filled (`LocalCache`).
#[test]
fn local_cache_reads_a_slot_once_per_block() {
    let ir = ir_of(|b| {
        let base = b.ins().iconst(types::I64, 4096);
        let mut cache = LocalCache::new(true, true);
        let first = cache.read(b, base, false, 2, 16);
        let second = cache.read(b, base, false, 2, 16);
        assert_eq!(first, second, "the same register value comes back");
        assert!(!cache.is_empty());
    });
    assert_eq!(count(&ir, "load"), 1, "one load for two reads:\n{ir}");
}

/// The counter-case: as soon as the slot can change, the copy is dropped and
/// the next read loads again.
#[test]
fn local_cache_drops_the_copy_when_the_slot_changes() {
    let ir = ir_of(|b| {
        let base = b.ins().iconst(types::I64, 4096);
        let mut cache = LocalCache::new(true, true);
        let _ = cache.read(b, base, false, 1, 8);
        cache.drop_slot(false, 1);
        let _ = cache.read(b, base, false, 1, 8);
        // The argument space is keyed separately from the local space, so
        // argument 1 keeps its own copy.
        let _ = cache.read(b, base, true, 1, 8);
        assert_eq!(cache.entries.len(), 2, "one copy per (space, index)");
    });
    assert_eq!(count(&ir, "load"), 3, "an invalidated slot reloads:\n{ir}");
}

/// Aliased slots (`fclosure`/refs) and mapped `arguments` formals are never
/// cached: every read is a load.
#[test]
fn local_cache_is_off_for_aliased_slots() {
    let ir = ir_of(|b| {
        let base = b.ins().iconst(types::I64, 4096);
        let mut cache = LocalCache::new(false, false);
        let _ = cache.read(b, base, false, 0, 0);
        let _ = cache.read(b, base, false, 0, 0);
        let _ = cache.read(b, base, true, 0, 0);
        cache.fill(false, 0, base);
        assert!(cache.is_empty(), "nothing may be cached");
    });
    assert_eq!(count(&ir, "load"), 3, "every read loads:\n{ir}");
}

/// The analysis that decides the two flags above.
#[test]
fn cacheable_slots_tracks_aliasing() {
    let verify_bytes = |bytes: Vec<u8>, vars: u16, consts: usize| {
        let mut snap = snapshot(bytes, 0, vars);
        snap.const_meta = vec![ConstMeta { tag: 0 }; consts].into_boxed_slice();
        verify(snap, VerifyLimits::default()).unwrap_or_else(|e| panic!("verify: {e:?}"))
    };

    // Plain locals and arguments: both cacheable.
    let plain = seq(&[
        &loc(op::OP_get_loc, 0),
        &loc(op::OP_put_loc, 0),
        &[op::OP_return_undef],
    ]);
    assert_eq!(cacheable_slots(&verify_bytes(plain, 1, 0)), (true, true));

    // A nested function may capture either space.
    let closure = seq(&[&[op::OP_fclosure], &u32b(0), &[op::OP_return]]);
    assert_eq!(
        cacheable_slots(&verify_bytes(closure, 0, 1)),
        (false, false)
    );

    // A reference object pointing at a local.
    let loc_ref = seq(&[&[op::OP_make_loc_ref, 0, 0, 0, 0, 0, 0], &[op::OP_return]]);
    assert_eq!(
        cacheable_slots(&verify_bytes(loc_ref, 1, 0)),
        (false, false)
    );

    // Mapped `arguments` aliases the formals only.
    let mapped = seq(&[&[op::OP_special_object, 1], &[op::OP_return]]);
    assert_eq!(cacheable_slots(&verify_bytes(mapped, 0, 0)), (true, false));

    // The strict (copying) `arguments` object does not.
    let strict = seq(&[&[op::OP_special_object, 0], &[op::OP_return]]);
    assert_eq!(cacheable_slots(&verify_bytes(strict, 0, 0)), (true, true));
}

/// A pop with no cached value falls back to a load — the counter-case to
/// the DCE above (this is what happens after a flush cleared the cache).
#[test]
fn stack_cache_pop_loads_when_the_slot_is_in_memory() {
    let ir = ir_of(|b| {
        let sp = b.ins().iconst(types::I64, 4096);
        let mut cache = StackCache::default();
        cache.anchor(1); // one slot already on the stack, never cached
        let (sp, loaded) = cache.pop(b, sp);
        assert_ne!(loaded, sp, "a fresh Value, not the stack pointer");
        assert!(cache.is_empty());
        let _ = sp;
    });
    assert_eq!(
        count(&ir, "load"),
        1,
        "the missing slot must be loaded:\n{ir}"
    );
}

/// Flush materializes every register-resident slot at the offset its
/// verified height implies: slot k sits at `sp + (k - 1 - height) * 8`,
/// so at height 2 that is `sp-16` and `sp-8`.
#[test]
fn stack_cache_flush_writes_each_dirty_slot_at_its_offset() {
    let ir = ir_of(|b| {
        let sp = b.ins().iconst(types::I64, 4096);
        let mut cache = StackCache::default();
        cache.anchor(0);
        let a = b.ins().iconst(types::I64, 1);
        let sp = cache.push(b, sp, a);
        let c = b.ins().iconst(types::I64, 2);
        let sp = cache.push(b, sp, c);
        cache.flush(b, sp);
        assert!(cache.is_empty(), "flush forgets what it wrote");
    });
    let emitted = stores(&ir);
    assert_eq!(emitted.len(), 2, "one store per dirty slot:\n{ir}");
    assert!(
        emitted[0].contains("-16"),
        "slot 1 at height 2 sits at sp-16:\n{ir}"
    );
    assert!(emitted[1].contains("-8"), "slot 2 sits at sp-8:\n{ir}");
    // The offsets ride on the memory instruction itself: no address math.
    assert_eq!(
        count(&ir, "iadd_imm"),
        2,
        "only the two pushes adjust sp:\n{ir}"
    );
}

/// `flush_except` writes everything *except* the slots a sibling branch
/// still needs in registers; those survive for a later flush, and the
/// stack itself never moves.
#[test]
fn stack_cache_flush_except_keeps_the_named_slots_dirty() {
    let ir = ir_of(|b| {
        let sp = b.ins().iconst(types::I64, 4096);
        let mut cache = StackCache::default();
        cache.anchor(0);
        let a = b.ins().iconst(types::I64, 1);
        let sp = cache.push(b, sp, a);
        let c = b.ins().iconst(types::I64, 2);
        let sp = cache.push(b, sp, c);

        cache.flush_except(b, sp, &[2]);
        let mid = format!("{}", b.func.display());
        assert_eq!(
            stores(&mid).len(),
            1,
            "only the unkept slot is written:\n{mid}"
        );
        assert_eq!(cache.height, 2, "a flush never moves the stack");
        assert!(!cache.is_empty(), "the kept slot stays register-resident");

        cache.flush(b, sp);
        assert!(cache.is_empty());
    });
    assert_eq!(
        stores(&ir).len(),
        2,
        "the kept slot is written by the second flush:\n{ir}"
    );
}

/// Height bookkeeping is what makes every slot address correct: each
/// instruction re-anchors to the verifier's height, and only the effects
/// the lowering itself models (`tracked`) are worth the cross-check.
#[test]
fn stack_cache_height_follows_the_verifier_contract() {
    ir_of(|b| {
        let sp = b.ins().iconst(types::I64, 4096);
        let mut cache = StackCache::default();

        cache.anchor(3);
        assert_eq!(cache.height, 3);
        assert!(!cache.tracked, "an anchored instruction starts untracked");

        let v = b.ins().iconst(types::I64, 7);
        let sp = cache.push(b, sp, v);
        assert_eq!(cache.height, 4);
        assert!(cache.tracked, "push is a tracked effect");

        let sp = cache.pop(b, sp).0;
        assert_eq!(cache.height, 3);
        cache.drop_top();
        assert_eq!(cache.height, 2, "drop discards a slot without reading it");

        // A binary op pops two and pushes one, and caches neither operand:
        // two base slots + two pushes − one net = three slots left.
        cache.anchor(2);
        let x = b.ins().iconst(types::I64, 1);
        let sp = cache.push(b, sp, x);
        let y = b.ins().iconst(types::I64, 2);
        let sp = cache.push(b, sp, y);
        cache.consumed_binary();
        assert_eq!(cache.height, 3, "pop two, push one");
        assert!(
            cache.is_empty(),
            "both operands are dead, result is in memory"
        );

        cache.anchor(2);
        cache.adjust(-1);
        assert_eq!(cache.height, 1, "a shuffle's raw effect is applied");

        cache.anchor(9);
        assert_eq!(cache.height, 9, "the next instruction re-anchors");
        assert!(!cache.tracked, "and clears the mark");
        let _ = sp;
    });
}

// ── Cross-block carry (unit level) ──

/// Verify a hand-built function and run the carry walk over it.
fn carried_of(
    bytes: Vec<u8>,
    args: u16,
    vars: u16,
) -> (
    std::collections::HashMap<u32, Vec<u32>>,
    std::collections::HashMap<u32, u16>,
) {
    let verified = verify(snapshot(bytes, args, vars), VerifyLimits::default())
        .unwrap_or_else(|e| panic!("verify rejected: {e:?}"));
    let osr_pcs = collect_osr_pcs(&verified);
    let strict = verified.snapshot().mode.contains(ExecMode::STRICT);
    block_carry_facts(&verified, &osr_pcs, strict)
}

/// A join block whose first instruction reads the merged slot in a register
/// (`put_loc`) carries it: one store/load pair per edge disappears.
/// Bytecode: `get_loc0; dup; if_false L; drop; get_loc1; L: put_loc0;
/// get_loc0; get_loc1; add; put_loc0; return_undef`.
#[test]
fn carried_sets_marks_a_slot_read_before_the_first_flush() {
    let bytes = seq(&[
        &loc(op::OP_get_loc, 0),
        &[op::OP_dup],
        &branch(op::OP_if_false, 8), // pc 4 → 4+1+8 = 13
        &[op::OP_drop],
        &loc(op::OP_get_loc, 1),
        &loc(op::OP_put_loc, 0), // L = 13, entry height 1
        &loc(op::OP_get_loc, 0),
        &loc(op::OP_get_loc, 1),
        &[op::OP_add],
        &loc(op::OP_put_loc, 0),
        &[op::OP_return_undef],
    ]);
    let (carried, heights) = carried_of(bytes.clone(), 0, 2);
    assert_eq!(
        carried.get(&13).map(Vec::as_slice),
        Some([1].as_slice()),
        "L reads slot 1 with `put_loc` before any flush"
    );
    // The fallthrough block only drops and pushes — nothing to carry.
    assert!(!carried.contains_key(&9), "drop/push read no entry slot");
    assert_eq!(heights.get(&13), Some(&1), "both edges merge at height 1");
    // And the plumbing lowers: parameters on the join, args on both edges.
    compile_bytes(bytes, 0, 2).expect("join with carried parameters compiles");
}

/// A block that starts with a helper call flushes before it reads anything,
/// so carrying into it would buy nothing (and cost a reload on the edge):
/// the walk stops at the first instruction.
#[test]
fn carried_sets_stops_at_the_first_flush() {
    let bytes = seq(&[
        &loc(op::OP_get_loc, 0),
        &[op::OP_dup],
        &branch(op::OP_if_false, 8), // pc 4 → 13
        &[op::OP_drop],
        &loc(op::OP_get_loc, 1),
        &[op::OP_plus], // L = 13: a helper call, entry height 1
        &loc(op::OP_put_loc, 0),
        &[op::OP_return_undef],
    ]);
    let (carried, _) = carried_of(bytes, 0, 2);
    assert!(
        !carried.contains_key(&13),
        "the helper's frame flush ends the walk: {carried:?}"
    );
}

/// The two-int fast path reads its operands from registers before it splits
/// (its `flush_except` spares exactly them), so a block entered with both
/// slots on the stack carries them:
/// `push1; push2; get_loc0; if_false L; push9; drop; goto L;
/// L: add; put_loc0; return_undef`.
#[test]
fn carried_sets_marks_both_fast_path_operands() {
    let bytes = seq(&[
        &push_i32(1),
        &push_i32(2),
        &loc(op::OP_get_loc, 0),
        &branch(op::OP_if_false, 15), // pc 13 → 14+15 = 29
        &push_i32(9),
        &[op::OP_drop],
        &branch(op::OP_goto, 4), // pc 24 → 25+4 = 29
        &[op::OP_add],           // L = 29, entry height 2
        &loc(op::OP_put_loc, 0),
        &[op::OP_return_undef],
    ]);
    let (carried, heights) = carried_of(bytes, 0, 1);
    assert_eq!(
        carried.get(&29).map(Vec::as_slice),
        Some([1, 2].as_slice()),
        "the fast path reads both entry slots before flushing"
    );
    assert!(!carried.contains_key(&18), "the middle block only shuffles");
    assert_eq!(heights.get(&29), Some(&2));
}

/// A join whose arms disagree only in an operand *proof* — a `Tagged`
/// operand against an `Int32` literal, the shape every short-circuit and
/// `??` produces (`x || 5`, `x ?? 7`, `c ? x : 7`) — widens the operand to
/// `Tagged` and verifies; the whole function then compiles.
#[test]
fn join_of_tagged_and_int32_operands_widens_to_tagged() {
    // Layout: get_loc0(0..3) dup(3..4) if_true(4..9) drop(9..10)
    // push5(10..15) return_undef(15..16); branch offset 15-5 = 10.
    let bytes = seq(&[
        &loc(op::OP_get_loc, 0),
        &[op::OP_dup],
        &branch(op::OP_if_true, 10),
        &[op::OP_drop],
        &push_i32(5),
        &[op::OP_return_undef],
    ]);
    let verified = verify(snapshot(bytes.clone(), 0, 1), VerifyLimits::default())
        .expect("a lost kind proof must not reject the function");
    assert_eq!(
        verified.stack_heights(),
        [0, 1, 2, 1, 0, 1],
        "a kind-only widening must not change the proven heights"
    );
    compile_bytes(bytes, 0, 1).expect("the widened join lowers");
}
