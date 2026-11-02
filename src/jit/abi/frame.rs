//! The frame layout, the exit protocol, the per-function slot and the
//! status codes every helper returns.

use super::*;

/// Status returned by the stack helpers: `0` is success, `1`/`2` exit the
/// native frame with `JitExitKind::Exception`/`Interrupt`, and any other value
/// is a status the caller interprets itself (see [`HELPER_WITH_MISS`]).
pub(crate) const HELPER_OK: u32 = 0;
pub(crate) const HELPER_EXCEPTION: u32 = 1;
pub(crate) const HELPER_INTERRUPT: u32 = 2;
/// `with_*`: the environment object did not have the name, so the helper
/// dropped it and the generated code falls through to the normal variable
/// access. `0` is the hit — the access was performed and the stack reshaped for
/// the instruction's branch target.
pub(crate) const HELPER_WITH_MISS: u32 = 3;

/// What the native entry returns. Stored as a `u32` so the ABI is a single
/// register (a big `#[repr(C)]` struct return would need an sret pointer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub(crate) enum JitExitKind {
    Done = 0,
    Exception = 1,
    Interrupt = 2,
    Resume = 6,
    Retry = 7,
}

impl JitExitKind {
    pub(crate) const fn from_raw(raw: u32) -> Self {
        match raw {
            0 => Self::Done,
            1 => Self::Exception,
            2 => Self::Interrupt,
            6 => Self::Resume,
            _ => Self::Retry,
        }
    }
}

/// The raw codes above, as the `i64` the lowering embeds. Derived from
/// [`JitExitKind`] so the two cannot drift.
pub(crate) const EXIT_DONE: i64 = JitExitKind::Done as i64;
pub(crate) const EXIT_RESUME: i64 = JitExitKind::Resume as i64;
pub(crate) const EXIT_RETRY: i64 = JitExitKind::Retry as i64;

/// The addend that turns a real double's raw bits into a `JSValue` payload
/// (`From<f64>`: the top 16 bits must land in the Float64 tag window).
pub(crate) const FLOAT64_ADDEND: i64 = 0xFFF9 << 48;

/// Which object a cached global variable lives on.
///
/// `let`/`const` bindings live on `global_var_obj`, which shadows the global
/// object; `var`/`function` declarations are ordinary properties of
/// `global_obj`.
pub(crate) const GLOBAL_CACHE_LEXICAL: u8 = 1;
pub(crate) const GLOBAL_CACHE_GLOBAL: u8 = 2;

/// One global-variable site's inline-guard cache.
///
/// The generated guard re-checks the owning object's shape (pointer + id) and
/// re-reads the slot and its tag on **every** access, so a change is a miss,
/// never a stale read; there is no invalidation channel. The entry is filled by
/// the same helpers that own the full lookup, so both sides cannot disagree
/// about what a hit means.
///
/// A lexical binding shadows a global-object property of the same name, so a
/// global-object entry also carries `global_var_obj`'s shape: if a lexical
/// binding appears, the guard misses instead of reading the shadowed property.
///
/// The shape pointers are compared only, never dereferenced: the object that
/// just matched holds the shape alive, and `owner_uid`/`lexical_uid` defeat
/// address reuse (the same contract as the inline-cache sites).
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct GlobalVarCache {
    /// The owning object's shape payload pointer (`global_obj` or
    /// `global_var_obj`, per `kind`).
    pub(crate) owner_shape: *const crate::object::ShapeImpl,
    /// `global_var_obj`'s shape payload pointer.
    pub(crate) lexical_shape: *const crate::object::ShapeImpl,
    /// The owning shape's id.
    pub(crate) owner_uid: u32,
    /// `global_var_obj`'s shape id.
    pub(crate) lexical_uid: u32,
    /// The property slot inside the owning object.
    pub(crate) slot: u32,
    /// [`GLOBAL_CACHE_LEXICAL`], [`GLOBAL_CACHE_GLOBAL`], or `0` (empty).
    pub(crate) kind: u8,
    /// The property's flags (`JS_PROP_*`), for the write guard's writability
    /// check. A flag change mints a fresh shape id, so a cached flag is never
    /// stale.
    pub(crate) flags: u8,
    pub(crate) _pad: u16,
}

