use std::{
    borrow::Cow,
    ffi::{c_char, CStr},
    mem::offset_of,
};

use fallible_collections::FallibleVec;

use crate::{
    atom::{consts::*, JSAtom, JSAtomType, JS_FreeAtomStruct, __JS_AtomToUInt32},
    class::JSClassExoticMethods,
    container_of,
    context::JSContext,
    function::JS_InvokeFree,
    mem::js_malloc_rt_typed,
    runtime::JSRuntime,
    types::{unlikely, JSRefCountHeader},
    value::{
        JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_IsException, HINT_STRING,
        JS_TAG_NULL, JS_TAG_STRING, JS_TAG_UNDEFINED, JS_UNDEFINED, JS_VALUE_GET_BOOL,
        JS_VALUE_GET_FLOAT64, JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_STRING, JS_VALUE_GET_TAG,
    },
    JS_MKPTR,
};

use super::{
    js_dtoa,
    object::{
        check_define_prop_flags, JSPropertyDescriptor, JS_DefineProperty, JS_PROP_ENUMERABLE,
        JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC,
    },
    primitives::JS_ToPrimitive,
    JS_IsNull, JS_IsUndefined, StringContent, JS_DTOA_VAR_FORMAT,
};

pub const JS_STRING_LEN_MAX: usize = (1 << 30) - 1;

#[derive(Debug)]
#[repr(C)]
pub struct JSString {
    // must come first, 32-bit
    pub header: JSRefCountHeader,

    hash_and_type: u32, // hash: 30 bits, atom_type: 2 bits
    pub hash_next: u32, // atom_index for JS_ATOM_TYPE_SYMBOL

    #[cfg(feature = "dump_leaks")]
    pub link: crate::types::list_head, // string list

    /// string content
    content: StringContent,
}

impl JSString {
    /// create a JSString with uninitialized content. internal use only.
    fn new_uninit(rt: &mut JSRuntime) -> Result<Box<Self>, i32> {
        let mut ss = unsafe {
            Box::from_raw(
                js_malloc_rt_typed::<JSString>(rt, 1, false)
                    .ok_or(-1)?
                    .as_ptr(),
            )
        };

        ss.header.ref_count = 1;
        ss.set_atom_type(JSAtomType::None);
        ss.set_hash(0);
        ss.hash_next = 0;
        ss.content = StringContent::Empty;

        #[cfg(feature = "dump_leaks")]
        unsafe {
            crate::types::list_add_tail(&mut ss.link, &mut rt.string_list);
        }

        Ok(ss)
    }

    /// create a jsstring with content bytes.
    #[inline(always)]
    pub fn from_content(rt: &mut JSRuntime, content: StringContent) -> Result<Box<Self>, i32> {
        let mut ss = Self::new_uninit(rt)?;
        ss.content = content;
        Ok(ss)
    }

    /// create string jsstring from an utf8 encoded str.
    #[inline(always)]
    pub fn from_str(rt: &mut JSRuntime, str: &str) -> Result<Box<Self>, i32> {
        JSString::from_content(rt, StringContent::from_str(str).map_err(|_| -1)?)
    }

    /// create jsstring from an utf8 encoded vec.
    pub fn from_utf8(rt: &mut JSRuntime, data: Vec<u8>) -> Result<Box<Self>, i32> {
        debug_assert!(!data.is_empty());

        if data.is_ascii() {
            Self::from_content(rt, StringContent::Utf8(data))
        } else {
            let mut u = Vec::<u16>::try_with_capacity(data.len() / 2).map_err(|_| -1)?;
            u.extend(unsafe { String::from_utf8_unchecked(data).encode_utf16() });
            u.shrink_to_fit();
            Self::from_content(rt, StringContent::Utf16(u))
        }
    }

    /// create a new jsstring copying from an other jsstring
    #[inline(always)]
    pub fn copy_from(rt: &mut JSRuntime, str: &JSString) -> Result<Box<Self>, i32> {
        Self::from_content(rt, str.content.clone().map_err(|_| -1)?)
    }

    #[inline(always)]
    pub fn content(&self) -> &StringContent {
        &self.content
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.content.len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline(always)]
    pub fn is_wide_char(&self) -> bool {
        self.content.is_utf16()
    }

    #[inline(always)]
    pub fn hash(&self) -> u32 {
        self.hash_and_type & 0x3FFFFFFF // Mask out the top 2 bits
    }

    pub fn set_hash(&mut self, h: u32) {
        self.hash_and_type = self.hash_and_type & 0xC000_000 | (h & 0x3FFF_FFFF);
    }

    pub fn atom_type(&self) -> JSAtomType {
        unsafe { std::mem::transmute((self.hash_and_type >> 30) as u8) }
    }

    pub fn set_atom_type(&mut self, t: JSAtomType) {
        self.hash_and_type |= (t as u32) << 30
    }

    // #[inline(always)]
    // pub fn as_str8(&self) -> *const u8 {
    //     self.content.as_ptr()
    // }

