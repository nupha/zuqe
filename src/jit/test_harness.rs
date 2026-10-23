// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

#![cfg(test)]

//! Test-only: assemble bytecode by hand, compile it with the real JIT, run it.
//!
//! **This file is test infrastructure.** It is declared with `#[cfg(test)]`
//! in `jit/mod.rs` *and* gated again with `#![cfg(test)]` here, so a normal
//! (non-test) build never compiles it; production code cannot reach it.
//!
//! The JS frontend never emits some opcodes (and the bytecode optimizer
//! rewrites others away), so a JS-level test cannot pin their lowering. This
//! module descends one level: it builds the opcode stream, verifies it, runs
//! the *same* compiler, maps the machine code executable, and calls it.
//!
//! Two entry points cover the two lowering shapes:
//! * [`run_function`] compiles a hand-written body as a whole function and
//!   reads `out_value` back (the normal tier-1 path).
//! * [`run_inline`] wraps [`super::inline_call::lower`] in a `(base) -> value`
//!   machine function, so the copied-body lowering can be checked on its own.
//!
//! Both runners are for **pure bodies**: no helper may run, because the
//! synthetic frame has no scope/arena/bytecode behind it. A helper call is a
//! bug in the test, not a supported input. A real heap object may still be
//! passed in as an argument (a typed array for the inline element paths): the
//! fast path only dereferences the object, so it needs no scope — and a guard
//! miss becomes a fault instead of a silently passing helper call.
//!
//! **What belongs here vs. the differential suites** (the canonical rule is
//! `docs/jit.md`, "Testing framework"; this is the short version):
//! * Here: facts the JS frontend cannot produce (opcodes/shapes it never
//!   emits), internals JS cannot observe (the argument-window layout, emission
//!   counts, raw element bytes, a store's width), and the proof that a given
//!   lowering *ran* (a synthetic frame has no scope, so a guard miss faults).
//! * Differential (`tests/jit/*`): everything user-observable under real
//!   engine state — guard misses, helpers, GC, detach/resize/shared buffers,
//!   prototypes, user code and exceptions.
//! * An **exhaustive per-opcode / per-conversion table with hardcoded expected
//!   values** also belongs here: differential only catches "the two sides
//!   disagree", not "both are wrong". Keep such a table to lowering facts; do
//!   not re-test user-visible semantics the differential suites already cover.

use std::cell::Cell;
use std::collections::HashMap;

use cranelift_codegen::{
    Context,
    control::ControlPlane,
    ir::{AbiParam, InstBuilder, MemFlags, Signature, UserFuncName, types},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};

use crate::{
    JSValue,
    function::GcScopeKind,
    jit::{
        abi::{HELPERS, JitExecFrame},
        bytecode::{VerifyLimits, verify},
        compiler,
        feedback::CallSite,
        inline_call,
        platform::CodeMem,
        snapshot::{CompileSnapshot, InlineCallee},
    },
    vm_stack::ExecMode,
};

pub(crate) const UNDEFINED: u64 = 3 << 48;
pub(crate) const NULL: u64 = 2 << 48;
pub(crate) const TRUE: u64 = (1 << 48) | 1;
pub(crate) const FALSE: u64 = 1 << 48;
/// `JSValueTag::Uninitialized` (the TDZ sentinel written by
/// `set_loc_uninitialized`).
pub(crate) const UNINITIALIZED: u64 = 4 << 48;

/// Not a valid `JSValue`: the body took the bailout edge.
const BAILED: u64 = 0xDEAD_BEEF_DEAD_BEEF;
const EXIT_DONE: u32 = 0;

