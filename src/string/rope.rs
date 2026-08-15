// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! String Rope implementation (ported from QuickJS `JSStringRope`).
//!
//! A rope is an immutable binary tree of strings. Concatenation (`+`,
//! template literals, `Array.prototype.join`, ...) builds a rope node in
//! O(1) without copying characters. The real contiguous string is only
//! materialised (flattened) when it is consumed as a continuous byte buffer,
//! and the result is memoised back into the rope node.
//!
//! Per the design doc, the rope is hidden *behind* [`StringRef`](super::StringRef):
//! `StringRef` stays a single (1-word) handle to a GC node head, and the
//! `flat`-vs-`rope` distinction is carried by the GC node's `dtype`
//! (`StringImpl` vs `RopeImpl`). This keeps `JSValue` size unchanged on both
//! the non-nan-boxing and nan-boxing representations.

use std::{cell::Cell, cmp::Ordering};

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    context::ScopedContext,
    gc::gc::GC_TYPE_REGISTRY,
    string::{JS_STRING_LEN_MAX, StringBuf, StringData, StringImpl, StringRef},
    utils::unlikely,
};

/// Right/left edge short-string absorption threshold.
/// Strings no longer than this are eagerly merged into a neighbouring rope
/// edge instead of being attached as a tiny leaf node.
pub(crate) const ROPE_SHORT_LEN: usize = 512;

/// Left operand direct-copy threshold. If both operands are flat and the
/// left one is no longer than this (and the right no longer than
/// [`ROPE_SHORT_LEN`]) we just do a plain memcpy concatenation.
pub(crate) const ROPE_SHORT2_LEN: usize = 8192;

/// Maximum allowed rope depth. Beyond this the tree is rebalanced into a
/// balanced binary tree to keep access cost at O(log n).
pub(crate) const ROPE_MAX_DEPTH: u8 = 60;

/// Maximum allowed number of leaves before the rope is *consolidated* (fully
/// flattened into a single flat node).
///
/// This is the companion threshold to [`ROPE_MAX_DEPTH`]: `rebalance` only
/// reshapes the tree (depth), it does **not** reduce the number of live GC
/// nodes. A long left-/right-associative build (`s += x` in a loop) keeps every
/// 1-char piece as a reachable leaf, so the live node count — and therefore the
/// cost of each GC scan — grows as O(n). When `leaves` exceeds this bound we
/// flatten the whole rope into one contiguous `StringImpl`, collapsing all rope
/// nodes and tiny leaves into a single GC object. This bounds live nodes at
/// ~O(`ROPE_CONSOLIDATE_LEAVES`) and relieves the O(n²) GC-scan pressure.
///
/// Consolidation is safe (it allocates a *new* flat node and never mutates an
/// existing one, so string immutability is preserved) at the cost of copying
/// the current content. Total copy over a left-assoc build of length `n` is
/// O(n² / `ROPE_CONSOLIDATE_LEAVES`), which is negligible for practical sizes.
pub(crate) const ROPE_CONSOLIDATE_LEAVES: u32 = 512;

/// The rope tree node. Lives as its own GC object (`RopeImpl`), on a par with
/// `StringImpl`. Its `left`/`right` children reuse `StringRef`, which can in
/// turn be flat or another rope.
pub struct RopeImpl {
    /// Runtime handle (used to materialise the flat node when flattening
    /// without an explicit context, e.g. for `content()`).
    pub(crate) rt: crate::runtime::RuntimePtr,
    /// Total code-unit length of the concatenated content.
    pub(crate) len: usize,
    /// True if any subtree contains a UTF-16 code unit.
    pub(crate) is_wide: bool,
    /// Sub-tree depth (flat leaves have depth 0, a new node has
    /// `max(left.depth, right.depth) + 1`).
    pub(crate) depth: u8,
    /// Number of flat leaves in this sub-tree (a flat leaf counts as 1). Used by
    /// the `leaves > ROPE_CONSOLIDATE_LEAVES` consolidation check.
    pub(crate) leaves: u32,
    /// Left sub-string (flat or rope).
    pub(crate) left: StringRef,
    /// Right sub-string (flat or rope).
    pub(crate) right: StringRef,
    /// Memoised flat node. Set on first flatten; later `to_string` returns
    /// this in O(1) and the rope can keep acting as a flat-string proxy.
    pub(crate) flat: Cell<Option<GcRef<StringImpl>>>,
}

impl GcTrace for RopeImpl {
    #[inline(always)]
    fn trace(&self, gcx: &mut GcTraceCtx) {
        // Children are traced via StringRef::trace which adds the node head;
        // GC then dispatches to the correct type-specific trace by dtype.
        self.left.trace(gcx);
        self.right.trace(gcx);
        if let Some(f) = self.flat.get() {
            gcx.add(f);
        }
    }
}

