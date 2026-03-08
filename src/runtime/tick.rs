// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    ScopedContext,
    context::{Context, ContextImpl},
};

use super::RuntimeImpl;

impl ContextImpl {
    pub fn add_tick_handler(&mut self, f: fn(&mut ScopedContext) -> bool) {
        self.tick_handlers.push(f);
    }

    pub fn remove_tick_handler(&mut self, f: fn(Context) -> bool) {
        self.tick_handlers.retain(|x| !std::ptr::fn_addr_eq(f, *x));
    }
}

pub enum TickStatus {
    Continue,
    Stop(i32),
}

impl RuntimeImpl {
    pub const fn stop(&mut self, code: i32) {
        self.stop_flag = Some(code);
    }

    pub fn tick(&mut self) -> Result<TickStatus, i32> {
        if let Some(n) = self.stop_flag.take() {
            return Ok(TickStatus::Stop(n));
        }

        self.execute_all_jobs().map_err(|_| -1)?;

        let mut need_next_tick = false;
        for ctx in self.context_list.iter().copied() {
            let mut cx2 = Context::from(ctx);
            let mut scope = ctx.current_scope().unwrap_or_else(|| cx2.new_scope());

            for f in ctx.tick_handlers.iter() {
                if f(&mut scope) {
                    need_next_tick = true;
                }
            }
        }

        if self.gc_req || self.gc_top_scope().should_gc() {
            log::trace!("[tick_gc_collect]");
            self.gc_collect();
        }

        Ok(if need_next_tick {
            TickStatus::Continue
        } else {
            TickStatus::Stop(0)
        })
    }
}
