// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use core::fmt;

use gc_lite::{GcNode, GcRef, GcTrace, GcTraceCtx};
use hash_link::HashList;

use crate::{ScopedContext, atom::Atom, context::ContextImpl, object::ShapeProperty};

use super::super::{obj::Object, property::JS_PROP_LENGTH};

pub(crate) struct ShapeImpl {
    /// Monotonically increasing unique ID. Used by inline cache (inline_cache
    /// feature) to detect ABA reuse of shape memory addresses — a cached
    /// (shape_ptr, uid) pair can never collide with a different shape.
    pub(crate) uid: u32,
    pub(crate) use_count: u32,
    /// cached shape hash value
    pub(crate) hash: u32,
    /// prototype object
    pub(crate) proto: Option<Object>,
    /// shape properties
    pub(crate) prop_defs: HashList<ShapeProperty>,
    /// If true, the shape may have small array index properties 'n' with 0..=u32::MAX;
    /// If false, the shape is guaranteed not to have small array index properties
    pub(crate) has_small_array_index: bool,
    /// Intrusive linked list next pointer for shape registry.
    /// Used to chain shapes in the same hash bucket.
    /// Traced during GC to keep the chain alive while registered.
    pub(super) next: Option<GcRef<ShapeImpl>>,
}

impl fmt::Debug for ShapeImpl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShapeImpl")
            .field("use_count", &self.use_count)
            .field("hash", &self.hash)
            .field("proto", &self.proto)
            .field("prop_defs", &self.prop_defs)
            .field("has_small_array_index", &self.has_small_array_index)
            .finish() // skip registry_next to avoid cycle
    }
}

impl GcTrace for ShapeImpl {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Some(x) = self.proto {
            x.trace(tr);
        }
        self.prop_defs.iter().for_each(|(_, p)| p.trace(tr));
        if let Some(next) = self.next {
            tr.add(next);
        }
    }
}

impl ShapeImpl {
    /// create an empty shape with prototype 'proto'.
    fn new(uid: u32, proto: Option<Object>, props: HashList<ShapeProperty>) -> Result<Self, i32> {
        //calc shape hash
        let mut hash = shape_initial_hash(proto.map_or(0, |p| p.id()));
        for (_, prs) in props.iter() {
            hash = shape_hasher_u32(shape_hasher_u32(hash, prs.atom.id()), prs.flags());
        }

        Ok(Self {
            uid,
            use_count: 1,
            proto,
            prop_defs: props,
            has_small_array_index: false,
            hash,
            next: None,
        })
    }

    /// shape property length
    #[inline(always)]
    pub const fn len(&self) -> usize {
        self.prop_defs.len()
    }

    /// shape proto object
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

    pub(crate) fn add_shape_properties(
        &mut self,
        ctx: &mut ContextImpl,
        extras: &[(Atom, u32)],
    ) -> usize {
        debug_assert!(!extras.is_empty());
        debug_assert_eq!(self.use_count, 1);

        let old_len = self.len();
        let new_len = old_len + extras.len();

        let shape_node = self.gc_ref().node_ptr();

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

            if !prop.is_const() && !prop.is_small_int() {
                // set Atom string gc node xref
                ctx.rt_ptr()
                    .gc_heap_mut()
                    .bind(shape_node, prop.descriptor(ctx.rt()).gc_node_ptr());
            }

            // update shape hash
            self.hash = shape_hasher_u32(shape_hasher_u32(self.hash, prop.id()), *prop_flags);
        }

        new_len
    }

    /// Duplicate a new shape by coping properties and proto.
    /// The new shape is allocated in current gc scope.
    /// The new shape is not registered.
    pub(crate) fn dup(&self, ctx: &mut ScopedContext) -> Result<Shape, i32> {
        // clone properties
        let mut props = HashList::<ShapeProperty>::new();
        props
            .try_reserve(self.prop_defs.count())
            .map_err(|_| ctx.throw_out_of_memory())?;
        for (_, prs) in self.prop_defs.iter() {
            props.push(*prs);
        }

        Shape::create_internal(ctx, self.proto, props)
    }
}

#[repr(transparent)]
#[derive(PartialEq)]
pub(crate) struct Shape(GcRef<ShapeImpl>);

impl GcTrace for Shape {
    fn trace(&self, tr: &mut GcTraceCtx) {
        tr.add(self.0);
    }
}

impl Drop for Shape {
    #[inline(always)]
    fn drop(&mut self) {
        debug_assert!(self.use_count > 0);
        self.use_count -= 1;
    }
}

impl Clone for Shape {
    fn clone(&self) -> Self {
        // SAFETY: use_count is a non-GC field, no write barrier needed.
        unsafe {
            let ptr: *mut ShapeImpl = self.0.as_ptr().as_ptr();
            (*ptr).use_count += 1;
        }
        Self(self.gc_ref())
    }
}

impl std::ops::Deref for Shape {
    type Target = ShapeImpl;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        unsafe { self.0.as_ref() }
    }
}

impl std::ops::DerefMut for Shape {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.0.as_mut() }
    }
}

impl std::fmt::Debug for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Shape {:?}>", self.gc_ref())
    }
}

impl Shape {
    /// create a shape with prototype 'proto' and property capacity.
    /// The new shape is not registered.
    pub(crate) fn create_internal(
        ctx: &mut ScopedContext,
        proto: Option<Object>,
        prop_defs: HashList<ShapeProperty>,
    ) -> Result<Self, i32> {
        let uid = ctx.rt_mut().next_shape_uid.get();
        ctx.rt_mut().next_shape_uid.set(uid.wrapping_add(1));
        ctx.alloc_local(ShapeImpl::new(uid, proto, prop_defs)?)
            .map(|s| {
                debug_assert_eq!(unsafe { s.as_ref() }.use_count, 1);
                Self(s)
            })
            .map_err(|_| ctx.throw_out_of_memory())
    }

    /// create a shape with prototype 'proto' and property capacity.
    /// alloc shape in expected scope (if given scope is null, then use current scope).
    /// new shape is not registered,
    /// new shape is added to GC, new shape is registered.
    pub(crate) fn create(
        ctx: &mut ScopedContext,
        proto: Option<Object>,
        prop_cap: i32,
    ) -> Result<Self, i32> {
        let mut prop_defs = HashList::<ShapeProperty>::new();
        prop_defs
            .try_reserve(prop_cap as _)
            .map_err(|_| ctx.throw_out_of_memory())?;

        let shape = Self::create_internal(ctx, proto, prop_defs)?;
        ctx.rt_mut().register_shape(&shape)?;
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

pub(crate) const fn shape_initial_hash(proto_id: usize) -> u32 {
    let mut h = shape_hasher_u32(1, proto_id as u32);
    if std::mem::size_of::<usize>() > 4 {
        h = shape_hasher_u32(h, ((proto_id as u64) >> 32) as u32);
    }
    h
}
