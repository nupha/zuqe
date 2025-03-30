use std::borrow::Cow;

use crate::{
    atom::JSAtomStruct, context::JSContext, function::JSFunctionBytecode, runtime::JSRuntime,
};

use super::{
    JSString, JSTag, JSValue, JSValueConst, JS_VALUE_GET_BOOL, JS_VALUE_GET_FLOAT64,
    JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ, JS_VALUE_GET_PTR,
    JS_VALUE_GET_STRING,
};

pub unsafe fn JS_DumpString(rt: *const JSRuntime, p: *const JSString) {
    if p.is_null() {
        print!("<null>");
        return;
    }

    print!("{}", (*p).ref_count());

    let sep = if (*p).ref_count() == 1 { b'\"' } else { b'\'' } as char;
    print!("{}", sep as char);

    for c in (*p).content().chars() {
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
                let p: *mut JSString = JS_VALUE_GET_STRING(self);
                JS_DumpString(rt, p);
                return;
            }
            JSTag::FunctionBytecode => {
                let b = JS_VALUE_GET_PTR(self).cast::<JSFunctionBytecode>();
                format!("[bytecode {}]", (*b).func_name.to_str(rt)).into()
            }
            JSTag::Object => {
                let p = JS_VALUE_GET_OBJ(self);
                let atom = (*(*rt).class_array.offset((*p).class_id() as isize)).class_name;
                format!("[{} {p:p}]", atom.to_str(rt)).into()
            }
            JSTag::Symbol => {
                let p = JS_VALUE_GET_PTR(self).cast::<JSAtomStruct>();
                format!("Symbol({})", (*p).atom_type().unwrap().atom().to_str(rt),).into()
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
