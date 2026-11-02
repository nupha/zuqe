//! The lowering state: frame values, the register caches and the
//! per-instruction result.

use super::*;

/// Values loaded once from the frame in the entry block; they dominate every
/// reachable block, so they can be used anywhere.
pub(super) struct FrameVals {
    pub(super) frame: Value,
    pub(super) sp_base: Value,
    pub(super) var_base: Value,
    pub(super) arg_base: Value,
    pub(super) cpools: Value,
    pub(super) cur_sp: Value,
    pub(super) helpers: Value,
    pub(super) this: Value,
    /// The two global objects (tagged) and the site cache. Functions with
    /// no global access pay three loads once per activation.
    pub(super) global_obj: Value,
    pub(super) global_var_obj: Value,
    pub(super) global_cache: Value,
}

pub(super) struct State<'a> {
    pub(super) blocks: HashMap<u32, Block>,
    pub(super) pending: Vec<u32>,
    pub(super) feedback: &'a [(u32, SiteFeedback)],
    /// The number of inline-cache sites the function has. The inline
    /// property guard reads a site's first way, so it may only be emitted for
    /// a site that exists — a function restored from a persistent blob has no
    /// inline cache at all (its sites are not restored), and its reads then
    /// keep the helper.
    pub(super) ic_count: u32,
    /// The number of global-variable site caches. A site is only inlined
    /// when its index is inside this count: a persistent restore has no cache,
    /// and its global accesses keep the helper.
    pub(super) global_cache_count: u32,
    pub(super) specialized_ops: &'a Cell<u64>,
    /// Whether tier-2 specialization is allowed (tier-down). False
    /// after a function's speculative sites missed too often: the recompile
    /// keeps the int fast paths but drops the speculation.
    pub(super) allow_speculation: bool,
    /// Block-local write-back cache for operand-stack slots (the stack-slot register cache).
    pub(super) stack: StackCache,
    /// Cross-block carry: per block start pc, the slots its code reads in
    /// registers before it can observe the frame. Each is a block parameter
    /// that every incoming edge supplies (see [`State::edge_args`]).
    pub(super) carried: HashMap<u32, Vec<u32>>,
    /// Carried locals: per block start pc, the locals/arguments its code reads
    /// before writing. They cross the incoming edges as block parameters too,
    /// so the successor's first read is a register, not a frame load.
    pub(super) carried_locals: HashMap<u32, Vec<LocalRef>>,
    /// Dead-store elimination: pcs whose local store is dead (no read before redefinition
    /// on any path), so the lowering skips the memory write. Empty unless the
    /// analysis' liveness is exact.
    pub(super) dead_stores: HashSet<u32>,
    /// The CSE pass: the value numbering's CSE plan (which expressions are
    /// repeats of an earlier one) and, per block, the registers those earlier
    /// computations published. Both are block-scoped like [`LocalCache`]: a
    /// value does not cross an edge as a register.
    pub(super) cse: RedundantExpressions,
    pub(super) cse_regs: HashMap<u32, Value>,
    /// Register copies of local/argument slots (the stack-slot register cache: fewer loads in a
    /// block). Cleared at every block entry — see [`LocalCache`].
    pub(super) locals: LocalCache,
    /// The pool's constant tags, for the `Int`-constant guard fact.
    pub(super) const_meta: &'a [ConstMeta],
    /// Proven entry stack height per block start pc. An edge's `sp` equals it,
    /// so it addresses the frame fallback loads even when the model's own
    /// height is stale (a helper-terminated block never re-anchors).
    pub(super) entry_heights: HashMap<u32, u16>,
    /// The write barrier's helper signature (`frame`, owner, value) — the property inline guard's
    /// inline store calls the barrier with its own arguments, so it needs a
    /// signature of its own.
    pub(super) barrier_sig: cranelift_codegen::ir::SigRef,
    /// The monomorphic call sites (the direct-call path): the opcode offset -> the site
    /// id the compiled code carries. A site that saw more than one callee is
    /// not here: its guard would miss every time.
    pub(super) direct_call_sites: HashMap<u32, u32>,
    /// Monomorphic sites whose callee body is copied into this function
    /// (the inlined call path). The value is the verified plan (the body plus
    /// its own nested sites); a site not in here keeps the direct-call helper.
    pub(super) inline_sites: HashMap<u32, crate::jit::inline_call::InlinePlan>,
    /// Call sites the interpreter only ever saw a non-bytecode callee at. The
    /// one such callee the compiler can serve directly is the builtin
    /// `Function.prototype.apply`: its helper recognizes it at run time, so no
    /// callee identity is baked in.
    pub(super) builtin_apply_sites: HashSet<u32>,
    /// Counter for the variable-id block an inlined body owns. Every emission
    /// gets a fresh block: Cranelift forbids declaring a variable twice.
    pub(super) inline_tag: u32,
    /// The merged object-literal windows, keyed by the `object` pc.
    pub(super) lit_windows: HashMap<u32, LiteralWindow>,
    /// instructions below this pc were consumed by a merged window
    /// lowered at `object`; the driver loop skips them.
    pub(super) skip_until_pc: u32,
}

