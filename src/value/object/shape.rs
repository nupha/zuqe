use std::{mem::ManuallyDrop, ptr::NonNull};

use indexmap::IndexMap;

use crate::{
    atom::{consts::JS_ATOM_length, JSAtom, JS_FreeAtomRT, JS_ATOM_NULL},
    class::JSClassID,
    context::JSContext,
    error::JSResult,
    gc::{add_gc_object, js_trigger_gc, remove_gc_object, JSGCObjectHeader, JSGCObjectTypeEnum},
    mem::{js_free_rt, js_malloc_typed, js_mallocz_rt},
    runtime::JSRuntime,
    types::list_find,
    value::{
        object::{JSObjectFlags, JSObjectUnion},
        JSTag, JSValue, JS_FreeValueRT,
    },
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT, JS_CLASS_BOOLEAN, JS_CLASS_C_FUNCTION,
    JS_CLASS_DATAVIEW, JS_CLASS_DATE, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_NUMBER, JS_CLASS_OBJECT,
    JS_CLASS_REGEXP, JS_CLASS_STRING, JS_CLASS_SYMBOL, JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    obj::{JSObject, JS_PROP_INITIAL_HASH_SIZE, JS_PROP_INITIAL_SIZE},
    property::{JSProperty, JSPropertyUnion, JS_PROP_LENGTH, JS_PROP_WRITABLE},
    JSShapeHandle,
};

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

    pub fn set_flags(&mut self, f: u32) {
        self.bitfield = (self.bitfield & !Self::FLAGS_MASK) | ((f & 0x3f) << 26);
    }

    // pub fn hash_next(&self) -> u32 {
    //     self.bitfield & Self::NEXT_HASH_MASK
    // }

    // pub fn set_hash_next(&mut self, h: u32) {
    //     self.bitfield = (self.bitfield & !Self::NEXT_HASH_MASK) | (h & Self::NEXT_HASH_MASK);
    // }
}

#[repr(C)]
pub struct JSShape {
    /// hash table of size hash_mask + 1 before the start of the structure (see prop_hash_end()).
    pub header: JSGCObjectHeader,

    /// true if the shape is inserted in the shape hash table. If not, JSShape.hash is not valid
    pub is_hashed: bool,

    /// If true, the shape may have small array index properties 'n' with 0..=u32::MAX;
    /// If false, the shape is guaranteed not to have small array index properties
    pub has_small_array_index: bool,

    /// current hash value
    pub hash: u32,
    /// in JSRuntime.shape_hash[h] list
    pub shape_hash_next: *mut JSShape,

    pub proto: *mut JSObject,

    /// shape property definations
    pub props: IndexMap<JSAtom, JSShapeProperty>,
}

#[cfg(debug_assertions)]
impl Drop for JSShape {
    fn drop(&mut self) {
        debug_assert_eq!(self.header.ref_count, 0);
        debug_assert!(!self.is_hashed);
        debug_assert!(self.proto.is_null());
        debug_assert!(self.props.is_empty());
    }
}

impl JSShape {
    /// increase shape ref_count
    #[inline(always)]
    pub fn dup(&mut self) -> &mut Self {
        self.header.ref_count += 1;
        self
    }

