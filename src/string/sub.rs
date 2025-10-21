use std::collections::TryReserveError;

use crate::string::{StringBuf, StringChar, StringCharIter, StringContent};

use super::UTF8_CHAR_LEN_MAX;

impl StringContent {
    pub fn substitution_buf(
        matched: &Self,     // 匹配到的子字符串（对应 $&）
        str: &Self,         // 原字串
        position: usize,    // 匹配发生的位置索引
        replacement: &Self, // 替换字符串或替换函数。
        captures: impl Fn(usize) -> Result<Option<StringContent>, i32>, // 捕获（对应 $1, $2 等）
        captures_len: usize, // 最大捕获ID
        named_captures: impl Fn(&str) -> Result<Option<StringContent>, i32>, // 命名捕获（对应 $<name>）
    ) -> Result<StringBuf, Option<TryReserveError>> {
        do_substitution(
            str,
            position,
            matched,
            replacement,
            captures,
            captures_len,
            named_captures,
        )
    }

    /// make quoted string from buf
    #[inline]
    pub fn quote(&self) -> Result<StringBuf, TryReserveError> {
        quote_string_internal(self.iter(), self.len())
    }
}

/// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
///
/// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
/// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
/// 返回: 包含替换后的字符串值
fn do_substitution(
    str: &StringContent,                                            // 原始字符串。
    position: usize,                                                // 匹配发生的位置索引
    matched: &StringContent,                                        // 匹配到的子字符串（对应 $&）
    replacement: &StringContent,                                    // 替换字符串或替换函数。
    captures: impl Fn(usize) -> Result<Option<StringContent>, i32>, // 捕获（对应 $1, $2 等）
    captures_len: usize,                                            // 最大捕获ID
    named_captures: impl Fn(&str) -> Result<Option<StringContent>, i32>, // 命名捕获（对应 $<name>）
) -> Result<StringBuf, Option<TryReserveError>> {
    // alloc result string buf
    let mut sbuf = StringBuf::with_capacity(matched.len(), false)?;
    let mut replace_chars = replacement.iter();

    // escape $-patterns in `replacement`
    while let Some((_, c)) = replace_chars.next() {
        if c == StringChar::Char('$') {
            if let Some((_, c1)) = replace_chars.next() {
                match c1 {
                    StringChar::Char('$') => {
                        // $$ -> $
                        sbuf.push_char('$')?;
                    }
                    StringChar::Char('&') => {
                        // $& -> matched
                        sbuf.push_content(matched)?;
                    }
                    StringChar::Char('`') => {
                        // $` -> str[0..position]
                        sbuf.push_content_range(str, 0..position)?;
                    }
                    StringChar::Char('\'') => {
                        // $' -> str[position+matched.len()..]
                        sbuf.push_content_range(str, (position + matched.len())..str.len())?;
                    }
                    StringChar::Char(a @ ('0'..='9')) => {
                        // $k -> captures[k]
                        let mut id = a as u32 - '0' as u32;
                        if let Some(c2) = replace_chars.peek()
                            && (c2 < 0x80 && (c2 as u8).is_ascii_digit())
                        {
                            let n1 = id * 10 + (c2 as u32 - '0' as u32);
                            if (1..captures_len as u32).contains(&n1) {
                                id = n1;
                                let _ = replace_chars.next(); // consume digit
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
                    StringChar::Char('<') => {
                        // $<name> -> names_captures[name]
                        let name_start = sbuf.len();
                        sbuf.push_str("$<")?;

                        for (_, c1) in replace_chars.by_ref() {
                            if c1 == StringChar::Char('>') {
                                // extract name
                                let name = sbuf
                                    .iter_since(name_start)
                                    .map(|(_, c)| match c {
                                        StringChar::Char(c) => c,
                                        StringChar::Code(_) => '\u{FFFD}',
                                    })
                                    .skip(2) // chop leading $<
                                    .collect::<String>();

                                sbuf.drain(name_start..sbuf.len());
                                if let Some(s) = named_captures(&name).map_err(|_| None)? {
                                    sbuf.push_content(&s)?;
                                }
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
    #[inline]
    pub fn quote(&self) -> Result<Self, TryReserveError> {
        quote_string_internal(self.iter(), self.len())
    }
}

/// quote string with leading and tailing '"'.
fn quote_string_internal(src: StringCharIter, len: usize) -> Result<StringBuf, TryReserveError> {
    let mut buf = StringBuf::with_capacity(len + 2, false)?;
    buf.push_char('"').unwrap();

    for (_, ch) in src {
        if buf.capacity() - buf.len() < UTF8_CHAR_LEN_MAX {
            buf.try_reserve(64)?;
        }

        match ch {
            StringChar::Char(c) => match c {
                '\t' => {
                    buf.push_str("\\t")?;
                    continue;
                }
                '\r' => {
                    buf.push_str("\\r")?;
                    continue;
                }
                '\n' => {
                    buf.push_str("\\n")?;
                    continue;
                }
                '\u{0008}' => {
                    buf.push_str("\\b")?;
                    continue;
                }
                '\u{000C}' => {
                    buf.push_str("\\f")?;
                    continue;
                }
                '"' => {
                    buf.push_str("\\\"")?;
                    continue;
                }
                '\\' => {
                    buf.push_str("\\\\")?;
                    continue;
                }
                c if (c as u32) < 32 => {
                    buf.push_str(&format!("\\u{:04x}", c as u8))?;
                }
                _ => {
                    buf.push_char(c)?;
                }
            },
            StringChar::Code(c) => {
                buf.push_str(&format!("\\u{:04x}", c))?;
            }
        }

        // if c < 0x80 {
        //     match c as u8 {
        //         b'\t' => {
        //             buf.push_str("\\t")?;
        //             continue;
        //         }
        //         b'\r' => {
        //             buf.push_str("\\r")?;
        //             continue;
        //         }
        //         b'\n' => {
        //             buf.push_str("\\n")?;
        //             continue;
        //         }
        //         0x08 => {
        //             buf.push_str("\\x08")?;
        //             continue;
        //         }
        //         0x0C => {
        //             buf.push_str("\\x0C")?;
        //             continue;
        //         }
        //         b'"' => {
        //             buf.push_str("\\\"")?;
        //             continue;
        //         }
        //         b'\\' => {
        //             buf.push_str("\\\\")?;
        //             continue;
        //         }
        //         c if c < 32 => {
        //             buf.push_str(&format!("\\u{:04x}", c))?;
        //         }
        //         _ => {
        //             buf.push_u8(c as u8)?;
        //         }
        //     }
        // } else if (0xd800..0xe000).contains(&c) {
        //     buf.push_str(&format!("\\u{:04x}", c))?;
        // } else {
        //     buf.push_u16(c)?;
        // }
    }

    buf.push_char('"')?;
    buf.shrink_to_fit();
    Ok(buf)
}
