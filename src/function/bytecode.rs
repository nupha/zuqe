// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::UnsafeCell, marker::PhantomData};

use gc_lite::{GcNode, GcRef, GcTrace, GcTraceCtx};

use crate::{
    atom::{Atom, is_const_atom, is_small_int_atom},
    bytecode::{OpcodeFormat, opcode_info_final},
    context::{ContextImpl, ContextPtr, ScopedContext},
    error::JSResult,
    eval::EvalType,
    function::{FunctionDef, VarRef},
    gcref_wrapper,
    utils::{get_leb128, get_sleb128, read_from_ptr},
    value::JSValue,
    vm_stack::{ExecMode, FrameIndex},
};

use super::{FunctionKind, closure::ClosureVar, var::JSVarDef};

/// A single inline-cache entry for a `get_field`/`get_field2`/`put_field` or
/// `get_array_el*`/`put_array_el` site.
///
/// Each entry caches the result of the last full shape-property lookup so that
/// subsequent executions of the same bytecode instruction can skip the
/// HashList traversal and directly index into prop_values.
///
/// The (shape_ptr, shape_uid, prop) triple together identify the cached
/// property: `prop` is the atom the site observed (the instruction's own atom
/// for `*_field`, the runtime key for array elements, whose atom is not known
/// statically). The (shape_ptr, shape_uid) pair alone defeats the ABA problem:
/// even if a freed shape's memory is reused by a new shape at the same address,
/// the uid will differ, preventing a stale cache hit.
#[cfg(feature = "inline_cache")]
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct InlineCacheEntry {
    /// ShapeImpl pointer from the last successful lookup. null = uninitialized.
    pub(crate) shape_ptr: *const crate::object::ShapeImpl,
    /// Shape's unique ID at cache time, paired with `shape_ptr` to prevent ABA.
    pub(crate) shape_uid: u32,
    /// Cached slot index into the object's prop_values array.
    pub(crate) slot: u32,
    /// Atom id of the cached property. Atoms are never freed or reused, so
    /// storing an id observed at runtime (array element keys) is safe.
    pub(crate) prop: u32,
    /// Cached property flags (e.g. Writable, Configurable, GetSet …).
    /// Only the low 6 bits are used (JS_PROP_*); stored as u8 for clarity.
    pub(crate) flags: u8,
}

/// The kind of an inline-cache entry (the property inline guard's tail): the ordinary one (an own
/// property, cached by its slot) or a typed array's `length`.
///
/// The typed array's `length` lives on the prototype, so the ordinary slot
/// cache cannot serve it: the entry keeps the instance's shape and the shape
/// that *owns* `length` in the prototype chain instead (see
/// [`InlineCacheSite::insert_typed_array_length`]).
///
/// The property flags use the low 6 bits, so the top bit is free for the kind.
#[cfg(feature = "inline_cache")]
pub(crate) const IC_FLAG_TYPED_ARRAY_LENGTH: u8 = 0x80;

/// Distinct shapes one inline-cache site may hold before it starts evicting
/// (the property inline cache). 4 keeps a monomorphic site a single compare on `ways[0]` while
/// covering the small polymorphic loops real code produces; past it a site
/// round-robin evicts, which is correct but stops being a win.
#[cfg(feature = "inline_cache")]
pub(crate) const IC_WAYS: usize = 4;

/// One property-access site: up to [`IC_WAYS`] cached `(shape, prop)` entries
/// plus the cursor that picks the eviction victim. Both the interpreter and the
/// JIT helpers go through [`Self::find`]/[`Self::insert`], so the two fast paths
/// can never drift apart.
#[cfg(feature = "inline_cache")]
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct InlineCacheSite {
    ways: [InlineCacheEntry; IC_WAYS],
    /// Round-robin victim index, advanced only when a new shape arrives at an
    /// already-full site.
    cursor: u8,
}

/// The layout the JIT's inline property guard reads.
///
/// The guard reads a site's **first way** — the shape the site saw last — to
/// learn in which slot the property lives, so it needs the offsets of that
/// way's fields and the size of a site. The site and its entries are
/// `#[repr(C)]`, so the offsets are the same on every target.
#[cfg(feature = "inline_cache")]
pub(crate) mod jit_layout {
    use super::{InlineCacheEntry, InlineCacheSite};

