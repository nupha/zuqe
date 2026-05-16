// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    Exception,
    context::ScopedContext,
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

impl<'c> ScopedContext<'c> {
    /// Returns `Err` if abort running with exception set
    #[inline(never)]
    pub(crate) fn poll_interrupts(&mut self) -> Result<(), i32> {
        if let Some(cb) = self.rt().interrupt_handler.as_ref() {
            let mut counter = self.interrupt_counter.get();
            counter -= 1;
            self.interrupt_counter.set(if counter > 0 {
                counter
            } else {
                JS_INTERRUPT_COUNTER_INIT
            });

            if counter <= 0 {
                // call interrupt callback
                if cb(self.rt()) {
                    self.rt_mut()
                        .set_exception(Exception::Interrupted(130.into()));
                    return Err(-1);
                }
            }
        }

        Ok(())
    }
}
