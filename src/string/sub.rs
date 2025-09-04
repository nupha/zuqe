use std::collections::TryReserveError;

use crate::string::{StringBuf, StringChar, StringContent};

impl StringContent {
    pub fn substitution_buf<'a>(
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
}

/// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
///
/// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
/// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
/// 返回: 包含替换后的字符串值
fn do_substitution<'a>(
    str: &StringContent,                                            // 原始字符串。
    position: usize,                                                // 匹配发生的位置索引
    matched: &StringContent,                                        // 匹配到的子字符串（对应 $&）
    replacement: &StringContent,                                    // 替换字符串或替换函数。
    captures: impl Fn(usize) -> Result<Option<StringContent>, i32>, // 捕获（对应 $1, $2 等）
    captures_len: usize,                                            // 最大捕获ID
    named_captures: impl Fn(&str) -> Result<Option<StringContent>, i32>, // 命名捕获（对应 $<name>）
) -> Result<StringBuf, Option<TryReserveError>> {
    // alloc result string buf
    let mut sbuf = StringBuf::with_capacity(matched.len(), str.is_wide())?;
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
                                sbuf.push_content(&s).map_err(|e| Some(e))?;
                            }
                        } else {
                            sbuf.push_char(c)?; // $ 
                            sbuf.push_char(a)?; // 0
                        }
                    }
                    StringChar::Char('<') => {
                        // $<name> -> names_captures[name]
                        let name_start = sbuf.buf_len();
                        sbuf.push_str("$<")?;

                        while let Some((_, c1)) = replace_chars.next() {
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

                                sbuf.drain(name_start..sbuf.buf_len());
                                if let Some(s) = named_captures(&name).map_err(|_| None)? {
                                    sbuf.push_content(&s)?;
                                }
                            } else {
                                sbuf.push_char(c1)?;
                            }

                            if sbuf.buf_len() + 8 < sbuf.capacity() {
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

        if sbuf.buf_len() + 8 < sbuf.capacity() {
            sbuf.try_reserve_exact(32)?;
        }
    }

    Ok(sbuf)
}
