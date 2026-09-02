// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Arbitrary-precision integer (BigInt) value type.
//!
//! Representation is **sign-magnitude** (not two's complement), matching the
//! engine's existing semantic-style number handling and avoiding complement
//! headaches in shifts / bitwise ops. `sign == true` means negative; zero is
//! always `+0` (`sign == false`, empty limb array).
//!
//! The value lives in a GC node so that `JSValue` can address it through the
//! pointer tag `JSValueTag::BigInt` (`-5`), mirroring the `StringImpl` /
//! `StringRef` pattern. There are no nested GC pointers inside `BigIntImpl`,
//! so its `GcTrace` is empty; the heap buffers (`Box<[u64]>`) need no tracing.

mod convert;
mod ops;

pub(crate) use convert::{
    f64_integer_to_limbs, from_f64, from_i64, from_u64, parse_str, string_to_bigint,
    string_to_bigint_decimal, to_bigint64, to_biguint64, to_f64, to_i64_exact, to_string_radix,
};
pub(crate) use ops::{MAX_BIGINT_BITS, divmod_mag, shl_mag};

use std::ptr::NonNull;

use gc_lite::{GcHead, GcRef, GcTrace, GcTraceCtx};

use crate::{context::ScopedContext, error::Exception, gc::gc::GC_TYPE_REGISTRY};

/// sign-magnitude arbitrary-precision integer.
pub struct BigIntImpl {
    /// true = negative; zero is always positive (`sign=false`, `limbs` empty).
    pub(crate) sign: bool,
    /// little-endian limbs; the highest limb is non-zero whenever `len >= 1`.
    pub(crate) limbs: Box<[u64]>,
}

impl GcTrace for BigIntImpl {
    #[inline(always)]
    fn trace(&self, _: &mut GcTraceCtx) {}
}

impl BigIntImpl {
    #[inline(always)]
    pub const fn sign(&self) -> bool {
        self.sign
    }

    #[inline(always)]
    pub fn limbs(&self) -> &[u64] {
        &self.limbs
    }

    /// `true` if the value is zero (empty limb array, `sign` cleared).
    #[inline(always)]
    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    /// Number of significant bits (0 for zero).
    pub fn bit_length(&self) -> usize {
        if self.limbs.is_empty() {
            return 0;
        }
        let top = self.limbs[self.limbs.len() - 1];
        (self.limbs.len() - 1) * 64 + (64 - top.leading_zeros() as usize)
    }

    /// Build the canonical form: strip leading zero limbs, and clear `sign`
    /// when the magnitude is zero.
    #[inline(always)]
    pub(crate) fn from_parts(sign: bool, limbs: Vec<u64>) -> Self {
        let limbs = normalize_limbs(limbs);
        if limbs.is_empty() {
            BigIntImpl { sign: false, limbs }
        } else {
            BigIntImpl { sign, limbs }
        }
    }

    /// Construct a zero value (no allocation).
    #[inline(always)]
    pub fn zero() -> Self {
        BigIntImpl {
            sign: false,
            limbs: Box::new([]),
        }
    }

    // /// Build a non-negative value from a `u64` magnitude (no allocation).
    // #[inline(always)]
    // pub(crate) fn from_u64(v: u64) -> Self {
    //     BigIntImpl::from_parts(false, vec![v])
    // }

    /// Build a value from an `i64` (no allocation).
    #[inline(always)]
    pub(crate) fn from_i64(v: i64) -> Self {
        if v >= 0 {
            BigIntImpl::from_parts(false, vec![v as u64])
        } else if v == i64::MIN {
            BigIntImpl::from_parts(true, vec![1u64 << 63])
        } else {
            BigIntImpl::from_parts(true, vec![(-v) as u64])
        }
    }
}

/// Strip trailing zero limbs so the highest limb is non-zero; empty == zero.
pub(crate) fn normalize_limbs(mut limbs: Vec<u64>) -> Box<[u64]> {
    while limbs.last() == Some(&0) {
        limbs.pop();
    }
    limbs.into_boxed_slice()
}

/// A 1-word handle to a `BigIntImpl` GC node.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub struct BigIntRef(NonNull<GcHead>);

impl GcTrace for BigIntRef {
    #[inline(always)]
    fn trace(&self, gcx: &mut GcTraceCtx) {
        // Add the node head; GC dispatches to `BigIntImpl::trace` (a no-op).
        gcx.add_node(self.0);
    }
}

impl PartialEq for BigIntRef {
    #[inline(always)]
    fn eq(&self, other: &Self) -> bool {
        let a = self.as_impl();
        let b = other.as_impl();
        a.sign == b.sign && a.limbs == b.limbs
    }
}