/// Block-local register copies of local/argument slots (the stack-slot register cache, the read half
/// of "locals in registers"): a `get_loc`/`get_arg` that repeats one already in
/// a register skips its load.
///
/// Memory stays authoritative — stores are still emitted, nothing is deferred —
/// so no flush point changes: a copy is simply dropped the moment its slot can
/// change. A slot can only change through this function's own writes, *unless*
/// a nested function or a reference object aliases it, which is why
/// [`cacheable_slots`] turns caching off for such functions. Values never cross
/// a block: a register only dominates its own block, so `lower` clears the
/// cache at every block entry.
#[derive(Default)]
pub(super) struct LocalCache {
    pub(super) entries: HashMap<(bool, u32), Value>,
    /// Whether local slots may be cached (false when the function creates
    /// closures or refs to them).
    pub(super) locals: bool,
    /// Same for argument slots (also false when `arguments` is the *mapped*
    /// object, which aliases the formals through `VarRef`s).
    pub(super) args: bool,
}

impl LocalCache {
    pub(super) fn new(locals: bool, args: bool) -> Self {
        Self {
            entries: HashMap::new(),
            locals,
            args,
        }
    }

    /// The register copy of slot `idx`, or a fresh load (which also fills the
    /// cache). `is_arg` selects the frame base and its cacheability.
    pub(super) fn read(
        &mut self,
        b: &mut FunctionBuilder,
        base: Value,
        is_arg: bool,
        idx: u32,
        offset: i32,
    ) -> Value {
        if let Some(&value) = self.entries.get(&(is_arg, idx)) {
            return value;
        }
        let value = b.ins().load(types::I64, MemFlags::new(), base, offset);
        if self.cacheable(is_arg) {
            self.entries.insert((is_arg, idx), value);
        }
        value
    }

    /// Record `value` as the slot's register copy. Memory is written
    /// separately, so the copy costs no flush.
    pub(super) fn fill(&mut self, is_arg: bool, idx: u32, value: Value) {
        if self.cacheable(is_arg) {
            self.entries.insert((is_arg, idx), value);
        }
    }

