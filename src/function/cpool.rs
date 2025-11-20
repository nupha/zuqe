use crate::value::JSValue;

use super::JSFunctionDef;

impl JSFunctionDef {
    /// returns index of `val` in constant pool index.
    /// Note: `val` is consumed
    pub fn cpool_add(&mut self, val: JSValue) -> Result<u16, i32> {
        if self.cpool.len() == self.cpool.capacity() {
            self.cpool.try_reserve_exact(8).map_err(|_| {
                self.ctx.throw_out_of_memory();
                -1
            })?;
        }

        self.cpool.push(val);
        Ok(self.cpool.len() as u16 - 1) // returns new const idx
    }
}
