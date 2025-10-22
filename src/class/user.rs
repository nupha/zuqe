use crate::{
    atom::{Atom, AtomTypeHint},
    class::{ClassDef, JSClassID},
    context::JSContext,
    runtime::JSRuntime,
};

impl JSRuntime {
    /// register a class def. returns the class_id
    pub fn register_class(&mut self, class_def: &ClassDef) -> Result<JSClassID, i32> {
        if class_def.class_name.is_empty() {
            return Err(-1);
        }

        // new class_id
        let class_id = self.class_defs.len() as JSClassID;

        self.add_class_internal(
            class_id,
            class_def,
            Atom::from_str(self, class_def.class_name, AtomTypeHint::String)?,
        )?;

        Ok(class_id)
    }
}

impl JSContext {
    pub fn use_class_id(&self, class_id: JSClassID) -> Result<(), i32> {
        if let Some(cd) = self.rt().class_defs.get(class_id as usize) {
            if !self.0.borrow().class_proto[class_id as usize].is_object() {
                // init class proto
                if let Some(f) = cd.initialize {
                    let proto = f(self, class_id)?;
                    self.0.borrow_mut().class_proto[class_id as usize] = proto.into();
                    Ok(())
                } else {
                    self.throw_internal_error("can't initialize native class");
                    Err(-1)
                }
            } else {
                Ok(())
            }
        } else {
            self.throw_internal_error("unknown native class");
            Err(-1)
        }
    }

    pub fn use_class(&self, name: &str) -> Result<(), i32> {
        let atom_name = Atom::from_str(self.rt(), name, AtomTypeHint::String)?;

        if let Some(id) = self
            .rt()
            .class_defs
            .iter()
            .position(|x| !x.class_name.is_null() && x.class_name == atom_name)
        {
            self.use_class_id(id as JSClassID)
        } else {
            self.throw_internal_error(&format!("unknown native class '{}'", name));
            Err(-1)
        }
    }
}
