use std::ffi::{c_char, CStr};

use libc::memcmp;

use crate::{
    atom::{
        JSAtom, JSAtomType, JS_AtomToString, JS_FreeAtomStruct, __JS_AtomIsTaggedInt,
        __JS_AtomToUInt32,
    },
    class::JSClassExoticMethods,
    container_of,
    context::JSContext,
    mem::{js_alloc_string, js_free_rt, js_malloc_usable_size},
    primitives::{js_dtoa, JS_ToPrimitive, JS_DTOA_VAR_FORMAT},
    property::{
        check_define_prop_flags, JSPropertyDescriptor, JS_DefineProperty, JS_PROP_ENUMERABLE,
        JS_PROP_HAS_VALUE, JS_PROP_NO_EXOTIC,
    },
    runtime::JSRuntime,
    stringbuf::{
        string_buffer_concat, string_buffer_end, string_buffer_free, string_buffer_init,
        string_buffer_init2, string_buffer_putc16, string_buffer_putc8, string_buffer_write8,
        string_get, StringBuffer,
    },
    types::{list_head, unlikely, JSRefCountHeader},
    unicode::Utf8ToUnicode16Iter,
    value::{
        JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_IsException, HINT_STRING,
        JS_EXCEPTION, JS_TAG_BIG_INT, JS_TAG_BOOL, JS_TAG_EXCEPTION, JS_TAG_FLOAT64,
        JS_TAG_FUNCTION_BYTECODE, JS_TAG_INT, JS_TAG_NULL, JS_TAG_OBJECT, JS_TAG_STRING,
        JS_TAG_SYMBOL, JS_TAG_UNDEFINED, JS_UNDEFINED, JS_VALUE_GET_BOOL, JS_VALUE_GET_FLOAT64,
        JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ, JS_VALUE_GET_STRING,
        JS_VALUE_GET_TAG,
    },
    JS_ATOM_empty_string, JS_ATOM_false, JS_ATOM_null, JS_ATOM_true, JS_ATOM_undefined, JS_MKPTR,
};

pub const JS_STRING_LEN_MAX: usize = (1 << 30) - 1;

#[repr(C)]
pub struct JSString {
    pub header: JSRefCountHeader, // must come first, 32-bit
    pub len_and_flags: u32,       // len: 31 bits, is_wide_char: 1 bit
    hash_and_type: u32,           // hash: 30 bits, atom_type: 2 bits
    pub hash_next: u32,           // atom_index for JS_ATOM_TYPE_SYMBOL

    #[cfg(feature = "dump_leaks")]
    pub link: list_head, // string list

    pub u: JSStringUnion,
}

#[repr(C)]
pub union JSStringUnion {
    pub str8: [u8; 0],   // 8-bit string data
    pub str16: [u16; 0], // 16-bit string data
}

impl std::fmt::Debug for JSString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JSString").finish()
    }
}

impl JSString {
    pub fn len(&self) -> usize {
        (self.len_and_flags & 0x7FFFFFFF) as _ // Mask out the top bit
    }

    pub fn set_len(&mut self, len: usize) {
        self.len_and_flags = (self.len_and_flags & 0x8000_0000) | (len as u32 & 0x7FFF_FFFF);
    }

    pub fn is_wide_char(&self) -> bool {
        (self.len_and_flags & 0x80000000) != 0 // Check the top bit
    }

    pub fn set_wide_char(&mut self, b: bool) {
        if b {
            self.len_and_flags |= 0x8000_0000
        } else {
            self.len_and_flags &= 0x7FFF_FFFF
        }
    }

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

    pub unsafe fn as_str8(&self) -> *const u8 {
        &self.u.str8 as *const u8
    }

    pub unsafe fn as_str16(&self) -> *const u16 {
        &self.u.str16 as *const u16
    }
}

pub unsafe fn js_free_string(rt: &mut JSRuntime, str: *mut JSString) {
    let s = str.as_mut().unwrap();

    s.header.ref_count -= 1;
    if s.header.ref_count <= 0 {
        if s.atom_type() != JSAtomType::None {
            JS_FreeAtomStruct(rt, s);
        } else {
            #[cfg(feature = "dump_leaks")]
            crate::types::list_del(&mut (*str).link);

            js_free_rt(rt, str.cast());
        }
    }
}

