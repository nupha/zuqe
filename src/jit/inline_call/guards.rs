//! The IR guards a copied body needs: the nested-callee identity
//! guard and the back-edge safepoint.

use super::*;

/// The nested site's guard: the live callee must be a bytecode function whose
/// bytecode node is the recorded one. Mirrors `h_inline_guard`/`callee_is`,
/// but the identity is a compile-time constant (the nested site belongs to the
/// copied body's bytecode, not the caller's feedback table). Returns the block
/// to emit the nested body in; a miss jumps to `bail`.
pub(super) fn callee_guard(
    b: &mut FunctionBuilder,
    func: Value,
    expected: u64,
    frame: Value,
    bail: Block,
) -> Block {
    let tag = b.ins().ushr_imm(func, 48);
    let is_object = b.ins().icmp_imm(IntCC::Equal, tag, 0xFFFF);
    let object = b.create_block();
    b.ins().brif(is_object, object, &[], bail, &[]);

    b.switch_to_block(object);
    // The object's node pointer: the low 48 bits of the tagged value.
    let node = b.ins().band_imm(func, 0x0000_ffff_ffff_ffff);
    let flags = b.ins().load(
        types::I32,
        MemFlags::new(),
        node,
        crate::jit::layout::object::FLAGS_OFFSET,
    );
    let class = b
        .ins()
        .band_imm(flags, crate::jit::layout::object::CLASS_ID_MASK);
    let is_function = b.ins().icmp_imm(
        IntCC::Equal,
        class,
        i64::from(crate::JS_CLASS_BYTECODE_FUNCTION),
    );
    let function = b.create_block();
    b.ins().brif(is_function, function, &[], bail, &[]);

    b.switch_to_block(function);
    let payload = b.ins().load(
        types::I64,
        MemFlags::new(),
        node,
        crate::jit::layout::object::PAYLOAD_OFFSET,
    );
    let bytecode = b.ins().load(
        types::I64,
        MemFlags::new(),
        payload,
        crate::jit::layout::function::BYTECODE_OFFSET,
    );
    let same = b.ins().icmp_imm(IntCC::Equal, bytecode, expected as i64);
    let hit = b.create_block();
    b.ins().brif(same, hit, &[], bail, &[]);
    b.switch_to_block(hit);
    let hits = b
        .ins()
        .load(types::I32, MemFlags::new(), frame, O_NESTED_INLINE_HITS);
    let hits = b.ins().iadd_imm(hits, 1);
    b.ins()
        .store(MemFlags::new(), hits, frame, O_NESTED_INLINE_HITS);
    hit
}

/// Leave the inline body when it is poisoned or when the caller frame asks to
/// stop, before a backward edge. The inline body owns no frame: the caller's
/// call window is the GC root set, and the values in registers are either
/// integer-domain or copies of values in that window.
pub(super) fn backedge_safepoint(
    b: &mut FunctionBuilder,
    poison: Variable,
    frame: Value,
    helpers: Value,
    helper_sig: SigRef,
    bail: Block,
) {
    // A poisoned condition could otherwise loop where the interpreter would
    // not; bail before the first poisoned iteration sees a second one.
    let poison = b.use_var(poison);
    let failed = b.ins().icmp_imm(IntCC::NotEqual, poison, 0);
    let clean = b.create_block();
    b.ins().brif(failed, bail, &[], clean, &[]);
    b.switch_to_block(clean);

    let safepoint = b.ins().load(
        types::I64,
        MemFlags::new(),
        helpers,
        (HELPER_SAFEPOINT * 8) as i32,
    );
    let call = b.ins().call_indirect(helper_sig, safepoint, &[frame]);
    let status = b.inst_results(call)[0];
    let failed = b.ins().icmp_imm(IntCC::NotEqual, status, 0);
    let cont = b.create_block();
    b.ins().brif(failed, bail, &[], cont, &[]);
    b.switch_to_block(cont);
}
