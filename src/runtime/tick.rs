use super::RuntimeImpl;

impl RuntimeImpl {
    pub fn tick(&mut self) -> Result<bool, i32> {
        if self.gc_req {
            // FIXME: gc is buggy.
            // self.gc();

            self.gc_threshold =
                self.malloc_state.malloc_size + (self.malloc_state.malloc_size >> 1);
            self.gc_req = false;
        }

        Ok(self.execute_job().map_err(|_| -1)?.is_some())
    }
}