pub unsafe fn js_string_memcmp(p1: *const JSString, p2: *const JSString, len: usize) -> i32 {
    let res: i32;
    if !(*p1).is_wide_char() {
        if !(*p2).is_wide_char() {
            res = memcmp(
                (*p1).u.str8.as_ptr().cast(),
                (*p2).u.str8.as_ptr().cast(),
                len as usize,
            );
        } else {
            res = -memcmp16_8(
                (*p2).u.str16.as_ptr().cast(),
                (*p1).u.str8.as_ptr().cast(),
                len as usize,
            );
        }
    } else {
        if !(*p2).is_wide_char() {
            res = memcmp16_8(
                (*p1).u.str16.as_ptr().cast(),
                (*p2).u.str8.as_ptr().cast(),
                len as usize,
            );
        } else {
            res = memcmp16(
                (*p1).u.str16.as_ptr().cast(),
                (*p2).u.str16.as_ptr().cast(),
                len as usize,
            );
        }
    }
    res
}

/* return < 0, 0 or > 0 */
pub unsafe fn js_string_compare(
    ctx: *mut JSContext,
    p1: *const JSString,
    p2: *const JSString,
) -> i32 {
    let len = std::cmp::min((*p1).len(), (*p2).len());
    let mut res = js_string_memcmp(p1, p2, len as _);
    if res == 0 {
        if (*p1).len() == (*p2).len() {
            res = 0;
        } else if (*p1).len() < (*p2).len() {
            res = -1;
        } else {
            res = 1;
        }
    }

    return res;
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

#[inline]
pub unsafe fn hash_string8(str: *const u8, len: usize, mut h: u32) -> u32 {
    for i in 0..len {
        h = h.wrapping_mul(263).wrapping_add(*str.add(i) as u32);
    }
    h
}

#[inline]
pub unsafe fn hash_string16(str: *const u16, len: usize, mut h: u32) -> u32 {
    for i in 0..len {
        h = h.wrapping_mul(263).wrapping_add(*str.add(i) as u32);
    }
    h
}

#[inline]
pub unsafe fn hash_string(str: *const JSString, h: u32) -> u32 {
    let s = &*str;
    let h = if s.is_wide_char() {
        hash_string16(s.u.str16.as_ptr(), s.len() as usize, h)
    } else {
        hash_string8(s.u.str8.as_ptr(), s.len() as usize, h)
    };
    h
}

#[inline(always)]
pub fn is_num(c: i32) -> bool {
    c >= '0' as i32 && c <= '9' as i32
}

pub unsafe fn is_num_string(pval: &mut u32, p: *const JSString) -> bool {
    let mut n: u32;
    let mut n64: u64;

    let len = (*p).len();
    if len == 0 || len > 10 {
        return false;
    }

    let c: i32 = if (*p).is_wide_char() {
        *(*p).u.str16.as_ptr() as i32
    } else {
        *(*p).u.str8.as_ptr() as i32
    };
    if !is_num(c) {
        return false;
    }

    if c == '0' as i32 {
        if len != 1 {
            return false;
        }
        n = 0;
    } else {
        n = (c - '0' as i32) as u32;
        for i in 1..len {
            let c = if (*p).is_wide_char() {
                *(*p).u.str16.as_ptr().offset(i as _) as i32
            } else {
                *(*p).u.str8.as_ptr().offset(i as _) as i32
            };
            if !is_num(c) {
                return false;
            }

            n64 = (n as u64) * 10 + (c as u64 - '0' as u64);
            if (n64 >> 32) != 0 {
                return false;
            }

            n = n64 as u32;
        }
    }

    *pval = n;
    true
}

pub unsafe fn JS_ToStringInternal(
    ctx: *mut JSContext,
    val: JSValueConst,
    is_ToPropertyKey: bool,
) -> JSValue {
    let mut str: *const c_char;
    let mut buf = [0u8; 32];

    let tag = JS_VALUE_GET_NORM_TAG(val);
    match tag {
        JS_TAG_STRING => return JS_DupValue(ctx, val),
        JS_TAG_INT => {
            libc::snprintf(
                buf.as_mut_ptr() as *mut c_char,
                buf.len(),
                b"%d\0".as_ptr() as *const c_char,
                JS_VALUE_GET_INT(val),
            );
            return JS_NewString(ctx, CStr::from_bytes_with_nul_unchecked(buf.as_slice()));
        }
        JS_TAG_BOOL => {
            let atom = if JS_VALUE_GET_BOOL(val) {
                JS_ATOM_true
            } else {
                JS_ATOM_false
            };
            return JS_AtomToString(ctx, atom);
        }
        JS_TAG_NULL => return JS_AtomToString(ctx, JS_ATOM_null),
        JS_TAG_UNDEFINED => return JS_AtomToString(ctx, JS_ATOM_undefined),
        JS_TAG_EXCEPTION => return JS_EXCEPTION,
        JS_TAG_OBJECT => {
            let val1 = JS_ToPrimitive(ctx, &val, HINT_STRING);
            if JS_IsException(val1) {
                return val1;
            }
            let ret = JS_ToStringInternal(ctx, val1, is_ToPropertyKey);
            JS_FreeValue(ctx, val1);
            return ret;
        }
        JS_TAG_FUNCTION_BYTECODE => {
            return JS_NewString(ctx, c"[function bytecode]");
        }
        JS_TAG_SYMBOL => {
            if is_ToPropertyKey {
                return JS_DupValue(ctx, val);
            } else {
                return ctx
                    .as_mut()
                    .unwrap()
                    .JS_ThrowTypeError(c"cannot convert symbol to string");
            }
        }
        JS_TAG_FLOAT64 => {
            return js_dtoa(ctx, JS_VALUE_GET_FLOAT64(val), 10, 0, JS_DTOA_VAR_FORMAT)
        }
        JS_TAG_BIG_INT => {
            unimplemented!()
            // return ((*ctx).rt).bigint_ops.to_string(ctx, val);
        }

        #[cfg(feature = "bignum")]
        JS_TAG_BIG_FLOAT => {
            return ((*ctx).rt).bigfloat_ops.to_string(ctx, val);
        }
        #[cfg(feature = "bignum")]
        JS_TAG_BIG_DECIMAL => {
            return ((*ctx).rt).bigdecimal_ops.to_string(ctx, val);
        }

        _ => {
            return JS_NewString(ctx, c"[unsupported type]");
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

#[inline(always)]
pub unsafe fn JS_IsEmptyString(v: JSValueConst) -> bool {
    JS_VALUE_GET_TAG(v) == JS_TAG_STRING && (*JS_VALUE_GET_STRING(v)).len() == 0
}

pub unsafe fn JS_ConcatString3(
    ctx: *mut JSContext,
    str1: &CStr,
    mut str2: JSValue,
    str3: &CStr,
) -> JSValue {
    macro_rules! fail {
        () => {
            JS_FreeValue(ctx, str2);
            return JS_EXCEPTION;
        };
    }

    let mut b_s: StringBuffer = std::mem::zeroed();
    let b = &mut b_s;

    if unlikely(JS_VALUE_GET_TAG(str2) != JS_TAG_STRING) {
        str2 = JS_ToStringFree(ctx, str2);
        if JS_IsException(str2) {
            fail!();
        }
    }
    let p = JS_VALUE_GET_STRING(str2);

    let len1 = str1.count_bytes();
    let len3 = str3.count_bytes();

    if string_buffer_init2(ctx, b, len1 + (*p).len() + len3, (*p).is_wide_char()) != 0 {
        fail!();
    }

    string_buffer_write8(b, str1.as_ptr().cast(), len1);
    string_buffer_concat(b, p, 0, (*p).len());
    string_buffer_write8(b, str3.as_ptr().cast(), len3);

    JS_FreeValue(ctx, str2);
    return string_buffer_end(b);
}

pub unsafe fn copy_str16(dst: *mut u16, p: *const JSString, offset: usize, len: usize) {
    if (*p).is_wide_char() {
        std::ptr::copy((*p).u.str16.as_ptr().add(offset), dst, len);
    } else {
        let src1 = (*p).u.str8.as_ptr().add(offset);
        for i in 0..len {
            dst.add(i).write(src1.add(i).read() as u16);
        }
    }
}

pub unsafe fn JS_ConcatString1(
    ctx: *mut JSContext,
    p1: *const JSString,
    p2: *const JSString,
) -> JSValue {
    let len = (*p1).len() + (*p2).len();
    if len > JS_STRING_LEN_MAX {
        return ctx
            .as_mut()
            .unwrap()
            .JS_ThrowInternalError(c"string too long");
    }
    let is_wide_char = (*p1).is_wide_char() | (*p2).is_wide_char();
    let p = js_alloc_string(ctx, len, is_wide_char);
    if p.is_null() {
        return JS_EXCEPTION;
    }
    if !is_wide_char {
        std::ptr::copy((*p1).u.str8.as_ptr(), (*p).u.str8.as_mut_ptr(), (*p1).len());
        std::ptr::copy(
            (*p2).u.str8.as_ptr(),
            (*p).u.str8.as_mut_ptr().add((*p1).len()),
            (*p2).len(),
        );
        *(*p).u.str8.as_mut_ptr().add(len) = 0;
    } else {
        copy_str16((*p).u.str16.as_mut_ptr(), p1, 0, (*p1).len());
        copy_str16(
            (*p).u.str16.as_mut_ptr().add((*p1).len()),
            p2,
            0,
            (*p2).len(),
        );
    }

    return JS_MKPTR!(JS_TAG_STRING, p as *mut _);
}

// op1 and op2 are converted to strings. For convience, op1 or op2 =
// JS_EXCEPTION are accepted and return JS_EXCEPTION.
pub unsafe fn JS_ConcatString(ctx: *mut JSContext, mut op1: JSValue, mut op2: JSValue) -> JSValue {
    if unlikely(JS_VALUE_GET_TAG(op1) != JS_TAG_STRING) {
        op1 = JS_ToStringFree(ctx, op1);
        if JS_IsException(op1) {
            JS_FreeValue(ctx, op2);
            return JS_EXCEPTION;
        }
    }
    if unlikely(JS_VALUE_GET_TAG(op2) != JS_TAG_STRING) {
        op2 = JS_ToStringFree(ctx, op2);
        if JS_IsException(op2) {
            JS_FreeValue(ctx, op1);
            return JS_EXCEPTION;
        }
    }

    let p1 = JS_VALUE_GET_STRING(op1);
    let p2 = JS_VALUE_GET_STRING(op2);

    // XXX: could also check if p1 is empty
    if (*p2).len() == 0 {
        // goto ret_op1; // C2RS: Replaced with a label and continue
        JS_FreeValue(ctx, op2);
        return op1;
    }

    let concat_len = (*p1).len() + (*p2).len();

    if (*p1).header.ref_count == 1
        && (*p1).is_wide_char() == (*p2).is_wide_char()
        && js_malloc_usable_size(ctx, p1.cast())
            >= std::mem::size_of::<JSString>()
                + if (*p2).is_wide_char() {
                    concat_len << 1
                } else {
                    concat_len + 1
                }
    {
        // Concatenate in place in available space at the end of p1
        if (*p1).is_wide_char() {
            std::ptr::copy(
                (*p2).u.str16.as_ptr(),
                (*p1).u.str16.as_mut_ptr().add((*p1).len()),
                (*p2).len() << 1,
            );
            (*p1).set_len(concat_len);
        } else {
            std::ptr::copy(
                (*p2).u.str8.as_ptr(),
                (*p1).u.str8.as_mut_ptr().add((*p1).len()),
                (*p2).len(),
            );
            (*p1).set_len(concat_len);
            *(*p1).u.str8.as_mut_ptr().add((*p1).len()) = 0;
        }
        // ret_op1: // C2RS: Replaced with a label and continue
        JS_FreeValue(ctx, op2);
        return op1;
    }

    let ret = JS_ConcatString1(ctx, p1, p2);
    JS_FreeValue(ctx, op1);
    JS_FreeValue(ctx, op2);
    return ret;
}

pub unsafe fn js_new_string8(ctx: *mut JSContext, buf: *const u8, len: usize) -> JSValue {
    if len <= 0 {
        return JS_AtomToString(ctx, JS_ATOM_empty_string);
    }

    let str = js_alloc_string(ctx, len, false);
    if str.is_null() {
        return JS_EXCEPTION;
    }

    std::ptr::copy_nonoverlapping(buf, (*str).u.str8.as_mut_ptr(), len as _);

    (*str).u.str8.as_mut_ptr().offset(len as _).write(0);
    JS_MKPTR!(JS_TAG_STRING, str.cast())
}

pub unsafe fn js_new_string16(ctx: *mut JSContext, buf: *const u16, len: usize) -> JSValue {
    let str = js_alloc_string(ctx, len, true);
    if str.is_null() {
        return JS_EXCEPTION;
    }

    std::ptr::copy_nonoverlapping(buf, (*str).u.str16.as_mut_ptr(), (len * 2) as _);
    JS_MKPTR!(JS_TAG_STRING, str.cast())
}

pub unsafe fn js_new_string_char(ctx: *mut JSContext, c: u16) -> JSValue {
    if c < 0x100 {
        let ch8 = c as u8;
        return js_new_string8(ctx, &ch8, 1);
    } else {
        return js_new_string16(ctx, &c, 1);
    }
}

pub unsafe fn JS_NewStringLen(ctx: *mut JSContext, buf: *const u8, buf_len: usize) -> JSValue {
    let p_start = buf;
    let p_end = p_start.offset(buf_len as isize);
    let mut p = p_start;
    while p < p_end && *p < 128 {
        p = p.offset(1);
    }

    let len1 = p.offset_from(p_start) as usize;
    if len1 > JS_STRING_LEN_MAX {
        return ctx
            .as_mut()
            .unwrap()
            .JS_ThrowInternalError(c"string too long");
    }

    if p == p_end {
        /* ASCII string */
        js_new_string8(ctx, buf, buf_len as _)
    } else {
        let mut sbuf = StringBuffer {
            ctx: std::ptr::null_mut(),
            size: 0,
            len: 0,
            is_wide_char: false,
            str: std::ptr::null_mut(),
            error_status: 0,
        };
        if string_buffer_init(ctx, &mut sbuf, buf_len as _) != 0 {
            // C2RS: goto fail replaced with early return
            string_buffer_free(&mut sbuf);
            return JS_EXCEPTION;
        }
        string_buffer_write8(&mut sbuf, p_start, len1 as _);

        /* convert utf-8 bytes to unicode 16, write to string buffer */
        let it = Utf8ToUnicode16Iter {
            bytes: std::slice::from_raw_parts(p, p_end.offset_from(p) as usize),
            size: p_end.offset_from(p) as usize,
            index: 0,
            next_unichar: None,
        };
        for uc in it {
            if uc < 128 {
                string_buffer_putc8(&mut sbuf, uc as _);
            } else {
                string_buffer_putc16(&mut sbuf, uc as _);
            }
        }

        // while p < p_end {
        //     if *p < 128 {
        //         string_buffer_putc8(b, *p as _);
        //         p = p.offset(1);
        //     } else {
        //         /* parse utf-8 sequence, return 0xFFFFFFFF for error */
        //         c = unicode_from_utf8(p, p_end.offset_from(p) as usize, &mut p_next);
        //         if c < 0x10000 {
        //             p = p_next;
        //         } else if c <= 0x10FFFF {
        //             p = p_next;
        //             /* surrogate pair */
        //             c -= 0x10000;
        //             string_buffer_putc16(b, (c >> 10) + 0xd800);
        //             c = (c & 0x3ff) + 0xdc00;
        //         } else {
        //             /* invalid char */
        //             c = 0xfffd;
        //             /* skip the invalid chars */
        //             /* XXX: seems incorrect. Why not just use c = *p++; ? */
        //             while p < p_end && (*p >= 0x80 && *p < 0xc0) {
        //                 p = p.offset(1);
        //             }
        //             if p < p_end {
        //                 p = p.offset(1);
        //                 while p < p_end && (*p >= 0x80 && *p < 0xc0) {
        //                     p = p.offset(1);
        //                 }
        //             }
        //         }
        //         string_buffer_putc16(b, c);
        //     }
        // }

        string_buffer_end(&mut sbuf)
    }
}

pub unsafe fn JS_NewString(ctx: *mut JSContext, str: &CStr) -> JSValue {
    JS_NewStringLen(ctx, str.as_ptr().cast(), libc::strlen(str.as_ptr()))
}

pub unsafe fn JS_FreeCString(ctx: *mut JSContext, ptr: *const c_char) {
    if ptr.is_null() {
        return;
    }

    /* purposely removing constness */
    let p = container_of!(ptr, JSString, u) as *mut JSString;
    JS_FreeValue(ctx, JS_MKPTR!(JS_TAG_STRING, p.cast()));
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
    if __JS_AtomIsTaggedInt(prop) {
        let p = JS_VALUE_GET_OBJ(obj).as_mut().unwrap();
        if JS_VALUE_GET_TAG((*p).u.object_data) == JS_TAG_STRING {
            let p1 = JS_VALUE_GET_STRING((*p).u.object_data).as_mut().unwrap();
            let idx = __JS_AtomToUInt32(prop);
            if idx < p1.len() as u32 {
                if !desc.is_null() {
                    let ch = if p1.is_wide_char() {
                        p1.u.str16.as_ptr().offset(idx as isize) as u32
                    } else {
                        p1.u.str8.as_ptr().offset(idx as isize) as u32
                    };

                    (*desc).flags = JS_PROP_ENUMERABLE as _;
                    (*desc).value = js_new_string_char(ctx, ch as _);
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

    if __JS_AtomIsTaggedInt(prop) {
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
            if string_get(p1, idx as _) != string_get(p2, 0) {
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
    if __JS_AtomIsTaggedInt(prop) {
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
