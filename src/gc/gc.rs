use crate::{
    context::JSContext,
    object::{GCObject, GCObjectWeakref},
    runtime::JSRuntime,
    value::JSValueConst,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSGCPhaseEnum {
    JS_GC_PHASE_NONE,
    /// Phase where objects with zero reference count are freed
    JS_GC_PHASE_DECREF,
    /// Phase where cyclic references are detected and freed
    JS_GC_PHASE_REMOVE_CYCLES,
}

pub fn JS_DumpObjectHeader() {
    println!(
        "{:>14} {:4} {:4} {:>14} {:>14} {:10} PROPS",
        "ADDRESS", "REFS", "SHRF", "SHAPE@", "PROTO", "CLASS",
    );
}

impl JSRuntime {
    /// Adds a GC object to the garbage collector's tracking list
    /// Initializes the object's mark and type, then adds it to the GC object list
    pub fn gc_add_object(&mut self, id: usize, gcob: GCObjectWeakref) {
        self.gc_obj_list.insert(id, gcob);
    }

    /// Executes garbage collection process once
    pub fn gc(&mut self) {
        // clear mark
        for (_, w) in self.gc_obj_list.iter() {
            if let Some(o) = w.upgrade() {
                o.gc_set_mark(false);
            }
        }

        // mark
        for cx in self.context_list.iter() {
            cx.gc_mark();
        }
        self.cur_exception.gc_mark();

        // sweep
        let mut zero_list = Vec::<GCObject>::with_capacity(self.gc_obj_list.len());
        self.gc_obj_list.retain(|_, w| {
            if let Some(o) = w.upgrade() {
                if o.gc_has_mark() {
                    true
                } else {
                    zero_list.push(o);
                    false
                }
            } else {
                false
            }
        });

        for obj in zero_list {
            // println!("[O.o] sweep gc obj {obj:?}");
            obj.gc_sweep();
        }
    }

    pub fn gc_dump_objects(&mut self, which: u8) {
        // let list = match which {
        //     0 => {
        //         println!("Zeroed GCObjects: {{");
        //         &self.gc_zero_ref_count_list
        //     }
        //     1 => {
        //         println!("GCObjects: {{");
        //         self.gc_obj_list
        //     }
        //     2 => {
        //         println!("Temp GCObjects: {{");
        //         &self.tmp_obj_list
        //     }
        //     _ => unreachable!(),
        // };

        // JS_DumpObjectHeader();

        // for node in list.iter() {
        //     // node.gc_dump(self);
        // }

        // println!("}}");
    }
}

pub fn js_trigger_gc(rt: &mut JSRuntime, size: usize) {
    let force_gc: bool;

    #[cfg(feature = "FORCE_GC_AT_MALLOC")]
    {
        force_gc = true;
    }
    #[cfg(not(feature = "FORCE_GC_AT_MALLOC"))]
    {
        force_gc = ((*rt).malloc_state.malloc_size + size) > (*rt).malloc_gc_threshold;
    }

    if force_gc {
        #[cfg(feature = "DUMP_GC")]
        {
            println!("GC: size={}", (*rt).malloc_state.malloc_size);
        }

        rt.gc();
        rt.malloc_gc_threshold = rt.malloc_state.malloc_size + (rt.malloc_state.malloc_size >> 1);
    }
}

/// Marks an object as potentially part of a function prototype cycle
pub fn set_cycle_flag(ctx: &JSContext, obj: &JSValueConst) {
    // to nothing
}