    // #[inline(always)]
    // pub unsafe fn as_str16(&self) -> *const u16 {
    //     self.content.as_ptr().cast::<u16>()
    // }

    // #[inline(always)]
    // pub unsafe fn as_u16_slice(&self) -> &[u16] {
    //     std::slice::from_raw_parts(self.content.as_ptr().cast::<u16>(), self.len())
    // }

    // #[inline(always)]
    // pub fn as_str8_mut(&mut self) -> *mut u8 {
    //     self.content.as_mut_ptr()
    // }

    // #[inline(always)]
    // pub unsafe fn as_str16_mut(&mut self) -> *mut u16 {
    //     self.content.as_mut_ptr().cast::<u16>()
    // }

    /// decrease ref count, returns updated ref count.
    #[inline(always)]
    pub fn dec_ref(&mut self) -> i32 {
        debug_assert!(self.header.ref_count > 1);
        self.header.ref_count -= 1;
        self.header.ref_count
    }

    /// decrease jsstring ref count.
    /// once ref count reaches zero, drop jsstring and returns None,
    /// otherwise if string is still alive, returns Some(_)
    pub fn free(mut self: Box<Self>, rt: &mut JSRuntime) -> Option<Box<Self>> {
        debug_assert!(self.header.ref_count > 0);

        self.header.ref_count -= 1;

        if self.header.ref_count == 0 {
            if self.atom_type() != JSAtomType::None {
                unsafe {
                    JS_FreeAtomStruct(rt, self.as_mut());
                }
            } else {
                #[cfg(feature = "dump_leaks")]
                unsafe {
                    crate::types::list_del(&mut self.link);
                }
            }
            None // dropped
        } else {
            Some(self) // still alive
        }
    }
}

impl JSContext {
    /// create string value with provieded raw content bytes as-is.
    /// returns JS_EXCEPTION if fail.
    pub unsafe fn create_string_from_content(&mut self, content: StringContent) -> JSValue {
        if let Ok(s) = JSString::from_content(self.rt_mut(), content) {
            JS_MKPTR!(JSTag::String, Box::into_raw(s).cast())
        } else {
            self.JS_ThrowOutOfMemory();
            JSValue::EXCEPTION
        }
    }

    /// create string value with content copied from a str content.
    /// str is not null terminated.
    /// returns JS_EXCEPTION if fail.
    #[deprecated]
    pub unsafe fn create_string8(&mut self, s: &str) -> JSValue {
        unreachable!("unused");

        // let len = s.len();
        // if len == 0 {
        //     JS_AtomToString(self, JS_ATOM_empty_string)
        // } else {
        //     JSString::with_capacity(self, len, false).map_or(JS_EXCEPTION, |mut str| {
        //         std::ptr::copy_nonoverlapping(s.as_ptr(), str.as_str8_mut(), len);
        //         // there must be an extra room for trailing '\0'
        //         str.as_str8_mut().offset(len as _).write(0);
        //         JS_MKPTR!(JSTag::String, Box::into_raw(str).cast())
        //     })
        // }
    }

    /// create string value with content copied from a u16 slice content.
    /// returns JS_EXCEPTION if fail.
    #[deprecated]
    pub unsafe fn create_string16(&mut self, data: &[u16]) -> JSValue {
        unreachable!("unused");

        // let len = data.len();
        // if len == 0 {
        //     JS_AtomToString(self, JS_ATOM_empty_string)
        // } else {
        //     JSString::with_capacity(self, len, true)
        //         .map(|mut s| {
        //             unsafe {
        //                 std::ptr::copy_nonoverlapping(data.as_ptr(), s.as_str16_mut(), len);
        //             }
        //             JS_MKPTR!(JSTag::String, Box::into_raw(s).cast())
        //         })
        //         .unwrap_or(JS_EXCEPTION)
        // }
    }

    /// create string value from a single u16 char.
    /// returns JS_EXCEPTION if fail.
    pub unsafe fn create_string_from_char(&mut self, ch: char) -> JSValue {
        self.create_string_from_content(if ch.is_ascii() {
            StringContent::Utf8(vec![ch as u8])
        } else {
            StringContent::Utf16(ch.encode_utf16(&mut [0; 2]).to_vec())
        })
    }

    /// create string jsvalue from an utf8 encoded str.
    pub fn create_string_from_utf8(&mut self, data: Vec<u8>) -> JSValue {
        if data.is_empty() {
            unsafe { JS_ATOM_empty_string.to_value(self, true) }
        } else if data.len() > JS_STRING_LEN_MAX {
            unsafe { self.JS_ThrowInternalError(c"string too long") }
        } else {
            if let Ok(ss) = JSString::from_utf8(self.rt_mut(), data) {
                JS_MKPTR!(JSTag::String, Box::into_raw(ss).cast())
            } else {
                unsafe {
                    self.JS_ThrowOutOfMemory();
                }
                JSValue::EXCEPTION
            }
        }
    }

