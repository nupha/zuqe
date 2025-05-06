use std::ffi::c_void;

use crate::{
    bytecode::opcodes,
    unicode::{unicode_from_utf8, UTF8_CHAR_LEN_MAX},
};

pub trait UncheckedFrom<T>: Sized {
    unsafe fn unchecked_from(value: T) -> Self;
}

pub trait UncheckedInto<T>: Sized {
    unsafe fn unchecked_into(self) -> T;
}

impl<T> UncheckedFrom<T> for T {
    #[inline(always)]
    unsafe fn unchecked_from(t: T) -> T {
        t
    }
}

impl<T, U> UncheckedInto<U> for T
where
    U: UncheckedFrom<T>,
{
    #[inline]
    unsafe fn unchecked_into(self) -> U {
        U::unchecked_from(self)
    }
}

fn get_frame_address(level: usize) -> Option<usize> {
    // NOTE: 除了libc, 还可以用backtrace实现

    // 定义一个数组来存储调用栈的地址
    const MAX_STACK_DEPTH: usize = 32;
    let mut addresses: [*mut c_void; MAX_STACK_DEPTH] = [std::ptr::null_mut(); MAX_STACK_DEPTH];

    // 使用 libc::backtrace 获取调用栈地址
    let depth = unsafe { libc::backtrace(addresses.as_mut_ptr(), MAX_STACK_DEPTH as i32) };

    // 检查 level 是否在有效范围内
    if level < depth as usize {
        Some(addresses[level] as usize)
    } else {
        None // 如果层级超出调用栈深度，返回 None
    }
}

#[inline(always)]
pub unsafe fn js_get_stack_pointer() -> usize {
    get_frame_address(0).unwrap()
}

pub fn JS_DumpObjectHeader() {
    println!(
        "{:14} {:4} {:4} {:14} {:14} {:10} {}",
        "ADDRESS", "REFS", "SHRF", "SHAPE@", "PROTO", "CLASS", "PROPS"
    );
}

//
// OPCODE UTILS
//

#[inline(always)]
pub fn M2(op1: i32, op2: i32) -> i32 {
    op1 | (op2 << 8)
}

#[inline(always)]
pub fn M3(op1: i32, op2: i32, op3: i32) -> i32 {
    op1 | (op2 << 8) | (op3 << 16)
}

#[inline(always)]
pub fn M4(op1: i32, op2: i32, op3: i32, op4: i32) -> i32 {
    op1 | (op2 << 8) | (op3 << 16) | (op4 << 24)
}

#[inline(always)]
pub fn get_u32(tab: *const u8) -> u32 {
    unsafe { tab.cast::<u32>().read_unaligned() }
}

#[inline(always)]
pub fn get_u16(tab: *const u8) -> u16 {
    unsafe { tab.cast::<u16>().read_unaligned() }
    // #[repr(packed)]
    // struct packed_u16 {
    //     v: u16,
    // }
    // unsafe { (*tab.cast::<packed_u16>()).v }
}

#[inline(always)]
pub fn put_u32(tab: *mut u8, val: u32) {
    #[repr(packed)]
    struct packed_u32(u32);

    unsafe {
        tab.cast::<packed_u32>().as_mut().unwrap().0 = val;
    }
}

#[inline(always)]
pub fn put_u16(tab: *mut u8, val: u16) {
    #[repr(packed)]
    struct packed_u16(u16);

    unsafe {
        tab.cast::<packed_u16>().as_mut().unwrap().0 = val;
    }
}

#[inline(always)]
pub fn put_u8(tab: *mut u8, val: u8) {
    unsafe {
        *tab = val;
    }
}

#[inline(always)]
pub fn push_vec_u8(buf: &mut Vec<u8>, val: u8) {
    buf.push(val);
}

#[inline(always)]
pub fn push_vec_u16(buf: &mut Vec<u8>, val: u16) {
    buf.extend_from_slice(&val.to_ne_bytes());
}