    /// The size of one inline-cache site.
    pub(crate) const SITE_SIZE: i64 = std::mem::size_of::<InlineCacheSite>() as i64;
    /// The offset of a way's cached shape pointer.
    pub(crate) const SHAPE_PTR_OFFSET: i32 =
        std::mem::offset_of!(InlineCacheEntry, shape_ptr) as i32;
    /// The offset of a way's cached shape id.
    pub(crate) const SHAPE_UID_OFFSET: i32 =
        std::mem::offset_of!(InlineCacheEntry, shape_uid) as i32;
    /// The offset of a way's cached slot index.
    pub(crate) const SLOT_OFFSET: i32 = std::mem::offset_of!(InlineCacheEntry, slot) as i32;
    /// The offset of a way's cached property id (the atom — or, for a typed
    /// array's `length`, the id of the shape that owns it in the chain).
    pub(crate) const PROP_OFFSET: i32 = std::mem::offset_of!(InlineCacheEntry, prop) as i32;
    /// The offset of a way's flags (the property flags, plus the kind bit).
    pub(crate) const FLAGS_OFFSET: i32 = std::mem::offset_of!(InlineCacheEntry, flags) as i32;
}

#[cfg(feature = "inline_cache")]
impl InlineCacheSite {
    /// The cached `(slot, flags)` for `(shape, prop)`, if this site has seen it.
    /// `shape_ptr` is only ever compared, never dereferenced (see the GC
    /// safety note on `BytecodeImpl::inline_cache`).
    pub(crate) fn find(
        &self,
        shape_ptr: *const crate::object::ShapeImpl,
        shape_uid: u32,
        prop: u32,
    ) -> Option<(u32, u8)> {
        self.ways
            .iter()
            .find(|w| w.shape_ptr == shape_ptr && w.shape_uid == shape_uid && w.prop == prop)
            .map(|w| (w.slot, w.flags))
    }

    /// Record `(shape, prop) -> (slot, flags)`: refresh the matching way, else
    /// fill the first empty way (a monomorphic site therefore stays on `ways[0]`
    /// and costs exactly what the old single entry cost), else evict
    /// round-robin.
    pub(crate) fn insert(
        &mut self,
        shape_ptr: *const crate::object::ShapeImpl,
        shape_uid: u32,
        prop: u32,
        slot: u32,
        flags: u8,
    ) {
        let way =
            match self.ways.iter().position(|w| {
                w.shape_ptr == shape_ptr && w.shape_uid == shape_uid && w.prop == prop
            }) {
                Some(i) => i,
                None => match self.ways.iter().position(|w| w.shape_ptr.is_null()) {
                    Some(i) => i,
                    None => {
                        let i = self.cursor as usize % IC_WAYS;
                        self.cursor = self.cursor.wrapping_add(1);
                        i
                    }
                },
            };
        self.ways[way] = InlineCacheEntry {
            shape_ptr,
            shape_uid,
            slot,
            prop,
            flags,
        };
    }

    /// Fill a free way only; once every way is taken, keep what is there.
    ///
    /// Array-element sites cache runtime keys, so a dynamic-key loop (for-in
    /// over a dictionary object, or `a[i]` over a sparse array) would otherwise
    /// evict round-robin on every miss and never converge. Field sites keep
    /// `insert`, whose static key makes eviction useful for polymorphic shapes.
    pub(crate) fn insert_fill_only(
        &mut self,
        shape_ptr: *const crate::object::ShapeImpl,
        shape_uid: u32,
        prop: u32,
        slot: u32,
        flags: u8,
    ) {
        if let Some(way) = self.ways.iter().position(|w| w.shape_ptr.is_null()) {
            self.ways[way] = InlineCacheEntry {
                shape_ptr,
                shape_uid,
                slot,
                prop,
                flags,
            };
        }
    }

    /// Record a typed array's `length` read (the property inline guard's tail).
    ///
    /// The property lives on the prototype, so the ordinary slot cache cannot
    /// serve it. The entry keeps two tokens instead:
    ///
    /// - the instance's shape (`shape_ptr` + `shape_uid`): an own `length`
    ///   changes the instance's shape, so the entry goes stale and the guard
    ///   declines;
    /// - the id of the shape that *owns* `length` in the prototype chain
    ///   (`owner_uid`): a patched prototype changes that shape, so the entry
    ///   goes stale the same way.
    ///
    /// `slot` is left at `u32::MAX`, which the ordinary guard's range check
    /// always rejects — so an entry of this kind can never be mistaken for a
    /// cached slot.
    pub(crate) fn insert_typed_array_length(
        &mut self,
        shape_ptr: *const crate::object::ShapeImpl,
        shape_uid: u32,
        owner_uid: u32,
    ) {
        let way = match self
            .ways
            .iter()
            .position(|w| w.shape_ptr == shape_ptr && w.shape_uid == shape_uid)
        {
            Some(i) => i,
            None => match self.ways.iter().position(|w| w.shape_ptr.is_null()) {
                Some(i) => i,
                None => {
                    let i = self.cursor as usize % IC_WAYS;
                    self.cursor = self.cursor.wrapping_add(1);
                    i
                }
            },
        };
        self.ways[way] = InlineCacheEntry {
            shape_ptr,
            shape_uid,
            slot: u32::MAX,
            // The atom is known at compile time, so the field is free: it
            // holds the owner's shape id instead.
            prop: owner_uid,
            flags: IC_FLAG_TYPED_ARRAY_LENGTH,
        };
    }
}