    /// create string jsvalue from an utf8 encoded str.
    pub fn create_string(&mut self, str: &str) -> JSValue {
        if str.is_empty() {
            unsafe { JS_ATOM_empty_string.to_value(self, true) }
        } else if str.len() > JS_STRING_LEN_MAX {
            unsafe { self.JS_ThrowInternalError(c"string too long") }
        } else {
            match JSString::from_str(self.rt_mut(), str) {
                Ok(ss) => {
                    JS_MKPTR!(JSTag::String, Box::into_raw(ss).cast())
                }
                Err(_) => {
                    unsafe {
                        self.JS_ThrowOutOfMemory();
                    }
                    JSValue::EXCEPTION
                }
            }
        }
    }
}

#[deprecated(note = "use JSString::new() instead")]
pub unsafe fn js_alloc_string_rt(
    rt: *mut JSRuntime,
    max_len: usize,
    is_wide_char: bool,
) -> *mut JSString {
    unreachable!("use JSString::new() instead");
    // match JSString::new_rt(rt.as_mut().unwrap(), max_len, is_wide_char) {
    //     Ok(s) => Box::into_raw(s),
    //     Err(_) => std::ptr::null_mut(),
    // }
}

#[deprecated(note = "use JSString::new() instead")]
pub unsafe fn js_alloc_string(
    ctx: *mut JSContext,
    max_len: usize,
    is_wide_char: bool,
) -> *mut JSString {
    unreachable!("use JSString::new() instead");

    // match JSString::with_capacity(ctx.as_mut().unwrap(), max_len, is_wide_char) {
    //     Ok(s) => Box::into_raw(s),
    //     Err(_) => std::ptr::null_mut(),
    // }
}

#[deprecated(note = "use JSString::free() instead")]
pub unsafe fn js_free_string(rt: &mut JSRuntime, str: *mut JSString) {
    debug_assert!(!str.is_null());
    let s = Box::from_raw(str);
    let r = s.free(rt);
    debug_assert!(r.is_none());
}

#[deprecated(note = "use StringContent::cmp() instead")]
pub unsafe fn js_string_memcmp(p1: *const JSString, p2: *const JSString, len: usize) -> i32 {
    unreachable!("use StringContent::cmp() instead");

    // let res: i32;
    // if !(*p1).is_wide_char() {
    //     if !(*p2).is_wide_char() {
    //         res = memcmp((*p1).as_str8().cast(), (*p2).as_str8().cast(), len as usize);
    //     } else {
    //         res = -memcmp16_8(
    //             (*p2).as_str16().cast(),
    //             (*p1).as_str8().cast(),
    //             len as usize,
    //         );
    //     }
    // } else {
    //     if !(*p2).is_wide_char() {
    //         res = memcmp16_8(
    //             (*p1).as_str16().cast(),
    //             (*p2).as_str8().cast(),
    //             len as usize,
    //         );
    //     } else {
    //         res = memcmp16(
    //             (*p1).as_str16().cast(),
    //             (*p2).as_str16().cast(),
    //             len as usize,
    //         );
    //     }
    // }
    // res
}