    /// The slot changed: its copy is stale.
    pub(super) fn drop_slot(&mut self, is_arg: bool, idx: u32) {
        self.entries.remove(&(is_arg, idx));
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(super) const fn cacheable(&self, is_arg: bool) -> bool {
        if is_arg { self.args } else { self.locals }
    }
}

/// Whether [`LocalCache`] may cache local and argument slots of this function.
/// A slot is unsafe to cache exactly when something other than a direct write
/// in this function can change it: a nested function may capture it
/// (`fclosure`), a reference object may point at it (`make_loc_ref` /
/// `make_arg_ref` / `make_var_ref_ref`), or the `arguments` object may alias a
/// formal (`special_object 1`, the mapped variant).
pub(super) fn cacheable_slots(verified: &VerifiedFunction) -> (bool, bool) {
    let mut creates_refs = false;
    let mut mapped_arguments = false;
    for instruction in verified.instructions() {
        match instruction.name() {
            "fclosure" | "make_loc_ref" | "make_arg_ref" | "make_var_ref_ref" => {
                creates_refs = true
            }
            "special_object" if instruction.operand_u8(1) == MAPPED_ARGUMENTS => {
                mapped_arguments = true;
            }
            _ => {}
        }
    }
    if creates_refs {
        return (false, false);
    }
    (true, !mapped_arguments)
}

/// `SpecialObject::MappedArguments`, whose `VarRef` properties alias the
/// formals (see `src/function/argument.rs`).
const MAPPED_ARGUMENTS: u8 = 1;

/// Write-back cache for operand-stack slots (the stack-slot register cache).
///
/// A slot index is 1-based from the stack bottom and is re-anchored from the
/// verifier's proven height at the start of every instruction, so entries
/// cannot drift across a helper call — whose net stack effect is only known at
/// run time. Entries hold values that exist **only in registers**: they must be
/// written back before anything can observe the frame, so the flush points are
/// helper calls, exits, branch terminators and the end of the block. Producers
/// that already store to memory (arithmetic fast paths, helpers, shuffles)
/// insert nothing, which keeps memory authoritative for them and means a flush
/// never has to reconcile two copies.
///
/// Across a block edge the successor's carried slots arrive as block
/// parameters, which [`lower`] seeds into the model, and the incoming edge
/// passes them on instead of storing them (the flush points above still apply
/// — a carried value is flushed like any other before the frame is
/// observable).
/// One register-resident operand-stack slot.
pub(super) struct CachedSlot {
    pub(super) value: Value,
    /// Statically known to be `Int` (tag 0). Only ever set for values the
    /// compiler itself produced — a `push_i32`, an `Int` constant from the pool
    /// — so it needs no runtime check to be sound. It dies with the entry, which
    /// is what keeps it conservative: anything that flushes the cache (a helper
    /// call, an exit, a branch) drops the fact with the value.
    pub(super) known_int: bool,
}

impl CachedSlot {
    pub(super) const fn unknown(value: Value) -> Self {
        Self {
            value,
            known_int: false,
        }
    }
}

#[derive(Default)]
pub(super) struct StackCache {
    pub(super) height: u32,
    pub(super) entries: HashMap<u32, CachedSlot>,
    /// Reused scratch for flush ordering: a flush runs before every helper
    /// call, so it must not allocate.
    pub(super) scratch: Vec<u32>,
    /// Whether the current instruction changed `height` through *this* cache
    /// (push/pop/drop/adjust). Helper arms leave `height` stale — the next
    /// instruction re-anchors it — so only tracked effects are worth the
    /// per-instruction cross-check against the verifier.
    pub(super) tracked: bool,
}

impl StackCache {
    /// Re-anchor to the verified height at the start of an instruction.
    pub(super) const fn anchor(&mut self, height: u32) {
        self.height = height;
        self.tracked = false;
    }

    /// Byte offset of slot `k` from the current `sp`. At every point this
    /// cache is consulted, `sp == sp_base + height * 8`; the offset goes
    /// straight into the memory instruction, so a flush costs no address math.
    pub(super) const fn slot_offset(height: u32, k: u32) -> i32 {
        ((k as i64 - 1 - height as i64) * 8) as i32
    }

    /// Keep `v` in a register instead of storing it; the store happens at the
    /// next flush, or never if the slot dies first (DCE).
    pub(super) fn push(&mut self, b: &mut FunctionBuilder, sp: Value, v: Value) -> Value {
        self.push_slot(b, sp, CachedSlot::unknown(v))
    }