thread_local! {
    /// How many inline bodies the current test thread lowered. A hand-written
    /// caller with a loop emits the same call site twice (normal block plus the
    /// post-call OSR tail); the test asserts that happened.
    static INLINE_EMISSIONS: Cell<u32> = const { Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn note_inline_emission() {
    INLINE_EMISSIONS.with(|count| count.set(count.get().saturating_add(1)));
}

pub(crate) fn inline_emissions() -> u32 {
    INLINE_EMISSIONS.with(Cell::get)
}

pub(crate) fn reset_inline_emissions() {
    INLINE_EMISSIONS.with(|count| count.set(0));
}

pub(crate) fn int(value: i32) -> u64 {
    u64::from(value as u32)
}

/// The tagged bits of a `Float64` (`js_new_float64`'s encoding).
pub(crate) fn float(value: f64) -> u64 {
    value.to_bits().wrapping_sub(0xFFF9 << 48)
}

/// A raw `JSValue` from its tagged bits (for tags JS tests cannot construct
/// easily, e.g. a Float64 that holds an integral value).
pub(crate) fn value(bits: u64) -> JSValue {
    JSValue(bits, std::marker::PhantomData)
}

/// Bytecode assembler with labels. Branch offsets are relative to `pc + 1`,
/// exactly like [`super::bytecode::decode::Instruction::branch_target`].
#[derive(Default)]
pub(crate) struct Asm {
    code: Vec<u8>,
}

impl Asm {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn pc(&self) -> u32 {
        self.code.len() as u32
    }

    pub(crate) fn op(&mut self, op: u8) -> &mut Self {
        self.code.push(op);
        self
    }

    pub(crate) fn op_u8(&mut self, op: u8, operand: u8) -> &mut Self {
        self.code.push(op);
        self.code.push(operand);
        self
    }

    pub(crate) fn op_u16(&mut self, op: u8, operand: u16) -> &mut Self {
        self.code.push(op);
        self.code.extend_from_slice(&operand.to_le_bytes());
        self
    }

    pub(crate) fn push_i32(&mut self, operand: i32) -> &mut Self {
        self.op(crate::bytecode::opcodes::OP_push_i32);
        self.code.extend_from_slice(&operand.to_le_bytes());
        self
    }

    /// Emit a relative branch to `target`. Forward and backward targets are
    /// both allowed; the caller must have emitted `target`'s instruction
    /// (forward targets are patched by knowing the final pc in advance).
    pub(crate) fn branch(&mut self, op: u8, target: u32) -> &mut Self {
        let pc = self.code.len();
        let relative = target as i64 - (pc as i64 + 1);
        let relative = i32::try_from(relative).expect("branch offset fits i32");
        self.code.push(op);
        self.code.extend_from_slice(&relative.to_le_bytes());
        self
    }

    /// Emit a branch whose target is not known yet; returns the instruction
    /// index for [`Self::patch_branch`].
    pub(crate) fn branch_placeholder(&mut self, op: u8) -> usize {
        let at = self.code.len();
        self.code.push(op);
        self.code.extend_from_slice(&0i32.to_le_bytes());
        at
    }

    /// Fill a placeholder emitted by [`Self::branch_placeholder`].
    pub(crate) fn patch_branch(&mut self, at: usize, target: u32) {
        let relative = target as i64 - (at as i64 + 1);
        let relative = i32::try_from(relative).expect("branch offset fits i32");
        self.code[at + 1..at + 5].copy_from_slice(&relative.to_le_bytes());
    }

    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.code.clone()
    }

    pub(crate) fn take(self) -> Vec<u8> {
        self.code
    }
}

fn host_isa() -> std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa> {
    crate::jit::build_host_isa().expect("host isa")
}

fn publish(code: &[u8]) -> CodeMem {
    let mem = CodeMem::alloc(code.len()).expect("code memory");
    mem.write(code).expect("write code");
    mem.make_executable().expect("make executable");
    mem
}

fn verified_function(
    body: &[u8],
    args: u16,
    vars: u16,
    stack_size: u16,
) -> super::bytecode::VerifiedFunction {
    let snapshot = CompileSnapshot::from_raw(body.to_vec(), args, vars, 0, stack_size);
    verify(snapshot, VerifyLimits::default()).expect("verify hand-written body")
}

/// Run a hand-written body as a whole compiled function.
///
/// Returns the tagged `out_value` for a normal `return`/`return_undef`;
/// `None` for any other exit (resume/exception/interrupt).
pub(crate) fn run_function(body: &[u8], args: u16, vars: u16, arguments: &[u64]) -> Option<u64> {
    let verified = verified_function(body, args, vars, 64);
    let isa = host_isa();
    let compiled = compiler::compile(&verified, isa.as_ref(), &Cell::new(0)).expect("compile body");

    let arg_count = usize::from(args).max(arguments.len());
    let mut arg_buf = vec![value(UNDEFINED); arg_count];
    for (slot, &bits) in arg_buf.iter_mut().zip(arguments) {
        *slot = value(bits);
    }
    let mut var_buf = vec![value(UNDEFINED); usize::from(vars)];
    let mut stack_buf = vec![value(UNDEFINED); 64];
    let mut cur_sp = 0i32;
    let mut frame = JitExecFrame {
        scope: std::ptr::null_mut(),
        arg_base: arg_buf.as_mut_ptr(),
        var_base: var_buf.as_mut_ptr(),
        sp_base: stack_buf.as_mut_ptr(),
        sp: stack_buf.as_mut_ptr(),
        cpools: std::ptr::null(),
        cur_sp_offset: &raw mut cur_sp,
        helpers: HELPERS.0.as_ptr().cast::<u8>(),
        this: JSValue::UNDEFINED,
        gc_scope_kind: GcScopeKind::None as u32,
        imm: 0,
        imm2: 0,
        pc: 0,
        bytecode: std::ptr::null(),
        #[cfg(feature = "inline_cache")]
        ic_sites: std::ptr::null(),
        out_value: JSValue::UNDEFINED,
        out_pc: 0,
        out_sp: 0,
        arg_real_count: arguments.len() as u32,
        arena: std::ptr::null_mut(),
        cur_frame_idx: 0,
        new_target: JSValue::UNDEFINED,
        home_obj: None,
        guard_misses: 0,
        property_guard_hits: 0,
        property_guard_misses: 0,
        global_guard_hits: 0,
        global_guard_misses: 0,
        backedges: 0,
        inline_call_hits: 0,
        frame_inline_hits: 0,
        nested_inline_hits: 0,
        inline_call_misses: 0,
        scratch: JSValue::UNDEFINED,
        global_obj: JSValue::UNDEFINED,
        global_var_obj: JSValue::UNDEFINED,
        global_var_cache: std::ptr::null_mut(),
    };

    let mem = publish(&compiled);
    // SAFETY: `compile` emitted the `(i64) -> i32` frame ABI, and every buffer
    // the frame points at outlives the call.
    let entry: unsafe extern "C" fn(*mut JitExecFrame) -> u32 =
        unsafe { std::mem::transmute(mem.entry()) };
    let raw = unsafe { entry(&raw mut frame) };
    (raw == EXIT_DONE).then_some(frame.out_value.0)
}

/// Run a hand-written *callee body* through [`super::inline_call::lower`] and
/// the machine code it emits.
///
/// `runtime_argc` is the number of arguments the imaginary caller passes;
/// `arguments` holds them. `method` selects the `call_method` window layout
/// (`[this, func, args...]`), which moves the arguments one slot further.
///
/// `None` means the poison flag sent the body to the general path.
pub(crate) fn run_inline(
    body: &[u8],
    args: u16,
    vars: u16,
    runtime_argc: u32,
    arguments: &[u64],
    method: bool,
) -> Option<u64> {
    let verified = verified_function(body, args, vars, 32);
    let plan = inline_call::Plan {
        body: verified,
        callee: 0,
        sites: HashMap::new(),
    };
    assert!(
        inline_call::eligible(&plan.body, &plan.sites),
        "hand-written body must be inlinable"
    );

    let isa = host_isa();
    let mut signature = Signature::new(isa.default_call_conv());
    signature.params.push(AbiParam::new(types::I64));
    signature.returns.push(AbiParam::new(types::I64));

    let mut cgen = Context::new();
    cgen.func.signature = signature;
    cgen.func.name = UserFuncName::user(0, 1);
    let mut builder_ctx = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut cgen.func, &mut builder_ctx);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let base = builder.block_params(entry)[0];
    let bail = builder.create_block();
    let merge = builder.create_block();
    builder.append_block_param(merge, types::I64);
    // No real frame here: a body with a back edge would call the safepoint
    // helper with these dummy values, so only forward bodies may be executed
    // through this runner (loop bodies are covered by the integration suite).
    let dummy_frame = builder.ins().iconst(types::I64, 0);
    let mut helper_sig = Signature::new(isa.default_call_conv());
    helper_sig.params.push(AbiParam::new(types::I64));
    helper_sig.returns.push(AbiParam::new(types::I32));
    let helper_sig = builder.import_signature(helper_sig);

    let arg_base = builder.ins().iadd_imm(base, if method { 16 } else { 8 });
    let undefined = builder.ins().iconst(types::I64, UNDEFINED as i64);
    let declared = u32::from(plan.body.snapshot().arg_count);
    let mut inline_args = Vec::with_capacity(declared as usize);
    for index in 0..declared {
        inline_args.push(if index < runtime_argc {
            builder
                .ins()
                .load(types::I64, MemFlags::new(), arg_base, (index as i32) * 8)
        } else {
            undefined
        });
    }
    let sp_after = builder.ins().iadd_imm(base, 8);
    let mut next_tag = 0u32;
    inline_call::lower(
        &mut builder,
        &plan,
        &inline_args,
        Some((base, sp_after)),
        &mut next_tag,
        entry,
        bail,
        merge,
        dummy_frame,
        dummy_frame,
        helper_sig,
    );

    builder.switch_to_block(bail);
    let marker = builder.ins().iconst(types::I64, BAILED as i64);
    builder.ins().return_(&[marker]);
    builder.switch_to_block(merge);
    let _sp = builder.block_params(merge)[0];
    let result = builder.ins().load(types::I64, MemFlags::new(), base, 0);
    builder.ins().return_(&[result]);
    builder.seal_all_blocks();
    builder.finalize();

    let compiled = cgen
        .compile(isa.as_ref(), &mut ControlPlane::default())
        .expect("compile hand-written inline body");
    let mem = publish(compiled.code_buffer());
    // SAFETY: the wrapper was compiled for exactly `(i64) -> i64`.
    let entry: unsafe extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(mem.entry()) };

    debug_assert!(
        runtime_argc as usize <= arguments.len(),
        "runtime argc exceeds values"
    );
    // `[result, (this,) func, args...]`: the result slot is also where the
    // callee/receiver sits for a real call.
    let mut window = vec![0u64; arguments.len() + if method { 2 } else { 1 }];
    window[if method { 2 } else { 1 }..].copy_from_slice(arguments);
    let out = unsafe { entry(window.as_mut_ptr()) };
    (out != BAILED).then_some(out)
}

