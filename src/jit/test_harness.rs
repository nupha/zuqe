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
//! bug in the test, not a supported input.

use std::cell::Cell;

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
        backedges: 0,
        inline_call_hits: 0,
        frame_inline_hits: 0,
        inline_call_misses: 0,
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
    assert!(
        inline_call::eligible(&verified),
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

    inline_call::lower(
        &mut builder,
        &verified,
        base,
        runtime_argc,
        method,
        0,
        entry,
        bail,
        merge,
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
    verify(snapshot, VerifyLimits::default()).is_ok_and(|body| inline_call::eligible(&body))
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
    let mut call_sites = Vec::with_capacity(sites.len());
    let mut callees = Vec::with_capacity(sites.len());
    for (index, (pc, callee)) in sites.iter().enumerate() {
        call_sites.push(CallSite {
            pc: *pc,
            callee: 1,
            polymorphic: false,
            inline: Some(index as u32),
        });
        let snapshot = CompileSnapshot::from_raw((*callee).to_vec(), 0, 0, 0, 32);
        let verified = verify(snapshot, VerifyLimits::default()).expect("verify inline callee");
        assert!(
            inline_call::eligible(&verified),
            "inline callee must be eligible"
        );
        callees.push(InlineCallee {
            bytecode: (*callee).into(),
            arg_count: 0,
            var_count: 0,
            local_buf_size: 32,
            stack_size: 32,
            mode: ExecMode::empty(),
            gc_scope_kind: GcScopeKind::None,
            const_meta: Box::new([]),
        });
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
        feedback: Box::new([]),
        call_sites: call_sites.into_boxed_slice(),
        inline_callees: callees.into_boxed_slice(),
        speculate: false,
    };
    let verified = verify(snapshot, VerifyLimits::default()).expect("verify caller");
    let isa = host_isa();
    compiler::compile(&verified, isa.as_ref(), &Cell::new(0)).map(|_| ())
}