    /// [`Self::push`] for a value the compiler produced as an `Int` (a literal
    /// or an `Int` constant), which then needs no tag check.
    pub(super) fn push_int(&mut self, b: &mut FunctionBuilder, sp: Value, v: Value) -> Value {
        self.push_slot(
            b,
            sp,
            CachedSlot {
                value: v,
                known_int: true,
            },
        )
    }

    pub(super) fn push_slot(
        &mut self,
        b: &mut FunctionBuilder,
        sp: Value,
        slot: CachedSlot,
    ) -> Value {
        self.height = self.height.saturating_add(1);
        self.tracked = true;
        self.entries.insert(self.height, slot);
        b.ins().iadd_imm(sp, 8)
    }

    /// Is slot `k` statically an `Int`? A miss (already in memory) is unknown.
    pub(super) fn known_int(&self, k: u32) -> bool {
        self.entries.get(&k).is_some_and(|slot| slot.known_int)
    }

    /// Pop the top slot: reuse the register-resident value when there is one,
    /// otherwise load it from the frame.
    pub(super) fn pop(&mut self, b: &mut FunctionBuilder, sp: Value) -> (Value, Value) {
        debug_assert!(self.height > 0, "verified code cannot underflow");
        let k = self.height;
        self.height = self.height.saturating_sub(1);
        self.tracked = true;
        match self.entries.remove(&k) {
            Some(slot) => (b.ins().iadd_imm(sp, -8), slot.value),
            None => {
                let sp = b.ins().iadd_imm(sp, -8);
                let v = b.ins().load(types::I64, MemFlags::new(), sp, 0);
                (sp, v)
            }
        }
    }

    /// Read slot `k` without consuming it.
    pub(super) fn read(&self, b: &mut FunctionBuilder, sp: Value, k: u32) -> Value {
        match self.entries.get(&k) {
            Some(slot) => slot.value,
            None => b.ins().load(
                types::I64,
                MemFlags::new(),
                sp,
                Self::slot_offset(self.height, k),
            ),
        }
    }

    /// Read the top slot without consuming it.
    pub(super) fn read_top(&self, b: &mut FunctionBuilder, sp: Value) -> Value {
        self.read(b, sp, self.height)
    }

    /// Forget the top slot without materializing it: the value is dead.
    pub(super) fn drop_top(&mut self) {
        let k = self.height;
        self.height = self.height.saturating_sub(1);
        self.tracked = true;
        self.entries.remove(&k);
    }

    /// A binary op popped two operands and pushed one result. Both operand
    /// entries die here and the result is *not* cached: the fast path and the
    /// helper each store it, so memory stays authoritative for slot `height`.
    pub(super) fn consumed_binary(&mut self) {
        let top = self.height;
        self.height = self.height.saturating_sub(1);
        self.tracked = true;
        self.entries.remove(&top);
        self.entries.remove(&(top - 1));
    }

    /// `put_array_el`: three operands died and nothing replaced them.
    pub(super) fn consumed_ternary(&mut self) {
        let top = self.height;
        self.height = self.height.saturating_sub(3);
        self.tracked = true;
        self.entries.remove(&top);
        self.entries.remove(&(top - 1));
        self.entries.remove(&(top - 2));
    }

    /// `get_array_el2`: the value took the key's place, and the array below
    /// it is no longer register-resident (the helper's path flushes it).
    pub(super) fn replaced_pair(&mut self) {
        let top = self.height;
        self.entries.remove(&top);
        self.entries.remove(&(top - 1));
    }

    /// `put_field`: two operands died and nothing replaced them.
    pub(super) fn consumed_pair(&mut self) {
        let top = self.height;
        self.height = self.height.saturating_sub(2);
        self.tracked = true;
        self.entries.remove(&top);
        self.entries.remove(&(top - 1));
    }

    /// The top slot was overwritten in place (a property read replaces its
    /// target): the slot keeps its height, but its register copy is stale.
    pub(super) fn replace_top(&mut self) {
        self.entries.remove(&self.height);
    }

