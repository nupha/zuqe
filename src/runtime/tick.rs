use crate::context::{Context, ContextImpl};

use super::RuntimeImpl;

impl ContextImpl {
    pub fn add_tick_handler(&mut self, f: fn(Context) -> bool) {
        self.tick_handlers.push(f);
    }
}

impl RuntimeImpl {
    pub fn tick(&mut self) -> Result<bool, i32> {
        self.execute_all_jobs().map_err(|_| -1)?;

        let mut need_next_tick = false;
        for c in self.context_list.iter() {
            for f in c.tick_handlers.iter() {
                if f((*c).into()) {
                    need_next_tick = true;
                }
            }
        }

        if self.gc_req || self.gc_threshold >= self.gc_info().memory_used() {
            self.gc_collect();
        }

        Ok(need_next_tick)
    }
}