/* return < 0, 0 or > 0 */
#[deprecated(note = "use StringContent::cmp() instead")]
pub unsafe fn js_string_compare(
    ctx: *mut JSContext,
    p1: *const JSString,
    p2: *const JSString,
) -> i32 {
    match (*p1).content().cmp(p2.as_ref().unwrap().content()) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

unsafe fn memcmp16(a: *const u16, b: *const u16, len: usize) -> i32 {
    for i in 0..len {
        let ai = *a.add(i);
        let bi = *b.add(i);
        if ai != bi {
            return ai as i32 - bi as i32;
        }
    }
    0
}

unsafe fn memcmp16_8(a: *const u16, b: *const u8, len: usize) -> i32 {
    for i in 0..len {
        let ai = *a.add(i);
        let bi = *b.add(i) as u16;
        if ai != bi {
            return ai as i32 - bi as i32;
        }
    }
    0
}

// #[inline(always)]
// pub unsafe fn hash_string8(str: &str, mut h: u32) -> u32 {
//     for c in str.as_bytes() {
//         h = hash_char(char::from_u32_unchecked((*c) as u32), h);
//     }
//     h
// }

// #[inline(always)]
// pub unsafe fn hash_string16(str: &[u16], mut h: u32) -> u32 {
//     for c in str {
//         h = hash_char(char::from_u32_unchecked(*c as u32), h);
//     }
//     h
// }

#[deprecated(note = "use StringContent::hash() instead")]
pub unsafe fn hash_string(str: *const JSString, h: u32) -> u32 {
    unreachable!("use StringContent::hash() instead");
}

#[inline(always)]
pub fn is_num(c: i32) -> bool {
    ('0' as i32..='9' as i32).contains(&c)
}

#[deprecated(note = "use StringContent::to_u32() instead")]
pub unsafe fn is_num_string(pval: &mut u32, p: *const JSString) -> bool {
    match (*p).content().parse_u32() {
        Ok(n) => {
            *pval = n;
            true
        }
        Err(_) => false,
    }

    // let mut n: u32;
    // let len = (*p).len();
    // if len == 0 || len > 10 {
    //     return false;
    // }

    // for (i, c) in (*p).content().chars().enumerate() {
    //     if c.is_digit(10) {
    //         if i == 0 && c == '0' {
    //             return if len != 1 {
    //                 false
    //             } else {
    //                 *pval = 0;
    //                 true
    //             };
    //         }

    //         let n64: u64 = (n as u64) * 10 + (c as u64 - '0' as u64);
    //         if (n64 >> 32) != 0 {
    //             return false; // u32 overflow
    //         }
    //         n = n64 as u32;
    //     } else {
    //         return false;
    //     }
    // }

    // *pval = n;
    // true
}

pub unsafe fn JS_ToStringInternal(
    ctx: *mut JSContext,
    val: JSValueConst,
    is_ToPropertyKey: bool,
) -> JSValue {
    match JS_VALUE_GET_NORM_TAG(val) {
        JSTag::String => JS_DupValue(ctx, val),
        JSTag::Int => (*ctx).create_string(&format!("{}", JS_VALUE_GET_INT(val))),
        JSTag::Bool => if JS_VALUE_GET_BOOL(val) {
            JS_ATOM_true
        } else {
            JS_ATOM_false
        }
        .to_value(ctx.as_mut().unwrap(), true),
        JSTag::Null => JS_ATOM_null.to_value(ctx.as_mut().unwrap(), true),
        JSTag::Undefined => JS_ATOM_undefined.to_value(ctx.as_mut().unwrap(), true),
        JSTag::Exception => JSValue::EXCEPTION,
        JSTag::Object => {
            let val1 = JS_ToPrimitive(ctx, &val, HINT_STRING);
            if JS_IsException(val1) {
                val1
            } else {
                let ret = JS_ToStringInternal(ctx, val1, is_ToPropertyKey);
                JS_FreeValue(ctx, val1);
                ret
            }
        }
        JSTag::FunctionBytecode => (*ctx).create_string("[function bytecode]"),
        JSTag::Symbol => {
            if is_ToPropertyKey {
                JS_DupValue(ctx, val)
            } else {
                ctx.as_mut()
                    .unwrap()
                    .JS_ThrowTypeError(c"cannot convert symbol to string")
            }
        }
        JSTag::Float64 => js_dtoa(ctx, JS_VALUE_GET_FLOAT64(val), 10, 0, JS_DTOA_VAR_FORMAT),
        JSTag::Module => (*ctx).create_string("[module]"),
        JSTag::Uninitialized => (*ctx).create_string("[uninitialized]"),
        JSTag::CatchOffset => (*ctx).create_string("[cacthoffset]"),
        JSTag::BigDecimal | JSTag::BigFloat | JSTag::BigInt => {
            unimplemented!()
        }
    }
}

pub unsafe fn JS_ToString(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    JS_ToStringInternal(ctx, val, false)
}

pub unsafe fn JS_ToStringFree(ctx: *mut JSContext, val: JSValue) -> JSValue {
    let ret = JS_ToString(ctx, val);
    JS_FreeValue(ctx, val);
    ret
}

pub unsafe fn JS_ToLocaleStringFree(ctx: *mut JSContext, val: JSValue) -> JSValue {
    if JS_IsUndefined(val) || JS_IsNull(val) {
        JS_ToStringFree(ctx, val)
    } else {
        JS_InvokeFree(ctx, val, JS_ATOM_toLocaleString, 0, std::ptr::null_mut())
    }
}

pub unsafe fn JS_ToStringCheckObject(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    if matches!(JS_VALUE_GET_TAG(val), JS_TAG_NULL | JS_TAG_UNDEFINED) {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"null or undefined are forbidden")
    } else {
        JS_ToString(ctx, val)
    }
}

#[inline(always)]
pub unsafe fn JS_IsEmptyString(v: JSValueConst) -> bool {
    JS_VALUE_GET_TAG(v) == JS_TAG_STRING && (*JS_VALUE_GET_STRING(v)).len() == 0
}

