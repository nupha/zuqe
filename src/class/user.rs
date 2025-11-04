use std::{ffi::c_void, ptr::NonNull};

use crate::{
    USER_CLASS_ID_START,
    atom::{Atom, AtomTypeHint},
    class::{ClassDef, ClassID},
    context::JSContext,
    object::{JSObject, ObjectPayload},
    runtime::JSRuntime,
    value::JSValue,
};

#[derive(Debug)]
pub(crate) struct OtherPayload {
    pub(crate) class_id: ClassID,
    pub(crate) opaque: *mut c_void,
}

impl<'a> JSRuntime<'a> {
    /// register a user class defination. returns the registry class_id for it.
    pub fn register_class(&mut self, mut class_def: ClassDef) -> Result<ClassID, i32> {
        if class_def.class_name.is_empty() {
            println!("invalid class name");
            return Err(-1);
        }

        if class_def.free_obj.is_some() {
            if class_def.finalize.is_some() {
                println!("set either 'free' or 'finalize'");
                return Err(-1);
            } else {
                class_def.finalize.replace(user_obj_finalize);
            }
        }

        let class_atom = Atom::from_str(self, class_def.class_name, AtomTypeHint::String)?;

        let class_id = self.class_registry.len() as ClassID; // new class_id
        debug_assert!(class_id >= USER_CLASS_ID_START);

        self.add_class_internal(class_id, &class_def, class_atom)?;
        Ok(class_id)
    }
}

fn user_obj_finalize(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {
    if let ObjectPayload::OTHER(u) = isa {
        if let Some(f) = rt.class_registry[u.class_id as usize].free_obj {
            f(rt, u.opaque, id)
        }
    } else {
        unreachable!();
    }
}

impl<'a> JSContext<'a> {
    pub fn use_classes(&self, class_ids: &[ClassID]) -> Result<(), i32> {
        if let Some(mx) = class_ids.iter().max().copied() {
            if self.rt().class_registry.get(mx as usize).is_some() {
                let n = self.0.borrow().class_proto.len();
                if mx as usize >= n {
                    // grow slot
                    let extra = mx as usize - n + 1;
                    let slot = &mut self.0.borrow_mut().class_proto;
                    slot.try_reserve_exact(extra).map_err(|_| {
                        self.throw_out_of_memory();
                        -1
                    })?;
                    slot.extend(std::iter::repeat_n(JSValue::NULL, extra));
                }

                for id in class_ids {
                    let class_id = *id;

                    if self.class_proto(class_id).is_none() {
                        // init class in ctx
                        let cdef = &self.rt().class_registry[class_id as usize];
                        if let Some(f) = cdef.initialize {
                            if let Some(proto) = f(self, class_id)? {
                                self.0.borrow_mut().class_proto[class_id as usize] = proto.into();
                            } else if !self.0.borrow().class_proto[class_id as usize].is_object() {
                                self.throw_internal_error(&format!(
                                    "class ({}) initialization must setup proto object",
                                    class_id
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
                self.throw_internal_error("unknown native class");
                Err(-1)
            }
        } else {
            Ok(())
        }
    }

    pub fn use_class(&self, class_id: ClassID) -> Result<(), i32> {
        self.use_classes(&[class_id])
    }

    pub fn use_class_name(&self, name: &str) -> Result<(), i32> {
        let atom_name = Atom::from_str(self.rt(), name, AtomTypeHint::String)?;

        if let Some(id) = self
            .rt()
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
        ctor: &JSValue<'a>,
        opaque: *mut c_void,
    ) -> Result<JSObject<'a>, i32> {
        self.create_object_from_ctor(
            ctor,
            ObjectPayload::OTHER(Box::new(OtherPayload { class_id, opaque })),
        )
        .map_err(|_| -1)
    }
}

impl<'a> JSObject<'a> {
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

impl<'a> JSValue<'a> {
    /// if the value is a user class instance, returns the opaque pointer if available
    pub fn opaque(&self) -> Option<NonNull<c_void>> {
        self.as_object()?.opaque()
    }

    pub fn opaque_ref<T>(&self) -> Option<&T> {
        self.as_object()?.opaque_ref::<T>()
    }
}