impl std::fmt::Debug for BigIntRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let i = self.as_impl();
        f.debug_struct("BigIntRef")
            .field("sign", &i.sign)
            .field("limbs", &i.limbs)
            .finish()
    }
}

impl BigIntRef {
    #[inline(always)]
    pub(crate) fn from_gc(g: GcRef<BigIntImpl>) -> Self {
        Self(g.node_ptr())
    }

    #[inline(always)]
    const fn head(&self) -> &GcHead {
        unsafe { self.0.as_ref() }
    }

    /// Borrow the `BigIntImpl` payload.
    #[inline(always)]
    pub fn as_impl(&self) -> &BigIntImpl {
        let payload = self.head().payload(&GC_TYPE_REGISTRY);
        unsafe { payload.cast::<BigIntImpl>().as_ref() }
    }

    /// GC node head pointer (alias used by `JSValue::make_bigint`).
    #[inline(always)]
    pub(crate) const fn gc_head_ptr(&self) -> NonNull<GcHead> {
        self.0
    }

    #[inline(always)]
    pub fn sign(&self) -> bool {
        self.as_impl().sign()
    }

    #[inline(always)]
    pub fn limbs(&self) -> &[u64] {
        self.as_impl().limbs()
    }

    #[inline(always)]
    pub fn is_zero(&self) -> bool {
        self.as_impl().is_zero()
    }

    /// Create a BigInt GC node in the current gc scope.
    ///
    /// `limbs` are normalized (leading zeros removed) and `sign` is cleared
    /// for zero, so `create` always produces a canonical node.
    pub(crate) fn create(
        ctx: &mut ScopedContext,
        sign: bool,
        limbs: Vec<u64>,
    ) -> Result<Self, i32> {
        ctx.alloc_local(BigIntImpl::from_parts(sign, limbs))
            .map(Self::from_gc)
            .map_err(|_| {
                ctx.rt_mut().set_exception(Exception::OutOfMemory);
                eprintln!("[O.o] alloc error");
                -1
            })
    }

    /// Create a zero BigInt node in the current gc scope.
    pub(crate) fn zero(ctx: &mut ScopedContext) -> Result<Self, i32> {
        Self::create(ctx, false, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impl_of(sign: bool, limbs: Vec<u64>) -> BigIntImpl {
        BigIntImpl::from_parts(sign, limbs)
    }

    #[test]
    fn normalize_removes_leading_zero_limbs() {
        assert_eq!(&*normalize_limbs(vec![]), &[] as &[u64]);
        assert_eq!(&*normalize_limbs(vec![0]), &[] as &[u64]);
        assert_eq!(&*normalize_limbs(vec![0, 0, 0]), &[] as &[u64]);
        assert_eq!(&*normalize_limbs(vec![1]), &[1]);
        assert_eq!(&*normalize_limbs(vec![1, 0, 0]), &[1]);
        assert_eq!(&*normalize_limbs(vec![1, 2, 0]), &[1, 2]);
        assert_eq!(&*normalize_limbs(vec![1, 0, 3]), &[1, 0, 3]);
    }

    #[test]
    fn from_parts_clears_sign_for_zero() {
        let z = impl_of(true, vec![0]);
        assert!(!z.sign(), "zero must be positive");
        assert!(z.is_zero());

        let neg = impl_of(true, vec![0, 5]);
        assert!(neg.sign());
        assert!(!neg.is_zero());

        let pos = impl_of(false, vec![0, 5]);
        assert!(!pos.sign());
    }

    #[test]
    fn zero_impl_is_zero() {
        let z = BigIntImpl::zero();
        assert!(z.is_zero());
        assert!(!z.sign());
        assert_eq!(z.bit_length(), 0);
    }

    #[test]
    fn bit_length_matches_top_limb() {
        assert_eq!(impl_of(false, vec![]).bit_length(), 0);
        assert_eq!(impl_of(false, vec![1]).bit_length(), 1);
        assert_eq!(impl_of(false, vec![0xff]).bit_length(), 8);
        assert_eq!(impl_of(false, vec![0, 1]).bit_length(), 65);
        assert_eq!(impl_of(false, vec![0, 0, 1]).bit_length(), 129);
        assert_eq!(
            impl_of(false, vec![0xff, 0, 1 << 40]).bit_length(),
            128 + 41
        );
    }

    #[test]
    fn create_roundtrips_through_gc_scope() {
        use crate::runtime::Runtime;
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt.create_context(|_| Ok(())).unwrap();
        ctx.with_new_scope(|scope| {
            let r = BigIntRef::create(scope, true, vec![0, 1, 0]).unwrap();
            assert!(r.sign());
            assert_eq!(r.limbs(), &[0, 1]);
            assert!(!r.is_zero());

            let z = BigIntRef::zero(scope).unwrap();
            assert!(!z.sign());
            assert!(z.is_zero());
        });
    }
}
