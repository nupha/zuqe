use crate::context::{Context, ContextImpl};

use super::RuntimeImpl;

impl ContextImpl {
    pub fn add_tick_handler(&mut self, f: fn(Context)) {
        self.tick_handlers.push(f);
    }
}

impl RuntimeImpl {
    pub fn tick(&mut self) -> Result<bool, i32> {
        for c in self.context_list.iter() {
            for f in c.tick_handlers.iter() {
                f((*c).into())
            }
        }

        let has_pending_jobs = self.execute_job().map_err(|_| -1)?.is_some();

        if self.gc_threshold >= self.gc_info().memory_used() {
            self.gc_collect();
        }

        Ok(has_pending_jobs)
    }
}