    /// Record that the current top slot (key `height`) is a known `Int`,
    /// without changing the height or `sp`. Only sound when *every* path that
    /// reaches the slot boxes an `Int` (the local phi design: `shl`/`sar`).
    pub(super) fn result_int(&mut self, sp: Value, value: Value) -> Value {
        self.entries.insert(
            self.height,
            CachedSlot {
                value,
                known_int: true,
            },
        );
        sp
    }

    /// Apply a raw stack effect the lowering computes itself — shuffles are
    /// the only ops that pop `n` and push `k` without touching cached values.
    pub(super) fn adjust(&mut self, delta: i32) {
        let height = i64::from(self.height) + i64::from(delta);
        debug_assert!(height >= 0, "verified code cannot underflow");
        self.height = height.max(0) as u32;
        self.tracked = true;
    }

    /// Write register-resident slots back to the frame, except `keep` — the
    /// slots a *sibling* branch still needs in registers (they die on the path
    /// that skips this flush). Keys are sorted so the emitted store order
    /// (and with it the generated code) is stable.
    ///
    /// A flush must be emitted in a block that dominates every path whose
    /// model it changes: clearing the model at compile time is only correct if
    /// the stores really execute on all those paths.
    pub(super) fn flush_except(&mut self, b: &mut FunctionBuilder, sp: Value, keep: &[u32]) {
        if self.entries.is_empty() {
            return;
        }
        self.scratch.clear();
        self.scratch
            .extend(self.entries.keys().copied().filter(|k| !keep.contains(k)));
        self.scratch.sort_unstable();
        let height = self.height;
        for &k in &self.scratch {
            let Some(slot) = self.entries.get(&k) else {
                continue;
            };
            b.ins().store(
                MemFlags::new(),
                slot.value,
                sp,
                Self::slot_offset(height, k),
            );
            self.entries.remove(&k);
        }
        self.scratch.clear();
    }