/// Is this hand-written body inside the inline subset? `stack_size` is exposed
/// so the boundary can be tested.
pub(crate) fn inlinable_with(body: &[u8], args: u16, vars: u16, stack_size: u16) -> bool {
    let snapshot = CompileSnapshot::from_raw(body.to_vec(), args, vars, 0, stack_size);
    verify(snapshot, VerifyLimits::default())
        .is_ok_and(|body| inline_call::eligible(&body, &HashMap::new()))
}

pub(crate) fn inlinable(body: &[u8], args: u16, vars: u16) -> bool {
    inlinable_with(body, args, vars, 32)
}

/// Verify and compile a hand-written *caller* with inline plans attached to
/// `call_sites` (pairs of `(call pc, callee bytes)`).
///
/// Compilation only: the generated guard needs a real `BytecodeImpl`, which a
/// hand-built caller does not have. This is the OSR-tail-duplication check:
/// `inline_emissions()` tells the caller how many bodies were lowered.
pub(crate) fn compile_caller_with_inline(
    caller: &[u8],
    args: u16,
    vars: u16,
    sites: &[(u32, &[u8])],
) -> Result<(), crate::jit::error::JitError> {
    let nested: Vec<(u32, &[u8], &[(u32, &[u8])])> = sites
        .iter()
        .map(|(pc, body)| (*pc, *body, &[][..]))
        .collect();
    compile_caller_with_nested_inline(caller, args, vars, &nested)
}

