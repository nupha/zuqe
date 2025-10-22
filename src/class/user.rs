use std::{ffi::c_void, ptr::NonNull};

use crate::{
    atom::{Atom, AtomTypeHint},
    class::{ClassDef, ClassInitializeFn, FreeObjFn, JSClassID},
    context::JSContext,
    object::{JSObject, ObjectPayload},
    runtime::JSRuntime,
    value::JSValue,
};

#[derive(Debug)]
pub(crate) struct OtherPayload {
    pub(crate) class_id: JSClassID,
    pub(crate) opaque: *mut c_void,
}

pub struct ClassDefBuilder {
    class_name: &'static str,

    /// initialize class proto, constructor to context
    init: Option<ClassInitializeFn>,
    /// class instance finalize callback
    free: Option<FreeObjFn>,
}

impl ClassDefBuilder {
    pub fn new(class_name: &'static str) -> Self {
        Self {
            class_name,
            init: None,
            free: None,
        }
    }

    pub fn init(mut self, f: ClassInitializeFn) -> Self {
        self.init = Some(f);
        self
    }

    pub fn free(mut self, f: FreeObjFn) -> Self {
        self.free = Some(f);
        self
    }
}

impl JSRuntime {
    /// register a class def. returns the class_id
    pub fn register_class(&mut self, class_def: ClassDefBuilder) -> Result<JSClassID, i32> {
        if class_def.class_name.is_empty() {
            return Err(-1);
        }

        // new class_id
        let class_id = self.class_registry.len() as JSClassID;

        self.add_class_internal(
            class_id,
            &ClassDef {
                class_name: class_def.class_name,
                free_obj: class_def.free,
                initialize: class_def.init,
                finalize: Some(user_obj_finalize),
                ..Default::default()
            },
            Atom::from_str(self, class_def.class_name, AtomTypeHint::String)?,
        )?;

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

impl JSContext {
    pub fn use_classes(&self, class_ids: &[JSClassID]) -> Result<(), i32> {
        if let Some(mx) = class_ids.iter().max().copied() {
            if let Some(cdef) = self.rt().class_registry.get(mx as usize) {
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

                    if !self.0.borrow().class_proto[class_id as usize].is_object() {
                        // init class in ctx
                        if let Some(f) = cdef.initialize {
                            let proto = f(self, class_id)?;
                            self.0.borrow_mut().class_proto[class_id as usize] = proto.into();
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

    pub fn use_class(&self, class_id: JSClassID) -> Result<(), i32> {
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
            self.use_class(id as JSClassID)
        } else {
            self.throw_internal_error(&format!("class '{}' was not registererd", name));
            Err(-1)
        }
    }

    /// create a new class instance
    pub fn new_instance(
        &self,
        class_id: JSClassID,
        ctor: &JSValue,
        opaque: *mut c_void,
    ) -> Result<JSObject, i32> {
        self.create_object_from_ctor(
            ctor,
            ObjectPayload::OTHER(Box::new(OtherPayload { class_id, opaque })),
        )
        .map_err(|_| -1)
    }
}

impl JSObject {
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
