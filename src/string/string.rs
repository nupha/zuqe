// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::TryReserveError,
    ffi::CStr,
    ops::{Deref, Range},
    ptr::NonNull,
};

use gc_lite::{GcHead, GcNode, GcRef, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_STRING,
    atom::{Atom, AtomInfo, AtomKind},
    class::ClassExoticMethods,
    context::ScopedContext,
    error::Exception,
    gc::gc::GC_TYPE_REGISTRY,
    number::{ATOD_ACCEPT_BIN_OCT, IntOrFloat, JS_DTOA_VAR_FORMAT, f64_stringify, js_aton},
    object::{
        JS_PROP_ENUMERABLE, JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC, JSPropertyDescriptor, Object,
        ObjectPayload, check_define_prop_flags,
    },
    runtime::RuntimePtr,
    string::{CharCode, StringCharIter, StringData},
    utils::{hash_u32_from_base, unlikely},
    value::{HINT_STRING, JSValue},
};

use super::StringBuf;
use super::rope::{
    ROPE_SHORT_LEN, ROPE_SHORT2_LEN, RopeImpl, RopeIter, rope_flatten_data, string_ref_cmp,
    string_ref_get,
};

/// Read the gc datatype id from a node head.
///
/// `GcHead::dtype` is `pub(crate)` inside gc-lite (0.6.0) and therefore not
/// reachable from this crate, so we read the `attrs` word directly.
/// `GcHead` is `#[repr(C)]` with `attrs: u32` as its first field and the
/// datatype id stored in bits 8..16 (see gc-lite `GcHead::dtype`). Pinned to
/// gc-lite 0.6.0.
#[inline(always)]
fn head_dtype(head: NonNull<GcHead>) -> u8 {
    let attrs = unsafe { head.cast::<u32>().as_ref() };
    ((*attrs & 0xFF00) >> 8) as u8
}

/// Maximum length allowed for JavaScript strings (1GB - 1)
pub const JS_STRING_LEN_MAX: usize = (1 << 30) - 1;

/// String data + atom info
pub struct StringImpl {
    rt: RuntimePtr,
    /// string content
    content: StringData,
    /// if string is used as atom, this stores the atom info.
    /// None means the string is not an atom.
    atom_info: Option<AtomInfo>,
}

impl GcTrace for StringImpl {
    #[inline(always)]
    fn trace(&self, _: &mut GcTraceCtx) {}
}

impl Drop for StringImpl {
    fn drop(&mut self) {
        if let Some(a) = &self.atom_info
            && !a.is_const()
            && !a.is_small_int()
        {
            self.rt().remove_atom(a);
        }
    }
}

impl Deref for StringImpl {
    type Target = StringData;

    fn deref(&self) -> &Self::Target {
        &self.content
    }
}

impl PartialEq for StringImpl {
    fn eq(&self, other: &Self) -> bool {
        self.content() == other.content()
    }
}

impl PartialOrd for StringImpl {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let a1 = self.atom_info();
        let a2 = other.atom_info();

        let r = match (a1.map(|x| x.kind()), a2.map(|x| x.kind())) {
            // symbol-atom vs non-atom: symbol is greater
            #[cfg(feature = "builtin_symbol")]
            (None, Some(AtomKind::Symbol | AtomKind::GlobalSymbol)) => Ordering::Less,
            #[cfg(feature = "builtin_symbol")]
            (Some(AtomKind::Symbol | AtomKind::GlobalSymbol), None) => Ordering::Greater,

            // symbol-atom vs string-atom: symbol is greater
            #[cfg(feature = "builtin_symbol")]
            (
                Some(AtomKind::String | AtomKind::Private),
                Some(AtomKind::Symbol | AtomKind::GlobalSymbol),
            ) => Ordering::Less,
            #[cfg(feature = "builtin_symbol")]
            (
                Some(AtomKind::Symbol | AtomKind::GlobalSymbol),
                Some(AtomKind::String | AtomKind::Private),
            ) => Ordering::Greater,

            // non-atom vs non-atom
            // string-atom vs string-atom
            // symbol-atom vs symbol-atom
            // string-atom vs non-atom
            _ => self.content.cmp(&other.content),
        };

        Some(r)
    }
}

impl std::fmt::Display for StringImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.content().encode_utf8(false);
        write!(f, "{s}")
    }
}

impl std::fmt::Debug for StringImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.content().encode_utf8(false);
        f.debug_struct("StringImpl")
            .field("content", &s)
            .field("atom_info", &self.atom_info)
            .finish()
    }
}

impl StringImpl {
    /// create StringImpl with string data, and atom info.
    pub(crate) const fn new(
        rt: RuntimePtr,
        content: StringData,
        atom_info: Option<AtomInfo>,
    ) -> Self {
        StringImpl {
            rt,
            content,
            atom_info,
        }
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    const fn rt(&self) -> RuntimePtr {
        self.rt
    }

    /// Returns a reference to the underlying string content
    /// Used for string operations and comparisons
    #[inline(always)]
    pub const fn content(&self) -> &StringData {
        &self.content
    }

    /// count string chars (include broken surrogate code).
    #[inline(always)]
    pub fn chars_count(&self) -> usize {
        self.content.chars_count()
    }

    /// Returns the codepoint length
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.content.len()
    }