impl Default for GlobalVarCache {
    fn default() -> Self {
        Self {
            owner_shape: std::ptr::null(),
            lexical_shape: std::ptr::null(),
            owner_uid: 0,
            lexical_uid: 0,
            slot: 0,
            kind: 0,
            flags: 0,
            _pad: 0,
        }
    }
}

/// Everything a compiled function needs. Reconstructed by the interpreter at
/// the call site; helpers re-derive `ScopedContext`/`RuntimeImpl` from the raw
/// pointers. Field offsets are read by generated code, so the layout is frozen.
#[repr(C)]
pub(crate) struct JitExecFrame {
    /// `*mut ScopedContext`.
    pub scope: *mut u8,
    /// Points at the frame's argument slots.
    pub arg_base: *mut JSValue,
    /// Points at the frame's local slots.
    pub var_base: *mut JSValue,
    /// Points at the frame's operand-stack base.
    pub sp_base: *mut JSValue,
    /// Current operand-stack top (absolute). Updated by the native code and by
    /// stack helpers.
    pub sp: *mut JSValue,
    /// `Bytecode::consts().as_ptr()`.
    pub cpools: *const JSValue,
    /// `&mut StackFrame::cur_sp_offset` (GC scans up to this many slots).
    pub cur_sp_offset: *mut i32,
    /// `HELPERS.as_ptr()`.
    pub helpers: *const u8,
    pub this: JSValue,
    /// `GcScopeKind` as a `u32` (its discriminant: 0 = Full, 1 = Light, 2 = None).
    pub gc_scope_kind: u32,
    /// Immediate for the few helpers that need one (`push_atom_value`, global
    /// variable index, property atom).
    pub imm: u32,
    /// Second immediate (inline-cache index for `get_field`/`put_field`).
    pub imm2: u32,
    /// Entry pc: 0 is the function entry; any other value is an OSR resume
    /// point (a loop header for a hot loop, or the pc after a call the
    /// trampoline performed on our behalf).
    pub pc: u32,
    /// The running function's bytecode, for helpers that need more than a
    /// scalar (global-variable names; the inline cache). Valid for the whole
    /// call: the frame's `func_obj` keeps it reachable.
    pub bytecode: *const crate::function::BytecodeImpl,
    /// The running function's inline-cache sites. The inline property
    /// guard reads the site's first way to learn in which slot the property
    /// lives.
    #[cfg(feature = "inline_cache")]
    pub ic_sites: *const crate::function::InlineCacheSite,
    /// Return value for `Done`.
    pub out_value: JSValue,
    /// Byte offset into the opcode stream for `Resume`/`Exception`.
    pub out_pc: u32,
    /// Slot offset from `sp_base` for `Resume`/`Exception`.
    pub out_sp: i32,
    /// Actual argument count of the current call (`StackFrame::arg_real_count`,
    /// i.e. before the `undefined` padding up to `arg_count`). `rest` collects
    /// `arg_base[start..arg_real_count]`, exactly like the interpreter's
    /// `args` slice.
    pub arg_real_count: u32,
    /// `*mut VmStack`: the running interpreter's arena. Frame-entry helpers
    /// (`special_object`, `fclosure`) reach the current `StackFrame` through
    /// `arena` + `cur_frame_idx`, the same way the interpreter does.
    pub arena: *mut VmStack,
    /// Index of the running frame inside `arena` (`FrameIndex::as_usize`).
    pub cur_frame_idx: u32,
    /// The activation's `new.target` (the interpreter's per-call value).
    pub new_target: JSValue,
    /// The activation's `[[HomeObject]]`, when the callee is a method.
    pub home_obj: Option<Object>,
    /// Tier-down instrumentation: speculative int fast paths that bailed to their
    /// helper during this activation. `invoke_native` folds it into
    /// `JitMetrics::specialized_guard_misses` after the call returns; counting
    /// per activation (not per function) keeps recursive calls from
    /// double-counting.
    pub guard_misses: u32,
    /// Property-guard instrumentation: property accesses the inline guard answered
    /// directly (an own data property in the shape the site recorded — a read
    /// or a write).
    /// `invoke_native` folds it into `JitMetrics::property_guard_hits`.
    pub property_guard_hits: u32,
    /// Property-guard instrumentation: property accesses the inline guard handed back
    /// to the helper (a prototype-chain property, an accessor, a non-object
    /// target, …). `invoke_native` folds it into
    /// `JitMetrics::property_guard_misses`.
    ///
    /// These two are *observability only*: a miss is expected (and common),
    /// so it must never feed the tier-down (that is `guard_misses`' job).
    pub property_guard_misses: u32,
    /// Global-variable accesses the inline guard answered
    /// directly (an own data property of `global_obj`/`global_var_obj`).
    /// `invoke_native` folds it into `JitMetrics::global_guard_hits`.
    pub global_guard_hits: u32,
    /// Global-variable accesses handed back to the helper
    /// (an unbound name, a TDZ slot, a read-only binding, an accessor, a shape
    /// change). Observability only, like the property counters.
    pub global_guard_misses: u32,
    /// Loop back edges this activation ran. The safepoint helper counts them
    /// (it runs on every back edge), and `invoke_native` folds the count into
    /// the function's hotness counters: the interpreter stops counting once
    /// the code is installed, so the second gate needs
    /// the native tier's own back-edge count.
    pub backedges: u32,
    /// Calls at a monomorphic site that took the direct path (the
    /// callee's compiled code was entered without the interpreter's call
    /// protocol), or the guard-passed inline site.
    pub mono_call_hits: u32,
    /// The inlined call path: calls whose site passed the inline guard and ran
    /// the callee's body copied into the caller.
    pub inline_body_hits: u32,
    /// The inlined call path: nested call sites inside a copied body
    /// whose constant-identity guard passed. Counted by the generated code.
    pub nested_inline_hits: u32,
    /// Calls at a monomorphic site whose guard missed (the callee
    /// was not the recorded one), or whose callee the direct path could not
    /// serve — those took the general call path.
    pub mono_call_misses: u32,
    /// The inlined call path: the value an allocation helper produced for
    /// an inlined body. The generated code reads it immediately; no GC point
    /// can run between the helper and the read.
    pub scratch: JSValue,
    /// The global object, as a tagged `JSValue`. Both global objects are
    /// roots of their context, so the frame needs no tracing for them.
    pub global_obj: JSValue,
    /// `global_var_obj`: the `let`/`const` bindings. It shadows
    /// [`Self::global_obj`].
    pub global_var_obj: JSValue,
    /// The running function's global-variable site cache, one entry per
    /// `global_var_refs` index.
    pub global_var_cache: *mut GlobalVarCache,
    /// The largest GC-scope LOCAL list this activation's safepoints saw,
    /// sampled just before a deferred collection clears it. A scope that is
    /// never cleared shows up as a peak that
    /// grows with the run. `invoke_native` folds it into
    /// `JitMetrics::scope_local_peak`.
    pub scope_local_peak: u32,
    /// the atoms of a merged object literal. The generated code stores
    /// them here right before `h_object_literal` runs (like `imm`, they are
    /// per-call immediates the helper signature cannot carry); only that
    /// helper reads them, and only the count in `imm` entries are live.
    pub lit_atoms: [u32; LIT_ATOM_MAX],
}

