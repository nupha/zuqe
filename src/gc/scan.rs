use crate::{
    gc::{gc_mark_children, GCObject},
    runtime::JSRuntime,
    utils::Node,
};

use super::{GCObjectType, JSGCPhaseEnum};

impl JSRuntime {
    /// Preserves objects with positive reference counts and their child objects.
    /// Identifies objects with zero reference count in the GC object list
    /// and moves them to a temporary list for removal.
    pub fn gc_scan(&mut self) {
        print!(
            "[O.o][gc_scan] gc_obj_list={}, tmp_obj_list={} ",
            self.gc_obj_list.len(),
            self.tmp_obj_list.len(),
        );

        let rt = self as *mut JSRuntime;

        for node in self.gc_obj_list.iter_mut() {
            debug_assert!(
                node.ref_count() > 0,
                "[gc_scan: node ref_count={}",
                node.ref_count()
            );

            // reset the mark for the next GC call
            node.set_gc_mark(0);

            unsafe {
                gc_mark_children(rt, node, |rt: *mut JSRuntime, p: *mut dyn GCObject| {
                    if (*p).inc_ref() == 1 {
                        // ref_count was 0
                        // in this case, `p` is now in tmp_obj_list, restore it to gc_obj_list

                        let node1 = (*p).gc_list_node();
                        Node::detach_from_list(node1);
                        (*rt).gc_obj_list.push_raw(node1);

                        // reset the mark for the next GC call
                        (*p).set_gc_mark(0);
                    }
                });
            }
        }

        // restore the refcount of the objects to be deleted.
        for node in self.tmp_obj_list.iter_mut() {
            unsafe {
                gc_mark_children(rt, node, |_, p: *mut dyn GCObject| {
                    (*p).inc_ref();
                });
            }
        }

        println!(
            " -> gc_obj_list={}, tmp_obj_list={} [/O.o]",
            self.gc_obj_list.len(),
            self.tmp_obj_list.len(),
        );
    }

    pub fn gc_free_cycles(&mut self) {
        #[cfg(feature = "dump_gc_free")]
        let mut header_done = false;

        self.gc_phase = JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES;

        while let Some(mut node) = self.tmp_obj_list.pop() {
            // Only need to free the GC object associated with JS values
            // or async functions. The rest will be automatically removed
            // because they must be referenced by them.
            match node.gc_type() {
                GCObjectType::Object(_)
                | GCObjectType::FunctionBytecode(_)
                | GCObjectType::AsyncFunction(_)
                | GCObjectType::Context(_) => {
                    #[cfg(feature = "dump_gc_free")]
                    {
                        if !header_done {
                            println!("Freeing cycles:");
                            JS_DumpObjectHeader();
                            header_done = true;
                        }
                        p.as_ref().gc_dump(self);
                    }

                    node.finalize(self);

                    if node.ref_count() != 0 {
                        // this gcobject is about to be freed cascading.
                        // temporarily add it to zero list,
                        // it won't live long till zeroed list purge step.
                        self.gc_zero_ref_count_list.push(node);
                    } else {
                        drop(node);
                    }
                }
                GCObjectType::Shape(_) | GCObjectType::VarRef(_) => {
                    // these objects are meant to be dropped cascading
                    self.gc_zero_ref_count_list.push(node);
                }
            }
        }

        self.gc_phase = JSGCPhaseEnum::JS_GC_PHASE_NONE;

        // debug O.o print gcobject in variant chains
        {
            println!("[O.o]----------- free cycles purge");
            self.gc_dump_objects(1);
            self.gc_dump_objects(2);
            self.gc_dump_objects(0);
        }

        // purge gz_zero_ref_count_list to drop gcobjects
        while let Some(node) = self.gc_zero_ref_count_list.pop() {
            debug_assert_eq!(node.ref_count(), 0);
            debug_assert!(
                matches!(
                    node.gc_type(),
                    GCObjectType::Object(_)
                        | GCObjectType::FunctionBytecode(_)
                        | GCObjectType::AsyncFunction(_)
                        | GCObjectType::Context(_)
                ),
                "Shape, VarRef can't live long in gc zero list upto remove cycles phase"
            );

            drop(node);
        }
    }
}