/// Note: string value `str2` is consumed.
pub unsafe fn JS_ConcatString3(
    ctx: *mut JSContext,
    str1: &CStr,
    mut str2: JSValue,
    str3: &CStr,
) -> JSValue {
    macro_rules! fail {
        () => {
            JS_FreeValue(ctx, str2);
            return JSValue::EXCEPTION;
        };
    }

    if unlikely(JS_VALUE_GET_TAG(str2) != JS_TAG_STRING) {
        str2 = JS_ToStringFree(ctx, str2);
        if str2.is_exception() {
            fail!();
        }
    }

    let p = JS_VALUE_GET_STRING(str2);
    let len1 = str1.count_bytes();
    let len3 = str3.count_bytes();

    let mut res_string = if let Ok(s) =
        StringContent::with_capacity(len1 + (*p).len() + len3, (*p).is_wide_char())
    {
        s
    } else {
        fail!();
    };

    let r = res_string
        .push_str(str1.to_string_lossy().as_ref())
        .and_then(|_| res_string.push((*p).content()))
        .and_then(|_| res_string.push_str(str3.to_string_lossy().as_ref()));

    JS_FreeValue(ctx, str2);

    if r.is_err() {
        fail!();
    }

    (*ctx).create_string_from_content(res_string)
}

pub unsafe fn copy_str16(dst: *mut u16, p: *const JSString, offset: usize, len: usize) {
    unreachable!("unused");

    // if (*p).is_wide_char() {
    //     std::ptr::copy((*p).as_str16().add(offset), dst, len);
    // } else {
    //     let src1 = (*p).as_str8().add(offset);
    //     for i in 0..len {
    //         dst.add(i).write(src1.add(i).read() as u16);
    //     }
    // }
}

#[deprecated]
pub unsafe fn JS_ConcatString1(
    ctx: *mut JSContext,
    p1: *const JSString,
    p2: *const JSString,
) -> JSValue {
    unreachable!("unused");

    // let len = (*p1).len() + (*p2).len();
    // if len > JS_STRING_LEN_MAX {
    //     return ctx
    //         .as_mut()
    //         .unwrap()
    //         .JS_ThrowInternalError(c"string too long");
    // }
    // let is_wide_char = (*p1).is_wide_char() | (*p2).is_wide_char();
    // let mut p = if let Ok(s) = JSString::with_capacity(ctx.as_mut().unwrap(), len, is_wide_char) {
    //     s
    // } else {
    //     return JSValue::EXCEPTION;
    // };

    // if !is_wide_char {
    //     std::ptr::copy((*p1).as_str8(), p.as_str8_mut(), (*p1).len());
    //     std::ptr::copy(
    //         (*p2).as_str8(),
    //         (*p).as_str8_mut().add((*p1).len()),
    //         (*p2).len(),
    //     );
    //     *p.as_str8_mut().add(len) = 0;
    // } else {
    //     copy_str16(p.as_str16_mut(), p1, 0, (*p1).len());
    //     copy_str16(p.as_str16_mut().add((*p1).len()), p2, 0, (*p2).len());
    // }

    // return JS_MKPTR!(JSTag::String, Box::into_raw(p).cast());
}

/// op1 and op2 are converted to strings.
/// For convience, op1 or op2 = JS_EXCEPTION are accepted and return JSValue::EXCEPTION.
/// op1 and op2 are freed
pub unsafe fn JS_ConcatString(ctx: *mut JSContext, op1: JSValue, op2: JSValue) -> JSValue {
    // if unlikely(JS_VALUE_GET_TAG(op1) != JS_TAG_STRING) {
    //     op1 = JS_ToStringFree(ctx, op1);
    //     if JS_IsException(op1) {
    //         JS_FreeValue(ctx, op2);
    //         return JSValue::EXCEPTION;
    //     }
    // }
    // if unlikely(JS_VALUE_GET_TAG(op2) != JS_TAG_STRING) {
    //     op2 = JS_ToStringFree(ctx, op2);
    //     if JS_IsException(op2) {
    //         JS_FreeValue(ctx, op1);
    //         return JSValue::EXCEPTION;
    //     }
    // }

    // let p1 = JS_VALUE_GET_STRING(op1);
    // let p2 = JS_VALUE_GET_STRING(op2);

    // if (*p1).is_empty() {
    //     JS_FreeValue(ctx, op1);
    //     return op2;
    // } else if (*p2).is_empty() {
    //     JS_FreeValue(ctx, op2);
    //     return op1;
    // }

    // if (*p1).header.ref_count == 1 {
    //     if (*p1).content.push(&(*p2).content).is_err() {
    //         (*ctx).JS_ThrowOutOfMemory();
    //         JS_EXCEPTION
    //     } else {
    //         JS_FreeValue(ctx, op2);
    //         op1
    //     }
    // } else if let Ok(new_sc) = (*p1).content().clone().and_then(|mut s| {
    //     s.push((*p2).content())?;
    //     Ok(s)
    // }) {
    //     let res = (*ctx).create_string_from_content(new_sc);
    //     if !JS_IsException(res) {
    //         JS_FreeValue(ctx, op1);
    //         JS_FreeValue(ctx, op2);
    //     }
    //     res
    // } else {
    //     JS_EXCEPTION
    // }

    let str1 = if unlikely(JS_VALUE_GET_TAG(op1) != JS_TAG_STRING) {
        let v = JS_ToStringFree(ctx, op1);
        if JS_IsException(op1) {
            JS_FreeValue(ctx, op2);
            return JSValue::EXCEPTION;
        }
        v.into_rc((*ctx).rt())
    } else {
        op1.into_rc((*ctx).rt())
    };

    let str2 = if unlikely(JS_VALUE_GET_TAG(op2) != JS_TAG_STRING) {
        let v = JS_ToStringFree(ctx, op2);
        if JS_IsException(v) {
            //JS_FreeValue(ctx, op1);
            return JSValue::EXCEPTION;
        }
        v.into_rc((*ctx).rt())
    } else {
        op2.into_rc((*ctx).rt())
    };

    let p1 = JS_VALUE_GET_STRING(*str1);
    let p2 = JS_VALUE_GET_STRING(*str2);

    if (*p1).is_empty() {
        return str2.into();
    } else if (*p2).is_empty() {
        return str1.into();
    }

    if (*p1).header.ref_count == 1 {
        if (*p1).content.push(&(*p2).content).is_err() {
            (*ctx).JS_ThrowOutOfMemory();
            JSValue::EXCEPTION
        } else {
            str1.into()
        }
    } else if let Ok(new_sc) = (*p1).content().clone().and_then(|mut s| {
        s.push((*p2).content())?;
        Ok(s)
    }) {
        (*ctx).create_string_from_content(new_sc)
    } else {
        JSValue::EXCEPTION
    }
}

