use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    object::{GCObject, JSProperty, JSShapeInner},
    runtime::JSRuntime,
    typed_array::typed_array_size_log2,
    value::JSValue,
};

use super::{JSObject, JSShape};

pub unsafe fn JS_DumpShapes(rt: &mut JSRuntime) {
    println!("JSShapes: {{");
    println!(
        "{:5} {:4} {:2} {:14} {:14} {:5} {:5} PROPS",
        "SLOT", "REFS", "GC", "ADDR", "PROTO", "SIZE", "COUNT"
    );

    // dump hashed shapes
    for (key, lst) in rt.shape_registry.iter() {
        for sh in lst {
            if let Some(sh) = sh.upgrade() {
                sh.0.borrow().dump(rt);
                debug_assert!(sh.0.borrow().is_hashed);
            }
        }
    }

    // dump non-hashed shapes
    for (_, w) in rt.gc_obj_list.iter() {
        if let Some(GCObject::Object(p)) = w.upgrade() {
            let sh = p.shape().unwrap();
            if !sh.is_hashed() {
                sh.0.borrow().dump(rt);
            }
        }
    }

    println!("}}");
}

#[deprecated(note = "use JSShape::dump() instead")]
pub unsafe fn JS_DumpShape(rt: &JSRuntime, i: i32, sh: &JSShape) {
    unreachable!("unused");
    // sh.dump(rt, i);
}

impl JSObject {
    /// print object debug info to console
    pub fn dump(&self, rt: &JSRuntime) {
        // XXX: should encode atoms with special characters
        print!("{:14p} ", self.id() as *const std::ffi::c_void);

        // NOTE: the shape can be NULL while freeing an object
        if let Some(sh) = self.shape() {
            let sh = sh.0.borrow();
            print!(
                "   {} {:14p} {:14p} ",
                if sh.is_hashed { "*" } else { " " },
                sh.id() as *const JSShapeInner,
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
            rt.class_defs[self.class_id() as usize]
                .class_name
                .to_utf8(rt),
        );

        if self.flags().is_exotic() && self.flags().is_fast_array() {
            print!("[ ");

            match self.class_id() {
                JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                    self.with_fast_array_values(|lst, _| {
                        for (i, v) in lst.iter().enumerate() {
                            if i != 0 {
                                print!(", ");
                            }
                            v.repr();
                        }
                    });
                }
                class_id @ (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY) => {
                    // 1, 2, 4, 8
                    let bytes_per_element = 1 << typed_array_size_log2(class_id);

                    self.0
                        .payload
                        .borrow()
                        .typed_array()
                        .unwrap()
                        .with_bytes(|lst| {
                            for (i, b) in lst.iter().enumerate() {
                                if i != 0 && i % bytes_per_element == 0 {
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

            let prop_values = self.0.prop_values.borrow();
            let mut is_first: bool = true;
            for (idx, (_, prs)) in sh.0.borrow().prop_defs.iter().enumerate() {
                if !is_first {
                    print!(", ");
                };

                // key
                print!("{}:", prs.atom.to_utf8(rt));

                // value
                if let Some(pr) = prop_values.get(idx) {
                    match pr {
                        JSProperty::Normal(val) => {
                            print!("{}", val.repr());
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
                        JSProperty::LazyInit {
                            realm,
                            init_type,
                            opaque,
                        } => {
                            print!(
                                "[autoinit 0x{:x} {:p}]",
                                realm.upgrade().unwrap().id(),
                                opaque
                            );
                        }
                    }
                } else {
                    // prop value slots is cleared
                    print!("<?>");
                }

                is_first = false;
            }
            print!(" }}");
        }

        if let Some(func) = self.0.payload.borrow().func() {
            func.bytecode.as_ref().unwrap().with_mut(|b| {
                if b.closure_var_count != 0 {
                    print!(" Closure:");
                    for vr in func.var_refs.iter() {
                        print!(" {}", vr.value().repr());
                    }

                    if let Some(o) = func.home_object.clone() {
                        print!(" HomeObject: {}", JSValue::Object(o).repr());
                    }
                }
            });
        }

        println!();
    }
}

impl JSShapeInner {
    /// print shape debug info to console
    pub fn dump(&self, rt: &JSRuntime) {
        // XXX: should output readable class prototype
        print!(
            "{:14p} {:14p} {:5} [",
            self.id() as *const std::ffi::c_void,
            self.proto.as_ref().map_or(0, |p| p.id()) as *const std::ffi::c_void,
            self.prop_defs.len(),
        );

        for (_, prs) in self.prop_defs.iter() {
            print!(" {}:{}", prs.atom.to_utf8(rt), prs.flags());
        }
        println!(" ]");
    }
}

impl JSShape {
    /// print shape debug info to console
    pub fn dump(&self, rt: &JSRuntime) {
        self.0.borrow().dump(rt);
    }
}
