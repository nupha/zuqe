use std::{
    ops::{Deref, DerefMut},
    ptr::NonNull,
};

use indexmap::IndexMap;

use crate::{
    atom::{consts::JS_ATOM_length, JSAtom, JS_FreeAtomRT, JS_ATOM_NULL},
    context::JSContext,
    gc::{
        js_trigger_gc, GCMarkFunc, GCObject, GCObjectHeader, GCObjectNodeData, GCObjectType,
        RefCounted,
    },
    mem::js_malloc_typed,
    runtime::JSRuntime,
    utils::{init_list_head, Node},
    value::{
        object::{JSObjectFlags, JSObjectIsa},
        JSTag, JSValue, JS_FreeValueRT,
    },
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_GENERATOR_FUNCTION,
    JS_CLASS_BIG_INT, JS_CLASS_BOOLEAN, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_C_FUNCTION,
    JS_CLASS_DATAVIEW, JS_CLASS_DATE, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_GENERATOR_FUNCTION,
    JS_CLASS_NUMBER, JS_CLASS_OBJECT, JS_CLASS_REGEXP, JS_CLASS_STRING, JS_CLASS_SYMBOL,
    JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    obj::{JSObject, JS_PROP_INITIAL_HASH_SIZE, JS_PROP_INITIAL_SIZE},
    property::{JSProperty, JSPropertyUnion, JS_PROP_LENGTH, JS_PROP_WRITABLE},
    JSShapeHandle,
};

/// Property defination in shape
#[derive(Debug)]
pub struct JSShapeProperty {
    // flags:   6 bits: JS_PROP_XXX
    // hash_next:  26 bits: 0 if last in list
    bitfield: u32,

    #[deprecated(note = "use Hash key instead")]
    pub atom: JSAtom,
}

impl JSShapeProperty {
    const NEXT_HASH_MASK: u32 = 0x03FF_FFFF; // lower 26-bits
    const FLAGS_MASK: u32 = 0xFC00_0000; // upper 6-bits

    #[inline(always)]
    pub const fn flags(&self) -> u32 {
        (self.bitfield & Self::FLAGS_MASK) >> 26
    }

    #[inline]
    pub const fn set_flags(&mut self, f: u32) {
        self.bitfield = (self.bitfield & !Self::FLAGS_MASK) | ((f & 0x3f) << 26);
    }
}

#[repr(C)]
pub struct JSShape {
    /// hash table of size hash_mask + 1 before the start of the structure (see prop_hash_end()).
    pub header: GCObjectHeader,

    /// true if the shape is inserted in the shape hash table. If not, JSShape.hash is not valid
    pub is_hashed: bool,

    /// If true, the shape may have small array index properties 'n' with 0..=u32::MAX;
    /// If false, the shape is guaranteed not to have small array index properties
    pub has_small_array_index: bool,

    /// current hash value
    pub hash: u32,

    /// in JSRuntime.shape_hash[h] list
    #[deprecated]
    pub shape_hash_next: *mut JSShape,

    pub proto: *mut JSObject,

    /// shape property definations
    pub props: IndexMap<JSAtom, JSShapeProperty>,
}

impl RefCounted for JSShape {
    fn ref_count(&self) -> i32 {
        self.header.ref_count
    }

    fn inc_ref(&mut self) -> i32 {
        self.header.ref_count += 1;
        self.header.ref_count
    }

    fn dec_ref(&mut self) -> i32 {
        self.header.ref_count -= 1;
        self.header.ref_count
    }
}

impl GCObject for JSShape {
    fn gc_type(&self) -> GCObjectType {
        GCObjectType::Shape(NonNull::from(self))
    }

    fn gc_mark(&self) -> u8 {
        self.header.gc_mark()
    }

    fn set_gc_mark(&mut self, mark: u8) {
        self.header.set_gc_mark(mark);
    }