#[deprecated(note = "use JSContext::create_string8() instead")]
pub unsafe fn js_new_string8(ctx: *mut JSContext, buf: *const u8, len: usize) -> JSValue {
    unreachable!("js_new_string8() is obsolete");
}

#[deprecated(note = "use JSContext::create_string16() instead")]
pub unsafe fn js_new_string16(ctx: *mut JSContext, buf: *const u16, len: usize) -> JSValue {
    unreachable!("js_new_string16() is obsolete");
}

#[deprecated(note = "use JSContext::create_string_char() instead")]
pub unsafe fn js_new_string_char(ctx: *mut JSContext, c: u16) -> JSValue {
    unreachable!("js_new_string_char() is obsolete");
}

#[deprecated(note = "use JSContext::create_string() instead")]
pub unsafe fn JS_NewStringLen(ctx: *mut JSContext, buf: *const u8, buf_len: usize) -> JSValue {
    (*ctx).create_string(std::str::from_utf8_unchecked(std::slice::from_raw_parts(
        buf, buf_len,
    )))
}

#[deprecated(note = "use JSContext::create_string_utf8() instead")]
pub unsafe fn JS_NewString(ctx: *mut JSContext, str: &CStr) -> JSValue {
    (*ctx).create_string(str.to_string_lossy().as_ref())
}

impl JSValue {
    /// Convert a JSValue to String. the value will not be freed.
    /// Returns either a CStr or a CString
    pub unsafe fn to_utf8_string(self, ctx: &mut JSContext) -> Result<Cow<'_, str>, i32> {
        let mut str_val: Option<JSValue> = None;
        let jss = if JS_VALUE_GET_TAG(self) == JSTag::String as i32 {
            JS_VALUE_GET_STRING(self)
        } else {
            let ts = JS_ToString(ctx, self);
            if JS_IsException(ts) {
                return Err(-1);
            }
            let s = JS_VALUE_GET_STRING(ts);
            str_val.replace(ts);
            s
        };

        let mut s = (*jss).content.to_str();
        if let Some(sv) = str_val {
            let _ = s.to_mut(); // owned
            JS_FreeValue(ctx, sv);
        }

        Ok(s)

        // if (*jss).is_wide_char() {
        //     // utf16 encoded
        //     let mut buf = SmallVec::<[u8; 128]>::new();
        //     let mut p = buf.as_mut_ptr();

        //     for &uc in (*jss).as_u16_slice() {
        //         let n = unicode_to_utf8(p, uc as _);
        //         if n > 0 {
        //             p = p.add(n as _);
        //             buf.set_len(buf.len() + n as usize);

        //             if buf.capacity() - buf.len() <= 7 {
        //                 if buf.try_reserve_exact(64).is_err() {
        //                     if let Some(sv) = str_val {
        //                         JS_FreeValue(ctx, sv);
        //                     }
        //                     return Err(-1);
        //                 }
        //             }
        //         }
        //     }

        //     buf.push(0);

        //     if let Ok(res) = CString::from_vec_with_nul(buf.into_vec()) {
        //         Ok(Cow::Owned(res))
        //     } else {
        //         if let Some(sv) = str_val {
        //             JS_FreeValue(ctx, sv);
        //         }
        //         Err(-1)
        //     }
        // } else {
        //     // utf8 encoded string
        //     if let Some(s) = str_val {
        //         let cs = CString::from_vec_with_nul((*jss).content.clone().into());
        //         JS_FreeValue(ctx, s);
        //         Ok(Cow::Owned(cs.map_err(|_| -1)?))
        //     } else {
        //         Ok(Cow::Borrowed(CStr::from_ptr(
        //             (*jss).content.as_ptr().cast(),
        //         )))
        //     }
        // }
    }
}