#[derive(Debug)]
pub(crate) struct BytecodeDebugInfo {
    pub filename: Atom,
    pub line_num: i32,
    pub source: Option<String>,
    pub pc2line_buf: Option<Vec<u8>>,

    #[cfg(debug_assertions)]
    pub(crate) debug_setting: super::BytecodeDebugSetting,
}

impl GcTrace for BytecodeDebugInfo {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.filename.trace(tr);
    }
}

// for the encoding of the pc2line table
pub const PC2LINE_BASE: i32 = -1;
pub const PC2LINE_RANGE: i32 = 5;
pub const PC2LINE_OP_FIRST: i32 = 1;
pub const PC2LINE_DIFF_PC_MAX: i32 = (255 - PC2LINE_OP_FIRST) / PC2LINE_RANGE;

/// Classifies whether a bytecode function needs a full GC scope during execution.
///
/// Derived at compile time by scanning opcodes for `OpcodeGcAlloc` markers:
/// - `None`: function never allocates LOCAL GC nodes → trampoline skips scope push/pop.
/// - `Light`: function only has `Never` + `Conditional` opcodes → allocations are
///   possible but rare. Non-inlined calls still push a scope; an inlined call may
///   run in the caller's scope when that scope is cleared from the trampoline base
///   at every safepoint (the allocation is then anchored in a traced slot before
///   the next safepoint).
/// - `Full`: function has at least one `Always` allocation opcode → standard scope path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
#[derive(Default)]
pub(crate) enum GcScopeKind {
    /// Has Always-allocation opcodes → standard scope push/pop/promote.
    #[default]
    Full = 0,
    /// Only Never + Conditional → full scope path at runtime (same as Full),
    /// but `Conditional` means cache is almost always empty (future optimisation
    /// target for lazy `Option<SmallVec>`).
    Light = 1,
    /// Only Never opcodes → trampoline skips scope push/pop entirely.
    None = 2,
}

#[derive(Debug, Clone)]
pub(crate) struct BytecodeAttrs {
    pub(crate) func_kind: FunctionKind,
    /// true if a prototype field is necessary
    pub(crate) has_prototype: bool,

    /// 是否有简单参数列表
    pub(crate) has_simple_parameter_list: bool,
    /// 是否是派生类构造函数
    pub(crate) is_derived_class_constructor: bool,
    /// 是否允许new.target
    pub(crate) new_target_allowed: bool,
    /// 是否允许super()
    pub(crate) super_call_allowed: bool,
    /// 是否允许super.或super[]
    pub(crate) super_allowed: bool,
    /// 是否允许arguments标识符
    pub(crate) arguments_allowed: bool,
    /// stop backtrace on this function
    pub(crate) backtrace_barrier: bool,
    /// for length function property
    pub(crate) defined_arg_count: u16,
    //// true if home_object needs to be initialized
    pub(crate) need_home_object: bool,
    /// execution mode
    pub(crate) exec_mode: ExecMode,
    /// GC scope classification: None / Light / Full.
    /// Set by `compute_gc_scope_kind()` during `create_bytecode`.
    pub(crate) gc_scope_kind: GcScopeKind,
}

impl Default for BytecodeAttrs {
    fn default() -> Self {
        Self {
            func_kind: FunctionKind::Normal,
            has_prototype: false,
            has_simple_parameter_list: false,
            is_derived_class_constructor: false,
            new_target_allowed: false,
            super_call_allowed: false,
            super_allowed: false,
            arguments_allowed: false,
            backtrace_barrier: false,
            defined_arg_count: 0,
            need_home_object: false,
            exec_mode: ExecMode::empty(),
            gc_scope_kind: GcScopeKind::Full,
        }
    }
}

pub struct BytecodeImpl {
    pub(crate) realm: ContextPtr,
    pub(crate) func_name: Atom,
    pub(crate) attrs: BytecodeAttrs,

    /// used by JS_GetScriptOrModuleName()
    pub(crate) is_direct_or_indirect_eval: bool,

    pub(crate) stack_size: u16,

    /// Precomputed `var_count + stack_size` for zero-copy arg calls.
    pub(crate) local_buf_size: u16,

    pub(crate) arg_count: u16,
    pub(crate) var_count: u16,
    pub(crate) global_var_ref_count: u16,
    pub(crate) closure_var_count: u16,

    /// binary image.
    /// SAFETY: may contains const, vararge, bytecode etc
    /// which may contain value and atom. must manually free them when dropped
    pub(crate) binary: Box<[u8]>,
    /// consts section (start_index, length)
    pub(crate) consts_section: Option<(u16, u16)>,
    /// start index of vardefs in binary >=0 for valid index
    pub(crate) vardefs_start: i32,
    /// start index of closure vars >=0 for valid index
    pub(crate) closure_var_start: i32,
    /// index where opcodes starts
    pub(crate) opcodes_start: u32,