#[inline(always)]
pub fn push_vec_u32(buf: &mut Vec<u8>, val: u32) {
    buf.extend_from_slice(&val.to_ne_bytes());
}

pub fn push_vec_short_code(buf: &mut Vec<u8>, op: u8, idx: u16) {
    #[cfg(feature = "short_opcode")]
    {
        if idx < 4 {
            match op {
                opcodes::OP_get_loc => {
                    push_vec_u8(buf, OP_get_loc0 + idx);
                    return;
                }
                opcodes::OP_put_loc => {
                    push_vec_u8(buf, OP_put_loc0 + idx);
                    return;
                }
                opcodes::OP_set_loc => {
                    push_vec_u8(buf, OP_set_loc0 + idx);
                    return;
                }
                opcodes::OP_get_arg => {
                    push_vec_u8(buf, OP_get_arg0 + idx);
                    return;
                }
                opcodes::OP_put_arg => {
                    push_vec_u8(buf, OP_put_arg0 + idx);
                    return;
                }
                opcodes::OP_set_arg => {
                    push_vec_u8(buf, OP_set_arg0 + idx);
                    return;
                }
                opcodes::OP_get_var_ref => {
                    push_vec_u8(buf, OP_get_var_ref0 + idx);
                    return;
                }
                opcodes::OP_put_var_ref => {
                    push_vec_u8(buf, OP_put_var_ref0 + idx);
                    return;
                }
                opcodes::OP_set_var_ref => {
                    push_vec_u8(buf, OP_set_var_ref0 + idx);
                    return;
                }
                opcodes::OP_call => {
                    push_vec_u8(buf, OP_call0 + idx);
                    return;
                }
                _ => {}
            }
        }

        if idx < 256 {
            match op {
                opcodes::OP_get_loc => {
                    push_vec_u8(buf, OP_get_loc8);
                    push_vec_u8(buf, idx);
                    return;
                }
                opcodes::OP_put_loc => {
                    push_vec_u8(buf, OP_put_loc8);
                    push_vec_u8(buf, idx);
                    return;
                }
                opcodes::OP_set_loc => {
                    push_vec_u8(buf, OP_set_loc8);
                    push_vec_u8(buf, idx);
                    return;
                }
                _ => {}
            }
        }
    }

    push_vec_u8(buf, op);
    push_vec_u16(buf, idx);
}

pub unsafe fn push_vec_short_int(buf: &mut Vec<u8>, val: i32) {
    #[cfg(feature = "short_opcode")]
    {
        if val >= -1 && val <= 7 {
            put_vec_u8(buf, OP_push_0 + val);
            return;
        }
        if val == (val as i8) as i32 {
            put_vec_u8(buf, OP_push_i8);
            put_vec_u8(buf, val as _);
            return;
        }
        if val == (val as i16) as i32 {
            put_vec_u8(buf, OP_push_i16);
            dbuf_put_u16(buf, val as _);
            return;
        }
    }

    push_vec_u8(buf, opcodes::OP_push_i32);
    push_vec_u32(buf, val as _);
}

pub fn push_vec_slice(buf: &mut Vec<u8>, data: *const u8, len: usize) -> Result<(), i32> {
    if buf.len() + len < buf.capacity() {
        buf.try_reserve(len).map_err(|_| -1)?;
    }
    buf.extend_from_slice(unsafe { std::slice::from_raw_parts(data, len) });
    Ok(())
}

pub unsafe fn push_vec_leb128(s: &mut Vec<u8>, val: u32) {
    let mut v = val;
    loop {
        let a = (v & 0x7f) as u8;
        v = v >> 7;

        if v != 0 {
            s.push(a | 0x80);
        } else {
            s.push(a);
            break;
        }
    }
}

pub unsafe fn push_vec_sleb128(s: &mut Vec<u8>, val: i32) {
    let v = if val < 0 { 2 * val + 1 } else { 2 * val } as u32;
    push_vec_leb128(s, v);
}