/* return (NULL, 0) if exception. */
/* return pointer into a JSString with a live ref_count */
/* cesu8 determines if non-BMP1 codepoints are encoded as 1 or 2 utf-8 sequences */
#[deprecated(note = "use JSValue::to_utf8_string() instead")]
pub unsafe fn JS_ToCStringLen2(
    ctx: *mut JSContext,
    plen: *mut usize,
    val1: JSValueConst,
    cesu8: bool,
) -> *const c_char {
    unreachable!("use JSValue::to_utf8_string() instead");

    // let val = if JS_VALUE_GET_TAG(val1) != JSTag::String as i32 {
    //     let v = JS_ToString(ctx, val1);
    //     if JS_IsException(v) {
    //         return std::ptr::null();
    //     }
    //     v
    // } else {
    //     JS_DupValue(ctx, val1)
    // };

    // let str = JS_VALUE_GET_STRING(val);
    // let len = (*str).len();

    // let str_new: *mut JSString;
    // let mut q: *mut u8;

    // if !(*str).is_wide_char() {
    //     let src = (*str).as_str8();

    //     /* count the number of non-ASCII characters */
    //     /* Scanning the whole string is required for ASCII strings,
    //       and computing the number of non-ASCII bytes is less expensive
    //       than testing each byte, hence this method is faster for ASCII
    //       strings, which is the most common case.
    //     */
    //     let mut count = 0_i32;
    //     for pos in 0..len {
    //         count += (*src.add(pos) >> 7) as i32;
    //     }
    //     if count == 0 {
    //         if !plen.is_null() {
    //             (*plen) = len;
    //         }

    //         return src as *const c_char;
    //     }

    //     str_new = if let Ok(mut s) =
    //         JSString::with_capacity(ctx.as_mut().unwrap(), len + count as usize, false)
    //     {
    //         q = s.as_str8_mut();
    //         for pos in 0..len {
    //             let c = *src.offset(pos as isize);
    //             if c < 0x80 {
    //                 *q = c;
    //                 q = q.offset(1);
    //             } else {
    //                 *q = (c >> 6) | 0xc0;
    //                 q = q.offset(1);
    //                 *q = (c & 0x3f) | 0x80;
    //                 q = q.offset(1);
    //             }
    //         }

    //         Box::into_raw(s)
    //     } else {
    //         return std::ptr::null();
    //     };
    // } else {
    //     // wide string
    //     let src = (*str).as_str16();

    //     /* Allocate 3 bytes per 16 bit code point. Surrogate pairs may
    //       produce 4 bytes but use 2 code points.
    //     */
    //     str_new = if let Ok(mut s) = JSString::with_capacity(ctx.as_mut().unwrap(), len * 3, false)
    //     {
    //         q = s.as_str8_mut();
    //         let mut pos = 0;
    //         while pos < len {
    //             let mut uc = *src.offset(pos as isize) as i32;
    //             pos += 1;

    //             if uc < 0x80 {
    //                 *q = uc as u8;
    //                 q = q.offset(1);
    //             } else {
    //                 if (0xd800..0xdc00).contains(&uc) {
    //                     if pos < len && !cesu8 {
    //                         let c1 = *src.offset(pos as isize) as i32;
    //                         if (0xdc00..0xe000).contains(&c1) {
    //                             pos += 1;
    //                             /* surrogate pair */
    //                             uc = (((uc & 0x3FF) << 10) | (c1 & 0x3FF)) + 0x10000;
    //                         }
    //                     }
    //                 }
    //                 let written = unicode_to_utf8(q, uc as _);
    //                 q = q.offset(written as isize);
    //             }
    //         }

    //         Box::into_raw(s)
    //     } else {
    //         return std::ptr::null();
    //     };
    // }

    // *q = 0;
    // (*str_new).set_len(q.offset_from((*str_new).as_str8()) as usize);
    // JS_FreeValue(ctx, val);

    // if !plen.is_null() {
    //     *plen = (*str_new).len() as usize;
    // }

    // (*str_new).as_str8().cast()
}

#[deprecated(note = "use JSValue::to_utf8_string() instead")]
pub unsafe fn JS_ToCStringLen(
    ctx: *mut JSContext,
    plen: *mut usize,
    val1: JSValueConst,
) -> *const c_char {
    JS_ToCStringLen2(ctx, plen, val1, false)
}

#[deprecated(note = "use JSValue::to_utf8_string() instead")]
pub unsafe fn JS_ToCString(ctx: *mut JSContext, val1: JSValueConst) -> *const c_char {
    JS_ToCStringLen2(ctx, std::ptr::null_mut(), val1, false)
}