    /// debug info
    pub(crate) debug: Option<BytecodeDebugInfo>,

    /// Inline-cache entries for GET_FIELD_IC / GET_FIELD2_IC instructions.
    /// Indexed by the ic_index embedded in each IC instruction; each element is
    /// one [`InlineCacheSite`] holding up to [`IC_WAYS`] shapes (the property inline cache).
    /// Empty / zero-initialized when the compiled function has no IC sites.
    ///
    /// Wrapped in UnsafeCell for interior mutability: the interpreter owns the
    /// bytecode via &self but must be able to populate/update cache entries.
    ///
    /// # GC Safety (inline_cache feature)
    ///
    /// Each entry stores a `*const ShapeImpl` raw pointer. This is safe because:
    ///
    /// 1. **Non-moving GC**: gc-lite never compacts — object addresses are
    ///    stable for their entire lifetime.
    ///
    /// 2. **Shape lifetime**: The `ShapeRegistry` holds `GcRef<ShapeImpl>` for
    ///    every shape with `use_count > 0`. GC traces the registry, so all
    ///    resident shapes are kept alive. A shape is only freed after
    ///    `use_count` drops to 0 **and** it is unregistered.
    ///
    /// 3. **No dangling dereference**: The cached `shape_ptr` is only used for
    ///    address comparison against the object's current shape pointer. On
    ///    cache miss (shape changed) the entry is overwritten before any
    ///    dereference. On cache hit, the shape is guaranteed alive because the
    ///    object that just confirmed the match holds a `GcRef` to it.
    ///
    /// 4. **ABA prevention via uid**: Even if a freed shape's address is
    ///    immediately reused by a new shape, the `shape_uid` field (global
    ///    monotonically increasing counter) differs, so the cache misses
    ///    correctly.
    #[cfg(feature = "inline_cache")]
    pub(crate) inline_cache: UnsafeCell<Box<[InlineCacheSite]>>,

    /// Global-variable site caches (T10), one per `global_var_refs` entry. The
    /// inline guard reads the entry's shape/slot; the `get_var`/`put_var`
    /// helpers refresh it. Not traced: the entries hold compare-only shape
    /// pointers, and both global objects are context roots anyway.
    #[cfg(feature = "jit")]
    pub(crate) global_var_cache: UnsafeCell<Box<[crate::jit::GlobalVarCache]>>,

    /// JIT state for this function (feature `jit`): the installed native-entry
    /// slot, which also owns the published code mapping (a strong `Rc`, so the slot owns it).
    /// Interior-mutable because the interpreter reaches it through `&Bytecode`.
    /// Deliberately **not** visited by `GcTrace`: the mapping holds no GC
    /// reference, and tracing it would keep a stale entry alive — the pool of
    /// mappings is bounded instead by this field's `Drop`.
    #[cfg(feature = "jit")]
    pub(crate) jit: crate::jit::FunctionSlot,

    /// Tier-2 type feedback for this function (feature `jit`): the
    /// types the interpreter observed at each feedback site (bytecode pc).
    /// Like `jit` it holds no GC reference and is not traced.
    #[cfg(feature = "jit")]
    pub(crate) feedback: crate::jit::FeedbackSlot,
}

impl GcTrace for BytecodeImpl {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        self.func_name.trace(gcx);
        self.consts().iter().for_each(|x| x.trace(gcx));
        self.var_defs().iter().for_each(|x| x.trace(gcx));
        self.global_var_refs().iter().for_each(|x| x.trace(gcx));
        self.closure_vars().iter().for_each(|x| x.trace(gcx));

        // trace atoms in opcodes
        let bc = self.opcodes();
        let bc_len = bc.len();
        let mut pos = 0;
        while pos < bc_len {
            let op = bc[pos];
            let oi = opcode_info_final(op);
            if matches!(
                oi.fmt,
                OpcodeFormat::ATOM
                    | OpcodeFormat::ATOM_U8
                    | OpcodeFormat::ATOM_U16
                    | OpcodeFormat::ATOM_LABEL_U8
                    | OpcodeFormat::ATOM_LABEL_U16
            ) {
                let id = unsafe { read_from_ptr::<u32>(bc.as_ptr().add(pos + 1)) };
                if id != 0 && !is_const_atom(id) && !is_small_int_atom(id) {
                    Atom::from_id(id).trace(gcx);
                }
            }

            pos += oi.size as usize;
        }

        if let Some(x) = self.debug.as_ref() {
            x.trace(gcx)
        }

        // The call-site feedback holds the recorded callees (the direct-call path): they
        // are GC nodes, and holding them is what keeps a recorded identity
        // unique.
        #[cfg(feature = "jit")]
        self.feedback.trace(gcx);
    }
}

