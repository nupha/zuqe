use std::{borrow::Cow, rc::Rc};

use smallvec::SmallVec;

use crate::{
    atom::JSAtomStruct, context::JSContext, function::JSFunctionBytecode, runtime::JSRuntime,
};

use super::{
    JSString, JSTag, JSValue, JSValueConst, JS_VALUE_GET_BOOL, JS_VALUE_GET_FLOAT64,
    JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ, JS_VALUE_GET_PTR,
    JS_VALUE_GET_STRING,
};

pub unsafe fn JS_DumpString(rt: *const JSRuntime, p: &Rc<JSString>) {
    // if p.is_null() {
    //     print!("<null>");
    //     return;
    // }

    let ref_cnt = Rc::strong_count(p);
    print!("{}", ref_cnt);

    let sep = if ref_cnt == 1 { b'\"' } else { b'\'' } as char;
    print!("{}", sep as char);

    for c in p.content().chars() {
        match c {
            '\'' | '"' if c == sep => print!("\\{c}"),
            '\n' => print!("\\n"),
            '\r' => print!("\\r"),
            '\t' => print!("\\t"),
            '\\' => print!("\\\\"),
            ' '..='~' => print!("{c}"),
            _ => print!("\\u{:04x}", c as u32),
        }
    }

    print!("{}", sep as char);
}

impl JSValue {
    /// get string representation of value
    pub fn repr(self, rt: &JSRuntime) -> Cow<'_, str> {
        match JS_VALUE_GET_NORM_TAG(self) {
            JSTag::Int => format!("{}", JS_VALUE_GET_INT(self)).into(),
            JSTag::Bool => if JS_VALUE_GET_BOOL(self) {
                "true"
            } else {
                "false"
            }
            .into(),
            JSTag::Null => "null".into(),
            JSTag::Exception => "<exception>".into(),
            JSTag::Uninitialized => "<uninitialized>".into(),
            JSTag::Undefined => "undefined".into(),
            JSTag::Float64 => format!("{:14}", JS_VALUE_GET_FLOAT64(self)).into(),
            JSTag::String => {
                let p = unsafe { self.string() };
                let sep = '\"';
                let mut buf = SmallVec::<[u8; 128]>::new();
                buf.push(sep as u8);
                for c in p.content().chars() {
                    match c {
                        '\'' | '"' if c == sep => {
                            buf.extend_from_slice(format!("\\{c}").as_bytes());
                        }
                        '\n' => {
                            buf.extend_from_slice(b"\\n");
                        }
                        '\r' => {
                            buf.extend_from_slice(b"\\r");
                        }
                        '\t' => {
                            buf.extend_from_slice(b"\\t");
                        }
                        '\\' => {
                            buf.extend_from_slice(b"\\\\");
                        }
                        ' '..='~' => {
                            buf.push(c as u8);
                        }
                        _ => {
                            buf.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes());
                        }
                    }
                }
                buf.push(sep as u8);
                String::from_utf8(buf.to_vec()).unwrap().into()
            }
            JSTag::FunctionBytecode => {
                let b = JS_VALUE_GET_PTR(self).cast::<JSFunctionBytecode>();
                format!("[bytecode {}]", unsafe { (*b).func_name.to_str(rt) }).into()
            }
            JSTag::Object => {
                let p = unsafe { self.object() };
                let atom = rt.class_array[p.class_id() as usize].class_name;
                format!("[{}]", atom.to_str(rt)).into()
            }
            JSTag::Symbol => {
                let p = JS_VALUE_GET_PTR(self).cast::<JSAtomStruct>();
                format!("Symbol({})", unsafe {
                    (*p).atom_info().unwrap().atom().to_str(rt)
                })
                .into()
            }
            JSTag::CatchOffset => "<catchoffset>".into(),
            JSTag::Module => "<module>".into(),
            JSTag::BigInt | JSTag::BigDecimal | JSTag::BigFloat => {
                unimplemented!();
            }
        }
    }

    #[deprecated(note = "use JSValue::repr() instead")]
    pub unsafe fn dump(self, rt: &JSRuntime) {
        let str: Cow<'static, str> = match JS_VALUE_GET_NORM_TAG(self) {
            JSTag::Int => format!("{}", JS_VALUE_GET_INT(self)).into(),
            JSTag::Bool => if JS_VALUE_GET_BOOL(self) {
                "true"
            } else {
                "false"
            }
            .into(),
            JSTag::Null => "null".into(),
            JSTag::Exception => "exception".into(),
            JSTag::Uninitialized => "uninitialized".into(),
            JSTag::Undefined => "undefined".into(),
            JSTag::Float64 => format!("{:14}", JS_VALUE_GET_FLOAT64(self)).into(),
            JSTag::String => {
                let p = JS_VALUE_GET_STRING(self);
                debug_assert!(!p.is_null());
                let ss = Rc::from_raw(p);
                JS_DumpString(rt, &ss);
                let _ = Rc::into_raw(ss);
                return;
            }
            JSTag::FunctionBytecode => {
                let b = JS_VALUE_GET_PTR(self).cast::<JSFunctionBytecode>();
                format!("[bytecode {}]", (*b).func_name.to_str(rt)).into()
            }
            JSTag::Object => {
                let p = JS_VALUE_GET_OBJ(self);
                let atom = (*rt).class_array[(*p).class_id() as usize].class_name;
                format!("[{} {p:p}]", atom.to_str(rt)).into()
            }
            JSTag::Symbol => {
                let p = JS_VALUE_GET_PTR(self).cast::<JSAtomStruct>();
                format!("Symbol({})", (*p).atom_info().unwrap().atom().to_str(rt),).into()
            }
            JSTag::CatchOffset => "catchoffset".into(),
            JSTag::Module => "[module]".into(),
            JSTag::BigInt | JSTag::BigDecimal | JSTag::BigFloat => {
                unimplemented!();
            }
        };

        print!("{}", str);
    }
}

#[deprecated(note = "use JSValue::dump() instead")]
pub unsafe fn JS_DumpValueShort(rt: *const JSRuntime, val: JSValueConst) {
    val.dump(rt.as_ref().unwrap());
}

#[deprecated(note = "use JSValue::dump() instead")]
pub unsafe fn JS_DumpValue(ctx: *const JSContext, val: JSValueConst) {
    JS_DumpValueShort((*ctx).rt(), val);
}
