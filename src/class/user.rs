use std::{cell::Cell, ffi::c_void, ptr::NonNull};

use gc_lite::GcTracable;

use crate::{
    ClassDef, USER_CLASS_ID_START,
    atom::{Atom, AtomKind},
    class::ClassID,
    context::{ContextImpl, ContextPtr},
    object::{Object, ObjectImpl, ObjectPayload},
    runtime::RuntimeImpl,
    value::JSValue,
};

#[derive(Debug)]
pub struct OtherPayload {
    pub(crate) class_id: ClassID,
    pub(crate) opaque: *mut c_void,
}

unsafe impl GcTracable for OtherPayload {
    #[inline(always)]
    fn trace(&self, _: &mut gc_lite::GcTracer) {}
}

impl OtherPayload {
    pub(crate) fn new(class_id: ClassID, opaque: *mut c_void) -> Box<Self> {
        debug_assert!(class_id >= USER_CLASS_ID_START);
        Box::new(Self { class_id, opaque })
    }

    #[inline(always)]
    pub const fn opaque(&self) -> *mut c_void {
        self.opaque
    }

    #[inline(always)]
    pub const unsafe fn replace_opaque(&mut self, opaque: *mut c_void) -> *mut c_void {
        std::mem::replace(&mut self.opaque, opaque)
    }
}

impl RuntimeImpl {
    /// register a class defination. returns the assigned class_id.
    pub fn register_class(&mut self, class_def: &ClassDef) -> Result<ClassID, i32> {
        debug_assert!(!class_def.class_name.is_empty());
        debug_assert!(class_def.finalize.is_some());

        thread_local! {
            static CLASS_ID_SERIAL: Cell<u16> = const { Cell::new(USER_CLASS_ID_START) };
        }
        let class_id = CLASS_ID_SERIAL.with(|ser| {
            let i = ser.get();
            ser.set(i + 1);
            i
        });
        debug_assert!(class_id >= USER_CLASS_ID_START);

        let class_atom = Atom::from_str(self.into(), class_def.class_name, AtomKind::String)?;
        self.add_class_internal(class_id, class_atom, class_def)?;

        Ok(class_id)
    }
}

impl ContextImpl {
    pub fn use_classes(&mut self, class_ids: &[ClassID]) -> Result<(), i32> {
        if let Some(mx) = class_ids.iter().max().copied() {
            if self.rt_ptr().class_registry.get(mx as usize).is_some() {
                let n = self.class_proto.len();
                if mx as usize >= n {
                    // grow slot
                    let extra = mx as usize - n + 1;
                    self.class_proto.try_reserve_exact(extra).map_err(|_| {
                        self.throw_out_of_memory();
                        -1
                    })?;
                    self.class_proto
                        .extend(std::iter::repeat_n(JSValue::NULL, extra));
                }

                for id in class_ids {
                    let class_id = *id;

                    if self.class_proto(class_id).is_none() {
                        // init class in ctx
                        if let Some(f) = self
                            .rt_ptr()
                            .class_registry
                            .get(class_id as usize)
                            .and_then(|c| c.initialize)
                        {
                            let r = f(
                                unsafe { ContextPtr::from_ref_unchecked(self).into() },
                                class_id,
                            )?;
                            if let Some(proto) = r {
                                self.class_proto[class_id as usize] = proto.into();
                            } else if !self.class_proto[class_id as usize].is_object() {
                                self.throw_internal_error(&format!(
                                    "class ({class_id}) initialization must setup proto object"
                                ));
                            }
                        } else {
                            self.throw_internal_error("can't initialize native class");
                            return Err(-1);
                        }
                    }
                }

                Ok(())
            } else {
                self.throw_internal_error(&format!("unknown class id {mx}"));
                Err(-1)
            }
        } else {
            Ok(())
        }
    }

    pub fn use_class(&mut self, class_id: ClassID) -> Result<(), i32> {
        self.use_classes(&[class_id])
    }

    pub fn use_class_name(&mut self, name: &str) -> Result<(), i32> {
        let atom_name = Atom::from_str(self.rt_ptr(), name, AtomKind::String)?;

        if let Some(id) = self
            .rt_ptr()
            .class_registry
            .iter()
            .position(|x| !x.class_name.is_null() && x.class_name == atom_name)
        {
            self.use_class(id as ClassID)
        } else {
            self.throw_internal_error(&format!("class '{}' was not registererd", name));
            Err(-1)
        }
    }

    /// create a new class instance
    pub fn new_instance(
        &self,
        class_id: ClassID,
        ctor: JSValue,
        opaque: *mut c_void,
    ) -> Result<Object, i32> {
        self.create_object_from_ctor(
            ctor,
            ObjectPayload::OTHER(Box::new(OtherPayload { class_id, opaque })),
        )
        .map_err(|_| -1)
    }
}

impl ObjectImpl {
    /// if the object is a user class instance, returns the opaque pointer if available
    pub fn opaque(&self) -> Option<NonNull<c_void>> {
        if let ObjectPayload::OTHER(u) = self.payload() {
            NonNull::new(u.opaque)
        } else {
            None
        }
    }

    /// if the object is a user class instance, returns the opaque pointer reference if available
    pub fn opaque_ref<T>(&self) -> Option<&T> {
        if let ObjectPayload::OTHER(u) = self.payload() {
            unsafe { u.opaque.cast::<T>().as_ref() }
        } else {
            None
        }
    }
}

impl JSValue {
    /// if the value is a user class instance, returns the opaque pointer if available
    pub fn opaque(&self) -> Option<NonNull<c_void>> {
        self.as_object()?.opaque()
    }

    pub fn opaque_ref<T>(&self) -> Option<&T> {
        self.as_object()?.opaque_ref::<T>()
    }
}
