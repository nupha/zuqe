use crate::{context::JSContext, parse::JSParseState, value::JSValue};

use super::JSFunctionDef;

impl JSFunctionDef {
    /// returns index of `val` in constant pool index.
    /// Note: `val` is consumed
    pub fn cpool_add(&mut self, ctx: &mut JSContext, val: JSValue) -> Result<u16, i32> {
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

// return the constant pool index. 'val' is not duplicated.
#[deprecated(note = "use JSParseState::cpool_add() instead")]
pub unsafe fn cpool_add(s: *mut JSParseState, ctx: &mut JSContext, val: JSValue) -> i32 {
    let fd = (*s).cur_func;
    (*fd).cpool_add(ctx, val).map_or(-1, |x| x as i32)

    // if js_resize_array(
    //     (*s).ctx.as_ptr(),
    //     std::ptr::addr_of_mut!((*fd).cpool).cast(),
    //     std::mem::size_of::<JSValue>() as _,
    //     &mut (*fd).cpool_size,
    //     (*fd).cpool_count + 1,
    // ) != 0
    // {
    //     return -1;
    // }
    // *((*fd).cpool.offset((*fd).cpool_count as isize)) = val;
    // (*fd).cpool_count += 1;
    // (*fd).cpool_count - 1
}
