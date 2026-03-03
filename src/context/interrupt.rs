// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    context::ContextImpl,
    error::JS_SetUncatchableError,
    runtime::{InterruptFn, RuntimeImpl},
};

// must be large enough to have a negligible runtime cost and small
// enough to call the interrupt callback often.
const JS_INTERRUPT_COUNTER_INIT: i32 = 10000;

impl RuntimeImpl {
    pub fn set_interrupt_handler<F: InterruptFn + 'static>(&mut self, f: F) {
        self.interrupt_handler
            .replace(Box::new(f) as Box<dyn InterruptFn>);
    }

    pub fn unset_interrupt_handler(&mut self) {
        self.interrupt_handler = None;
    }
}

impl ContextImpl {
    #[inline(never)]
    pub(crate) fn poll_interrupts(&mut self) -> Result<(), i32> {
        let rt = self.rt_ptr();

        if let Some(cb) = rt.interrupt_handler.as_ref() {
            let mut counter = self.interrupt_counter.get();
            counter -= 1;
            self.interrupt_counter.set(if counter > 0 {
                counter
            } else {
                JS_INTERRUPT_COUNTER_INIT
            });

            if counter <= 0 {
                // call interrupt callback
                if cb(&rt) {
                    // XXX: should set a specific flag to avoid catching
                    self.throw_internal_error("interrupted");
                    JS_SetUncatchableError(rt.exception(), true);
                    return Err(-1);
                };
            }
        }

        Ok(())
    }
}
