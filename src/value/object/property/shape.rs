use crate::{
    atom::{JS_FreeAtom, JSAtom},
    context::JSContext,
    value::{JSValue, object::JSObject},
};

impl JSObject {
    /// Check if shape property `atom` exists.
    /// `prop` is not consumed.
    #[inline]
    pub fn has_shape_property(&self, prop: JSAtom) -> bool {
        self.shape()
            .is_some_and(|sh| sh.0.borrow().prop_defs.get(&prop.0).is_some())
    }

    pub fn has_shape_property_str(&self, ctx: &JSContext, prop: &str) -> Result<bool, i32> {
        let atom = ctx.create_atom(prop)?;
        let b = self.has_shape_property(atom);
        JS_FreeAtom(ctx, atom);
        Ok(b)
    }

    /// Find shape property by prop atom.
    /// Returns shape property index and flags if the prop exists.
    /// `prop` is not consumed.
    #[inline]
    pub fn find_shape_property(&self, prop: JSAtom) -> Option<(usize, u32)> {
        self.shape().and_then(|sh| {
            sh.0.borrow()
                .prop_defs
                .get_full(&prop.0)
                .map(|(i, _, pr)| (i, pr.flags()))
        })
    }
}

impl JSValue {
    pub fn has_shape_property(&self, ctx: &JSContext, prop: JSAtom) -> Result<bool, i32> {
        if let JSValue::Object(p) = self {
            Ok(p.has_shape_property(prop))
        } else {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        }
    }

    pub fn has_shape_property_str(&self, ctx: &JSContext, prop: &str) -> Result<bool, i32> {
        if let JSValue::Object(p) = self {
            p.has_shape_property_str(ctx, prop)
        } else {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        }
    }
}