impl RopeImpl {
    /// Build a new rope node from two sub-strings. Returns a (possibly
    /// rebalanced) [`StringRef`]. The node itself is O(1); characters are
    /// only copied when the rope is flattened.
    pub(crate) fn create(
        ctx: &mut ScopedContext,
        left: StringRef,
        right: StringRef,
    ) -> Result<StringRef, i32> {
        let len = left.len() + right.len();
        if unlikely(len > JS_STRING_LEN_MAX) {
            return Err(ctx.throw_internal_error("string too long"));
        }

        let is_wide = left.is_wide() || right.is_wide();
        let depth = left.depth().max(right.depth()) + 1;
        let leaves = left.leaves() + right.leaves();

        let node = RopeImpl {
            rt: ctx.rt_ptr(),
            len,
            is_wide,
            depth,
            leaves,
            left,
            right,
            flat: Cell::new(None),
        };

        let g = match ctx.alloc_local(node) {
            Ok(g) => g,
            Err(_) => {
                return Err(ctx.throw_out_of_memory());
            }
        };

        if depth > ROPE_MAX_DEPTH {
            // Depth ran away (e.g. left-associative `s += x` in a loop):
            // rebuild a balanced binary tree so access cost returns to
            // O(log n).
            return Self::rebalance_build(ctx, &g);
        }

        if leaves > ROPE_CONSOLIDATE_LEAVES {
            // Too many live leaves (each keeps a GC node alive). Collapse the
            // whole rope into one flat node so the live node count drops back
            // to O(1). Safe: allocates a fresh flat, never mutates `g`.
            return StringRef::from_rope(g).flatten(ctx);
        }

        Ok(StringRef::from_rope(g))
    }

    /// Collect the leaves of the rope rooted at `g` in left-to-right order.
    fn collect_leaves(node: &StringRef, out: &mut Vec<StringRef>) {
        if node.is_flat() {
            out.push(*node);
        } else {
            let ri = node.as_rope();
            Self::collect_leaves(&ri.left, out);
            Self::collect_leaves(&ri.right, out);
        }
    }

    /// Recursively build a height-balanced rope from an in-order leaf slice.
    fn build_balanced(ctx: &mut ScopedContext, leaves: &[StringRef]) -> Result<StringRef, i32> {
        if leaves.len() == 1 {
            return Ok(leaves[0]);
        }
        let mid = leaves.len() / 2;
        let left = Self::build_balanced(ctx, &leaves[..mid])?;
        let right = Self::build_balanced(ctx, &leaves[mid..])?;
        Self::create(ctx, left, right)
    }

    /// Walk the leaves of the deep node `g` and rebuild a balanced tree.
    fn rebalance_build(ctx: &mut ScopedContext, g: &GcRef<RopeImpl>) -> Result<StringRef, i32> {
        let mut leaves = Vec::new();
        {
            let ri = unsafe { g.as_ref() };
            Self::collect_leaves(&ri.left, &mut leaves);
            Self::collect_leaves(&ri.right, &mut leaves);
        }
        let balanced = Self::build_balanced(ctx, &leaves)?;
        // Rebalancing only fixes depth, not the leaf count. If the balanced
        // tree still carries more than `ROPE_CONSOLIDATE_LEAVES` leaves, collapse
        // it into a single flat node to keep live nodes bounded.
        if !balanced.is_flat() && balanced.leaves() > ROPE_CONSOLIDATE_LEAVES {
            return balanced.flatten(ctx);
        }
        Ok(balanced)
    }
}

/// In-order iterator over the flat leaves of a (possibly deep) rope.
///
/// Uses an explicit stack instead of call recursion so it is safe even for
/// ropes at the maximum depth.
pub(crate) struct RopeIter {
    stack: Vec<StringRef>,
}

impl RopeIter {
    pub(crate) fn new(root: &StringRef) -> Self {
        let mut stack = Vec::with_capacity(8);
        stack.push(*root);
        Self { stack }
    }

    /// Returns the next flat leaf in left-to-right order, or `None` when exhausted.
    pub(crate) fn next(&mut self) -> Option<StringRef> {
        while let Some(n) = self.stack.pop() {
            if n.is_flat() {
                return Some(n);
            }
            let ri = n.as_rope();
            // push right first so the left child is popped/visited first.
            self.stack.push(ri.right);
            self.stack.push(ri.left);
        }
        None
    }
}

/// Borrow the contiguous content of a flat leaf with a *free* lifetime.
///
/// Unlike `StringRef::content` (whose result is tied to `&self`), this derives
/// the `&StringData` straight from the node head pointer via `NonNull::as_ref`,
/// so the reference is valid for as long as the GC node lives — independent of
/// any temporary `StringRef` value. Needed by `string_ref_cmp`/`string_ref_get`
/// where leaves are owned temporaries.
#[inline(always)]
fn flat_content_ref<'a>(l: &StringRef) -> &'a StringData {
    debug_assert!(l.is_flat());
    let head = l.gc_node_ptr();
    unsafe {
        head.as_ref()
            .payload(&GC_TYPE_REGISTRY)
            .cast::<StringImpl>()
            .as_ref()
    }
    .content()
}