    /// Checks if the string is empty (zero length)
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0 && !self.is_ident()
    }

    #[inline(always)]
    pub const fn is_ident(&self) -> bool {
        self.content.is_ident()
    }

    /// Checks if the string uses UTF-16 encoding
    /// Returns true for strings containing non-ASCII characters
    /// Returns false for ASCII-only strings (stored as UTF-8)
    #[inline(always)]
    pub const fn is_wide(&self) -> bool {
        self.content.is_wide()
    }

    /// Returns the atom info associated to this string.
    /// If string is not an Atom, returns None.
    #[inline(always)]
    pub const fn atom_info(&self) -> Option<&AtomInfo> {
        self.atom_info.as_ref()
    }

    pub(crate) fn set_atom_info(&mut self, atom_info: AtomInfo) {
        debug_assert!(self.atom_info.is_none());
        self.atom_info.replace(atom_info);
    }

    /// get raw code at index `nth`
    pub fn get(&self, nth: usize) -> Option<u16> {
        self.content().get(nth)
    }

    /// coerce string to number. if can't convert string to number, returns NAN.
    pub fn to_number(&self, ctx: &mut ScopedContext) -> IntOrFloat {
        let str = self.content().encode_utf8(false);
        let str = str.as_ref().trim();

        if str.is_empty() {
            IntOrFloat::Int64(0)
        } else {
            if let Ok((num, consumed)) = js_aton(ctx, str.as_bytes(), 0, ATOD_ACCEPT_BIN_OCT) {
                if consumed != str.len() {
                    // `str` has remaining characters that are not part of the number:
                    // not a valid number.
                    IntOrFloat::Float64(f64::NAN)
                } else {
                    num
                }
            } else {
                ctx.clear_exception();
                IntOrFloat::Float64(f64::NAN)
            }
        }
    }

    /// convert String value to String object
    pub fn objectify(&self, ctx: &mut ScopedContext, ctor: JSValue) -> Result<Object, i32> {
        let len = self.len();
        let mut o = ctx
            .create_instance_with_ctor(ctor, ObjectPayload::STRING(self.gc_ref().into()))
            .map_err(|_| -1)?;

        o.define_property_value(ctx, Atom::CONST_length, JSValue::from(len as u32), 0)?;
        Ok(o)
    }

    /// treat value `other` as string, and concat self with other.
    pub(crate) fn concat(&self, ctx: &mut ScopedContext, other: JSValue) -> Result<StringRef, i32> {
        let s2 = other.to_string(ctx)?;

        if self.is_empty() {
            return Ok(s2);
        } else if s2.is_empty() {
            return Ok(self.gc_ref().into());
        }

        // concat self, s2
        let buf = self
            .content()
            .to_buf()
            .and_then(|mut sb| sb.push_content(s2.content()).map(|_| sb))
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        StringRef::create(ctx, StringData::from_buf(buf))
    }

    /// make sub string with content of given range.
    /// range is about buffer index, and end index is exclusive.
    pub fn sub_string(
        &self,
        ctx: &mut ScopedContext,
        range: Range<usize>,
    ) -> Result<StringRef, i32> {
        let end = std::cmp::min(range.end, self.len());

        if range.start == 0 && end == self.len() {
            Ok(self.gc_ref().into())
        } else {
            let count = end.saturating_sub(range.start);
            if count > 0 {
                if let Ok(sbuf) = self.content().to_buf_range(range)
                    && let Ok(s) = ctx.create_string_with_buf(sbuf)
                {
                    Ok(s)
                } else {
                    ctx.throw_out_of_memory();
                    Err(-1)
                }
            } else {
                ctx.create_string_with_content(StringData::Empty)
            }
        }
    }
}

/// A 1-word handle to a string GC node. Whether the backing node is a flat
/// `StringImpl` or a lazy `RopeImpl` is decided by the node's `dtype`
/// (`GcHead::dtype()`), never by this Rust type. Because `StringRef` stays a
/// single pointer, `JSValue` size is unchanged on both the non-nan-boxing
/// and nan-boxing representations, and flat strings pay zero extra memory.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct StringRef(NonNull<GcHead>);

impl GcTrace for StringRef {
    #[inline(always)]
    fn trace(&self, gcx: &mut GcTraceCtx) {
        // Add the node head; GC dispatches to the type-specific trace by dtype
        // (StringImpl::trace is a no-op, RopeImpl::trace recurses children).
        gcx.add_node(self.0);
    }
}

impl StringRef {
    // ---- construction / dtype dispatch accessors ----