    /// Append property defination to shape.
    /// `obj` is an JSObject that uses the shape,
    /// if provided, should ensure this object has enough property value slots capacity.
    ///
    /// Note: `atom`, `obj` are not consumed.
    pub fn add_shape_property(
        &mut self,
        ctx: &mut JSContext,
        mut obj: Option<&mut JSObject>,
        atom: JSAtom,
        prop_flags: u32,
    ) -> Result<(), i32> {
        debug_assert_eq!(self.header.ref_count, 1);

        let new_shape_hash = if self.is_hashed {
            // Update shape hash
            unsafe {
                js_shape_hash_unlink(ctx.rt_mut(), self);
            }
            shape_hasher(shape_hasher(self.hash, atom.0), prop_flags)
        } else {
            0
        };

        if let Some(o) = obj.as_deref_mut() {
            // ensure object's prop values slot capacity
            if o.grow_prop_value_slots(self.props.len() + 1).is_err() {
                if self.is_hashed {
                    unsafe {
                        js_shape_hash_link(ctx.rt_mut(), self);
                    }
                }
                ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        if self.is_hashed {
            self.hash = new_shape_hash;
            unsafe {
                js_shape_hash_link(ctx.rt_mut(), self);
            }
        }

        let mut pr = JSShapeProperty {
            bitfield: 0,
            atom: atom.dup(ctx.rt_mut()),
        };
        pr.set_flags(prop_flags);
        self.props.insert(atom, pr);

        self.has_small_array_index |= atom.is_int_type();

        #[cfg(debug_assertions)]
        if let Some(o) = obj.as_deref() {
            debug_assert_eq!(self.props.len(), o.prop_values.len())
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
            header: JSGCObjectHeader::new(1),
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
        let sh = self.clone_to_new(ctx)?;

        // take ownership of JSShape
        let raw_sh = Box::into_raw(sh);

        unsafe {
            debug_assert_eq!(
                raw_sh as usize,
                std::ptr::addr_of!((*raw_sh).header) as usize
            );
            add_gc_object(
                (*ctx).rt_mut(),
                &mut (*raw_sh).header,
                JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_SHAPE,
            );
        }

        Ok(raw_sh.into())
    }

    /// drop Box<Self>
    pub fn dispose(mut self: Box<Self>, rt: &mut JSRuntime) {
        debug_assert_eq!(self.header.ref_count, 0);

        if self.is_hashed {
            unsafe {
                js_shape_hash_unlink(rt, self.as_mut());
            }
            self.is_hashed = false;
        }

        if !self.proto.is_null() {
            unsafe {
                JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, self.proto.cast()));
            }
            self.proto = std::ptr::null_mut();
        }

        // clear prop definations and free key atoms
        for (_, prs) in self.props.drain(..) {
            JS_FreeAtomRT(rt, prs.atom);
        }

        if !self.header.link.next.is_null() && !self.header.link.prev.is_null() {
            unsafe {
                remove_gc_object(&mut self.header);
            }
        }

        drop(self);
    }
}

impl JSContext {
    /// create a new empty shape with prototype 'proto'.
    /// Note: `proto` is not consumed.
    #[inline(never)]
    pub fn create_shape2(
        &mut self,
        proto: Option<&mut JSObject>,
        hash_size: usize,
        prop_size: i32,
    ) -> Result<JSShapeHandle, i32> {
        let rt = self.rt_mut();

        /* resize the shape hash table if necessary */
        if 2 * (rt.shape_hash_count + 1) > rt.shape_hash_size {
            unsafe {
                resize_shape_hash(rt, rt.shape_hash_bits + 1);
            }
        }

        // alloc for prop defination
        let mut props = IndexMap::<JSAtom, JSShapeProperty>::new();
        if props.try_reserve_exact(prop_size as _).is_err() {
            self.JS_ThrowOutOfMemory();
            return Err(-1);
        }

        let proto_ptr = proto.map_or(std::ptr::null_mut(), |p| p as *mut JSObject);

        if !proto_ptr.is_null() {
            JS_MKPTR!(JSTag::Object, proto_ptr.cast()).dup();
        }

        let mut shape = Box::new(JSShape {
            header: JSGCObjectHeader::new(1),
            is_hashed: false,
            has_small_array_index: false,
            hash: 0,
            shape_hash_next: std::ptr::null_mut(),
            proto: proto_ptr,
            props,
        });

        debug_assert_eq!(
            shape.as_ref() as *const JSShape as usize,
            std::ptr::addr_of!((*shape).header) as usize
        );

        unsafe {
            add_gc_object(
                rt,
                &mut shape.header,
                JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_SHAPE,
            );
        }

        // /* resize the shape hash table if necessary */
        // if 2 * (rt.shape_hash_count + 1) > rt.shape_hash_size {
        //     resize_shape_hash(rt, rt.shape_hash_bits + 1);
        // }

        // let sh_alloc = js_malloc(ctx, get_shape_size(hash_size as usize, prop_size as usize));
        // if sh_alloc.is_null() {
        //     return std::ptr::null_mut();
        // }
        // let sh = get_shape_from_alloc(sh_alloc.cast(), hash_size)
        //     .as_mut()
        //     .unwrap();

        // sh.header.ref_count = 1;

        // add_gc_object(rt, &mut sh.header, JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_SHAPE);
        // if !proto.is_null() {
        //     JS_DupValue(ctx, JS_MKPTR!(JS_TAG_OBJECT, proto.cast()));
        // }
        // sh.proto = proto;

        // std::ptr::write_bytes(
        //     prop_hash_end(sh).sub(hash_size),
        //     0,
        //     hash_size, // * std::mem::size_of::<u32>(),
        // );

        // sh.prop_hash_mask = hash_size as u32 - 1;
        // sh.prop_size = prop_size;
        // sh.prop_count = 0;
        // sh.deleted_prop_count = 0;

        // insert in the hash table
        shape.hash = shape_initial_hash(proto_ptr);
        shape.is_hashed = true;

        let res = unsafe {
            let mut sh_ptr = NonNull::new_unchecked(Box::into_raw(shape));
            js_shape_hash_link(rt, sh_ptr.as_ptr());

            // ensure new shape if in gc_obj_list
            debug_assert!(list_find(
                &mut sh_ptr.as_mut().header.link,
                &mut rt.gc_obj_list
            ));

            sh_ptr
        };

        Ok(res.as_ptr().into())
    }

