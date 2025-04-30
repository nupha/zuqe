use crate::{
    function::js_class_has_bytecode,
    gc::{JSGCObjectHeader, JSGCObjectTypeEnum},
    list_entry,
    runtime::JSRuntime,
    typed_array::typed_array_size_log2,
    types::list_foreach,
    value::{
        object::{JS_PROP_AUTOINIT, JS_PROP_GETSET, JS_PROP_VARREF},
        JSTag,
    },
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{js_autoinit_get_id, js_autoinit_get_realm, JSObject, JSShape, JS_PROP_TMASK};

impl JSShape {
    /// print shape to console
    pub unsafe fn dump(&self, rt: &JSRuntime, i: i32) {
        // XXX: should output readable class prototype
        print!(
            "{:5} {:3}{} {:2} {:14p} {:14p} {:5} {:5}",
            i,
            self.header.ref_count,
            if self.is_hashed { '*' } else { ' ' }, // hashed?
            if self.header.link.next.is_null() && self.header.link.prev.is_null() {
                " " // not in gc list
            } else {
                "+" // in gc list
            },
            self,
            self.proto,
            self.props.len(),
            self.props.len(),
        );

        for key in self.props.keys() {
            print!(" {}", key.to_str(rt));
        }

        println!();
    }
}

pub unsafe fn JS_DumpShapes(rt: *mut JSRuntime) {
    println!("JSShapes: {{");
    println!(
        "{:5} {:4} {:2} {:14} {:14} {:5} {:5} {}",
        "SLOT", "REFS", "GC", "ADDR", "PROTO", "SIZE", "COUNT", "PROPS"
    );

    // dump hashed shapes
    for i in 0..(*rt).shape_hash_size {
        let mut sh = *(*rt).shape_hash.offset(i as isize);
        while !sh.is_null() {
            (*sh).dump(rt.as_ref().unwrap(), i);
            debug_assert!((*sh).is_hashed);
            sh = (*sh).shape_hash_next;
        }
    }

    // dump non-hashed shapes
    list_foreach(&mut (*rt).gc_obj_list, |el| {
        let gp = list_entry!(el, JSGCObjectHeader, link);
        if (*gp).gc_obj_type() == JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_JS_OBJECT {
            let p = gp.cast::<JSObject>();
            let sh = (*p).shape().unwrap();
            if !sh.is_hashed {
                sh.dump(rt.as_ref().unwrap(), -1);
            }
        }
    });

    println!("}}");
}

#[deprecated(note = "use JSShape::dump() instead")]
pub unsafe fn JS_DumpShape(rt: *const JSRuntime, i: i32, sh: *const JSShape) {
    (*sh).dump(rt.as_ref().unwrap(), i);
}

impl JSObject {
    /// print object to console
    pub unsafe fn dump(&self, rt: &JSRuntime) {
        // XXX: should encode atoms with special characters
        print!("{:14p} {:4} ", self, self.header.ref_count);

        // NOTE: the shape can be NULL while freeing an object
        if let Some(sh) = self.shape() {
            print!(
                "{:3}{} {:14p} {:14p} ",
                sh.header.ref_count,
                if sh.is_hashed { "*" } else { " " },
                sh,
                sh.proto
            );
        } else {
            print!("{:3}  {:14} {:14p} ", "-", "-", "-");
        }

        // print class name
        print!(
            "{:10} ",
            (*rt).class_array[self.class_id() as usize]
                .class_name
                .to_str(rt),
        );

        if self.detailed.is_exotic() && self.detailed.fast_array() {
            print!("[ ");

            match self.class_id() {
                JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                    let lst = self.fast_array_values_unchecked();
                    for (i, v) in lst.iter().enumerate() {
                        if i != 0 {
                            print!(", ");
                        }
                        v.dump(rt);
                    }
                }
                class_id @ (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY) => {
                    // 1, 2, 4, 8
                    let bytes_per_element = 1 << typed_array_size_log2(class_id);

                    self.as_typed_array_obj_unchecked()
                        .with_data::<u8, _>(|lst| {
                            for (i, b) in lst.iter().enumerate() {
                                if i != 0 && i % bytes_per_element == 0 {
                                    print!(", ");
                                }
                                print!("{:02X}", *b);
                            }
                        });

                    // let buf = std::slice::from_raw_parts(
                    //     self.u.array.data.ptr.cast::<u8>(),
                    //     self.array_length_unchecked() * bytes_per_element,
                    // );
                    // for (i, b) in buf.iter().enumerate() {
                    //     if i != 0 && i % bytes_per_element == 0 {
                    //         print!(", ");
                    //     }
                    //     print!("{:02X}", *b);
                    // }
                }
                _ => unimplemented!(),
            }

            print!(" ] ");
        }

        // print properties
        if let Some(sh) = self.shape() {
            print!("{{ ");

            let mut is_first: bool = true;
            for (idx, (atom, prs)) in sh.props.iter().enumerate() {
                if !is_first {
                    print!(", ");
                };

                // key
                print!("{}:", atom.to_str(rt));

                // value
                if let Some(pr) = self.prop_values.get(idx) {
                    match prs.flags() & JS_PROP_TMASK {
                        JS_PROP_GETSET => {
                            print!(
                                "[getset {:p} {:p}]",
                                (*pr).u.getset.getter,
                                (*pr).u.getset.setter
                            );
                        }
                        JS_PROP_VARREF => {
                            print!("[varref {:p}]", (*pr).u.var_ref);
                        }
                        JS_PROP_AUTOINIT => {
                            print!(
                                "[autoinit {:p} {} {:p}]",
                                js_autoinit_get_realm(pr),
                                js_autoinit_get_id(pr),
                                (*pr).u.init.opaque
                            );
                        }
                        _ => {
                            (*pr).u.value.dump(rt);
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

        if js_class_has_bytecode(self.class_id()) {
            let b = self.u.func.function_bytecode;
            if (*b).closure_var_count != 0 {
                print!(" Closure:");
                for vr in self.u.func.var_refs.iter() {
                    print!(" ");
                    (*vr.as_ref().pvalue).dump(rt);
                }

                if !self.u.func.home_object.is_null() {
                    print!(" HomeObject: ");
                    JS_MKPTR!(JSTag::Object, self.u.func.home_object.cast()).dump(rt);
                }
            }
        }

        println!();
    }
}

// for debug only: dump an object without side effect
#[deprecated(note = "use JSObject::dump() instead")]
pub unsafe fn JS_DumpObject(rt: *const JSRuntime, p: *const JSObject) {
    (*p).dump(rt.as_ref().unwrap());
}
