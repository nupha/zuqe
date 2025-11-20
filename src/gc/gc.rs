use crate::{
    context::ContextImpl,
    object::{GCObject, GCObjectWeakref},
    runtime::RuntimeImpl,
    value::JSValue,
};

pub fn js_trigger_gc(rt: &mut RuntimeImpl, size: usize) {
    let force_gc: bool;

    #[cfg(feature = "FORCE_GC_AT_MALLOC")]
    {
        force_gc = true;
    }
    #[cfg(not(feature = "FORCE_GC_AT_MALLOC"))]
    {
        force_gc = ((*rt).malloc_state.malloc_size + size) > (*rt).gc_threshold;
    }

    if force_gc {
        todo!();

        #[cfg(feature = "DUMP_GC")]
        {
            println!("GC: size={}", (*rt).malloc_state.malloc_size);
        }

        //rt.gc();
        rt.gc_threshold = rt.malloc_state.malloc_size + (rt.malloc_state.malloc_size >> 1);
    }
}

/// Marks an object as potentially part of a function prototype cycle
pub fn set_cycle_flag(ctx: &ContextImpl, obj: &JSValue) {
    // to nothing
}
