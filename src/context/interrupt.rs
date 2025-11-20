use crate::{context::ContextImpl, error::JS_SetUncatchableError};

/* must be large enough to have a negligible runtime cost and small
enough to call the interrupt callback often. */
pub const JS_INTERRUPT_COUNTER_INIT: i32 = 10000;

impl ContextImpl {
    #[inline(never)]
    pub fn poll_interrupts(&self) -> Result<(), i32> {
        let mut intr = self.interrupt_counter.get();
        intr -= 1;
        self.interrupt_counter.set(if intr > 0 {
            intr
        } else {
            JS_INTERRUPT_COUNTER_INIT
        });

        if intr <= 0 {
            let rt = self.rt();

            if let Some(cb) = rt.interrupt_handler
                && cb(&rt, rt.interrupt_opaque) != 0
            {
                // XXX: should set a specific flag to avoid catching
                self.throw_internal_error("interrupted");
                JS_SetUncatchableError(rt.exception(), true);
                return Err(-1);
            }
        }

        Ok(())
    }
}
