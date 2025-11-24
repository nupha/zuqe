use super::RuntimeImpl;

impl RuntimeImpl {
    pub fn tick(&mut self) -> Result<bool, i32> {
        let has_pending_jobs = self.execute_job().map_err(|_| -1)?.is_some();

        if self.gc_threshold >= self.gc_info().memory_used() {
            self.gc_collect();
        }

        Ok(has_pending_jobs)
    }
}
