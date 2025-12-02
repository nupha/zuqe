use std::{
    ops::{Deref, DerefMut},
    os::raw::c_void,
};

use gc_lite::{Collect, GcRef};
use hashlist::{CompactHashList, HashList};

use crate::{atom::Atom, context::ContextImpl, object::ShapeProperty, runtime::RuntimePtr};

use super::super::{obj::ObjectRef, property::JS_PROP_LENGTH};

#[derive(Debug)]
pub struct ShapeImpl {
    /// runtime raw pointer, None indicates the shape impl is dropped.
    rt: Option<RuntimePtr>,

    /// count of uses
    pub(super) use_count: u32,

    /// If true, the shape may have small array index properties 'n' with 0..=u32::MAX;
    /// If false, the shape is guaranteed not to have small array index properties
    pub has_small_array_index: bool,

    /// cached shape hash value
    pub hash: u32,

    pub(crate) proto: Option<ObjectRef>,

    /// shape properties
    pub(crate) prop_defs: CompactHashList<ShapeProperty>,
}

unsafe impl Collect for ShapeImpl {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.proto.trace(tracer);
        for p in self.prop_defs.iter() {
            p.trace(tracer);
        }
    }
}

impl Drop for ShapeImpl {
    fn drop(&mut self) {
        // Note: when ShapeImpl is dropped, set rt to None.
        // So that ShapeHandle will aware its inner shape was droppped.
        self.rt.take().unwrap();
    }
}

impl ShapeImpl {
    /// create a new empty shape with prototype 'proto' and given capacity.
    fn new(
        ctx: &ContextImpl,
        proto: Option<ObjectRef>,
        props: CompactHashList<ShapeProperty>,
    ) -> Result<Self, i32> {
        //calc shape hash
        let mut hash = shape_initial_hash(proto.map_or(0, |p| p.id()));
        for prs in props.iter() {
            hash = shape_hasher_u32(shape_hasher_u32(hash, prs.atom.id()), prs.flags());
        }

        Ok(Self {
            rt: Some(ctx.rt()),
            use_count: 1,
            proto,
            prop_defs: props,
            has_small_array_index: false,
            hash,
        })
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    const fn rt(&self) -> Option<RuntimePtr> {
        self.rt
    }

    #[inline(always)]
    pub const fn use_count(&self) -> u32 {
        self.use_count
    }

    /// shape property length
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.prop_defs.len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.prop_defs.len() == 0
    }

    /// get shape proto
    #[inline(always)]
    pub const fn proto(&self) -> Option<ObjectRef> {
        self.proto
    }

    /// get shape hash
    #[inline(always)]
    pub const fn hash(&self) -> u32 {
        self.hash
    }

    /// fully re-calculate and update shape hash
    pub(super) fn full_rehash(&mut self) -> u32 {
        let mut hash = shape_initial_hash(self.proto.as_ref().map_or(0, |p| p.id()));
        for prs in self.prop_defs.iter() {
            hash = shape_hasher_u32(shape_hasher_u32(hash, prs.atom.id()), prs.flags());
        }
        self.hash = hash;
        hash
    }

    pub(crate) fn add_shape_properties(&mut self, extras: &[(Atom, u32)]) -> usize {
        debug_assert!(!extras.is_empty());
        debug_assert_eq!(self.use_count(), 1);

        let old_len = self.len();
        let new_len = old_len + extras.len();

        for (i, (prop, prop_flags)) in extras.iter().enumerate() {
            #[cfg(debug_assertions)]
            if prop_flags & JS_PROP_LENGTH != 0 {
                debug_assert_eq!(old_len + i, 0, "length must be the first shape prop")
            }

            let is_small_int = prop.is_small_int();

            let mut prs = ShapeProperty {
                atom: *prop,
                flags: 0,
            };
            prs.set_flags(*prop_flags);

            self.prop_defs.push(prs);
            self.has_small_array_index |= is_small_int;

            // update shape hash
            self.hash = shape_hasher_u32(shape_hasher_u32(self.hash, prop.id()), *prop_flags);
        }

        new_len
    }