    #[inline(always)]
    pub(crate) fn from_flat(g: GcRef<StringImpl>) -> Self {
        Self(g.node_ptr())
    }

    #[inline(always)]
    pub(crate) fn from_rope(g: GcRef<RopeImpl>) -> Self {
        Self(g.node_ptr())
    }

    #[inline(always)]
    const fn head(&self) -> &GcHead {
        unsafe { self.0.as_ref() }
    }

    /// True if the backing node is a flat `StringImpl` (not a rope).
    #[inline(always)]
    pub fn is_flat(&self) -> bool {
        // UAF guard: a `StringRef` is a `Copy` raw GC pointer. If it outlived
        // its only root and GC freed the node, any access here would silently
        // corrupt the heap. Panic with the real call stack instead.
        #[cfg(debug_assertions)]
        unsafe {
            self.0.as_ref().debug_assert_node_valid_simple();
        }

        head_dtype(self.0) == StringImpl::GC_TYPE_ID
    }

    #[inline(always)]
    pub(crate) fn as_flat(&self) -> &StringImpl {
        debug_assert!(self.is_flat());
        let payload = self.head().payload(&GC_TYPE_REGISTRY);
        unsafe { payload.cast::<StringImpl>().as_ref() }
    }

    #[allow(clippy::mut_from_ref)]
    #[inline(always)]
    pub(crate) fn as_flat_mut(&self) -> &mut StringImpl {
        debug_assert!(self.is_flat());
        let payload = self.head().payload(&GC_TYPE_REGISTRY);
        unsafe { payload.cast::<StringImpl>().as_mut() }
    }

    #[inline(always)]
    pub(crate) fn as_flat_gc(&self) -> GcRef<StringImpl> {
        debug_assert!(self.is_flat());
        unsafe { GcRef::<StringImpl>::from_ref_unchecked(self.as_flat()) }
    }

    #[inline(always)]
    pub(crate) fn as_rope(&self) -> &RopeImpl {
        debug_assert!(!self.is_flat());
        // UAF guard: `StringRef` is a `Copy` raw GC pointer. If a handle
        // outlived its only root and GC freed the node, any later access here
        // would silently corrupt the heap. Panic with the real call stack
        // instead. (`debug_assert_node_valid_simple` fails once a node's
        // sentinel byte is clobbered to 0xDE on free.)
        #[cfg(debug_assertions)]
        unsafe {
            self.gc_node_ptr().as_ref().debug_assert_node_valid_simple();
        }
        let payload = self.head().payload(&GC_TYPE_REGISTRY);
        unsafe { payload.cast::<RopeImpl>().as_ref() }
    }

    /// GC node head pointer (for `JSValue::gc_node_ptr` / trace roots).
    #[inline(always)]
    pub(crate) const fn gc_node_ptr(&self) -> NonNull<GcHead> {
        self.0
    }

    /// GC node head (borrow), for atom/registry descriptor inspection.
    #[inline(always)]
    pub(crate) const fn gc_head(&self) -> &GcHead {
        self.head()
    }

    /// GC node head pointer, for atom/registry descriptor inspection.
    #[inline(always)]
    pub(crate) const fn gc_head_ptr(&self) -> NonNull<GcHead> {
        self.0
    }

    /// Set the atom descriptor. Ropes must be flattened before atomising, so
    /// this is only ever called on a flat `StringImpl`.
    #[inline(always)]
    pub(crate) fn set_atom_info(&self, info: AtomInfo) {
        debug_assert!(self.is_flat());
        self.as_flat_mut().set_atom_info(info);
    }

    #[inline(always)]
    pub(crate) fn depth(&self) -> u8 {
        if self.is_flat() {
            0
        } else {
            self.as_rope().depth
        }
    }

    /// Number of flat leaves this string spans. A flat string is a single leaf;
    /// a rope sums its children's leaf counts. Used by the consolidation
    /// threshold to bound live GC nodes.
    #[inline(always)]
    pub(crate) fn leaves(&self) -> u32 {
        if self.is_flat() {
            1
        } else {
            self.as_rope().leaves
        }
    }

    // ---- unified read API (replaces the old `Deref` to `StringImpl`) ----