/// Compare two string references without flattening.
///
/// The comparison walks both ropes leaf-by-leaf but compares the underlying
/// **code-unit** stream (matching `StringData::cmp`), advancing independently
/// within each leaf. This yields the same ordering as if both sides were
/// fully concatenated, regardless of how the leaves are split, and avoids
/// allocating a flat buffer.
pub(crate) fn string_ref_cmp(a: &StringRef, b: &StringRef) -> Ordering {
    let mut ita = RopeIter::new(a);
    let mut itb = RopeIter::new(b);
    // Current leaf content and code-unit cursor *within that leaf*.
    let mut ca: Option<&StringData> = None;
    let mut cb: Option<&StringData> = None;
    let mut pa = 0usize;
    let mut pb = 0usize;
    loop {
        if ca.is_none() {
            ca = ita.next().map(|l| flat_content_ref(&l));
            pa = 0;
        }
        if cb.is_none() {
            cb = itb.next().map(|l| flat_content_ref(&l));
            pb = 0;
        }
        match (ca, cb) {
            (None, None) => return Ordering::Equal,
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (Some(mut da), Some(mut db)) => {
                loop {
                    // Advance past the current leaf boundary of *either* side
                    // independently. Using `da.get(pa)`'s `None` as the boundary
                    // signal is wrong here: a single flat leaf may span the whole
                    // string, so `get` would keep returning `Some` while the other
                    // (rope) side's small leaf has already been exhausted.
                    if pa >= da.len() && pb >= db.len() {
                        ca = ita.next().map(|l| flat_content_ref(&l));
                        cb = itb.next().map(|l| flat_content_ref(&l));
                        pa = 0;
                        pb = 0;
                        break;
                    }
                    if pa >= da.len() {
                        ca = ita.next().map(|l| flat_content_ref(&l));
                        pa = 0;
                        match ca {
                            None => return Ordering::Less,
                            Some(d) => da = d,
                        }
                        continue;
                    }
                    if pb >= db.len() {
                        cb = itb.next().map(|l| flat_content_ref(&l));
                        pb = 0;
                        match cb {
                            None => return Ordering::Greater,
                            Some(d) => db = d,
                        }
                        continue;
                    }
                    let x = da.get(pa).unwrap();
                    let y = db.get(pb).unwrap();
                    if x != y {
                        return x.cmp(&y);
                    }
                    pa += 1;
                    pb += 1;
                }
            }
        }
    }
}

/// Random character access over a rope, O(depth).
pub(crate) fn string_ref_get(node: &StringRef, idx: usize) -> Option<u16> {
    if node.is_flat() {
        node.as_flat().content().get(idx)
    } else {
        let ri = node.as_rope();
        let llen = ri.left.len();
        if idx < llen {
            string_ref_get(&ri.left, idx)
        } else {
            string_ref_get(&ri.right, idx - llen)
        }
    }
}

/// Build the flat [`StringData`] for a rope by concatenating all leaves.
/// Used as the fallback when a rope is queried for its contiguous content
/// without an explicit context (e.g. `StringRef::content`).
pub(crate) fn rope_flatten_data(node: &StringRef) -> StringData {
    let mut buf = StringBuf::Empty;
    rec_collect(node, &mut buf);
    StringData::from_buf(buf)
}

fn rec_collect(node: &StringRef, buf: &mut StringBuf) {
    if node.is_flat() {
        // push_content appends the leaf's bytes (promoting width if needed).
        let _ = buf.push_content(node.as_flat().content());
    } else {
        let ri = node.as_rope();
        if let Some(f) = ri.flat.get() {
            let _ = buf.push_content(unsafe { f.as_ref() }.content());
        } else {
            rec_collect(&ri.left, buf);
            rec_collect(&ri.right, buf);
        }
    }
}

/// Fibonacci bucket lengths (QuickJS `rope_bucket_len`), kept for reference
/// and potential future fine-grained rebalancing. Current rebalance uses a
/// simple balanced-tree rebuild which is sufficient and shape-correct.
#[allow(dead_code)]
const ROPE_BUCKET_LEN: [u32; 44] = [
    1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233, 377, 610, 987, 1597, 2584, 4181, 6765, 10946,
    17711, 28657, 46368, 75025, 121393, 196418, 317811, 514229, 832040, 1346269, 2178309, 3524578,
    5702887, 9227465, 14930352, 24157817, 39088169, 63245986, 102334155, 165580141, 267914296,
    433494437, 701408733, 1134903170,
];