/// Like [`compile_caller_with_inline`], with each site's callee carrying its
/// own `(pc, body)` nested sites.
pub(crate) fn compile_caller_with_nested_inline(
    caller: &[u8],
    args: u16,
    vars: u16,
    sites: &[(u32, &[u8], &[(u32, &[u8])])],
) -> Result<(), crate::jit::error::JitError> {
    fn callee(body: &[u8], nested: &[(u32, &[u8])]) -> InlineCallee {
        let mut call_sites = Vec::with_capacity(nested.len());
        let mut callees = Vec::with_capacity(nested.len());
        for (index, (pc, nested_body)) in nested.iter().enumerate() {
            call_sites.push(CallSite {
                pc: *pc,
                callee: 1,
                polymorphic: false,
                inline: Some(index as u32),
            });
            callees.push(callee(nested_body, &[]));
        }
        InlineCallee {
            bytecode: body.into(),
            arg_count: 0,
            var_count: 0,
            local_buf_size: 32,
            stack_size: 32,
            mode: ExecMode::empty(),
            gc_scope_kind: GcScopeKind::None,
            const_meta: Box::new([]),
            call_sites: call_sites.into_boxed_slice(),
            inline_callees: callees.into_boxed_slice(),
        }
    }

    let mut call_sites = Vec::with_capacity(sites.len());
    let mut callees = Vec::with_capacity(sites.len());
    for (index, (pc, body, nested)) in sites.iter().enumerate() {
        call_sites.push(CallSite {
            pc: *pc,
            callee: 1,
            polymorphic: false,
            inline: Some(index as u32),
        });
        callees.push(callee(body, nested));
    }
    let snapshot = CompileSnapshot {
        function_id: 0,
        bytecode: caller.into(),
        arg_count: args,
        var_count: vars,
        global_var_ref_count: 0,
        closure_var_count: 0,
        local_buf_size: vars.saturating_add(64),
        stack_size: 64,
        mode: ExecMode::empty(),
        gc_scope_kind: GcScopeKind::None,
        const_meta: Box::new([]),
        ic_count: 0,
        global_cache_count: 0,
        feedback: Box::new([]),
        call_sites: call_sites.into_boxed_slice(),
        inline_callees: callees.into_boxed_slice(),
        speculate: false,
    };
    let verified = verify(snapshot, VerifyLimits::default()).expect("verify caller");
    let isa = host_isa();
    compiler::compile(&verified, isa.as_ref(), &Cell::new(0)).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::{Asm, float, int, run_function, value};
    use crate::{
        Context, EvalFlags, EvalName, EvalType, JSValue, Runtime, ScopedContext,
        bytecode::opcodes as op,
    };

    /// `[target, 0] = value`, both operands from the argument window, then
    /// return `undefined` — the store is the only work.
    fn store_body() -> Vec<u8> {
        let mut asm = Asm::new();
        asm.op_u16(op::OP_get_arg, 0);
        asm.push_i32(0);
        asm.op_u16(op::OP_get_arg, 1);
        array_el(&mut asm, op::OP_put_array_el);
        asm.op(op::OP_return_undef);
        asm.take()
    }

    /// `[target, index] -> value`, with the index in the argument window too.
    fn read_body() -> Vec<u8> {
        let mut asm = Asm::new();
        asm.op_u16(op::OP_get_arg, 0);
        asm.op_u16(op::OP_get_arg, 1);
        array_el(&mut asm, op::OP_get_array_el);
        asm.op(op::OP_return);
        asm.take()
    }

    /// The element opcodes carry the inline-cache site index when the feature
    /// is on (the fast path ignores it).
    fn array_el(asm: &mut Asm, opcode: u8) {
        #[cfg(feature = "inline_cache")]
        asm.op_u16(opcode, 0);
        #[cfg(not(feature = "inline_cache"))]
        asm.op(opcode);
    }

    /// Every numeric width, a view with a byte offset, and both
    /// `Uint32Array` read shapes (an `Int` and a `Float64`). Element 1 is the
    /// neighbour the store test checks.
    const TYPED_ARRAY_SETUP: &str = "\
        globalThis.i8 = new Int8Array([-5, 7]); \
        globalThis.u8 = new Uint8Array([250, 7]); \
        globalThis.i16 = new Int16Array([-300, 7]); \
        globalThis.u16 = new Uint16Array([65000, 7]); \
        globalThis.i32 = new Int32Array([-5, 7]); \
        globalThis.u32 = new Uint32Array([4294967295, 7]); \
        globalThis.u32s = new Uint32Array([7, 9]); \
        globalThis.c8 = new Uint8ClampedArray([250, 7]); \
        globalThis.f32 = new Float32Array([1.5, 7.5]); \
        globalThis.f32f = new Float32Array([0.1, 7.5]); \
        globalThis.f32i = new Float32Array([100, 7.5]); \
        globalThis.f64 = new Float64Array([1.5, 7.5]); \
        globalThis.f64i = new Float64Array([3, 7.5]); \
        globalThis.f64z = new Float64Array([-0, 7.5]); \
        globalThis.f64big = new Float64Array([2147483648, 7.5]); \
        globalThis.f64min = new Float64Array([-2147483648, 7.5]); \
        globalThis.off = new Int32Array(new ArrayBuffer(32), 8); off[1] = 77;";

    fn eval_setup(ctx: &mut Context) {
        ctx.eval(
            TYPED_ARRAY_SETUP.as_bytes(),
            &EvalName::Name("jit-harness-typed-array.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .expect("setup");
    }

    /// The raw bytes of a real typed array's view. The generated store must
    /// have hit its inline path: the synthetic frame has no scope, so a helper
    /// fallback would fault instead of failing an assertion.
    fn view_bytes(bits: u64) -> Vec<u8> {
        let v = value(bits);
        let object = v.as_object().expect("a typed array object");
        object
            .payload_typed_array()
            .expect("a typed array payload")
            .bytes()
            .to_vec()
    }

    fn global(ctx: &mut Context, name: &str) -> JSValue {
        let src = format!("{name};");
        ctx.eval(
            src.as_bytes(),
            &EvalName::Name("jit-harness-typed-array.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap_or_else(|code| panic!("{name} threw {code}"))
    }

    /// Every numeric typed array class, one `(value, expected element bytes)`
    /// table each. The element's width also pins the store's width: a wider
    /// store would spill into element 1, which the test checks separately.
    fn views() -> Vec<(&'static str, usize, Vec<(u64, Vec<u8>)>)> {
        let i = int;
        let f = float;
        vec![
            (
                "i8",
                1,
                vec![
                    (i(300), vec![44]),
                    (i(-300), vec![(-44i8) as u8]),
                    (f(300.9), vec![44]),
                    (f(-1.5), vec![(-1i8) as u8]),
                    (f(1e300), vec![0]),
                    (f(f64::NAN), vec![0]),
                ],
            ),
            (
                "u8",
                1,
                vec![
                    (i(300), vec![44]),
                    (i(-1), vec![255]),
                    (f(-1.5), vec![255]),
                    (f(300.9), vec![44]),
                ],
            ),
            (
                "i16",
                2,
                vec![
                    (i(70000), 4464i16.to_le_bytes().to_vec()),
                    (f(70000.5), 4464i16.to_le_bytes().to_vec()),
                    (f(-2.5), (-2i16).to_le_bytes().to_vec()),
                ],
            ),
            (
                "u16",
                2,
                vec![
                    (i(-1), 65535u16.to_le_bytes().to_vec()),
                    (f(70000.5), 4464u16.to_le_bytes().to_vec()),
                ],
            ),
            (
                "i32",
                4,
                vec![
                    (i(7), 7i32.to_le_bytes().to_vec()),
                    (f(4294967297.5), 1i32.to_le_bytes().to_vec()),
                    (f(-2147483648.5), i32::MIN.to_le_bytes().to_vec()),
                    (f(-1.9), (-1i32).to_le_bytes().to_vec()),
                ],
            ),
            (
                "u32",
                4,
                vec![
                    (f(-1.5), u32::MAX.to_le_bytes().to_vec()),
                    (f(4294967295.5), u32::MAX.to_le_bytes().to_vec()),
                ],
            ),
            (
                "c8",
                1,
                vec![
                    (f(0.5), vec![0]),
                    (f(1.5), vec![2]),
                    (f(2.5), vec![2]),
                    (f(127.5), vec![128]),
                    (f(254.5), vec![254]),
                    (f(300.4), vec![255]),
                    (f(-5.0), vec![0]),
                    (f(f64::NAN), vec![0]),
                    (f(f64::INFINITY), vec![255]),
                    (i(-5), vec![0]),
                    (i(300), vec![255]),
                ],
            ),
            (
                "f32",
                4,
                vec![
                    (
                        i(16777217),
                        (16777217i64 as f32).to_bits().to_le_bytes().to_vec(),
                    ),
                    (f(0.1), (0.1f64 as f32).to_bits().to_le_bytes().to_vec()),
                ],
            ),
            (
                "f64",
                8,
                vec![
                    (i(3), 3f64.to_bits().to_le_bytes().to_vec()),
                    (f(1.75), 1.75f64.to_bits().to_le_bytes().to_vec()),
                ],
            ),
        ]
    }

    /// The typed array store's conversion matrix runs in the generated code:
    /// every numeric width, every `ToInt32` / clamped / `ToNumber` shape, and
    /// the element's width (no spill into the neighbour).
    #[test]
    fn typed_array_stores_convert_in_the_generated_code() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        eval_setup(&mut ctx);
        let body = store_body();
        for (name, width, cases) in views() {
            let target = global(&mut ctx, name).0;
            for (value_bits, expected) in cases {
                let before = view_bytes(target);
                let out = run_function(&body, 2, 0, &[target, value_bits]);
                assert_eq!(out, Some(JSValue::UNDEFINED.0), "{name}: unexpected exit");
                let after = view_bytes(target);
                assert_eq!(
                    &after[..width],
                    expected.as_slice(),
                    "{name}: wrong element for value {value_bits:#x}"
                );
                assert_eq!(
                    &after[width..],
                    &before[width..],
                    "{name}: the store spilled past element 0"
                );
            }
        }
    }

    /// The typed array read's width / byte-offset / `Uint32` sign paths run in
    /// the generated code, with the exact tags the JIT produces. Every float
    /// element follows `From<f64> for JSValue` (T9): an integral value that
    /// fits an `i32` folds to an `Int`, `-0.0` and out-of-`i32` magnitudes stay
    /// `Float64`.
    #[test]
    fn typed_array_reads_use_the_inline_path() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        eval_setup(&mut ctx);
        let body = read_body();
        let cases = [
            ("i8", 0, int(-5)),
            ("u8", 0, int(250)),
            ("i16", 0, int(-300)),
            ("u16", 0, int(65000)),
            ("i32", 0, int(-5)),
            // The sign bit set: `box_bit_result`'s `From<u32>` rule.
            ("u32", 0, float(4294967295.0)),
            ("u32s", 0, int(7)),
            ("c8", 0, int(250)),
            ("f32", 0, float(1.5)),
            ("f32f", 0, float(f64::from(0.1f32))),
            // T9: the integral folds run in the generated code too.
            ("f32i", 0, int(100)),
            ("f64", 0, float(1.5)),
            ("f64i", 0, int(3)),
            // `-0.0` keeps the float tag (`1/-0.0 === -Infinity`).
            ("f64z", 0, float(-0.0)),
            // The `i32` boundary: inside folds to `Int`, outside stays `f64`.
            ("f64min", 0, int(i32::MIN)),
            ("f64big", 0, float(2147483648.0)),
            // The view's byte offset is part of the address.
            ("off", 1, int(77)),
        ];
        for (name, index, expected) in cases {
            let target = global(&mut ctx, name).0;
            let out = run_function(&body, 2, 0, &[target, int(index)]);
            assert_eq!(out, Some(expected), "{name}: wrong read");
        }
    }
}