/// The largest object literal a merged window covers in one `h_object_literal` call.
/// A window with more fields keeps the per-field helper sequence, so this is
/// a code-shape bound, not a semantic one.
pub(crate) const LIT_ATOM_MAX: usize = 8;

/// The object in the prototype chain that *owns* `prop` (the one whose own
/// property it is), and how far up the chain it was found.
///
/// The depth counts the instance as 0, its prototype as 1, and so on. The
/// typed array's `length` is owned by `%TypedArray%.prototype`, which sits
/// two levels up (the typed array's own prototype is in between).
pub(super) fn proto_chain_owner_of(
    obj: &crate::object::Object,
    prop: crate::atom::Atom,
) -> Option<(crate::object::Object, usize)> {
    for (depth, cur) in obj.shape_proto_iter(true).enumerate() {
        if cur.find_shape_property(prop).is_some() {
            return Some((cur, depth));
        }
    }
    None
}

/// The compiled entry. Returns a [`JitExitKind`] as a `u32`.
pub(crate) type NativeEntry = unsafe extern "C" fn(*mut JitExecFrame) -> u32;

/// `imm2` value the speculative `add_loc`/`inc_loc`/`dec_loc` slow paths write
/// before calling their helper, so the helper knows this call is a guard miss
/// (tier-down) and not the ordinary helper path. Chosen out of range of the other
/// `imm2` users (`get_field`/`put_field` inline-cache indices).
pub(crate) const SPEC_MISS_IMM2: u32 = u32::MAX;

