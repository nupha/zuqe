use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    builtins::typed_array::TypedArray,
    object::{JSProperty, ShapeImpl},
    runtime::RuntimeImpl,
};

use super::ObjectRef;

// pub unsafe fn JS_DumpShapes(rt: &mut RuntimeImpl) {
//     println!("JSShapes: {{");
//     println!(
//         "{:5} {:4} {:2} {:14} {:14} {:5} {:5} PROPS",
//         "SLOT", "REFS", "GC", "ADDR", "PROTO", "SIZE", "COUNT"
//     );

//     // dump hashed shapes
//     for (_, lst) in rt.shape_registry.iter() {
//         for sh in lst {
//             if let Some(sh) = sh.upgrade() {
//                 sh.0.borrow().dump();
//                 debug_assert!(sh.0.borrow().is_hashed);
//             }
//         }
//     }

//     // dump non-hashed shapes
//     for (_, w) in rt.gc_obj_list.iter() {
//         if let Some(GCObject::Object(p)) = w.upgrade() {
//             let sh = p.shape().unwrap();
//             unsafe {
//                 if !sh.as_ref().is_hashed() {
//                     sh.0.as_ref().dump();
//                 }
//             }
//         }
//     }

//     println!("}}");
// }

#[cfg(debug_assertions)]
impl ObjectRef {
    /// print object debug info to console
    pub fn dump(&self, rt: &RuntimeImpl) {
        // XXX: should encode atoms with special characters
        print!("{:14p} ", self.id() as *const std::ffi::c_void);

        // NOTE: the shape can be NULL while freeing an object
        if let Some(sh) = self.shape() {
            print!(
                "   {:14p} {:14p} ",
                sh.id() as *const ShapeImpl,
                sh.proto
                    .as_ref()
                    .map_or(std::ptr::null(), |o| o as *const _)
            );
        } else {
            print!("{:>3}  {:>14} {:>14} ", "-", "-", "-");
        }

        // print class name
        print!(
            "{:10} ",
            rt.class_registry[self.class_id() as usize]
                .class_name
                .to_utf8(),
        );

        if self.flags().is_exotic() && self.flags().is_fast_array() {
            print!("[ ");

            match self.class_id() {
                JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                    self.with_fast_array_values(|lst| {
                        for (i, v) in lst.iter().enumerate() {
                            if i != 0 {
                                print!(", ");
                            }
                            print!("{v:?}");
                        }
                    });
                }
                class_id @ (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY) => {
                    // 1, 2, 4, 8
                    let bpe = 1 << TypedArray::SIZE_LOG2(class_id);
                    self.payload().typed_array().unwrap().with_bytes(|lst| {
                        for (i, b) in lst.iter().enumerate() {
                            if i != 0 && i % bpe == 0 {
                                print!(", ");
                            }
                            print!("{:02X}", *b);
                        }
                    });
                }
                class_id => unimplemented!("{}", class_id),
            }

            print!(" ] ");
        }

        // print properties
        if let Some(sh) = self.shape() {
            print!("{{ ");

            let mut is_first: bool = true;
            for (idx, prs) in sh.prop_defs.iter().enumerate() {
                if !is_first {
                    print!(", ");
                };

                // key
                print!("{}:", prs.atom.to_utf8());

                // value
                if let Some(pr) = self.prop_values.get(idx) {
                    match pr {
                        JSProperty::Normal(val) => {
                            print!("{val:?}");
                        }
                        JSProperty::GetSet { getter, setter } => {
                            print!(
                                "[getset 0x{:x} 0x{:x}]",
                                getter.as_ref().map_or(0, |o| o.id()),
                                setter.as_ref().map_or(0, |o| o.id())
                            );
                        }
                        JSProperty::VarRef(vref) => {
                            print!("[varref {:p}", vref.value_ptr())
                        }
                        JSProperty::LazyInit { realm, opaque, .. } => {
                            print!(
                                "[autoinit 0x{:x} {:p}]",
                                unsafe { realm.upgrade(rt.gc_heap()).unwrap().as_ref().id() },
                                opaque
                            );
                        }
                        JSProperty::Deleted => {}
                    }
                } else {
                    // prop value slots is cleared
                    print!("<?>");
                }

                is_first = false;
            }
            print!(" }}");
        }

        if let Some(bcf) = self.payload().bc_function()
            && bcf.bytecode().closure_var_count != 0
        {
            print!(" Closure:");
            for vr in bcf.var_refs.iter() {
                print!(" {:?}", vr.value());
            }

            if let Some(o) = bcf.home_object() {
                print!(" HomeObject: {o}");
            }
        }

        println!();
    }
}

// impl ShapeImpl {
//     /// print shape debug info to console
//     pub fn dump(&self) {
//         // XXX: should output readable class prototype
//         print!(
//             "{:14p} {:14p} {:5} [",
//             self.id() as *const std::ffi::c_void,
//             self.proto.as_ref().map_or(0, |p| p.id()) as *const std::ffi::c_void,
//             self.prop_defs.len(),
//         );

//         for (_, prs) in self.prop_defs.iter() {
//             print!(" {}:{}", prs.atom.to_utf8(), prs.flags());
//         }
//         println!(" ]");
//     }
// }