impl BytecodeImpl {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        ctx: &mut ContextImpl,
        fd: &FunctionDef,
        binary: Box<[u8]>,
        consts_section: Option<(u16, u16)>,
        vardefs_start: i32,
        arg_count: u16,
        var_count: u16,
        global_var_ref_count: u16,
        closure_var_start: i32,
        closure_var_count: u16,
        opcodes_start: u32,
        stack_size: u16,
    ) -> Self {
        Self {
            realm: ctx.gc_ref().into(),
            func_name: fd.func_name,

            attrs: BytecodeAttrs {
                need_home_object: fd.attrs.need_home_object || fd.home_object_var_idx.is_some(),
                ..fd.attrs
            },
            is_direct_or_indirect_eval: matches!(
                fd.eval_type,
                EvalType::Direct | EvalType::Indirect
            ),
            stack_size,
            local_buf_size: var_count.saturating_add(stack_size),

            binary,
            consts_section,
            vardefs_start,
            arg_count,
            var_count,
            global_var_ref_count,
            closure_var_start,
            closure_var_count,
            opcodes_start,

            debug: Some(BytecodeDebugInfo {
                filename: Atom::NULL,
                line_num: 0,
                source: None,
                pc2line_buf: None,
                #[cfg(debug_assertions)]
                debug_setting: fd.debug,
            }),

            #[cfg(feature = "inline_cache")]
            inline_cache: UnsafeCell::new(Box::new([])),

            #[cfg(feature = "jit")]
            global_var_cache: UnsafeCell::new(Box::new([])),

            #[cfg(feature = "jit")]
            jit: crate::jit::FunctionSlot::default(),

            #[cfg(feature = "jit")]
            feedback: crate::jit::FeedbackSlot::default(),
        }
    }

    /// get bytecode realm context
    #[inline(always)]
    pub(crate) const fn realm(&self) -> ContextPtr {
        self.realm
    }

    #[inline(always)]
    pub const fn func_kind(&self) -> FunctionKind {
        self.attrs.func_kind
    }

    #[inline(always)]
    pub(crate) const fn mode(&self) -> ExecMode {
        self.attrs.exec_mode
    }

    #[inline(always)]
    pub const fn has_prototype(&self) -> bool {
        self.attrs.has_prototype
    }

    /// GC scope kind: None / Light / Full.
    /// Determines whether the trampoline skips scope push/pop for this function.
    #[inline(always)]
    pub(crate) const fn gc_scope_kind(&self) -> GcScopeKind {
        self.attrs.gc_scope_kind
    }

    /// The inline-cache sites, for the JIT's inline property guard.
    ///
    /// The array never moves (the sites are allocated once, with the
    /// function), so the pointer stays valid for the function's life.
    #[cfg(all(feature = "jit", feature = "inline_cache"))]
    #[inline(always)]
    pub(crate) fn inline_cache_sites(&self) -> *const InlineCacheSite {
        // SAFETY: single-threaded; the sites live as long as the function.
        unsafe { (*self.inline_cache.get()).as_ptr() }
    }

    /// The global-variable site caches, for the JIT's inline guard (T10). Empty
    /// when the function has no global references (or was restored from a
    /// persistent blob, which does not carry the cache).
    #[cfg(feature = "jit")]
    #[inline(always)]
    pub(crate) fn global_var_cache_sites(&self) -> *mut crate::jit::GlobalVarCache {
        // SAFETY: single-threaded; the cache lives as long as the function.
        unsafe { (*self.global_var_cache.get()).as_mut_ptr() }
    }

    #[inline(always)]
    pub const fn need_home_object(&self) -> bool {
        self.attrs.need_home_object
    }

    /// get nth arg or local var. for example:
    /// get 2nd var def: (2, false)
    /// get 3rd arg def: (3, true)
    fn get_local_vardef(&self, nth: u16, is_arg: bool) -> Option<&JSVarDef> {
        if self.vardefs_start >= 0
            && nth
                < if is_arg {
                    self.arg_count
                } else {
                    self.var_count
                }
        {
            unsafe {
                self.binary
                    .as_ptr()
                    .add(self.vardefs_start as usize)
                    .cast::<JSVarDef>()
                    .add(nth as usize + if is_arg { 0 } else { self.arg_count as usize })
                    .as_ref()
            }
        } else {
            None
        }
    }

    /// get nth argument def
    #[inline(always)]
    pub(crate) fn get_arg(&self, nth: u16) -> Option<&JSVarDef> {
        self.get_local_vardef(nth, true)
    }

    /// get nth local var def
    #[inline(always)]
    pub(crate) fn get_var(&self, nth: u16) -> Option<&JSVarDef> {
        self.get_local_vardef(nth, false)
    }

    /// get nth closure var ref
    pub(crate) fn get_closure_var(&self, nth: u16) -> Option<&ClosureVar> {
        if self.closure_var_start >= 0 && nth < self.closure_var_count {
            unsafe {
                self.binary
                    .as_ptr()
                    .add(self.closure_var_start as usize)
                    .cast::<ClosureVar>()
                    .add(nth as usize)
                    .as_ref()
            }
        } else {
            None
        }
    }

    pub fn arguments(&self) -> &[JSVarDef] {
        if self.arg_count > 0 {
            debug_assert!(self.vardefs_start >= 0);
            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.vardefs_start as usize)
                        .cast::<JSVarDef>(),
                    self.arg_count as usize,
                )
            }
        } else {
            &[]
        }
    }

    pub fn local_vars(&self) -> &[JSVarDef] {
        if self.var_count > 0 {
            debug_assert!(self.vardefs_start >= 0);
            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.vardefs_start as usize)
                        .cast::<JSVarDef>()
                        .add(self.arg_count as usize),
                    self.var_count as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// arguments + local_vars.
    fn var_defs(&self) -> &[JSVarDef] {
        if self.vardefs_start >= 0 {
            debug_assert!(self.arg_count > 0 || self.var_count > 0);
            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.vardefs_start as usize)
                        .cast::<JSVarDef>(),
                    (self.arg_count + self.var_count) as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// get global var refs slice (first N entries in the closure-vars binary section)
    pub(crate) fn global_var_refs(&self) -> &[ClosureVar] {
        if self.global_var_ref_count > 0 && self.closure_var_start >= 0 {
            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.closure_var_start as usize)
                        .cast::<ClosureVar>(),
                    self.global_var_ref_count as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// get closure vars slice (after global var refs in the closure-vars binary section)
    pub(crate) fn closure_vars(&self) -> &[ClosureVar] {
        if self.closure_var_count > 0 {
            debug_assert!(self.closure_var_start >= 0);

            unsafe {
                std::slice::from_raw_parts(
                    self.binary
                        .as_ptr()
                        .add(self.closure_var_start as usize)
                        .cast::<ClosureVar>()
                        .add(self.global_var_ref_count as usize),
                    self.closure_var_count as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// get opcodes slice
    #[inline(always)]
    pub(crate) fn opcodes(&self) -> &[u8] {
        &self.binary[self.opcodes_start as usize..]
    }

    /// get const values
    pub(crate) fn consts(&self) -> &[JSValue] {
        if let Some((start, len)) = self.consts_section {
            debug_assert_eq!(start, 0);
            debug_assert!(len > 0);
            unsafe {
                std::slice::from_raw_parts(
                    self.binary.as_ptr().add(start as usize).cast::<JSValue>(),
                    len as usize,
                )
            }
        } else {
            &[]
        }
    }

    /// Get nth const value.
    #[cfg(any(feature = "persistent", feature = "dump_bytecode"))]
    #[inline(always)]
    pub(crate) fn load_const(&self, nth: usize) -> Option<JSValue> {
        if let Some((start, len)) = self.consts_section
            && nth < len as usize
        {
            debug_assert_eq!(start, 0);
            Some(unsafe {
                *self
                    .binary
                    .as_ptr()
                    .add(start as usize)
                    .cast::<JSValue>()
                    .add(nth)
            })
        } else {
            None
        }
    }

    /// Max bytes for a LEB128-encoded u32.
    const LEB128_U32_MAX_BYTES: usize = 5;

    /// Find the source line and column for a given PC value.
    /// Returns `(line_num, line_pos)` or None if the function was stripped.
    pub(crate) fn find_line_num_col(&self, pc_value: u32) -> Option<(i32, u32)> {
        if self.debug.as_ref().is_none_or(|d| d.pc2line_buf.is_none()) {
            // function was stripped
            return None;
        }

        let debug = self.debug.as_ref().unwrap();
        let mut p = debug.pc2line_buf.as_ref().unwrap().as_ptr();
        let p_end = unsafe { p.add(debug.pc2line_buf.as_ref().map(|x| x.len()).unwrap()) };

        let mut pc = 0;
        let mut line_num = debug.line_num;
        let mut line_pos = 0_u32;

        while p < p_end {
            unsafe {
                let mut op = *p as i32;
                p = p.offset(1);

                let new_line_num: i32;

                if op == 0 {
                    let mut val = 0;
                    let ret = if let Ok(n) = get_leb128(
                        &mut val,
                        std::slice::from_raw_parts(p, Self::LEB128_U32_MAX_BYTES),
                    ) {
                        n
                    } else {
                        return Some((debug.line_num, line_pos));
                    };
                    pc += val as i32;
                    p = p.add(ret);

                    let mut v = 0_i32;
                    let ret = if let Ok(n) = get_sleb128(
                        &mut v,
                        std::slice::from_raw_parts(p, Self::LEB128_U32_MAX_BYTES),
                    ) {
                        n
                    } else {
                        return Some((debug.line_num, line_pos));
                    };
                    p = p.add(ret);
                    new_line_num = line_num + v;
                } else {
                    op -= PC2LINE_OP_FIRST;
                    pc += op / PC2LINE_RANGE;
                    new_line_num = line_num + (op % PC2LINE_RANGE) + PC2LINE_BASE;
                }

                // Column delta always follows — both short and long encoding
                let mut vp = 0_i32;
                let ret = if let Ok(n) = get_sleb128(
                    &mut vp,
                    std::slice::from_raw_parts(p, Self::LEB128_U32_MAX_BYTES),
                ) {
                    n
                } else {
                    return Some((debug.line_num, line_pos));
                };
                p = p.add(ret);
                let new_line_pos = (line_pos as i32 + vp) as u32;

                if pc_value < pc as u32 {
                    return Some((line_num, line_pos));
                }
                line_num = new_line_num;
                line_pos = new_line_pos;
            }
        }

        Some((line_num, line_pos))
    }

    pub(crate) fn find_line_num(&self, pc_value: u32) -> Option<i32> {
        self.find_line_num_col(pc_value).map(|(ln, _)| ln)
    }
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq)]
pub struct Bytecode(pub(crate) GcRef<BytecodeImpl>);

gcref_wrapper!(Bytecode, BytecodeImpl);

impl From<Bytecode> for JSValue {
    #[inline(always)]
    fn from(value: Bytecode) -> Self {
        Self(
            crate::JSValueTag::FunctionBytecode.mknode(value.gc_node_ptr()),
            PhantomData,
        )
    }
}

impl std::fmt::Debug for Bytecode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Bytecode {:?}>", self.0)
    }
}

impl Bytecode {
    /// Returns the function's name as a UTF-8 string (empty for anonymous).
    pub fn func_name(&self, rt: &crate::RuntimeImpl) -> String {
        self.func_name.to_utf8(rt)
    }
}

impl Bytecode {
    /// Create Function from `self` Bytecode with captured variables, then calls the function.
    /// Always executes via arena (trampoline) path — `BytecodeFunction::call` no longer
    /// has a legacy fallback. Closure creation still routes based on `ctx.is_arena_mode()`:
    /// arena mode uses `make_closure_for_arena` (resolves var refs from arena frames);
    /// legacy mode uses `make_closure` (resolves from the provided `parent_stack_frame`).
    pub(crate) fn run_as_function(
        &self,
        ctx: &mut ScopedContext,
        parent_frame_idx: FrameIndex,
        this_obj: JSValue,
        captured_vars: &[VarRef],
    ) -> JSResult {
        // Always uses the arena closure creation path. Legacy `make_closure`
        // (which requires a `&mut StackFrame` from the legacy interpreter) is
        // no longer needed — `BytecodeFunction::call` unconditionally enters
        // the trampoline, and closure variable resolution from arena frames
        // is handled by `make_closure_for_arena`.
        //
        // `parent_frame_idx` is the frame the created closure captures from.
        // It is passed explicitly by the caller (e.g. eval passes the caller's
        // frame) rather than read from the global `active_frame_index`, which
        // may be NULL when clobbered by a nested call (such as a `to_string`
        // performed while preparing a direct eval). When there is genuinely no
        // caller frame (top-level entry from the test runner or eval), pass
        // `FrameIndex::NULL` — the callee has no closure variables to resolve
        // from a parent frame.

        let mut rt = ctx.rt_ptr();
        self.make_closure_for_arena(
            ctx,
            None,
            captured_vars,
            rt.vm_stack_mut(),
            parent_frame_idx,
        )?
        .call(ctx, this_obj, &[])
    }
}

#[cfg(all(test, feature = "inline_cache"))]
mod inline_cache_site_tests {
    use super::*;

    /// A fabricated but distinct, non-null shape address. `find`/`insert` only
    /// compare these, never dereference them.
    fn shape_at(seed: usize) -> *const crate::object::ShapeImpl {
        (std::ptr::NonNull::<crate::object::ShapeImpl>::dangling().as_ptr() as usize + seed * 16)
            as *const crate::object::ShapeImpl
    }

    /// Insert with `prop == seed` for the shape-only tests.
    fn insert(site: &mut InlineCacheSite, seed: u32, slot: u32) {
        site.insert(shape_at(seed as usize), seed, seed, slot, 0);
    }

    /// The layout the JIT's inline property guard reads: the first
    /// way's shape pointer, shape id and slot index.
    ///
    /// The guard reads a site's first way to learn in which slot the property
    /// lives — so the offsets and the widths are a contract: reading the
    /// wrong one would make the guard miss (or read the wrong slot).
    #[test]
    fn the_inline_cache_site_layout_is_stable() {
        let mut site = InlineCacheSite::default();
        site.insert(shape_at(1), 7, 9, 3, 0);

        // The shape pointer: 8 bytes, at the site's start.
        assert_eq!(jit_layout::SHAPE_PTR_OFFSET, 0);
        assert_eq!(std::mem::size_of::<*const crate::object::ShapeImpl>(), 8);
        // The shape id and the slot: 4 bytes each, right after it.
        assert_eq!(jit_layout::SHAPE_UID_OFFSET, 8);
        assert_eq!(jit_layout::SLOT_OFFSET, 12);

        let base = &site as *const InlineCacheSite as *const u8;
        let shape_ptr = unsafe {
            *base
                .add(jit_layout::SHAPE_PTR_OFFSET as usize)
                .cast::<*const crate::object::ShapeImpl>()
        };
        let shape_uid = unsafe {
            *base
                .add(jit_layout::SHAPE_UID_OFFSET as usize)
                .cast::<u32>()
        };
        let slot = unsafe { *base.add(jit_layout::SLOT_OFFSET as usize).cast::<u32>() };
        assert_eq!(shape_ptr, shape_at(1));
        assert_eq!(shape_uid, 7);
        assert_eq!(slot, 3);

        // A site is a whole number of entries: the next site starts right
        // after this one.
        assert_eq!(
            jit_layout::SITE_SIZE,
            std::mem::size_of::<InlineCacheSite>() as i64
        );
        const {
            assert!(jit_layout::SITE_SIZE >= 4 * 24);
        }
    }

    #[test]
    fn empty_site_never_hits() {
        let site = InlineCacheSite::default();
        assert_eq!(site.find(shape_at(1), 1, 1), None);
    }

    #[test]
    fn site_holds_every_way() {
        let mut site = InlineCacheSite::default();
        for seed in 0..IC_WAYS as u32 {
            insert(&mut site, seed, seed * 10);
        }
        // Nothing evicted yet: all shapes stay reachable with their own slot.
        for seed in 0..IC_WAYS as u32 {
            assert_eq!(
                site.find(shape_at(seed as usize), seed, seed),
                Some((seed * 10, 0))
            );
        }
        // An unknown shape misses.
        assert_eq!(site.find(shape_at(99), 99, 99), None);
    }

    #[test]
    fn known_shape_updates_in_place() {
        let mut site = InlineCacheSite::default();
        insert(&mut site, 1, 1);
        insert(&mut site, 2, 2);
        // Re-inserting a seen (shape, prop) refreshes its slot instead of
        // consuming a way or advancing the eviction cursor.
        insert(&mut site, 1, 7);
        assert_eq!(site.find(shape_at(1), 1, 1), Some((7, 0)));
        assert_eq!(site.find(shape_at(2), 2, 2), Some((2, 0)));
    }

    /// Array-element sites see a runtime key, so the same shape with a
    /// different property atom must not resolve to the first key's slot.
    #[test]
    fn different_prop_on_the_same_shape_misses() {
        let mut site = InlineCacheSite::default();
        site.insert(shape_at(1), 1, 7, 3, 0);
        assert_eq!(site.find(shape_at(1), 1, 7), Some((3, 0)));
        assert_eq!(site.find(shape_at(1), 1, 8), None);
    }

    #[test]
    fn full_site_evicts_round_robin() {
        let mut site = InlineCacheSite::default();
        for seed in 0..IC_WAYS as u32 {
            insert(&mut site, seed, seed);
        }
        insert(&mut site, 100, 100); // evicts the first-empty fill order: seed 0
        assert_eq!(site.find(shape_at(0), 0, 0), None);
        for seed in 1..IC_WAYS as u32 {
            assert_eq!(
                site.find(shape_at(seed as usize), seed, seed),
                Some((seed, 0))
            );
        }
        assert_eq!(site.find(shape_at(100), 100, 100), Some((100, 0)));

        insert(&mut site, 101, 101); // next victim is the next way
        assert_eq!(site.find(shape_at(1), 1, 1), None);
        assert_eq!(site.find(shape_at(101), 101, 101), Some((101, 0)));
    }

    /// An ABA-shaped stale entry must never resolve: same address, new uid.
    #[test]
    fn uid_mismatch_defeats_address_reuse() {
        let mut site = InlineCacheSite::default();
        site.insert(shape_at(1), 11, 11, 3, 0);
        assert_eq!(site.find(shape_at(1), 22, 11), None);
    }

    /// Array sites fill free ways and then stop, so a dynamic-key loop cannot
    /// churn the cache forever.
    #[test]
    fn fill_only_keeps_the_first_ways() {
        let mut site = InlineCacheSite::default();
        for seed in 0..10u32 {
            site.insert_fill_only(shape_at(seed as usize), seed, seed, seed, 0);
        }
        for seed in 0..IC_WAYS as u32 {
            assert_eq!(
                site.find(shape_at(seed as usize), seed, seed),
                Some((seed, 0))
            );
        }
        assert_eq!(site.find(shape_at(4), 4, 4), None);
    }
}