    /// Code-unit length (flat: `content.len()`; rope: stored `len`).
    #[inline(always)]
    pub fn len(&self) -> usize {
        if self.is_flat() {
            self.as_flat().len()
        } else {
            self.as_rope().len
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_wide(&self) -> bool {
        if self.is_flat() {
            self.as_flat().is_wide()
        } else {
            self.as_rope().is_wide
        }
    }

    pub fn is_ident(&self) -> bool {
        if self.is_flat() {
            self.as_flat().is_ident()
        } else {
            false
        }
    }

    pub fn chars_count(&self) -> usize {
        if self.is_flat() {
            self.as_flat().chars_count()
        } else {
            let ri = self.as_rope();
            ri.left.chars_count() + ri.right.chars_count()
        }
    }

    /// Random character access. Flat: `content.get`; rope: O(depth).
    pub fn get(&self, idx: usize) -> Option<u16> {
        if self.is_flat() {
            self.as_flat().get(idx)
        } else {
            string_ref_get(self, idx)
        }
    }

    /// Atom info, if this is an atom-backed flat string. Ropes are never atoms.
    pub fn atom_info(&self) -> Option<&AtomInfo> {
        if self.is_flat() {
            self.as_flat().atom_info()
        } else {
            None
        }
    }

    /// Stable identity for the backing GC node (used as a hash/identity key).
    #[inline(always)]
    pub fn id(&self) -> usize {
        self.0.as_ptr() as usize
    }

    /// Hash. Lazy and shape-independent: a rope folds its in-order leaf hashes
    /// so two ropes of the same content hash equally regardless of tree shape
    /// (required for object/Map key lookup).
    pub fn hash(&self) -> u32 {
        if self.is_flat() {
            let f = self.as_flat();
            f.atom_info().map_or_else(
                || Atom::calc_atom_hash(f.content(), AtomKind::String),
                |a| a.hash(),
            )
        } else {
            // Fold codepoints in left-to-right order, exactly mirroring
            // `Atom::calc_atom_hash` over the concatenated content, so a rope
            // hashes identically to an equal-content flat string (required
            // for object/Map key lookup). Folding per-leaf codepoints (not
            // per-leaf hashes) makes the result independent of leaf shape.
            let mut h = AtomKind::String as u32;
            let mut it = RopeIter::new(self);
            while let Some(leaf) = it.next() {
                for (_, c) in leaf.as_flat().content().chars() {
                    h = hash_u32_from_base(h, c.to_u32());
                }
            }
            h
        }
    }

    /// Return the contiguous content. For a flat string this is direct; for a
    /// rope it materialises (and memoises) the flat node. Ropes should
    /// normally be flattened via [`flatten`](Self::flatten) before being
    /// consumed as a byte buffer; this fallback keeps all legacy `.content()`
    /// call sites correct even if they receive a rope.
    pub fn content(&self) -> &StringData {
        if self.is_flat() {
            self.as_flat().content()
        } else {
            let ri = self.as_rope();
            // Materialise the flat node on first use and memoise it back into
            // the rope. The memoised node lives in the GC heap independently of
            // `self`, so the returned `&StringData` stays valid.
            if ri.flat.get().is_none() {
                let data = rope_flatten_data(self);
                let mut rt = ri.rt;
                let node = StringImpl::new(rt, data, None);
                let flat_g = match rt.gc_alloc_root(node) {
                    Ok(g) => g,
                    Err(_) => panic!("out of memory while flattening rope"),
                };
                // `gc_alloc_root` may have run GC and freed `self` (the node
                // `ri` borrows) if it was unrooted. Re-validate before writing
                // back into `ri.flat`.
                #[cfg(debug_assertions)]
                unsafe {
                    GcRef::<RopeImpl>::from_ref_unchecked(ri).node_info();
                }
                ri.flat.set(Some(flat_g));
            }
            let f = ri.flat.get().unwrap();
            let head = f.node_ptr();
            unsafe {
                head.as_ref()
                    .payload(&GC_TYPE_REGISTRY)
                    .cast::<StringImpl>()
                    .as_ref()
            }
            .content()
        }
    }

    // The following methods delegate to `content()`; they work for both flat
    // and rope strings (a rope is materialised on first use).
    pub fn chars(&self) -> StringCharIter<'_> {
        self.content().chars()
    }
    pub fn chars_since(&self, index: usize) -> StringCharIter<'_> {
        self.content().chars_since(index)
    }
    pub fn chars_range(&self, range: Range<usize>) -> StringCharIter<'_> {
        self.content().chars_range(range)
    }
    pub fn chars_lossy(&self) -> impl Iterator<Item = char> + '_ {
        self.content().chars_lossy()
    }
    pub fn chars_range_lossy(&self, range: Range<usize>) -> impl Iterator<Item = char> + '_ {
        self.content().chars_range_lossy(range)
    }
    pub fn encode_utf8(&self, escape: bool) -> Cow<'_, str> {
        self.content().encode_utf8(escape)
    }
    pub fn encode_utf8_bytes(&self, allow_invalid: bool) -> Cow<'_, [u8]> {
        self.content().encode_utf8_bytes(allow_invalid)
    }
    pub fn encode_utf8_range(&self, range: Range<usize>, escape: bool) -> Cow<'_, str> {
        self.content().encode_utf8_range(range, escape)
    }
    pub fn index_of_code(&self, code: u16, start: usize) -> Option<usize> {
        self.content().index_of_code(code, start)
    }
    pub fn index_of(&self, needle: &StringData, start: usize) -> Option<usize> {
        self.content().index_of(needle, start)
    }
    pub fn index_of_char<C: Into<CharCode>>(&self, ch: C, start: usize) -> Option<usize> {
        self.content().index_of_char(ch, start)
    }
    pub fn index_of_next(&self, pos: usize, must_char: bool) -> Option<usize> {
        self.content().index_of_next(pos, must_char)
    }
    pub fn index_of_next_i64(&self, pos: i64, must_char: bool) -> Option<i64> {
        self.content().index_of_next_i64(pos, must_char)
    }
    pub fn eq_str(&self, other: &str) -> bool {
        self.content().eq_str(other)
    }
    pub fn eq_buf(&self, other: &StringBuf) -> bool {
        self.content().eq_buf(other)
    }
    /// If this string is exactly one ASCII code unit, return it (else `None`).
    pub fn ascii(&self) -> Option<&[u8]> {
        self.content().ascii()
    }
    pub fn to_buf(&self) -> Result<StringBuf, TryReserveError> {
        self.content().to_buf()
    }
    pub fn to_buf_range(&self, range: Range<usize>) -> Result<StringBuf, TryReserveError> {
        self.content().to_buf_range(range)
    }
    pub fn parse_u32(&self) -> Option<u32> {
        self.content().parse_u32()
    }
    pub fn parse_i64(&self) -> Option<i64> {
        self.content().parse_i64()
    }

    pub fn to_number(&self, ctx: &mut ScopedContext) -> IntOrFloat {
        let flat = self
            .flatten(ctx)
            .unwrap_or_else(|_| panic!("out of memory while flattening rope"));
        flat.as_flat().to_number(ctx)
    }

    pub fn objectify(&self, ctx: &mut ScopedContext, ctor: JSValue) -> Result<Object, i32> {
        let flat = self.flatten(ctx)?;
        flat.as_flat().objectify(ctx, ctor)
    }

    pub fn sub_string(
        &self,
        ctx: &mut ScopedContext,
        range: Range<usize>,
    ) -> Result<StringRef, i32> {
        let flat = self.flatten(ctx)?;
        flat.as_flat().sub_string(ctx, range)
    }

    /// Convert this string to an atom. Flattens first so the atom descriptor
    /// is always a flat `StringImpl` (atoms must be flat, per design doc §7C).
    pub fn to_atom(&self, ctx: &mut ScopedContext, kind: AtomKind) -> Result<Atom, i32> {
        let flat = self.flatten(ctx)?;
        flat.as_flat().to_atom(ctx, kind)
    }

    /// Return a flat `StringRef`. For a flat node, returns itself. For a rope,
    /// linearises (with memoisation) and returns the flat node.
    pub(super) fn flatten(&self, ctx: &mut ScopedContext) -> Result<StringRef, i32> {
        if self.is_flat() {
            return Ok(*self);
        }

        let ri = self.as_rope();
        if let Some(f) = ri.flat.get() {
            return Ok(StringRef::from_flat(f));
        }

        let mut buf = StringBuf::Empty;
        self.collect_into(&mut buf).map_err(|_| -1)?;
        let flat = ctx.create_string_with_buf(buf)?;
        let s = flat.as_flat_gc();
        // `create_string_with_buf` may have run GC and freed `self` (the node
        // `ri` borrows) if it was unrooted. Re-validate before writing back.
        #[cfg(debug_assertions)]
        unsafe {
            GcRef::<RopeImpl>::from_ref_unchecked(ri).node_info();
        }
        ri.flat.set(Some(s));

        Ok(flat)
    }

    fn collect_into(&self, buf: &mut StringBuf) -> Result<(), i32> {
        if self.is_flat() {
            buf.push_content(self.as_flat().content()).map_err(|_| -1)
        } else {
            let ri = self.as_rope();
            if let Some(f) = ri.flat.get() {
                buf.push_content(StringRef::from_flat(f).content())
                    .map_err(|_| -1)
            } else {
                ri.left.collect_into(buf)?;
                ri.right.collect_into(buf)
            }
        }
    }

    /// Lazy concatenation. Keep the rope if possible (O(1)); only do a real
    /// copy for short flat strings (see `ROPE_SHORT_LEN`/`ROPE_SHORT2_LEN`).
    pub(crate) fn concat(&self, ctx: &mut ScopedContext, other: JSValue) -> Result<StringRef, i32> {
        let s2 = other.to_string(ctx)?;
        StringRef::concat2(ctx, *self, s2)
    }

    pub(crate) fn concat2(
        ctx: &mut ScopedContext,
        s1: StringRef,
        s2: StringRef,
    ) -> Result<StringRef, i32> {
        if s1.is_empty() {
            return Ok(s2);
        }
        if s2.is_empty() {
            return Ok(s1);
        }
        if s1.is_flat() && s2.is_flat() && s1.len() <= ROPE_SHORT2_LEN && s2.len() <= ROPE_SHORT_LEN
        {
            return s1.as_flat().concat(ctx, JSValue::from(s2));
        }
        RopeImpl::new(ctx, s1, s2)
    }

    /// Compare two string references (rope-aware, no flatten unless needed by
    /// the leaf comparison).
    #[inline(always)]
    pub fn cmp(&self, other: &Self) -> Ordering {
        string_ref_cmp(self, other)
    }

    /// create String with string data, and optional atom info (root gc node).
    pub(crate) fn create_root(
        mut rt: RuntimePtr,
        content: StringData,
        atom_info: Option<AtomInfo>,
    ) -> Result<Self, i32> {
        let s = StringImpl::new(rt, content, atom_info);
        rt.gc_alloc_root(s).map(StringRef::from_flat).map_err(|_| {
            eprintln!("[O.o] alloc error");
            -1
        })
    }

    /// create String with string data, and optional atom info (in gc scope).
    pub(crate) fn create_internal(
        ctx: &mut ScopedContext,
        content: StringData,
        atom_info: Option<AtomInfo>,
    ) -> Result<Self, i32> {
        ctx.alloc_local(StringImpl::new(ctx.rt_ptr(), content, atom_info))
            .map(StringRef::from_flat)
            .map_err(|_| {
                ctx.rt_mut().set_exception(Exception::OutOfMemory);
                eprintln!("[O.o] alloc error");
                -1
            })
    }

    #[inline(always)]
    pub(crate) fn create(ctx: &mut ScopedContext, content: StringData) -> Result<Self, i32> {
        Self::create_internal(ctx, content, None)
    }

    /// Create StringRef from &str
    pub(crate) fn create_str(ctx: &mut ScopedContext, s: &str) -> Result<Self, i32> {
        Self::create_internal(ctx, StringData::from_buf(StringBuf::from_str(s)?), None)
    }

    /// Create StringRef from utf8 encoded bytes
    pub(crate) fn create_utf8(ctx: &mut ScopedContext, utf8_bytes: Vec<u8>) -> Result<Self, i32> {
        Self::create(
            ctx,
            StringData::from_buf(StringBuf::from_utf8(utf8_bytes).map_err(|_| -1)?),
        )
    }

    /// static str `s` must be ascii str.
    #[inline(always)]
    pub(crate) fn create_static(
        ctx: &mut ScopedContext,
        s: &'static str,
        atom_info: Option<AtomInfo>,
    ) -> Result<Self, i32> {
        Self::create_internal(ctx, StringData::from_static(s), atom_info)
    }
}

