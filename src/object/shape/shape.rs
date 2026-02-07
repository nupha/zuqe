use std::{
    ops::{Deref, DerefMut},
    os::raw::c_void,
};

use gc_lite::{GcRef, GcTracable, GcTraceOp};
use hash_link::HashList;

use crate::{atom::Atom, context::ContextImpl, object::ShapeProperty};

use super::super::{obj::Object, property::JS_PROP_LENGTH};

#[derive(Debug)]
pub struct ShapeImpl {
    pub(super) use_count: u32,

    /// If true, the shape may have small array index properties 'n' with 0..=u32::MAX;
    /// If false, the shape is guaranteed not to have small array index properties
    pub has_small_array_index: bool,

    /// cached shape hash value
    pub hash: u32,

    pub(crate) proto: Option<Object>,

    /// shape properties
    pub(crate) prop_defs: HashList<ShapeProperty>,
}

unsafe impl GcTracable for ShapeImpl {
    fn trace(&self, tr: GcTraceOp) {
        self.proto.trace(tr);
        self.prop_defs.iter().for_each(|(_, p)| p.trace(tr));
    }
}

impl ShapeImpl {
    /// create a new empty shape with prototype 'proto' and given capacity.
    fn new(proto: Option<Object>, props: HashList<ShapeProperty>) -> Result<Self, i32> {
        //calc shape hash
        let mut hash = shape_initial_hash(proto.map_or(0, |p| p.id()));
        for (_, prs) in props.iter() {
            hash = shape_hasher_u32(shape_hasher_u32(hash, prs.atom.id()), prs.flags());
        }

        Ok(Self {
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
    pub const fn use_count(&self) -> u32 {
        self.use_count
    }

    #[inline(always)]
    pub const fn inc_count(&mut self) {
        self.use_count += 1;
    }

    #[inline(always)]
    pub fn dec_count(&mut self) {
        debug_assert!(self.use_count > 0);
        self.use_count -= 1;
    }

    #[deprecated(note = "remove this")]
    pub(crate) fn dec_count_noop(&mut self) {}

    /// shape property length
    #[inline(always)]
    pub const fn len(&self) -> usize {
        self.prop_defs.len()
    }

    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        self.prop_defs.len() == 0
    }

    /// get shape proto
    #[inline(always)]
    pub const fn proto(&self) -> Option<Object> {
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
        for (_, prs) in self.prop_defs.iter() {
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
    pub(crate) fn dup(&self, ctx: &ContextImpl) -> Result<Shape, i32> {
        // clone properties
        let mut props = HashList::<ShapeProperty>::new();
        props.try_reserve(self.prop_defs.count()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        for (_, prs) in self.prop_defs.iter() {
            props.push(*prs);
        }

        Shape::new_internal(ctx, self.proto, props)
    }
}

#[repr(transparent)]
#[derive(PartialEq)]
pub struct Shape(GcRef<ShapeImpl>);

unsafe impl GcTracable for Shape {
    fn trace(&self, mut tr: GcTraceOp) {
        tr.add(self.0);
    }
}

impl Drop for Shape {
    fn drop(&mut self) {
        self.dec_count();

        log::trace!(
            "[Shape::drop] {:?}, new_use_count={}",
            self.gc_ref().node_info(),
            self.use_count()
        );

        if self.use_count() == 0 {
            self.gc_ref().node_info_mut().unset_xref_partition();
        }
    }
}

impl Clone for Shape {
    fn clone(&self) -> Self {
        let mut gr = self.gc_ref();
        gr.inc_count();
        Self(gr)
    }
}

impl Deref for Shape {
    type Target = ShapeImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}

impl DerefMut for Shape {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.deref_mut()
    }
}

impl std::fmt::Debug for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Shape {:p}>", self.id() as *const c_void)
    }
}

impl Shape {
    /// create a shape with prototype 'proto' and property capacity.
    /// The new shape is not registered.
    pub(crate) fn new_internal(
        ctx: &ContextImpl,
        proto: Option<Object>,
        prop_defs: HashList<ShapeProperty>,
    ) -> Result<Self, i32> {
        let shape = ShapeImpl::new(proto, prop_defs)?;

        ctx.rt_ptr().gc_alloc(shape).map(Self).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })
    }

    /// create a shape with prototype 'proto' and property capacity.
    /// new shape is not registered,
    /// new shape is added to GC, new shape is registered.
    pub fn new(ctx: &ContextImpl, proto: Option<Object>, prop_cap: i32) -> Result<Self, i32> {
        let mut prop_defs = HashList::<ShapeProperty>::new();
        prop_defs.try_reserve(prop_cap as _).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        let mut shape = Self::new_internal(ctx, proto, prop_defs)?;
        ctx.rt_ptr().register_shape(&shape).inspect_err(|_| {
            shape.dec_count_noop();
        })?;

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
