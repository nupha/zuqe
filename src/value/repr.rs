use std::{borrow::Cow, rc::Rc};

use smallvec::SmallVec;

use crate::{
    atom::consts::{
        JS_ATOM_false, JS_ATOM_null, JS_ATOM_toLocaleString, JS_ATOM_true, JS_ATOM_undefined,
    },
    context::JSContext,
    error::JSResult,
    function::JS_Invoke,
    runtime::JSRuntime,
};

use super::{HINT_STRING, JSString, JSValue, f64_stringify};

impl JSValue {
    /// get string representation of value.
    pub fn repr(&self) -> Cow<'_, str> {
        match self {
            Self::Int(i) => format!("{}", *i).into(),
            Self::Bool(b) => if *b { "true" } else { "false" }.into(),
            Self::UNDEFINED => "undefined".into(),
            Self::NULL => "null".into(),
            Self::EXCEPTION => "[exception]".into(),
            Self::UNINITIALIZED => "[uninitialized]".into(),
            Self::Float64(n) => format!("{:14}", *n).into(),
            Self::String(p) => {
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
            Self::FunctionBytecode(b) => {
                format!("[bytecode {}]", b.0.borrow().func_name.to_str(b.rt())).into()
            }
            Self::Object(p) => {
                let atom = p.rt().class_defs[p.class_id() as usize].class_name;
                #[cfg(not(debug_assertions))]
                {
                    format!("[{}]", atom.to_str(p.rt())).into()
                }
                #[cfg(debug_assertions)]
                {
                    use std::os::raw::c_void;

                    format!("[{} {:p}]", atom.to_str(p.rt()), p.id() as *const c_void).into()
                }
            }
            Self::Symbol(p) => {
                format!("Symbol({})", p.atom_info().unwrap().atom().to_str(p.rt())).into()
            }
            Self::Module(_) => "[module]".into(),
            Self::CatchOffset(n) => format!("[catchoffset({})]", *n).into(),
            Self::AsyncFuncResume(_) => "[asyncfuncresume]".into(),
        }
    }

    // /// get string representation of value
    // pub fn repr(&self, rt: &JSRuntime) -> Cow<'_, str> {
    //     match self {
    //         Self::Int(i) => format!("{}", *i).into(),
    //         Self::Bool(b) => if *b { "true" } else { "false" }.into(),
    //         Self::UNDEFINED => "undefined".into(),
    //         Self::NULL => "null".into(),
    //         Self::EXCEPTION => "[exception]".into(),
    //         Self::UNINITIALIZED => "[uninitialized]".into(),
    //         Self::Float64(n) => format!("{:14}", *n).into(),
    //         Self::String(p) => {
    //             let sep = '\"';
    //             let mut buf = SmallVec::<[u8; 128]>::new();
    //             buf.push(sep as u8);
    //             for c in p.content().chars() {
    //                 match c {
    //                     '\'' | '"' if c == sep => {
    //                         buf.extend_from_slice(format!("\\{c}").as_bytes());
    //                     }
    //                     '\n' => {
    //                         buf.extend_from_slice(b"\\n");
    //                     }
    //                     '\r' => {
    //                         buf.extend_from_slice(b"\\r");
    //                     }
    //                     '\t' => {
    //                         buf.extend_from_slice(b"\\t");
    //                     }
    //                     '\\' => {
    //                         buf.extend_from_slice(b"\\\\");
    //                     }
    //                     ' '..='~' => {
    //                         buf.push(c as u8);
    //                     }
    //                     _ => {
    //                         buf.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes());
    //                     }
    //                 }
    //             }
    //             buf.push(sep as u8);
    //             String::from_utf8(buf.to_vec()).unwrap().into()
    //         }
    //         Self::FunctionBytecode(b) => {
    //             format!("[bytecode {}]", b.0.borrow().func_name.to_str(rt)).into()
    //         }
    //         Self::Object(p) => {
    //             let atom = rt.class_array[p.class_id() as usize].class_name;
    //             format!("[{}]", atom.to_str(rt)).into()
    //         }
    //         Self::Symbol(p) => {
    //             format!("Symbol({})", p.atom_info().unwrap().atom().to_str(rt)).into()
    //         }
    //         Self::Module(_) => "[module]".into(),
    //         Self::CatchOffset(n) => format!("[catchoffset({})]", *n).into(),
    //         Self::AsyncFuncResume(_) => "[asyncfuncresume]".into(),
    //     }
    // }

    /// Convert value `self` to string value.
    fn do_coerce_to_string_value(&self, ctx: &JSContext, as_propkey: bool) -> Result<JSValue, i32> {
        let ret: JSValue = match self {
            Self::String(s) => s.clone().into(),
            Self::Int(i) => ctx.create_string_from_utf8(format!("{}", *i).into_bytes()),
            Self::Bool(b) => if *b { JS_ATOM_true } else { JS_ATOM_false }.to_value(ctx, true),
            Self::NULL => JS_ATOM_null.to_value(ctx, true),
            Self::UNDEFINED => JS_ATOM_undefined.to_value(ctx, true),
            Self::EXCEPTION => {
                return Err(-1);
            }
            Self::Object(_) => {
                let t = self.coerce_to_primitive(ctx, HINT_STRING)?;
                t.do_coerce_to_string_value(ctx, as_propkey)?
            }
            Self::FunctionBytecode(_) => ctx.create_string("[function bytecode]"),
            Self::Symbol(_) => {
                if as_propkey {
                    self.clone()
                } else {
                    ctx.JS_ThrowTypeError("cannot convert symbol to string");
                    JSValue::EXCEPTION
                }
            }
            Self::Module(_) => ctx.create_string("[module]"),
            Self::Float64(n) => {
                let s = f64_stringify(*n, 10, None, 0);
                ctx.create_string(s.as_ref())
            }
            Self::UNINITIALIZED => ctx.create_string("[uninitialized]"),
            Self::CatchOffset(_) => ctx.create_string("[cacthoffset]"),
            Self::AsyncFuncResume(_) => ctx.create_string("[asyncfuncresume]"),
        };

        ret.check_exception()
    }

    /// Coerce `self` value to string value.
    /// `self` is not consumed.
    #[inline(always)]
    pub fn coerce_to_string_value(&self, ctx: &JSContext) -> JSResult {
        self.do_coerce_to_string_value(ctx, false)
    }

    /// Invoking `toLocaleString()` method, returns result value.
    /// `self` is not consumed.
    pub fn to_locale_string(&self, ctx: &JSContext) -> JSResult {
        if self.is_undefined_or_null() {
            self.coerce_to_string_value(ctx)
        } else {
            JS_Invoke(ctx, self, JS_ATOM_toLocaleString, &[]).check_exception()
        }
    }

    /// Coerce `self` value to property key value.
    /// `self` is not consumed.
    #[inline(always)]
    pub fn coerce_to_propkey_value(&self, ctx: &JSContext) -> Result<JSValue, i32> {
        self.do_coerce_to_string_value(ctx, true)
    }
}

pub unsafe fn JS_DumpString(rt: &JSRuntime, p: &JSString) {
    // if p.is_null() {
    //     print!("<null>");
    //     return;
    // }

    let ref_cnt = Rc::strong_count(&p.0);
    print!("{ref_cnt}");

    let sep = if ref_cnt == 1 { b'\"' } else { b'\'' } as char;
    print!("{sep}");

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

    print!("{sep}");
}