impl PartialEq for StringRef {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        // Quick length short-circuit (same as QuickJS string comparison).
        self.len() == other.len() && self.cmp(other) == Ordering::Equal
    }
}

impl PartialOrd for StringRef {
    #[inline(always)]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for StringRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_flat() {
            write!(f, "{}", self.as_flat())
        } else {
            let mut it = RopeIter::new(self);
            while let Some(leaf) = it.next() {
                write!(f, "{}", leaf.as_flat())?;
            }
            Ok(())
        }
    }
}

impl std::fmt::Debug for StringRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_flat() {
            write!(f, "{:?}", self.as_flat())
        } else {
            write!(f, "Rope(")?;
            let mut it = RopeIter::new(self);
            while let Some(leaf) = it.next() {
                write!(f, "{:?}", leaf.as_flat())?;
            }
            write!(f, ")")
        }
    }
}

impl From<GcRef<StringImpl>> for StringRef {
    fn from(r: GcRef<StringImpl>) -> Self {
        Self::from_flat(r)
    }
}

impl From<GcRef<StringImpl>> for JSValue {
    #[inline(always)]
    fn from(value: GcRef<StringImpl>) -> Self {
        Self::String(StringRef::from_flat(value))
    }
}

impl From<StringRef> for JSValue {
    #[inline(always)]
    fn from(value: StringRef) -> Self {
        Self::String(value)
    }
}