/// Per-function storage for the installed native entry, living on
/// `BytecodeImpl`: `None` until a compile installs one. It holds a strong `Rc`
/// to its [`CodeSlot`], so the mapping lives exactly as long as a function
/// that can call it. Written through `&Bytecode` (interior mutability); all
/// access happens on the single runtime thread.
///
/// [`CodeSlot`]: crate::jit::code_cache::CodeSlot
pub(crate) struct FunctionSlot {
    slot: std::cell::UnsafeCell<Option<std::rc::Rc<crate::jit::code_cache::CodeSlot>>>,
    /// Whether the installed code is at the **baseline** tier, i.e. whether
    /// it may still be promoted to the optimizing tier (the second gate).
    /// Only such a function needs the native tier's per-call bookkeeping —
    /// the flag sits next to the entry pointer, so the check is a cache hit.
    promotable: std::cell::Cell<bool>,
    /// This bytecode's identity in the coordinator's record table, assigned on
    /// first use (`0` = not assigned yet).
    ///
    /// The record table cannot be keyed by the GC node address: gc-lite reuses
    /// freed addresses, so a *new* function allocated where a dead one lived
    /// would inherit its heat, tier and queue state (and could be skipped as
    /// "already compiled"). A serial is unique for the process and travels with
    /// the bytecode, so the record outlives exactly the function it describes.
    serial: std::cell::Cell<u64>,
}

/// Source of [`FunctionSlot::serial`]. Process-wide (not per runtime): a
/// bytecode can be shared across runtimes of the same realm, and a per-runtime
/// counter could hand the same bytecode a different id in each of them.
static NEXT_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Default for FunctionSlot {
    fn default() -> Self {
        Self {
            slot: std::cell::UnsafeCell::new(None),
            promotable: std::cell::Cell::new(false),
            serial: std::cell::Cell::new(0),
        }
    }
}

impl FunctionSlot {
    /// This bytecode's record-table identity, assigned on first use.
    ///
    /// One relaxed load on the hot path (the hooks already read this struct for
    /// the entry pointer); the atomic is only touched the first time a
    /// function's hotness is recorded.
    #[inline]
    pub(crate) fn serial(&self) -> u64 {
        let serial = self.serial.get();
        if serial != 0 {
            return serial;
        }
        let serial = NEXT_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.serial.set(serial);
        serial
    }

    /// The installed entry, or `None` while the function is interpreted (or
    /// after the cache evicted its mapping). The returned pointer stays valid
    /// for as long as this slot holds the code.
    #[inline]
    pub(crate) fn entry(&self) -> Option<NativeEntry> {
        // SAFETY: single-threaded; a short read with no aliasing reference held
        // across it (the caller copies the entry pointer out immediately).
        let slot = unsafe { &*self.slot.get() }.as_ref()?;
        let code = slot.entry()?;
        // SAFETY: `code` is the start of code produced by `compiler::compile`
        // for the same `NativeEntry` ABI.
        Some(unsafe { std::mem::transmute::<*const u8, NativeEntry>(code) })
    }

    /// Whether code is installed, without counting a native entry. The
    /// presence checks (`note_call`, `note_loop`, the direct-call path) must
    /// use this: reading the entry pointer alone does not mean the code ran.
    pub(crate) fn has_code(&self) -> bool {
        // SAFETY: single-threaded read.
        let slot = unsafe { &*self.slot.get() }.as_ref();
        slot.is_some_and(|slot| slot.has_code())
    }

    /// Publish `slot` (whose code is already executable). `poll` is the only
    /// caller and runs between native activations, so no in-flight call
    /// observes the swap. Returns the mapping it replaced, if any.
    pub(crate) fn install(
        &self,
        slot: std::rc::Rc<crate::jit::code_cache::CodeSlot>,
    ) -> Option<std::rc::Rc<crate::jit::code_cache::CodeSlot>> {
        // SAFETY: single-threaded; see the type's note.
        unsafe { (*self.slot.get()).replace(slot) }
    }

    /// Whether the installed code may still be promoted to the optimizing
    /// tier (the second gate).
    #[inline]
    pub(crate) const fn promotable(&self) -> bool {
        self.promotable.get()
    }

    /// Record whether the installed code may still be promoted.
    #[inline]
    pub(crate) fn set_promotable(&self, promotable: bool) {
        self.promotable.set(promotable);
    }
}