    fn finalize(&mut self, rt: &mut JSRuntime) {
        if self.is_hashed {
            rt.unregister_shape(self.into());
            self.is_hashed = false;
        }

        if !self.proto.is_null() {
            unsafe {
                debug_assert!(self.proto.is_aligned());
                JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, self.proto.cast()));
            }
            self.proto = std::ptr::null_mut();
        }

        // free prop key atoms
        for (_, prs) in self.props.drain(..) {
            JS_FreeAtomRT(rt, prs.atom);
        }
    }

    fn gc_mark_children(&mut self, rt: &mut JSRuntime, mark_func: GCMarkFunc) {
        if !self.proto.is_null() {
            unsafe {
                mark_func(rt, self.proto);
            }
        }
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!(
            "{:14p} {:4} [shape {:12x}]",
            self,
            self.ref_count(),
            self.hash
        );
    }

    fn gc_node_ptr(&self) -> NonNull<Node<GCObjectNodeData>> {
        debug_assert!(!self.header.list_node.is_null());
        unsafe { NonNull::new_unchecked(self.header.list_node) }
    }
}

#[cfg(debug_assertions)]
impl Drop for JSShape {
    fn drop(&mut self) {
        debug_assert_eq!(self.ref_count(), 0);
        debug_assert!(!self.is_hashed);
        debug_assert!(self.proto.is_null());
        debug_assert!(self.props.is_empty());
    }
}

