// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::collections::TryReserveError;

use crate::string::{CharCode, CharCodeDecoder, StringBuf, StringData, Utf8Stream, Utf16Stream};

impl StringData {
    #[allow(clippy::too_many_arguments)]
    pub fn substitution_buf<NC: Fn(&str) -> Result<Option<StringData>, i32>>(
        matched: &Self,     // 匹配到的子字符串（对应 $&）
        str: &Self,         // 原字串
        position: usize,    // 匹配发生的位置索引
        replacement: &Self, // 替换字符串或替换函数。
        captures: impl Fn(usize) -> Result<Option<StringData>, i32>, // 捕获（对应 $1, $2 等）
        captures_len: usize, // 最大捕获ID
        has_named_captures: bool, // enable named_captures
        named_captures: NC, // 命名捕获（对应 $<name>）
    ) -> Result<StringBuf, Option<TryReserveError>> {
        do_substitution(
            str,
            position,
            matched,
            replacement,
            captures,
            captures_len,
            has_named_captures,
            named_captures,
        )
    }

    /// make quoted string
    pub fn quote(&self) -> Result<StringBuf, TryReserveError> {
        if self.is_empty() {
            Ok(StringBuf::Empty)
        } else {
            match self {
                Self::Ascii(s) => quote_string_internal(Utf8Stream::new(s), s.len()),
                Self::AsciiStatic(s) => {
                    quote_string_internal(Utf8Stream::new(s.as_bytes()), s.len())
                }
                Self::Utf16(s) => quote_string_internal(Utf16Stream::new(s), s.len()),
                _ => {
                    #[cfg(debug_assertions)]
                    unreachable!("{self:?}");
                    #[cfg(not(debug_assertions))]
                    unsafe {
                        std::hint::unreachable_unchecked();
                    }
                }
            }
        }
    }
}

/// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
///
/// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
/// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
/// 返回: 包含替换后的字符串值
#[allow(clippy::too_many_arguments)]
fn do_substitution<NC: Fn(&str) -> Result<Option<StringData>, i32>>(
    str: &StringData,                                            // 原始字符串。
    position: usize,                                             // 匹配发生的位置索引
    matched: &StringData,                                        // 匹配到的子字符串（对应 $&）
    replacement: &StringData,                                    // 替换字符串或替换函数。
    captures: impl Fn(usize) -> Result<Option<StringData>, i32>, // 捕获（对应 $1, $2 等）
    captures_len: usize,                                         // 最大捕获ID
    has_named_captures: bool,                                    // enable named_captures handler
    named_captures: NC,                                          // 命名捕获（对应 $<name>）
) -> Result<StringBuf, Option<TryReserveError>> {
    // alloc result string buf
    let mut sbuf = StringBuf::with_capacity(matched.len(), false)?;
    let mut replace_chars = replacement.chars();

    // escape $-patterns in `replacement`
    while let Some((_, c)) = replace_chars.next() {
        if c == CharCode::Char('$') {
            if let Some((_, c1)) = replace_chars.next() {
                match c1 {
                    CharCode::Char('$') => {
                        // $$ -> $
                        sbuf.push_u8(b'$')?;
                    }
                    CharCode::Char('&') => {
                        // $& -> matched
                        sbuf.push_content(matched)?;
                    }
                    CharCode::Char('`') => {
                        // $` -> str[0..position]
                        sbuf.push_content_range(str, 0..position)?;
                    }
                    CharCode::Char('\'') => {
                        // $' -> str[position+matched.len()..]
                        sbuf.push_content_range(str, (position + matched.len())..str.len())?;
                    }
                    CharCode::Char(a @ ('0'..='9')) => {
                        // $k -> captures[k]
                        let mut id = a as u32 - '0' as u32;
                        if let Some((CharCode::Char(c2), _)) = replace_chars.current_char()
                            && c2.is_ascii_digit()
                        {
                            let n1 = id * 10 + (c2 as u32 - '0' as u32);
                            if (1..captures_len as u32).contains(&n1) {
                                id = n1;
                                replace_chars.seek_relative(1); // consume digit
                            }
                        }

                        if (1..captures_len as u32).contains(&id) {
                            if let Some(s) = captures(id as usize).map_err(|_| None)? {
                                sbuf.push_content(&s).map_err(Some)?;
                            }
                        } else {
                            sbuf.push_char(c)?; // $ 
                            sbuf.push_char(a)?; // 0
                        }
                    }
                    CharCode::Char('<') if has_named_captures => {
                        // $<name> -> names_captures[name]
                        let name_start = sbuf.len();
                        sbuf.push_str("$<")?;

                        for (_, c1) in replace_chars.by_ref() {
                            if c1 == CharCode::Char('>') {
                                let name = sbuf
                                    .chars_since(name_start)
                                    .map(|(_, c)| match c {
                                        CharCode::Char(c) => c,
                                        CharCode::Code(_) => '\u{FFFD}',
                                    })
                                    .skip(2) // chop leading $<
                                    .collect::<String>();

                                sbuf.drain(name_start..sbuf.len());
                                if let Some(s) = named_captures(&name).map_err(|_| None)? {
                                    sbuf.push_content(&s)?;
                                }
                                break; // end of $<...>
                            } else {
                                sbuf.push_char(c1)?;
                            }

                            if sbuf.len() + 8 < sbuf.capacity() {
                                sbuf.try_reserve_exact(16)?;
                            }
                        }
                    }
                    _ => {
                        sbuf.push_char(c)?;
                        sbuf.push_char(c1)?;
                    }
                }
            } else {
                sbuf.push_char(c)?; // '$'
            }
        } else {
            sbuf.push_char(c)?;
        }

        if sbuf.len() + 8 < sbuf.capacity() {
            sbuf.try_reserve_exact(32)?;
        }
    }

    Ok(sbuf)
}

