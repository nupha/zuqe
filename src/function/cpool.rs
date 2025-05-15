use crate::{context::JSContextHandle, value::JSValue};

use super::JSFunctionDef;

impl JSFunctionDef {
    /// returns index of `val` in constant pool index.
    /// Note: `val` is consumed
    pub fn cpool_add(&mut self, mut ctx: JSContextHandle, val: JSValue) -> Result<u16, i32> {
        if self.cpool.len() == self.cpool.capacity() {
            if self.cpool.try_reserve_exact(4).is_err() {
                ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        self.cpool.push(val);
        Ok(self.cpool.len() as u16 - 1) // returns new const idx
    }
}