#[deprecated(note = "not used")]
pub unsafe fn JS_FreeCString(ctx: *mut JSContext, ptr: *const c_char) {
    unreachable!("JS_FreeCString() is not used any more");

    if ptr.is_null() {
        return;
    }

    // ptr是 JS_ToCString() 系列返回返回的指针，它是JSString中 content 字段的地址
    // 可以通过ptr计算出JSString的地址
    let off = offset_of!(JSString, content) as isize;
    let p = ptr.offset(-off).cast::<JSString>() as *mut JSString;
    // let p = container_of!(ptr, JSString, content) as *mut JSString;
    println!("ptr={ptr:p}, p={p:p}");
    debug_assert_eq!(p, container_of!(ptr, JSString, content) as *mut JSString);

    JS_FreeValue(ctx, JS_MKPTR!(JSTag::String, p.cast()));
}

pub unsafe fn js_string_obj_get_length(ctx: *mut JSContext, obj: JSValueConst) -> usize {
    // This is a class exotic method: obj class_id is JS_CLASS_STRING
    let p = JS_VALUE_GET_OBJ(obj);

    if JS_VALUE_GET_TAG((*p).u.object_data) == JS_TAG_STRING {
        let p1 = JS_VALUE_GET_STRING((*p).u.object_data);
        (*p1).len()
    } else {
        0
    }
}

/// starts_with() equivelent
pub unsafe fn strstart(str: *const u8, val: *const u8, ptr: *mut *const u8) -> bool {
    let mut p = str;
    let mut q = val;
    while *q != 0 {
        if *p != *q {
            return false;
        }
        p = p.offset(1);
        q = q.offset(1);
    }
    if !ptr.is_null() {
        *ptr = p;
    }
    return true;
}

//
// String class methods
//
pub unsafe fn js_string_get_own_property(
    ctx: *mut JSContext,
    desc: *mut JSPropertyDescriptor,
    obj: JSValueConst,
    prop: JSAtom,
) -> i32 {
    // This is a class exotic method: obj class_id is JS_CLASS_STRING
    if prop.is_int_type() {
        let p = JS_VALUE_GET_OBJ(obj).as_mut().unwrap();
        if JS_VALUE_GET_TAG((*p).u.object_data) == JS_TAG_STRING {
            let p1 = JS_VALUE_GET_STRING((*p).u.object_data).as_mut().unwrap();
            let idx = __JS_AtomToUInt32(prop);
            if idx < p1.len() as u32 {
                if !desc.is_null() {
                    let ch = p1.content().chars().nth(idx as usize).unwrap();
                    (*desc).flags = JS_PROP_ENUMERABLE as _;
                    (*desc).value = (*ctx).create_string_from_char(ch);
                    (*desc).getter = JS_UNDEFINED;
                    (*desc).setter = JS_UNDEFINED;
                }

                return 1;
            }
        }
    }

    0
}

pub unsafe fn js_string_define_own_property(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValueConst,
    getter: JSValueConst,
    setter: JSValueConst,
    flags: i32,
) -> i32 {
    macro_rules! fail {
        () => {
            return ctx
                .as_mut()
                .unwrap()
                .JS_ThrowTypeErrorOrFalse(flags as _, c"property is not configurable");
        };
    }
    macro_rules! def {
        () => {
            return JS_DefineProperty(
                ctx,
                this_obj,
                prop,
                val,
                getter,
                setter,
                flags as u32 | JS_PROP_NO_EXOTIC,
            );
        };
    }

    if prop.is_int_type() {
        let idx = __JS_AtomToUInt32(prop);
        let p = JS_VALUE_GET_OBJ(this_obj).as_mut().unwrap();
        if JS_VALUE_GET_TAG((*p).u.object_data) != JS_TAG_STRING {
            def!();
        }
        let p1 = JS_VALUE_GET_STRING((*p).u.object_data).as_mut().unwrap();
        if idx >= (*p1).len() as u32 {
            def!();
        }
        if !check_define_prop_flags(JS_PROP_ENUMERABLE, flags as _) {
            fail!();
        }
        // check that the same value is configured
        if flags & JS_PROP_HAS_VALUE as i32 != 0 {
            if JS_VALUE_GET_TAG(val) != JS_TAG_STRING {
                fail!();
            }
            let p2 = JS_VALUE_GET_STRING(val).as_mut().unwrap();
            if (*p2).len() != 1 {
                fail!();
            }
            if (*p1).content().chars().nth(idx as usize) != (*p2).content().chars().next() {
                fail!();
            }
        }
        return 1;
    } else {
        def!();
    }
}

pub unsafe fn js_string_delete_property(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
) -> i32 {
    if prop.is_int_type() {
        let idx = __JS_AtomToUInt32(prop);
        if idx < js_string_obj_get_length(ctx, obj) as u32 {
            return 0;
        }
    }
    return 0;
}

pub static js_string_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    get_own_property: Some(js_string_get_own_property),
    define_own_property: Some(js_string_define_own_property),
    delete_property: Some(js_string_delete_property),
    get_own_property_names: None,
    has_property: None,
    get_property: None,
    set_property: None,
};