// 成功时返回读取的字节数（usize）
pub fn get_leb128(pval: &mut u32, buf: &[u8]) -> Result<usize, i32> {
    let mut ptr = buf.iter();
    let mut v: u32 = 0;

    for i in 0..5 {
        match ptr.next() {
            Some(&byte) => {
                v |= ((byte & 0x7F) as u32) << (i * 7);
                if byte & 0x80 == 0 {
                    *pval = v;
                    return Ok(i + 1);
                }
            }
            None => {
                return Err(-1); // "Buffer too short"
            }
        }
    }

    *pval = 0;
    Err(-1) //  Invalid LEB128 encoding
}

pub fn get_sleb128(pval: &mut i32, buf: &[u8]) -> Result<usize, i32> {
    let mut val = 0u32;
    let ret = get_leb128(&mut val, buf)?;
    *pval = (val as i32 >> 1) ^ -(val as i32 & 1);
    Ok(ret)
}

pub unsafe fn skip_spaces(pc: *const u8) -> usize {
    let p_start = pc;
    let mut p = pc;
    let mut p_next = std::ptr::null::<u8>();

    loop {
        let c = *p as char;

        if c.is_ascii() {
            if !matches!(c, '\t'..='\r' | ' ') {
                break;
            }
            p = p.offset(1);
        } else {
            if let Ok(uc) = unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as _, &mut p_next) {
                if !uc.is_whitespace() {
                    break;
                } else {
                    p = p_next;
                }
            }
        }
    }

    p.offset_from(p_start) as usize
}

//
// PROP DEFS
//

#[macro_export]
macro_rules! JS_CFUNC_NATIVE_DEF {
    ($name:expr, $length:expr, $func:expr, $magic:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: (crate::value::object::JS_PROP_WRITABLE
                | crate::value::object::JS_PROP_CONFIGURABLE) as u8,
            def_type: crate::value::JS_DEF_CFUNC,
            magic: $magic,
            u: crate::function::JSCFunctionListEntryUnion {
                func: crate::function::JSCFunctionListEntryFunc {
                    length: $length,
                    cproto: crate::function::JSCFunctionEnum::NativeFunc,
                    cfunc: crate::function::JSCFunctionType {
                        native_callback: $func,
                    },
                },
            },
        }
    };
}

#[macro_export]
macro_rules! JS_CGETSET_NATIVE_DEF {
    ($name:expr, $getter:expr, NONE, $magic:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: crate::value::object::JS_PROP_CONFIGURABLE as u8,
            def_type: crate::value::JS_DEF_CGETSET_MAGIC,
            magic: $magic,
            u: crate::function::JSCFunctionListEntryUnion {
                getset: crate::function::JSCFunctionListEntryGetSet {
                    get: Some(crate::function::JSCFunctionType {
                        native_getter: $getter,
                    }),
                    set: None,
                },
            },
        }
    };

    ($name:expr, NONE, $setter:expr, $magic:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: crate::value::object::JS_PROP_CONFIGURABLE as u8,
            def_type: crate::value::JS_DEF_CGETSET_MAGIC,
            magic: $magic,
            u: crate::function::JSCFunctionListEntryUnion {
                getset: crate::function::JSCFunctionListEntryGetSet {
                    get: NONE,
                    set: Some(crate::function::JSCFunctionType {
                        native_setter: $setter,
                    }),
                },
            },
        }
    };

    ($name:expr, $getter:expr, $setter:expr, $magic:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: crate::value::object::JS_PROP_CONFIGURABLE as u8,
            def_type: crate::value::JS_DEF_CGETSET_MAGIC,
            magic: $magic,
            u: crate::function::JSCFunctionListEntryUnion {
                getset: crate::function::JSCFunctionListEntryGetSet {
                    get: Some(crate::function::JSCFunctionType {
                        native_getter: $getter,
                    }),
                    set: Some(crate::function::JSCFunctionType {
                        native_setter: $setter,
                    }),
                },
            },
        }
    };
}