impl StringBuf {
    /// make quoted string from buf
    pub fn quote(&self) -> Result<Self, TryReserveError> {
        if self.is_empty() {
            Ok(Self::Empty)
        } else {
            match self {
                Self::Ascii(s) => quote_string_internal(Utf8Stream::new(s), s.len()),
                Self::Utf16(s) => quote_string_internal(Utf16Stream::new(s), s.len()),
                _ => {
                    #[cfg(debug_assertions)]
                    unreachable!();
                    #[cfg(not(debug_assertions))]
                    unsafe {
                        std::hint::unreachable_unchecked();
                    }
                }
            }
        }
    }
}

/// quote string with leading and tailing '"'.
fn quote_string_internal<S: CharCodeDecoder>(
    src: S,
    len: usize,
) -> Result<StringBuf, TryReserveError> {
    let mut buf = StringBuf::with_capacity(len + 2, false)?;
    buf.push_u8(b'"').unwrap();

    for (_, ch) in src {
        if buf.capacity() - buf.len() < 6 {
            buf.try_reserve(64)?;
        }

        match ch {
            CharCode::Char(c) => match c {
                '\t' => {
                    buf.push_u8_slice(b"\\t")?;
                    continue;
                }
                '\r' => {
                    buf.push_u8_slice(b"\\r")?;
                    continue;
                }
                '\n' => {
                    buf.push_u8_slice(b"\\n")?;
                    continue;
                }
                '\u{0008}' => {
                    buf.push_u8_slice(b"\\b")?;
                    continue;
                }
                '\u{000C}' => {
                    buf.push_u8_slice(b"\\f")?;
                    continue;
                }
                '"' => {
                    buf.push_u8_slice(b"\\\"")?;
                    continue;
                }
                '\\' => {
                    buf.push_u8_slice(b"\\\\")?;
                    continue;
                }
                c if (c as u32) < 32 => {
                    buf.push_u8_slice(format!("\\u{:04x}", c as u8).as_bytes())?;
                }
                _ => {
                    buf.push_char(c)?;
                }
            },
            CharCode::Code(c) => {
                buf.push_u8_slice(format!("\\u{:04x}", c).as_bytes())?;
            }
        }
    }

    buf.push_u8(b'"')?;
    buf.shrink_to_fit();
    Ok(buf)
}
