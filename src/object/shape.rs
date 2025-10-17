use std::{
    cell::RefCell,
    os::raw::c_void,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use indexmap::IndexMap;

use crate::{
    atom::{Atom, AtomRef},
    context::JSContext,
    gc::GCObjectHeader,
    object::{GCObject, GCObjectWeakref, JS_PROP_WRITABLE},
    runtime::JSRuntime,
};

use super::{
    ObjectInner,
    obj::{JS_PROP_INITIAL_HASH_SIZE, JSObject},
    property::JS_PROP_LENGTH,
};

/// Property defination in shape
#[derive(Debug)]
pub struct JSShapeProperty {
    pub atom: Atom,
    flags: u32,
}

impl JSShapeProperty {
    #[inline(always)]
    pub const fn flags(&self) -> u32 {
        self.flags
    }

    #[inline]
    pub const fn set_flags(&mut self, f: u32) {
        self.flags = f;
    }

    /// is special property `length`?
    #[inline(always)]
    pub fn is_length(&self) -> bool {
        self.flags & JS_PROP_LENGTH != 0
    }

    #[inline(always)]
    pub fn is_writable(&self) -> bool {
        self.flags & JS_PROP_WRITABLE != 0
    }
}

#[derive(Debug)]
pub(crate) struct JSShapeInner {
    /// hash table of size hash_mask + 1 before the start of the structure (see prop_hash_end()).
    pub header: GCObjectHeader,

    /// If true, the shape may have small array index properties 'n' with 0..=u32::MAX;
    /// If false, the shape is guaranteed not to have small array index properties
    pub has_small_array_index: bool,

    /// shape hash value.
    pub hash: u32,
    /// TODO: hash is calculated on the fly. is_hashed is always true, should be removed
    pub is_hashed: bool,

    pub proto: Option<JSObject>,

    /// shape property definitions
    pub prop_defs: IndexMap<u32, JSShapeProperty>,
}

impl Drop for JSShapeInner {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl JSShapeInner {
    /// create a new empty shape with prototype 'proto' and given capacity.
    fn new(
        ctx: &JSContext,
        proto: Option<JSObject>,
        props: IndexMap<u32, JSShapeProperty>,
    ) -> Result<Self, i32> {
        //calc shape hash
        let mut hash = shape_initial_hash(proto.as_ref().map_or(0, |p| p.id()));
        for (_, prs) in props.iter() {
            hash = shape_hasher_u32(shape_hasher_u32(hash, prs.atom.id()), prs.flags());
        }

        Ok(Self {
            header: GCObjectHeader::new(ctx.rt_mut(), 1),
            proto,
            prop_defs: props,
            has_small_array_index: false,
            hash,
            is_hashed: true,
        })
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    pub fn rt_ptr(&self) -> NonNull<JSRuntime> {
        self.header.rt_ptr()
    }

    fn finalize(&mut self) {
        let rt = unsafe { self.rt_ptr().as_mut() };

        // free prop key atoms
        for (_, mut prs) in self.prop_defs.drain(..) {
            std::mem::replace(&mut prs.atom, Atom::NULL).free(rt);
        }

        if self.is_hashed {
            rt.unregister_shape(self.id(), self.hash);
        }
    }
}

#[repr(transparent)]
#[derive(Clone)]
pub struct JSShape(pub(crate) Rc<RefCell<JSShapeInner>>);

impl PartialEq for JSShape {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl From<JSShape> for GCObject {
    fn from(value: JSShape) -> Self {
        Self::Shape(value)
    }
}

impl std::fmt::Debug for JSShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<Shape {:p}>", self.id() as *const c_void)
    }
}

impl JSShape {
    /// create a shape with prototype 'proto' and property capacity.
    /// new shape is not registered,
    /// new shape is added to gc list.
    pub fn new(ctx: &JSContext, proto: Option<JSObject>, prop_cap: i32) -> Result<Self, i32> {
        let mut prop_defs = IndexMap::<u32, JSShapeProperty>::new();
        prop_defs.try_reserve_exact(prop_cap as _).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        let shape = Self(Rc::new(RefCell::new(JSShapeInner::new(
            ctx, proto, prop_defs,
        )?)));

        ctx.rt_mut()
            .gc_add_object(shape.id(), shape.downgrade().into());

        Ok(shape)
    }

    // /// create a new empty shape with prototype 'proto'.
    // /// Note: `proto` is not consumed.
    // #[inline(always)]
    // pub fn new(ctx: &JSContext, proto: Option<JSObject>) -> Result<Self, i32> {
    //     Self::new(
    //         ctx,
    //         proto,
    //         JS_PROP_INITIAL_HASH_SIZE,
    //         JS_PROP_INITIAL_SIZE as _,
    //     )
    // }

    /// Clone to a new shape from this shape by coping property definitions.
    /// The new shape is not registered. The new shape is added to gc object list.
    pub fn clone_to_new(&self, ctx: &JSContext) -> Result<Self, i32> {
        let b = self.0.borrow();

        // clone properties
        let mut props = IndexMap::<u32, JSShapeProperty>::new();
        {
            let props0 = &b.prop_defs;

            props.try_reserve(props0.len()).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            for (id, sp) in props0.iter() {
                props.insert(
                    *id,
                    JSShapeProperty {
                        flags: sp.flags,
                        atom: sp.atom.dup(ctx.rt()),
                    },
                );
            }
        }

        let sh = JSShapeInner::new(ctx, b.proto.clone(), props)?;
        let shape = Self(Rc::new(RefCell::new(sh)));

        ctx.rt_mut()
            .gc_add_object(shape.id(), shape.downgrade().into());

        Ok(shape)
    }

    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    pub fn strong_count(&self) -> usize {
        Rc::strong_count(&self.0)
    }

    pub fn with<R>(&self, f: impl FnOnce(&JSShapeInner) -> R) -> R {
        f(&self.0.borrow())
    }
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut JSShapeInner) -> R) -> R {
        f(&mut self.0.borrow_mut())
    }

    pub fn is_hashed(&self) -> bool {
        self.0.borrow().is_hashed
    }

    pub fn hash(&self) -> u32 {
        self.0.borrow().hash
    }

    pub fn len(&self) -> usize {
        self.0.borrow().prop_defs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Append a property definition to shape, returns final shape property length.
    /// If obj is provided, it's an object that would use this shape,
    /// will try to enlarge obj's value slots if a prop is added to shape.
    ///
    /// When returns Err, regardless it's add shape prop failure or enlarge slot failure,
    /// the shape is guaranteed to be unregistered.
    ///
    /// Note:
    /// - Shape `self` must *NOT* be shared when calling this.
    /// - `prop` is not consumed.
    pub(crate) fn add_shape_property(
        &self,
        ctx: &JSContext,
        prop: AtomRef,
        prop_flags: u32,
        obj: Option<&ObjectInner>,
    ) -> Result<usize, i32> {
        debug_assert_eq!(self.strong_count(), 1, "shape must not be shared");

        let (shape_id, is_hashed, old_len, old_hash) = {
            let s = self.0.borrow();
            (s.id(), s.is_hashed, s.prop_defs.len(), s.hash)
        };

        #[cfg(debug_assertions)]
        if prop_flags & JS_PROP_LENGTH != 0 {
            debug_assert_eq!(old_len, 0, "length must be the first shape prop")
        }

        let new_hash = if is_hashed {
            ctx.rt_mut().unregister_shape(shape_id, old_hash);
            shape_hasher_u32(shape_hasher_u32(old_hash, prop.0), prop_flags)
        } else {
            0
        };
        let new_len = old_len + 1;

        if let Some(ob) = obj {
            ob.grow_prop_value_slots(new_len).inspect_err(|_| {
                ctx.throw_out_of_memory();
            })?;
        }

        if is_hashed {
            self.0.borrow_mut().hash = new_hash;
            ctx.rt_mut().register_shape(self).inspect_err(|_| {
                ctx.throw_out_of_memory();
            })?;
        }

        let mut prs = JSShapeProperty {
            atom: prop.dup(ctx.rt()),
            flags: 0,
        };
        prs.set_flags(prop_flags);

        let mut s = self.0.borrow_mut();
        s.prop_defs.insert(prop.0, prs);
        s.has_small_array_index |= prop.is_small_int();

        Ok(new_len)
    }

    pub fn downgrade(&self) -> JSShapeWeakref {
        JSShapeWeakref(Rc::downgrade(&self.0))
    }

    pub fn gc_has_mark(&self) -> bool {
        self.0.borrow().header.gc_has_mark()
    }

    pub fn gc_set_mark(&self, mark: bool) {
        self.0.borrow_mut().header.gc_set_mark(mark);
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14p} [shape {:12x}]", self, self.hash());
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct JSShapeWeakref(Weak<RefCell<JSShapeInner>>);

impl JSShapeWeakref {
    pub fn upgrade(&self) -> Option<JSShape> {
        self.0.upgrade().map(JSShape)
    }
}

impl From<JSShapeWeakref> for GCObjectWeakref {
    fn from(value: JSShapeWeakref) -> Self {
        Self::Shape(value)
    }
}

impl JSRuntime {
    /// register a shape
    pub fn register_shape(&mut self, shape: &JSShape) -> Result<(), i32> {
        debug_assert!(shape.0.borrow().is_hashed);

        // resize the shape hash table
        if self.shape_registry.len() == self.shape_registry.capacity() {
            self.shape_registry
                .try_reserve(JS_PROP_INITIAL_HASH_SIZE)
                .map_err(|_| -1)?;
        }

        let hash = shape.0.borrow().hash;
        let weak = shape.downgrade();

        self.shape_registry
            .entry(hash)
            .and_modify(|lst| {
                lst.push(weak.clone());
            })
            .or_insert_with(|| vec![weak]);

        Ok(())
    }

    /// unregister a shape
    pub fn unregister_shape(&mut self, shape_id: usize, hash: u32) {
        if let Some(lst) = self.shape_registry.get_mut(&hash) {
            lst.retain(|s| {
                if let Some(sh) = s.upgrade() {
                    sh.id() != shape_id
                } else {
                    false
                }
            });
            if lst.is_empty() {
                self.shape_registry.remove(&hash);
            }
        }
    }

    /// find a registered empty shape matching given prototype.
    /// Return None if not found.
    pub fn find_shape_by_proto(&self, proto: Option<&JSObject>) -> Option<JSShape> {
        let h = shape_initial_hash(proto.map_or(0, |o| o.id()));

        self.shape_registry.get(&h).and_then(|lst| {
            lst.iter().find_map(|w| {
                w.upgrade().filter(|s| {
                    let s = s.0.borrow();
                    debug_assert_eq!(s.hash, h);
                    s.proto.as_ref().eq(&proto) && s.prop_defs.is_empty()
                })
            })
        })
    }

    /// find a registered shape matching base_shape plus an extra (prop, prop_flags).
    /// Return None if not found
    pub fn find_shape_with_extend_prop(
        &self,
        base_shape: &JSShape,
        prop: AtomRef,
        prop_flags: u32,
    ) -> Option<JSShape> {
        let base_shape = base_shape.0.borrow();

        // calc hash for base_shape + (atom, prop_flags)
        let mut h = base_shape.hash;
        h = shape_hasher_u32(h, prop.0);
        h = shape_hasher_u32(h, prop_flags);

        let expect_len = base_shape.prop_defs.len() + 1;

        if let Some(found) = self.shape_registry.get(&h).and_then(|lst| {
            lst.iter().find_map(|w| {
                if let Some(sh) = w.upgrade() {
                    let s = sh.0.borrow();
                    if s.proto == base_shape.proto && s.prop_defs.len() == expect_len {
                        Some(sh.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        }) {
            if base_shape
                .prop_defs
                .iter()
                .zip(found.0.borrow().prop_defs.iter())
                .all(|(a, b)| a.0 == b.0 && a.1.atom == b.1.atom && a.1.flags() == b.1.flags())
            {
                // all prop defs are match
                if found
                    .0
                    .borrow()
                    .prop_defs
                    .last()
                    .map(|(_, pr)| pr.atom.as_ref() == prop && pr.flags() == prop_flags)
                    .unwrap_or(false)
                {
                    // last prop (atom, prop_flags) is match too
                    return Some(found);
                }
            }
        }

        None
    }
}

/// calc hash for u32.
/// Same magic hash multiplier as the Linux kernel
#[inline(always)]
const fn shape_hasher_u32(h: u32, val: u32) -> u32 {
    h.wrapping_add(val).wrapping_mul(0x9e3779b9)
}

pub const fn shape_initial_hash(proto_id: usize) -> u32 {
    let mut h = shape_hasher_u32(1, proto_id as u32);
    if std::mem::size_of::<usize>() > 4 {
        h = shape_hasher_u32(h, ((proto_id as u64) >> 32) as u32);
    }
    h
}

impl ObjectInner {
    /// ensure that the shape can be safely modified.
    /// if shape of `self` is changed, returns true.
    /// after calling, object's shape is guaranteed to be exclusive, and not registered.
    fn shape_prepare_update(&self, ctx: &JSContext) -> Result<bool, i32> {
        // Note: short borrow in scope to avoid conflict with later ::borrow_mut()
        let (shape_id, is_hashed, hash, strong_count) = {
            let b = self.shape.borrow();
            let sh = b.as_ref().unwrap();
            (sh.id(), sh.is_hashed(), sh.hash(), sh.strong_count())
        };

        let changed = if is_hashed {
            if strong_count != 1 {
                // shape is shared: clone and use a new shape (the new shape is not registered)
                let new_sh = self.shape.borrow().as_ref().unwrap().clone_to_new(ctx)?;
                self.shape.borrow_mut().replace(new_sh);
                true
            } else {
                // exclusive: unregister
                ctx.rt_mut().unregister_shape(shape_id, hash);
                false
            }
        } else {
            false
        };

        Ok(changed)
    }

    /// Updates flag of a shape property.
    /// if object's shape was changed, returns true
    pub fn update_shape_property_flags(
        &self,
        ctx: &JSContext,
        prop: AtomRef,
        flags: u32,
    ) -> Result<bool, i32> {
        let cur_flags = self
            .shape
            .borrow()
            .as_ref()
            .unwrap()
            .0
            .borrow()
            .prop_defs
            .get(&prop.0)
            .map(|p| p.flags())
            .unwrap();

        if cur_flags != flags {
            let changed = self.shape_prepare_update(ctx)?;

            self.shape
                .borrow_mut()
                .as_mut()
                .unwrap()
                .0
                .borrow_mut()
                .prop_defs
                .get_mut(&prop.0)
                .unwrap()
                .set_flags(flags);

            Ok(changed)
        } else {
            Ok(false)
        }
    }
}

impl JSObject {
    /// ensure that the shape can be safely modified.
    /// if object's shape is changed, returns true.
    /// after calling, object's shape is guaranteed to be exclusive, and not registered.
    #[inline(always)]
    pub fn shape_prepare_update(&self, ctx: &JSContext) -> Result<bool, i32> {
        self.0.shape_prepare_update(ctx)
    }

    /// Updates flag of object's shape property.
    /// if object's shape was changed, returns true
    #[inline(always)]
    pub fn update_shape_property_flags(
        &self,
        ctx: &JSContext,
        prop: AtomRef,
        flags: u32,
    ) -> Result<bool, i32> {
        self.0.update_shape_property_flags(ctx, prop, flags)
    }
}