impl JSShape {
    /// create a new empty shape with prototype 'proto' and given capacity.
    /// Note: `proto` is not consumed.
    #[inline(never)]
    pub fn with_capacity(
        ctx: &mut JSContext,
        proto: Option<&mut JSObject>,
        hash_size: usize,
        prop_size: i32,
    ) -> Result<JSShapeHandle, i32> {
        let rt = ctx.rt_mut();

        // resize the shape hash table if necessary
        if rt.shape_registry.len() == rt.shape_registry.capacity() {
            if rt.shape_registry.try_reserve(hash_size).is_err() {
                ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        // alloc for prop defination
        let mut props = IndexMap::<JSAtom, JSShapeProperty>::new();
        if props.try_reserve_exact(prop_size as _).is_err() {
            ctx.JS_ThrowOutOfMemory();
            return Err(-1);
        }

        let proto_ptr = proto.map_or(std::ptr::null_mut(), |p| p as *mut JSObject);

        if !proto_ptr.is_null() {
            JS_MKPTR!(JSTag::Object, proto_ptr.cast()).dup();
        }

        let mut shape = Box::new(Self {
            header: GCObjectHeader::new(1),
            has_small_array_index: false,
            hash: shape_initial_hash(proto_ptr),
            is_hashed: true,
            shape_hash_next: std::ptr::null_mut(),
            proto: proto_ptr,
            props,
        });

        debug_assert_eq!(
            shape.as_ref() as *const JSShape as usize,
            std::ptr::addr_of!(shape.header) as usize
        );

        let sh_ptr = JSShapeHandle::from(NonNull::from(shape.as_mut()).as_ptr());
        rt.gc_add_object(shape);
        rt.register_shape(sh_ptr);

        Ok(sh_ptr)
    }

    /// create a new empty shape with prototype 'proto'.
    /// Note: `proto` is not consumed.
    #[inline(always)]
    pub fn new(ctx: &mut JSContext, proto: Option<&mut JSObject>) -> Result<JSShapeHandle, i32> {
        Self::with_capacity(
            ctx,
            proto,
            JS_PROP_INITIAL_HASH_SIZE,
            JS_PROP_INITIAL_SIZE as _,
        )
    }

    /// Append property defination to shape.
    /// `obj` is an JSObject that uses the shape,
    /// if provided, should ensure this object has enough property value slots capacity.
    ///
    /// Note: `atom`, `obj` are not consumed.
    /// TODO: rename to add_property_defination
    pub fn add_shape_property(
        &mut self,
        ctx: &mut JSContext,
        mut obj: Option<&mut JSObject>,
        atom: JSAtom,
        prop_flags: u32,
    ) -> Result<(), i32> {
        debug_assert_eq!(self.ref_count(), 1);

        let new_shape_hash = if self.is_hashed {
            // Recalc shape hash
            ctx.rt_mut().unregister_shape(self.into());
            shape_hasher_u32(shape_hasher_u32(self.hash, atom.0), prop_flags)
        } else {
            0
        };

        if let Some(o) = obj.as_deref_mut() {
            // ensure object's prop values slot capacity
            if o.grow_prop_value_slots(self.props.len() + 1).is_err() {
                // in case of error, restore the shape into registration.
                // shape is still valid if resize failed
                if self.is_hashed {
                    ctx.rt_mut().register_shape(self.into());
                }
                ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        if self.is_hashed {
            self.hash = new_shape_hash;
            ctx.rt_mut().register_shape(self.into());
        }

        let mut pr = JSShapeProperty {
            bitfield: 0,
            atom: atom.dup(ctx.rt_mut()),
        };
        pr.set_flags(prop_flags);
        self.props.insert(atom, pr);

        self.has_small_array_index |= atom.is_int_type();

        #[cfg(debug_assertions)]
        {
            if prop_flags & JS_PROP_LENGTH != 0 {
                debug_assert_eq!(self.props.len(), 1, "length property must be indexed 0")
            }
            if let Some(o) = obj.as_deref() {
                debug_assert_eq!(self.props.len(), o.prop_values.len())
            }
        }

        Ok(())
    }

    /// Clone to a new shape from this coping property definations.
    /// The new shape is not inserted in the shape hash table, and not added to gc object list.
    fn clone_to_new(&self, ctx: &mut JSContext) -> Result<Box<Self>, i32> {
        // clone properties
        let mut props = IndexMap::<JSAtom, JSShapeProperty>::new();
        if props.try_reserve(self.props.len()).is_err() {
            ctx.JS_ThrowOutOfMemory();
            return Err(-1);
        }

        let rt = ctx.rt();
        for (key, pr) in self.props.iter() {
            props.insert(
                *key,
                JSShapeProperty {
                    bitfield: pr.bitfield,
                    atom: pr.atom.dup(rt),
                },
            );
        }

        let sh2 = Box::new(Self {
            header: GCObjectHeader::new(1),
            is_hashed: false,
            has_small_array_index: self.has_small_array_index,
            hash: self.hash,
            shape_hash_next: self.shape_hash_next,
            proto: self.proto,
            props,
        });

        if !sh2.proto.is_null() {
            JS_MKPTR!(JSTag::Object, sh2.proto.cast()).dup();
        }

        Ok(sh2)
    }

    /// Clone to a new shape. The new shape is not inserted in the shape hash table
    pub fn clone(&mut self, ctx: &mut JSContext) -> Result<JSShapeHandle, i32> {
        let mut sh = self.clone_to_new(ctx)?;
        let sh_ptr = sh.as_mut() as *mut JSShape;

        (*ctx).rt_mut().gc_add_object(sh);
        Ok(sh_ptr.into())
    }

    /// decrement ref count of `shape`, if ref count reaches 0, dispose it.
    pub fn free(rt: &mut JSRuntime, mut sh: JSShapeHandle) {
        debug_assert!(sh.ref_count() > 0);
        if sh.dec_ref() == 0 {
            // clear up and drop the shape
            let mut node = sh.deref().gc_node_ptr();
            unsafe {
                Node::unlink(node);
                node.as_mut().finalize(rt);
                drop(Box::from_raw(node.as_ptr()));
            }
        }
    }
}

impl JSContext {
    /// create instance of `isa` type, based on given shape and isa payload data.
    /// when fail, isa is returned back in Err, caller should free it.
    /// Note: `shape` is consumed.
    pub fn create_instance_from_shape(
        &mut self,
        shape: JSShapeHandle,
        isa: JSObjectIsa,
    ) -> Result<JSValue, JSObjectIsa> {
        js_trigger_gc(self.rt_mut(), std::mem::size_of::<JSObject>());

        // alloc prop value slots, initializing with default value
        let mut prop_values = Vec::<JSProperty>::new();
        let prop_count = shape.props.len();
        if prop_values
            .try_reserve_exact(std::cmp::max(JS_PROP_INITIAL_SIZE as usize, prop_count))
            .is_err()
        {
            JSShape::free(self.rt_mut(), shape);
            self.JS_ThrowOutOfMemory();
            return Err(isa);
        }

        for _ in 0..prop_count {
            prop_values.push(JSProperty {
                u: JSPropertyUnion {
                    value: JSValue::UNDEFINED,
                },
            })
        }

        // alloc JSObject
        let mut obj = if let Some(p) = unsafe { js_malloc_typed::<JSObject>(self, 1, false) } {
            let mut b = unsafe {
                p.write(JSObject {
                    header: GCObjectHeader::new(1),
                    detailed: {
                        let mut flags = JSObjectFlags::default();
                        flags.set_extensible(true);
                        flags.set_free_mark(false);
                        flags.set_exotic(false);
                        flags.set_fast_array(false);
                        flags.set_constructor(false);
                        flags.set_uncatchable_error(false);
                        flags.set_tmp_mark(false);
                        flags.set_htmldda(false);
                        flags
                    },
                    shape: shape.as_ptr(),
                    prop_values,
                    first_weak_ref: std::ptr::null_mut(),
                    isa,
                });
                Box::from_raw(p.as_ptr())
            };

            b
        } else {
            JSShape::free(self.rt_mut(), shape);
            return Err(isa);
        };

        match obj.class_id() {
            JS_CLASS_OBJECT => {}

            JS_CLASS_BYTECODE_FUNCTION
            | JS_CLASS_GENERATOR_FUNCTION
            | JS_CLASS_ASYNC_FUNCTION
            | JS_CLASS_ASYNC_GENERATOR_FUNCTION => {}

            JS_CLASS_ARRAY => {
                obj.detailed.set_exotic(true);
                obj.detailed.set_fast_array(true);

                // Array: the length property is always at index 0
                let pr0: *mut JSProperty = if shape.as_ptr() == self.array_shape {
                    obj.prop_values.first_mut().unwrap()
                } else {
                    // only used for the fast array, cannot fail
                    obj.add_property(self, JS_ATOM_length, JS_PROP_WRITABLE | JS_PROP_LENGTH)
                        .unwrap()
                };
                unsafe {
                    (*pr0).u.value = JSValue::ZERO;
                }
            }
            JS_CLASS_ARGUMENTS | JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                obj.detailed.set_exotic(true);
                obj.detailed.set_fast_array(true);
            }
            JS_CLASS_DATAVIEW => {}

            class_id @ (JS_CLASS_NUMBER | JS_CLASS_STRING | JS_CLASS_BOOLEAN | JS_CLASS_SYMBOL
            | JS_CLASS_DATE | JS_CLASS_BIG_INT) => {
                if !self.rt().class_array[class_id as usize].exotic.is_null() {
                    obj.detailed.set_exotic(true);
                }
            }

            JS_CLASS_C_FUNCTION => {}

            JS_CLASS_REGEXP => {
                unimplemented!("JS_CLASS_REGEXP");
                // *(*p).u.regexp.pattern = std::ptr::null_mut();
                // (*p).u.regexp.bytecode = std::ptr::null_mut();
                // if !(*(*ctx).rt)
                //     .class_array
                //     .wrapping_add(class_id as usize)
                //     .as_mut()
                //     .unwrap()
                //     .exotic
                //     .is_null()
                // {
                //     (*p). detailed.set_exotic(true);
                // }
            }

            class_id => {
                if !self.rt().class_array[class_id as usize].exotic.is_null() {
                    obj.detailed.set_exotic(true);
                }
            }
        }

        // take ownership of object
        let p = obj.as_mut() as *mut JSObject;
        self.rt_mut().gc_add_object(obj);

        Ok(JS_MKPTR!(JSTag::Object, p.cast()))
    }
}

impl JSRuntime {
    /// register a shape
    pub fn register_shape(&mut self, sh: JSShapeHandle) {
        debug_assert!(sh.is_hashed);

        self.shape_registry
            .entry(sh.hash)
            .and_modify(|lst| {
                debug_assert!(!lst.contains(&sh));
                lst.push(sh);
            })
            .or_insert_with(|| vec![sh]);
    }

    /// unregister a shape
    pub fn unregister_shape(&mut self, sh: JSShapeHandle) {
        debug_assert!(sh.is_hashed);

        let h = sh.hash;

        if let Some(lst) = self.shape_registry.get_mut(&h) {
            lst.retain(|s| !sh.eq(s));

            if lst.is_empty() {
                self.shape_registry.remove(&h);
            }
        }
    }

    /// find a registered empty shape matching given prototype.
    /// Return None if not found.
    /// `proto` is not consumed.
    pub fn find_shape_by_proto(&self, proto: *mut JSObject) -> Option<JSShapeHandle> {
        let h = shape_initial_hash(proto);

        self.shape_registry.get(&h).and_then(|lst| {
            lst.iter()
                .find(|s| {
                    debug_assert_eq!(s.hash, h);
                    s.proto == proto && s.props.is_empty()
                })
                .copied()
        })
    }

    /// find a registered shape matching base_shape + (prop, prop_flags).
    /// Return None if not found
    pub fn find_shape_with_prop(
        &self,
        base_shape: JSShapeHandle,
        atom: JSAtom,
        prop_flags: u32,
    ) -> Option<JSShapeHandle> {
        // calc hash for base_shape + (atom, prop_flags)
        let mut h = base_shape.hash;
        h = shape_hasher_u32(h, atom.0);
        h = shape_hasher_u32(h, prop_flags);

        if let Some(found) = self.shape_registry.get(&h).and_then(|lst| {
            lst.iter()
                .find(|s| {
                    debug_assert_eq!(s.hash, h);
                    s.proto == base_shape.proto && s.props.len() == base_shape.props.len() + 1
                })
                .copied()
        }) {
            if base_shape
                .props
                .iter()
                .zip(found.props.iter())
                .all(|(a, b)| a.0 == b.0 && a.1.atom == b.1.atom && a.1.flags() == b.1.flags())
            {
                // all prop defs are match
                if found
                    .props
                    .last()
                    .map(|(k, pr)| *k == atom && pr.flags() == prop_flags)
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

pub fn shape_initial_hash(proto: *mut JSObject) -> u32 {
    let mut h = shape_hasher_u32(1, proto as u32);
    if std::mem::size_of::<*mut JSObject>() > 4 {
        h = shape_hasher_u32(h, ((proto as u64) >> 32) as u32);
    }
    h
}

// // Truncate the shape hash to 'hash_bits' bits
// pub const fn shape_hash_trim(h: u32, hash_bits: i32) -> u32 {
//     h >> (32 - hash_bits)
// }

impl JSObject {
    /// ensure that the shape can be safely modified.
    pub unsafe fn shape_prepare_update(
        &mut self,
        ctx: *mut JSContext,
        pprs: *mut *mut JSShapeProperty,
    ) -> i32 {
        let mut sh = JSShapeHandle::from(self.shape);

        if sh.is_hashed {
            if sh.ref_count() != 1 {
                let atom: JSAtom = if !pprs.is_null() {
                    (*(*pprs)).atom
                } else {
                    JS_ATOM_NULL
                };

                // clone the shape (the resulting one is no longer hashed)
                match sh.deref_mut().clone(ctx.as_mut().unwrap()) {
                    Ok(mut sh1) => {
                        // free old shape, and use new one
                        JSShape::free((*ctx).rt_mut(), sh);
                        self.shape = sh1.into();
                        if !pprs.is_null() {
                            *pprs = sh1.props.get_mut(&atom).unwrap();
                        }
                    }
                    Err(_) => {
                        return -1;
                    }
                }
            } else {
                (*ctx).rt_mut().unregister_shape(sh);
                sh.is_hashed = false;
            }
        }

        0
    }
}

#[deprecated(note = "use JSObject::shape_prepare_update() instead")]
pub unsafe fn js_shape_prepare_update(
    ctx: *mut JSContext,
    p: *mut JSObject,
    pprs: *mut *mut JSShapeProperty,
) -> i32 {
    (*p).shape_prepare_update(ctx, pprs)
}