impl TryFrom<Object> for StringRef {
    type Error = i32;

    fn try_from(obj: Object) -> Result<Self, Self::Error> {
        Self::try_from(JSValue::from(obj))
    }
}

impl TryFrom<JSValue> for StringRef {
    type Error = i32;

    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        value.as_string().copied().ok_or(-1)
    }
}

impl<'c> ScopedContext<'c> {
    /// create string value from string buf.
    pub(crate) fn create_string_with_buf(&mut self, sbuf: StringBuf) -> Result<StringRef, i32> {
        if sbuf.is_empty() {
            Atom::CONST_empty_string.force_string(self)
        } else {
            let data = StringData::from_buf(sbuf);
            StringRef::create(self, data).map_err(|_| {
                self.throw_out_of_memory();
                -1
            })
        }
    }

    /// create string value from string content.
    pub(crate) fn create_string_with_content(
        &mut self,
        content: StringData,
    ) -> Result<StringRef, i32> {
        if content.is_empty() {
            Atom::CONST_empty_string.force_string(self)
        } else {
            StringRef::create(self, content).map_err(|_| {
                self.throw_out_of_memory();
                -1
            })
        }
    }

    /// create string from utf8 encoded data.
    pub fn create_string_from_utf8(&mut self, utf8_bytes: Vec<u8>) -> Result<StringRef, i32> {
        if utf8_bytes.is_empty() {
            Atom::CONST_empty_string.force_string(self)
        } else if utf8_bytes.len() > JS_STRING_LEN_MAX {
            self.throw_internal_error("string too long");
            Err(-1)
        } else {
            StringRef::create_utf8(self, utf8_bytes).inspect_err(|_| {
                self.throw_out_of_memory();
            })
        }
    }