    /// Duplicate a new shape by coping properties and proto.
    /// The new shape is not registered.
    pub(crate) fn dup(&self, ctx: &ContextImpl) -> Result<ShapeHandle, i32> {
        // clone properties
        let mut props = CompactHashList::<ShapeProperty>::new();
        props
            .try_reserve_exact(self.prop_defs.count())
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        for prs in self.prop_defs.iter() {
            props.push(*prs);
        }

        ShapeHandle::new_internal(ctx, self.proto, props)
    }
}

#[repr(transparent)]
pub struct ShapeHandle(GcRef<ShapeImpl>);

unsafe impl Collect for ShapeHandle {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        tracer.mark(self.0);
    }
}

impl Drop for ShapeHandle {
    fn drop(&mut self) {
        if let Some(mut rt) = self.rt() {
            rt.shape_registry.dec_use(self);
        } else {
            // ShapeImpl was dropped proior to ShapeHandle dropping.
            // this could be happen when GcRef<ShapeImpl> is dropped by gc collect,
            // while GcRef<ObjectImpl> which may hold a ShapeHandle is still alive in gc.
            // This mostly happens in Runtime finalize when collecting all gc objects in a non-determined order.
        }
    }
}

impl Clone for ShapeHandle {
    fn clone(&self) -> Self {
        if let Some(mut rt) = self.rt() {
            rt.shape_registry.inc_use(self)
        } else {
            #[cfg(debug_assertions)]
            unreachable!("ShapeImpl was dropped");
        };

        Self(self.0)
    }
}

impl Deref for ShapeHandle {
    type Target = ShapeImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl DerefMut for ShapeHandle {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl PartialEq for ShapeHandle {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl std::fmt::Debug for ShapeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Shape {:p}>", self.id() as *const c_void)
    }
}

impl ShapeHandle {
    /// create a shape with prototype 'proto' and property capacity.
    /// The new shape is not registered.
    pub(crate) fn new_internal(
        ctx: &ContextImpl,
        proto: Option<ObjectRef>,
        prop_defs: CompactHashList<ShapeProperty>,
    ) -> Result<Self, i32> {
        let mut rt = ctx.rt();
        let sh = rt
            .gc_alloc(ShapeImpl::new(ctx, proto, prop_defs)?)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        Ok(Self(sh))
    }

    /// create a shape with prototype 'proto' and property capacity.
    /// new shape is not registered,
    /// new shape is added to GC, new shape is registered.
    pub fn new(ctx: &ContextImpl, proto: Option<ObjectRef>, prop_cap: i32) -> Result<Self, i32> {
        let mut prop_defs = CompactHashList::<ShapeProperty>::new();
        prop_defs.try_reserve_exact(prop_cap as _).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        let shape = Self::new_internal(ctx, proto, prop_defs)?;
        ctx.rt().shape_registry.register(&shape)?;

        Ok(shape)
    }

    #[inline(always)]
    pub(crate) const fn from(sh: GcRef<ShapeImpl>) -> Self {
        Self(sh)
    }

    #[inline(always)]
    pub const fn gc_ref(&self) -> GcRef<ShapeImpl> {
        self.0
    }
}

/// calc hash for u32.
/// Same magic hash multiplier as the Linux kernel
#[inline(always)]
pub(super) const fn shape_hasher_u32(h: u32, val: u32) -> u32 {
    h.wrapping_add(val).wrapping_mul(0x9e3779b9)
}

pub const fn shape_initial_hash(proto_id: usize) -> u32 {
    let mut h = shape_hasher_u32(1, proto_id as u32);
    if std::mem::size_of::<usize>() > 4 {
        h = shape_hasher_u32(h, ((proto_id as u64) >> 32) as u32);
    }
    h
}
