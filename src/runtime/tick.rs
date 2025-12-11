use crate::context::{Context, ContextImpl};

use super::RuntimeImpl;

impl ContextImpl {
    pub fn add_tick_handler(&mut self, f: fn(Context) -> bool) {
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
        self.stop = Some(code);
    }

    pub fn tick(&mut self) -> Result<TickStatus, i32> {
        if let Some(n) = self.stop.take() {
            return Ok(TickStatus::Stop(n));
        }

        self.execute_all_jobs().map_err(|_| -1)?;

        let mut need_next_tick = false;
        for c in self.context_list.iter() {
            for f in c.tick_handlers.iter() {
                if f((*c).into()) {
                    need_next_tick = true;
                }
            }
        }

        if self.gc_req
            || (self.gc_threshold > 0 && self.gc_info().memory_used() >= self.gc_threshold)
        {
            self.gc_collect();
        }

        Ok(if need_next_tick {
            TickStatus::Continue
        } else {
            TickStatus::Stop(0)
        })
    }
}