    /// create string from str
    pub fn create_string_from_str(&mut self, str: &str) -> Result<StringRef, i32> {
        if unlikely(str.is_empty()) {
            Atom::CONST_empty_string.force_string(self)
        } else if unlikely(str.len() > JS_STRING_LEN_MAX) {
            self.throw_internal_error("string too long");
            Err(-1)
        } else {
            StringRef::create_str(self, str).inspect_err(|_| {
                self.throw_out_of_memory();
            })
        }
    }

    /// create string from static str
    pub fn create_string_from_static(&mut self, str: &'static str) -> Result<StringRef, i32> {
        if unlikely(str.is_empty()) {
            Atom::CONST_empty_string.force_string(self)
        } else {
            StringRef::create_static(self, str, None).inspect_err(|_| {
                self.throw_out_of_memory();
            })
        }
    }

    /// create string value from a single string char.
    pub fn create_string_from_char(&mut self, ch: CharCode) -> Result<StringRef, i32> {
        let sbuf = match ch.encode_utf16(&mut [0; 2]) {
            &[b @ (0..0x80), _] => StringBuf::Ascii(vec![b as u8]),
            bb => StringBuf::Utf16(bb.to_vec()),
        };
        self.create_string_with_buf(sbuf)
    }
}

pub(crate) fn JS_ConcatString3(
    ctx: &mut ScopedContext,
    str1: &CStr,
    str2: JSValue,
    str3: &CStr,
) -> Result<StringRef, i32> {
    let p = if let JSValue::String(s) = str2 {
        s
    } else {
        str2.to_string(ctx)?
    };

    let len1 = str1.count_bytes();
    let len3 = str3.count_bytes();

    let res_string = StringBuf::with_capacity(len1 + p.len() + len3, false)
        .and_then(|mut s| {
            // str1 + str2 + str3
            s.push_str(str1.to_string_lossy().as_ref())
                .and_then(|_| s.push_content(p.content()))
                .and_then(|_| s.push_str(str3.to_string_lossy().as_ref()))
                .map(|_| s)
        })
        .map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

    ctx.create_string_with_buf(res_string)
}

impl JSValue {
    /// get a reference to value's string description. won't change value's ref_count.
    /// will panic if value is not string type.
    #[inline(always)]
    pub unsafe fn string_unchecked(&self) -> &StringRef {
        if let Self::String(s) = self {
            s
        } else {
            #[cfg(debug_assertions)]
            unreachable!();
            #[cfg(not(debug_assertions))]
            std::hint::unreachable_unchecked();
        }
    }

    /// Lazy string concatenation that keeps ropes lazy. `to_string` preserves
    /// any rope in `a`/`b`, and `concat2` builds a new rope in O(1). Used by the
    /// `+` operator and template literals.
    pub fn concat_value(ctx: &mut ScopedContext, a: JSValue, b: JSValue) -> Result<JSValue, i32> {
        let s1 = a.to_string(ctx)?;
        let s2 = b.to_string(ctx)?;
        StringRef::concat2(ctx, s1, s2).map(JSValue::from)
    }

    /// An alias for [`JSValue::to_string_or_symbol`](Self::to_string_or_symbol).
    #[inline(always)]
    pub fn to_propkey(&self, ctx: &mut ScopedContext) -> Result<JSValue, i32> {
        self.to_string_or_symbol(ctx)
    }