//
//
//

#[macro_export]
macro_rules! JS_CFUNC_DEF {
    ($name:expr, $length:expr, $func1:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: (crate::value::object::JS_PROP_WRITABLE
                | crate::value::object::JS_PROP_CONFIGURABLE) as u8,
            def_type: crate::value::JS_DEF_CFUNC,
            magic: 0,
            u: crate::function::JSCFunctionListEntryUnion {
                func: crate::function::JSCFunctionListEntryFunc {
                    length: $length,
                    cproto: crate::function::JSCFunctionEnum::Generic,
                    cfunc: crate::function::JSCFunctionType { generic: $func1 },
                },
            },
        }
    };
}

#[macro_export]
macro_rules! JS_CFUNC_MAGIC_DEF {
    ($name:expr, $length:expr, $func1:expr, $magic:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: (crate::value::object::JS_PROP_WRITABLE
                | crate::value::object::JS_PROP_CONFIGURABLE) as u8,
            def_type: crate::value::JS_DEF_CFUNC,
            magic: $magic,
            u: crate::function::JSCFunctionListEntryUnion {
                func: crate::function::JSCFunctionListEntryFunc {
                    length: $length,
                    cproto: crate::function::JSCFunctionEnum::GenericMagic,
                    cfunc: crate::function::JSCFunctionType {
                        generic_magic: $func1,
                    },
                },
            },
        }
    };
}

#[macro_export]
macro_rules! JS_CGETSET_DEF {
    ($name:expr, NONE, $setter:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: crate::property::JS_PROP_CONFIGURABLE as u8,
            def_type: crate::value::JS_DEF_CGETSET,
            magic: 0,
            u: crate::function::JSCFunctionListEntryUnion {
                getset: crate::function::JSCFunctionListEntryGetSet {
                    get: None,
                    set: Some(crate::function::JSCFunctionType { setter: $setter }),
                },
            },
        }
    };

    ($name:expr, $getter:expr, NONE) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: crate::value::object::JS_PROP_CONFIGURABLE as u8,
            def_type: crate::value::JS_DEF_CGETSET,
            magic: 0,
            u: crate::function::JSCFunctionListEntryUnion {
                getset: crate::function::JSCFunctionListEntryGetSet {
                    get: Some(crate::function::JSCFunctionType { getter: $getter }),
                    set: None,
                },
            },
        }
    };

    ($name:expr, $getter:expr, $setter:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: crate::value::object::JS_PROP_CONFIGURABLE as u8,
            def_type: crate::value::JS_DEF_CGETSET,
            magic: 0,
            u: crate::function::JSCFunctionListEntryUnion {
                getset: crate::function::JSCFunctionListEntryGetSet {
                    get: Some(crate::function::JSCFunctionType { getter: $getter }),
                    set: Some(crate::function::JSCFunctionType { setter: $setter }),
                },
            },
        }
    };
}

#[macro_export]
macro_rules! JS_CGETSET_MAGIC_DEF {
    ($name:expr, NONE, $setter:expr, $magic:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: crate::value::object::JS_PROP_CONFIGURABLE as u8,
            def_type: crate::value::JS_DEF_CGETSET_MAGIC,
            magic: $magic,
            u: crate::function::JSCFunctionListEntryUnion {
                getset: crate::function::JSCFunctionListEntryGetSet {
                    get: None,
                    set: Some(crate::function::JSCFunctionType {
                        setter_magic: $setter,
                    }),
                },
            },
        }
    };

    ($name:expr, $getter:expr, NONE, $magic:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: crate::value::object::JS_PROP_CONFIGURABLE as u8,
            def_type: crate::value::JS_DEF_CGETSET_MAGIC,
            magic: $magic,
            u: crate::function::JSCFunctionListEntryUnion {
                getset: crate::function::JSCFunctionListEntryGetSet {
                    get: Some(crate::function::JSCFunctionType {
                        getter_magic: $getter,
                    }),
                    set: None,
                },
            },
        }
    };

    ($name:expr, $getter:expr, $setter:expr, $magic:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: crate::value::object::JS_PROP_CONFIGURABLE as u8,
            def_type: crate::value::JS_DEF_CGETSET_MAGIC,
            magic: $magic,
            u: crate::function::JSCFunctionListEntryUnion {
                getset: crate::function::JSCFunctionListEntryGetSet {
                    get: Some(crate::function::JSCFunctionType {
                        getter_magic: $getter,
                    }),
                    set: Some(crate::function::JSCFunctionType {
                        setter_magic: $setter,
                    }),
                },
            },
        }
    };
}