    /// create a new empty shape with prototype 'proto'.
    /// Note: `proto` is not consumed.
    #[inline(always)]
    pub fn create_shape(&mut self, proto: Option<&mut JSObject>) -> Result<JSShapeHandle, i32> {
        self.create_shape2(proto, JS_PROP_INITIAL_HASH_SIZE, JS_PROP_INITIAL_SIZE as _)
    }

    /// create object based on specified shape and class_id.
    /// Note: `shape` is consumed.
    pub fn create_object_from_shape(
        &mut self,
        shape: JSShapeHandle,
        class_id: JSClassID,
    ) -> JSResult {
        debug_assert!(class_id > 0);

        js_trigger_gc(self.rt_mut(), std::mem::size_of::<JSObject>());

        // alloc prop value slots, initializing with default value
        let mut prop_values = Vec::<JSProperty>::new();
        let prop_count = shape.props.len();
        if prop_values
            .try_reserve_exact(std::cmp::max(JS_PROP_INITIAL_SIZE as usize, prop_count))
            .is_err()
        {
            unsafe {
                js_free_shape(self.rt_mut(), shape.into());
            }
            self.JS_ThrowOutOfMemory();
            return Err(-1);
        }

        prop_values.extend(std::iter::repeat_n(
            JSProperty {
                u: JSPropertyUnion {
                    value: JSValue::UNDEFINED,
                },
            },
            prop_count,
        ));

        // alloc JSObject
        let mut obj = if let Some(p) = unsafe { js_malloc_typed::<JSObject>(self, 1, false) } {
            let o = JSObject {
                header: JSGCObjectHeader::new(1),
                detailed: {
                    let mut flags = JSObjectFlags::default();
                    flags.set_class_id(class_id);
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
                u: JSObjectUnion {
                    opaque: std::ptr::null_mut(),
                },
            };

            unsafe {
                p.write(o);
                Box::from_raw(p.as_ptr())
            }
        } else {
            unsafe {
                js_free_shape(self.rt_mut(), shape.into());
            }
            return Err(-1);
        };

        match class_id {
            JS_CLASS_OBJECT => {}
            JS_CLASS_ARRAY => {
                obj.detailed.set_exotic(true);
                obj.detailed.set_fast_array(true);
                unsafe {
                    (*obj.u.array).data.values = ManuallyDrop::new(Vec::new());
                    (*obj.u.array).u1.size = 0;
                }

                // Array: the length property is always the first one
                let pr0: *mut JSProperty = if shape.as_ptr() == self.array_shape {
                    obj.prop_values.first_mut().unwrap()
                } else {
                    // only used for the first array, cannot fail
                    obj.add_property(self, JS_ATOM_length, JS_PROP_WRITABLE | JS_PROP_LENGTH)
                        .unwrap()
                };
                unsafe {
                    (*pr0).u.value = JSValue::ZERO;
                }
            }
            JS_CLASS_ARGUMENTS => {
                obj.detailed.set_exotic(true);
                obj.detailed.set_fast_array(true);
                unsafe {
                    (*obj.u.array).data.values = ManuallyDrop::new(Vec::new());
                }
            }
            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                obj.detailed.set_exotic(true);
                obj.detailed.set_fast_array(true);
                unsafe {
                    (*obj.u.array).data.ptr = std::ptr::null_mut();
                    (*obj.u.array).typed_array_count = 0;
                }
            }
            JS_CLASS_DATAVIEW => unsafe {
                (*obj.u.array).data.ptr = std::ptr::null_mut();
                (*obj.u.array).typed_array_count = 0;
            },

            JS_CLASS_NUMBER | JS_CLASS_STRING | JS_CLASS_BOOLEAN | JS_CLASS_SYMBOL
            | JS_CLASS_DATE | JS_CLASS_BIG_INT => {
                obj.u.object_data = JSValue::UNDEFINED;
                if !self.rt().class_array[class_id as usize].exotic.is_null() {
                    obj.detailed.set_exotic(true);
                }
            }

            JS_CLASS_C_FUNCTION => {
                // FIXME: prop_values is not initialized, panic!
                // obj.prop_values[0].u.value = JSValue::UNDEFINED;
            }

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

            _ => {
                if !self.rt().class_array[class_id as usize].exotic.is_null() {
                    obj.detailed.set_exotic(true);
                }
            }
        }

        // take ownership of object
        let p = Box::into_raw(obj);

        debug_assert_eq!(p as usize, unsafe {
            std::ptr::addr_of!((*p).header) as usize
        });

        unsafe {
            add_gc_object(
                self.rt_mut(),
                &mut (*p).header,
                JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT,
            );
        }

        Ok(JS_MKPTR!(JSTag::Object, p.cast()))
    }
}

#[deprecated(note = "use JSShape::dispose() instead")]
unsafe fn js_free_shape0(rt: *mut JSRuntime, sh: *mut JSShape) {
    debug_assert_eq!((*sh).header.ref_count, 0);
    Box::from_raw(sh).dispose(rt.as_mut().unwrap());

    // let sh = sh.as_mut().unwrap();

    // if sh.is_hashed {
    //     js_shape_hash_unlink(rt.as_mut().unwrap(), sh);
    //     sh.is_hashed = false;
    // }

    // if !sh.proto.is_null() {
    //     JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, sh.proto.cast()));
    //     sh.proto = std::ptr::null_mut();
    // }

    // // clear prop definations and free key atoms
    // for (_, prs) in sh.props.drain(..) {
    //     JS_FreeAtomRT(rt, prs.atom);
    // }

    // if !sh.header.link.next.is_null() && !sh.header.link.prev.is_null() {
    //     remove_gc_object(&mut sh.header);
    // }

    // // take ownershape of *JSShape and drop
    // drop(Box::from_raw(sh));
}

pub unsafe fn js_free_shape(rt: *mut JSRuntime, sh: *mut JSShape) {
    debug_assert!((*sh).header.ref_count > 0);

    (*sh).header.ref_count -= 1;
    if (*sh).header.ref_count == 0 {
        js_free_shape0(rt, sh);
    }
}

pub unsafe fn js_free_shape_null(rt: *mut JSRuntime, sh: *mut JSShape) {
    if !sh.is_null() {
        js_free_shape(rt, sh.as_mut().unwrap());
    }
}

pub unsafe fn init_shape_hash(rt: &mut JSRuntime) -> i32 {
    rt.shape_hash_bits = 4; // 16 shapes
    rt.shape_hash_size = 1 << rt.shape_hash_bits;
    rt.shape_hash_count = 0;
    rt.shape_hash = js_mallocz_rt(
        rt,
        std::mem::size_of::<*mut JSShape>() * rt.shape_hash_size as usize,
    )
    .cast();
    if rt.shape_hash.is_null() {
        return -1;
    }
    0
}

/// calc hash for u32
pub const fn shape_hasher(h: u32, val: u32) -> u32 {
    // Same magic hash multiplier as the Linux kernel
    h.wrapping_add(val).wrapping_mul(0x9e3779b9)
}

// Truncate the shape hash to 'hash_bits' bits
pub const fn shape_hash_trim(h: u32, hash_bits: i32) -> u32 {
    h >> (32 - hash_bits)
}

pub fn shape_initial_hash(proto: *mut JSObject) -> u32 {
    let mut h = shape_hasher(1, proto as u32);
    if std::mem::size_of::<*mut JSObject>() > 4 {
        h = shape_hasher(h, ((proto as u64) >> 32) as u32);
    }
    h
}

pub unsafe fn resize_shape_hash(rt: &mut JSRuntime, new_shape_hash_bits: i32) -> i32 {
    let new_shape_hash_size = 1 << new_shape_hash_bits;
    let new_shape_hash = js_mallocz_rt(
        rt,
        std::mem::size_of::<*mut JSShape>() * new_shape_hash_size,
    )
    .cast::<*mut JSShape>();
    if new_shape_hash.is_null() {
        return -1;
    }

    for i in 0..rt.shape_hash_size {
        let mut sh: *mut JSShape = *rt.shape_hash.offset(i as isize);
        while !sh.is_null() {
            let sh_next = (*sh).shape_hash_next;
            let h = shape_hash_trim((*sh).hash, new_shape_hash_bits);
            (*sh).shape_hash_next = *new_shape_hash.add(h as usize);
            *new_shape_hash.add(h as usize) = sh;

            sh = sh_next;
        }
    }

    js_free_rt(rt, rt.shape_hash.cast());

    rt.shape_hash_bits = new_shape_hash_bits;
    rt.shape_hash_size = new_shape_hash_size as i32;
    rt.shape_hash = new_shape_hash;

    0
}

pub unsafe fn js_shape_hash_link(rt: &mut JSRuntime, sh: *mut JSShape) {
    let h = shape_hash_trim((*sh).hash, rt.shape_hash_bits);
    (*sh).shape_hash_next = *rt.shape_hash.offset(h as isize);
    *rt.shape_hash.offset(h as isize) = sh;
    rt.shape_hash_count += 1;
}

pub unsafe fn js_shape_hash_unlink(rt: &mut JSRuntime, sh: *mut JSShape) {
    let h = shape_hash_trim((*sh).hash, rt.shape_hash_bits);
    let psh = rt.shape_hash.offset(h as isize);
    while !(*psh).is_null() && *psh != sh {
        *psh = (**psh).shape_hash_next;
    }
    if !(*psh).is_null() {
        *psh = (*sh).shape_hash_next;
        rt.shape_hash_count -= 1;
    }
}

// find a hashed empty shape matching the prototype. Return None if not found
pub unsafe fn find_hashed_shape_proto(
    rt: &mut JSRuntime,
    proto: *mut JSObject,
) -> Option<*mut JSShape> {
    let h = shape_initial_hash(proto);
    let h1 = shape_hash_trim(h, rt.shape_hash_bits);
    let mut sh1: *mut JSShape = *rt.shape_hash.offset(h1 as _);
    while !sh1.is_null() {
        if (*sh1).hash == h && (*sh1).proto == proto && (*sh1).props.is_empty() {
            return Some(sh1);
        }
        sh1 = (*sh1).shape_hash_next;
    }
    None
}

// find a hashed shape matching sh + (prop, prop_flags). Return None if not found
pub unsafe fn find_hashed_shape_prop(
    rt: &mut JSRuntime,
    sh: &mut JSShape,
    atom: JSAtom,
    prop_flags: u32,
) -> Option<*mut JSShape> {
    let mut h = sh.hash;
    h = shape_hasher(h, atom.0);
    h = shape_hasher(h, prop_flags);
    let h1 = shape_hash_trim(h, rt.shape_hash_bits);
    let mut sh1 = *rt.shape_hash.offset(h1 as _);
    while !sh1.is_null() {
        if (*sh1).hash == h
            && (*sh1).proto == sh.proto
            && (*sh1).props.len() == (sh.props.len() + 1)
        {
            if sh
                .props
                .iter()
                .zip((*sh1).props.iter())
                .any(|(a, b)| a.0 != b.0 || a.1.atom != b.1.atom || a.1.flags() != b.1.flags())
            {
                continue; // not same
            }

            if (*sh1)
                .props
                .last()
                .map(|(k, pr)| *k != atom || pr.flags() != prop_flags)
                .unwrap()
            {
                // last prop not match (atom, prop_flags)
                continue;
            }

            return Some(sh1);
        }

        sh1 = (*sh1).shape_hash_next;
    }

    None
}

impl JSObject {
    /// ensure that the shape can be safely modified
    pub unsafe fn js_shape_prepare_update(
        &mut self,
        ctx: *mut JSContext,
        pprs: *mut *mut JSShapeProperty,
    ) -> i32 {
        let sh = self.shape.as_mut().unwrap();

        if sh.is_hashed {
            if sh.header.ref_count != 1 {
                let atom: JSAtom = if !pprs.is_null() {
                    (*(*pprs)).atom
                } else {
                    JS_ATOM_NULL
                };

                // clone the shape (the resulting one is no longer hashed)
                match sh.clone(ctx.as_mut().unwrap()) {
                    Ok(mut sh1) => {
                        // free old shape, and use new one
                        js_free_shape((*ctx).rt_mut(), sh);
                        self.shape = sh1.into();
                        if !pprs.is_null() {
                            *pprs = sh1.props.get_mut(&atom).unwrap();
                        }
                    }
                    Err(e) => {
                        return -1;
                    }
                }
            } else {
                js_shape_hash_unlink((*ctx).rt_mut(), sh);
                sh.is_hashed = false;
            }
        }

        0
    }
}