    /// Write every register-resident slot back to the frame.
    pub(super) fn flush(&mut self, b: &mut FunctionBuilder, sp: Value) {
        self.flush_except(b, sp, &[]);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<'a> State<'a> {
    pub(super) fn block_for(&mut self, b: &mut FunctionBuilder, pc: u32) -> Block {
        if let Some(block) = self.blocks.get(&pc) {
            return *block;
        }
        let block = b.create_block();
        // Cross-block carry: the slots this block consumes in registers are block
        // parameters. They are appended here, before any edge exists, so every
        // edge to this block sees the same signature (`edge_args` supplies it).
        // Stack slots come first, then the carried locals.
        for _ in 0..self.carried_for(pc).len() {
            b.append_block_param(block, types::I64);
        }
        for _ in 0..self.carried_locals_for(pc).len() {
            b.append_block_param(block, types::I64);
        }
        self.blocks.insert(pc, block);
        self.pending.push(pc);
        block
    }

    /// Slots block `pc` reads from registers before it can observe the frame —
    /// empty when it reads none, or for a pc the walk never saw.
    pub(super) fn carried_for(&self, pc: u32) -> &[u32] {
        self.carried.get(&pc).map_or(&[], Vec::as_slice)
    }

    /// Locals/arguments block `pc` reads before writing — carried as block
    /// parameters right after the carried stack slots.
    pub(super) fn carried_locals_for(&self, pc: u32) -> &[LocalRef] {
        self.carried_locals.get(&pc).map_or(&[], Vec::as_slice)
    }

    /// Sorted, deduplicated union of several successors' carried sets: the
    /// `keep` a shared flush has to spare.
    pub(super) fn carried_union(&self, pcs: &[u32]) -> Vec<u32> {
        let mut keep = Vec::new();
        for &pc in pcs {
            keep.extend_from_slice(self.carried_for(pc));
        }
        keep.sort_unstable();
        keep.dedup();
        keep
    }

    /// Leave on edges to every `(block, pc)` in `targets`: write back what none
    /// of them carries, then read each carried slot as a block argument — the
    /// register value when this path still holds one, the frame otherwise. The
    /// model is delivered here (cleared); the successor re-seeds it from the
    /// parameters (see [`lower`]), so a carried slot crosses the edge with
    /// neither a store nor a reload.
    ///
    /// `flush_except` only emits stores for slots outside the union, so it must
    /// run where those stores dominate every path this edge can be reached
    /// from: the fallthrough and a forward `goto` are single-path, and the
    /// conditional arms are reached after `lower_if`'s shared full flush, which
    /// already left the model empty. Never call it once per arm of an
    /// unflushed split — the stores would exist on one path only.
    pub(super) fn edge_args(
        &mut self,
        b: &mut FunctionBuilder,
        fv: &FrameVals,
        sp: Value,
        targets: &[(Block, u32)],
    ) -> Vec<Vec<Value>> {
        let pcs: Vec<u32> = targets.iter().map(|&(_, pc)| pc).collect();
        let keep = self.carried_union(&pcs);
        self.stack.flush_except(b, sp, &keep);
        let model_height = self.stack.height;
        let mut out = Vec::with_capacity(targets.len());
        for &(_, pc) in targets {
            debug_assert!(
                self.entry_heights.contains_key(&pc),
                "edge target pc {pc} was never walked"
            );
            let entry_h = self
                .entry_heights
                .get(&pc)
                .map_or(model_height, |&h| u32::from(h));
            // A stale model height implies a flushed (empty) model — helpers
            // leave `height` at the anchor and clear every entry.
            debug_assert!(self.stack.entries.is_empty() || model_height == entry_h);
            let mut args = Vec::with_capacity(self.carried_for(pc).len());
            for &k in self.carried_for(pc) {
                let value = match self.stack.entries.get(&k) {
                    Some(slot) => slot.value,
                    None => b.ins().load(
                        types::I64,
                        MemFlags::new(),
                        sp,
                        StackCache::slot_offset(entry_h, k),
                    ),
                };
                args.push(value);
            }
            // The carried locals follow, in the same order
            // `block_for` appended their parameters. Memory is authoritative
            // here (every write stores), so a miss is a plain frame load.
            for slot in self.carried_locals_for(pc) {
                let value = match self.locals.entries.get(&(slot.is_arg, slot.idx)) {
                    Some(&value) => value,
                    None => {
                        let base = if slot.is_arg {
                            fv.arg_base
                        } else {
                            fv.var_base
                        };
                        b.ins()
                            .load(types::I64, MemFlags::new(), base, (slot.idx * 8) as i32)
                    }
                };
                args.push(value);
            }
            out.push(args);
        }
        // Delivered as block arguments; the successor seeds its own model.
        self.stack.entries.clear();
        out
    }

    /// Speculative fast path: did type feedback observe *only* `Int` at this site? Feedback is
    /// sorted by pc, so a binary search answers it. `check_rhs` is false for
    /// single-value sites (`inc_loc`/`dec_loc`), whose `rhs` slot stays 0.
    /// Always false once tier-down disabled specialization for this function.
    pub(super) fn int_only(&self, pc: u32, check_rhs: bool) -> bool {
        self.allow_speculation && int_only(self.feedback, pc, check_rhs)
    }

    pub(super) fn note_specialized(&self) {
        self.specialized_ops.set(self.specialized_ops.get() + 1);
    }

    /// The CSE pass: the register holding the value this instruction is a
    /// repeat of, if the pass planned a reuse and the definition actually ran on
    /// the path taken to here (an OSR entry in the middle of the block skips
    /// it, and then the instruction computes normally).
    pub(super) fn cse_reuse(&self, pc: u32) -> Option<Value> {
        self.cse
            .reuse(pc)
            .and_then(|id| self.cse_regs.get(&id).copied())
    }

    /// Whether a later instruction reuses this one's result, which then has to
    /// stay in a register.
    pub(super) fn cse_keep(&self, pc: u32) -> Option<u32> {
        self.cse.keep(pc)
    }
}

pub(super) enum Step {
    Continue(Value),
    /// The block was terminated (branch, return, or `Resume` exit).
    Stop,
}
