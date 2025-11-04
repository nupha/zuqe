use crate::{context::JSContext, error::JS_SetUncatchableError};

/* must be large enough to have a negligible runtime cost and small
enough to call the interrupt callback often. */
pub const JS_INTERRUPT_COUNTER_INIT: i32 = 10000;

impl<'a> JSContext<'a> {
    #[inline(never)]
    pub fn poll_interrupts(&self) -> Result<(), i32> {
        let intr = self.with_mut(|ctx| {
            ctx.interrupt_counter -= 1;
            let b = ctx.interrupt_counter <= 0;
            ctx.interrupt_counter = JS_INTERRUPT_COUNTER_INIT;
            b
        });

        if intr {
            let rt = self.rt();

            if let Some(cb) = rt.interrupt_handler
                && cb(rt, rt.interrupt_opaque) != 0
            {
                // XXX: should set a specific flag to avoid catching
                self.throw_internal_error("interrupted");
                JS_SetUncatchableError(&rt.cur_exception, true);
                return Err(-1);
            }
        }

        Ok(())
    }
}

// #[inline(never)]
// pub fn __js_poll_interrupts(ctx: &JSContext) -> i32 {
//     let rt = ctx.rt_mut();
//     ctx.0.borrow_mut().interrupt_counter = JS_INTERRUPT_COUNTER_INIT;

//     if let Some(cb) = rt.interrupt_handler {
//         if cb(rt, rt.interrupt_opaque) != 0 {
//             // XXX: should set a specific flag to avoid catching
//             ctx.JS_ThrowInternalError("interrupted");
//             JS_SetUncatchableError(&rt.current_exception, true);
//             return -1;
//         }
//     }

//     return 0;
// }

// #[inline(always)]
// pub fn js_poll_interrupts(ctx: &JSContext) -> i32 {
//     let icnt = ctx.with_mut(|ctx| {
//         ctx.interrupt_counter -= 1;
//         ctx.interrupt_counter
//     });

//     if icnt <= 0 {
//         __js_poll_interrupts(ctx)
//     } else {
//         0
//     }
// }