#[macro_export]
macro_rules! JS_PROP_STRING_DEF {
    ($name:expr, $cstr:expr, $prop_flags:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: $prop_flags as u8,
            def_type: crate::value::JS_DEF_PROP_STRING,
            magic: 0,
            u: crate::function::JSCFunctionListEntryUnion { str: $cstr },
        }
    };
}

#[macro_export]
macro_rules! JS_PROP_INT32_DEF {
    ($name:expr, $val:expr, $prop_flags:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: $prop_flags as u8,
            def_type: crate::value::JS_DEF_PROP_INT32,
            magic: 0,
            u: crate::function::JSCFunctionListEntryUnion { i32: $val },
        }
    };
}

#[macro_export]
macro_rules! JS_ALIAS_DEF {
    ($name:expr, $from:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: (crate::value::object::JS_PROP_WRITABLE
                | crate::value::object::JS_PROP_CONFIGURABLE) as u8,
            def_type: crate::value::JS_DEF_ALIAS,
            magic: 0,
            u: crate::function::JSCFunctionListEntryUnion {
                alias: crate::function::JSCFunctionListEntryAlias {
                    name: $from,
                    base: -1,
                },
            },
        }
    };
}

#[macro_export]
macro_rules! JS_ITERATOR_NEXT_DEF {
    ($name:expr, $length:expr, $func1:expr, $magic:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: (crate::value::object::JS_PROP_WRITABLE
                | crate::value::object::JS_PROP_CONFIGURABLE) as u8,
            def_type: crate::value::JS_DEF_CFUNC,
            magic: $magic,
            u: crate::function::JSCFunctionListEntryUnion {
                func: crate::function::JSCFunctionListEntryFunc {
                    length: $length,
                    cproto: crate::function::JSCFunctionEnum::IteratorNext,
                    cfunc: crate::function::JSCFunctionType {
                        iterator_next: $func1,
                    },
                },
            },
        }
    };
}

#[macro_export]
macro_rules! JS_PROP_DOUBLE_DEF {
    ($name:expr, $val:expr, $flags:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: $flags as u8,
            def_type: crate::value::JS_DEF_PROP_DOUBLE,
            magic: 0,
            u: crate::function::JSCFunctionListEntryUnion { f64: $val },
        }
    };
}

#[macro_export]
macro_rules! JS_PROP_UNDEFINED_DEF {
    ($name:expr, $flags:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: $flags as u8,
            def_type: crate::value::JS_DEF_PROP_UNDEFINED,
            magic: 0,
            u: crate::function::JSCFunctionListEntryUnion { i32: 0 },
        }
    };
}

#[macro_export]
macro_rules! JS_ALIAS_BASE_DEF {
    ($name:expr, $from:expr, $base:expr) => {
        crate::function::JSCFunctionListEntry {
            name: $name,
            prop_flags: (crate::value::object::JS_PROP_WRITABLE
                | crate::value::object::JS_PROP_CONFIGURABLE) as u8,
            def_type: crate::value::JS_DEF_ALIAS,
            magic: 0,
            u: crate::function::JSCFunctionListEntryUnion {
                alias: crate::function::JSCFunctionListEntryAlias {
                    name: $from,
                    base: $base,
                },
            },
        }
    };
}
