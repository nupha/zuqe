// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::process::ExitCode;

use crate::{
    config::JS_INTERRUPT_COUNTER_INIT,
    context::ScopedContext,
    runtime::{InterruptFn, RuntimeImpl},
};

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
    /// Poll interrupt handler callback and return true if an interrupt was triggered.
    /// The bytecode executor should stop execution when this returns true.
    /// This is a control-flow signal, NOT related to exceptions.
    ///
    /// Called with the interpreter's back edges, loop helpers and function
    /// entry. The fast path is a single counter decrement: the handler is
    /// invoked once per `JS_INTERRUPT_COUNTER_INIT` polls, so a tight loop pays
    /// a load + store per iteration.
    #[inline(always)]
    pub(crate) fn poll_interrupts(&mut self) -> bool {
        // No handler installed: the counter is not maintained, so this is one
        // pointer load.
        if self.rt().interrupt_handler.is_none() {
            return false;
        }

        let counter = self.interrupt_counter.get() - 1;
        if counter > 0 {
            self.interrupt_counter.set(counter);
            return false;
        }

        // Re-arm before the callback: a callback that does not interrupt must
        // not fire on every subsequent poll.
        self.interrupt_counter.set(JS_INTERRUPT_COUNTER_INIT);
        self.poll_interrupts_cb()
    }

    /// Call interrupt handler callback.
    /// Returns `true` if the callback triggered an interrupt.
    #[cold]
    #[inline(never)]
    fn poll_interrupts_cb(&mut self) -> bool {
        let cb = unsafe { self.rt().interrupt_handler.as_ref().unwrap_unchecked() };
        if cb(self.rt()) {
            self.rt_mut().interrupt(ExitCode::from(130));
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use crate::{
        Context, Runtime, ScopedContext,
        config::JS_INTERRUPT_COUNTER_INIT,
        eval::{EvalFlags, EvalName, EvalType},
    };

    fn eval(ctx: &mut Context, src: &str) -> crate::JSResult {
        ctx.eval(
            src.as_bytes(),
            &EvalName::Name("interrupt-poll.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
    }

    /// VM-P5: only loop back edges poll. A forward `if`/`else` chain is bounded
    /// within the frame and must not consume the interrupt window.
    ///
    /// Every `eval` goes through `do_call_internal`, which polls once on entry
    /// and therefore fires the pre-armed window. What follows is purely the
    /// script's own polling: nothing for forward branches, one per loop
    /// iteration for a back edge.
    #[test]
    fn forward_branches_do_not_poll_but_back_edges_do() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        let polls = Rc::new(Cell::new(0u32));
        let seen = polls.clone();
        ctx.rt_mut().set_interrupt_handler(move |_| {
            seen.set(seen.get() + 1);
            false
        });

        // Arm the window: the entry poll fires the handler and re-arms it.
        ctx.interrupt_counter.set(1);
        eval(
            &mut ctx,
            "var a = 0; \
             if (true) { a = 1; } else { a = 2; } \
             if (a === 1) { a = 3; } else { a = 4; }",
        )
        .unwrap();
        assert_eq!(polls.get(), 1, "the eval entry must poll exactly once");
        assert_eq!(
            ctx.interrupt_counter.get(),
            JS_INTERRUPT_COUNTER_INIT,
            "forward branches must not consume the interrupt window"
        );

        ctx.interrupt_counter.set(1);
        eval(&mut ctx, "var s = 0; for (var i = 0; i < 4; i++) s += i;").unwrap();
        assert_eq!(
            polls.get(),
            2,
            "the second eval entry must poll exactly once"
        );
        assert_eq!(
            ctx.interrupt_counter.get(),
            JS_INTERRUPT_COUNTER_INIT - 4,
            "a 4-iteration loop must poll once per back edge"
        );
    }

    /// A handler that reports an interrupt must stop a running loop; the
    /// callback is delivered from the back edge.
    #[test]
    fn handler_interrupts_a_running_loop() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();
        ctx.rt_mut().set_interrupt_handler(|_| true);

        ctx.interrupt_counter.set(1);
        let res = eval(&mut ctx, "var i = 0; while (i < 1000000) { i++; }");
        assert!(
            ctx.rt_mut().take_interrupt().is_some(),
            "a loop without an interrupt never stopped"
        );
        assert!(res.is_err(), "an interrupted eval must fail");
    }
}