    /// Coerce value `self` to string. if `self` is symbol, returns Err.
    ///
    /// If `self` is already a string, it is returned `as-is` (a rope is
    /// preserved; materialisation to a flat buffer is an internal decision made
    /// lazily by the `StringRef` operations that need it, not here).
    pub fn to_string(&self, ctx: &mut ScopedContext) -> Result<StringRef, i32> {
        match self {
            Self::String(s) => {
                let s = *s;

                // NOTE on GC safety: a value returned by `to_string` must stay reachable
                // until the caller stores it in a traced location. Fresh string
                // allocations are young GC roots, but an *existing* rope node is only alive
                // while some rooted JSValue still references it. If that reference drops
                // while the caller keeps the returned `StringRef` (a `Copy` raw GC pointer)
                // across a GC safepoint, the node can be freed and any later use of it is a
                // use-after-free. We therefore mark the node LOCAL for the current scope so
                // it survives until the scope ends. We use `gc_protect_transient_force`
                // rather than the feature-gated `gc_protect_transient` because the
                // protection must apply even when that feature is disabled (the engine
                // otherwise relies solely on "fresh allocation = root", which this path
                // intentionally avoids to keep ropes lazy).
                //
                // Only ropes need this: flat strings already preserved the
                // "returns a rooted node" invariant before (the old `flatten`
                // returned the flat node as-is too), whereas ropes are what this
                // `to_string` change newly exposes to the unrooted-old-node risk.
                if !s.is_flat() {
                    ctx.gc_protect_transient_force(&JSValue::String(s));
                }

                Ok(s)
            }
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(_) => {
                ctx.throw_type_error("cannot convert symbol to string");
                Err(-1)
            }
            _ => match self.to_string_or_symbol(ctx)? {
                JSValue::String(s) => Ok(s),
                #[cfg(feature = "builtin_symbol")]
                JSValue::Symbol(_) => {
                    ctx.throw_type_error("cannot convert symbol to string");
                    Err(-1)
                }
                _ => unreachable!(),
            },
        }
    }

    /// Coerce value `val` to string, but null or undefined are forbidden.
    pub fn to_string_non_undefined_or_null(
        &self,
        ctx: &mut ScopedContext,
    ) -> Result<StringRef, i32> {
        if unlikely(self.is_undefined_or_null()) {
            ctx.throw_type_error("null or undefined are forbidden");
            Err(-1)
        } else {
            self.to_string(ctx)
        }
    }

    /// Invoking `toLocaleString()` method.
    pub fn to_locale_string(&self, ctx: &mut ScopedContext) -> Result<JSValue, i32> {
        if self.is_undefined_or_null() {
            self.to_string(ctx).map(JSValue::from)
        } else {
            self.invoke(ctx, Atom::CONST_toLocaleString, &[])
        }
    }
}

//
// String class exotic methods
//

fn string_define_own_property(
    ctx: &mut ScopedContext,
    mut this_obj: JSValue,
    prop: Atom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32> {
    macro_rules! fail {
        () => {
            return if ctx.rt().can_throw(flags as _) {
                ctx.throw_type_error("property is not configurable");
                Err(-1)
            } else {
                Ok(false)
            };
        };
    }

    macro_rules! def {
        () => {
            // use clone to make `this_obj` &Value -> &mut JSValue
            return this_obj.define_property(
                ctx,
                prop,
                val,
                getter,
                setter,
                flags as u32 | JS_PROP_NO_EXOTIC,
            );
        };
    }

    if let Some(idx) = prop.to_small_int() {
        let ob = unsafe { this_obj.as_object_unchecked() };
        if ob.class_id() == JS_CLASS_STRING {
            let s = ob.payload().string().unwrap();
            let len = s.len();
            let nth_char = s.content().chars_since(idx as usize).next().map(|(_, c)| c);

            if idx >= len as u32 {
                def!();
            }
            if !check_define_prop_flags(JS_PROP_ENUMERABLE, flags as _) {
                fail!();
            }

            // check that the same value is configured
            if flags & JS_PROP_HAS_VALUE as i32 != 0 {
                if !val.is_string() {
                    fail!();
                }

                let p2 = unsafe { val.string_unchecked() };
                let len2 = p2.len();
                let nth_char2 = p2.content().chars().next().map(|(_, c)| c);

                if len2 != 1 {
                    fail!();
                }
                if nth_char != nth_char2 {
                    fail!();
                }
            }
            Ok(true)
        } else {
            def!();
        }
    } else {
        def!();
    }
}

pub(crate) const js_string_exotic_methods: ClassExoticMethods = ClassExoticMethods {
    get_own_property: Some(
        |ctx: &mut ScopedContext,
         obj: JSValue,
         prop: Atom|
         -> Result<Option<JSPropertyDescriptor>, i32> {
            // This is a class exotic method for JS_CLASS_STRING object
            if let Some(idx) = prop.to_small_int()
                && let Some(s) = obj.as_string()
                && idx < s.len() as u32
            {
                let code = s.content().get(idx as usize).unwrap();
                return Ok(Some(JSPropertyDescriptor {
                    flags: JS_PROP_ENUMERABLE as _,
                    value: ctx.create_string_from_char(code.into())?.into(),
                    getter: JSValue::UNDEFINED,
                    setter: JSValue::UNDEFINED,
                }));
            }

            Ok(None)
        },
    ),

    define_own_property: Some(string_define_own_property),

    delete_property: Some(|_, obj: JSValue, prop: Atom| -> Result<bool, i32> {
        if let Some(idx) = prop.to_small_int() {
            let buflen = StringRef::try_from(obj).unwrap().len();
            if (idx as usize) < buflen {
                return Ok(false);
            }
        }

        Ok(true)
    }),

    get_own_property_names: None,
    has_property: None,
    get_property: None,
    set_property: None,
    get_prototype: None,
    set_prototype: None,
};
