use std::{borrow::Cow, collections::TryReserveError};

use crate::string::{StringBuf, StringChar, StringContent, content::StringBufSlice};

impl StringBuf {
    // /// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
    // ///
    // /// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
    // /// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
    // /// 返回: 包含替换后的字符串值
    // fn do_substitution(
    //     text: &Self,                         // 原始字符串。
    //     position: usize,                     // 匹配发生的位置索引
    //     matched: &Self,                      // 匹配到的子字符串（对应 $&）
    //     replacement: &Self,                  // 替换字符串或替换函数。
    //     captures: &[Cow<str>],               // 命名捕获组对象。（对应 $1, $2 等）
    //     named_captures: &[(&str, Cow<str>)], // 命名捕获组。（对应 $<name>）
    // ) -> Result<Self, TryReserveError> {
    //     let matched_len = matched.buf_len();
    //     let captures_len = captures.len() as u32;
    //     let rep_len = replacement.buf_len();

    //     // alloc result string buf
    //     let mut sbuf = StringBuf::with_capacity(text.buf_len(), text.is_wide())?;

    //     // escape $-patterns in `replacement`
    //     let mut i = 0_usize;
    //     loop {
    //         let mut j = if let Some(x) = replacement.index_of_char('$', i) {
    //             if x + 1 >= rep_len {
    //                 break;
    //             }
    //             x as usize
    //         } else {
    //             break;
    //         };
    //         debug_assert!(j >= i);

    //         if j > i {
    //             sbuf.push_content_slice(&replacement.slice(i..j))?;
    //         }

    //         let j0 = j;
    //         j += 1;
    //         let (_, c) = replacement.iter_since(j).next().unwrap();
    //         j += 1;

    //         match c {
    //             StringChar::Char('$') => {
    //                 // $$ -> $
    //                 sbuf.push_char(c)?;
    //             }
    //             StringChar::Char('&') => {
    //                 // $& -> matched
    //                 sbuf.push(matched)?;
    //             }
    //             StringChar::Char('`') => {
    //                 // $` -> str[0..position]
    //                 sbuf.push_content_slice(&text.slice(0..position))?;
    //             }
    //             StringChar::Char('\'') => {
    //                 // $' -> str[position+matched.len()..]
    //                 sbuf.push_content_slice(&text.slice((position + matched_len)..text.buf_len()))?;
    //             }
    //             StringChar::Char(a @ ('0'..'9')) => {
    //                 // $k -> captures[k]
    //                 let mut k = a as u32 - '0' as u32;
    //                 if j < rep_len {
    //                     if let Some((_, StringChar::Char(c1))) = replacement.iter_since(j).next()
    //                         && c1.is_ascii_digit()
    //                     {
    //                         let k1 = k * 10 + (c1 as u32 - '0' as u32);
    //                         if k1 >= 1 && k1 < captures_len {
    //                             k = k1;
    //                             j += 1;
    //                         }
    //                     }
    //                 }

    //                 if (1..captures_len).contains(&k) {
    //                     let s = &captures[k as usize];
    //                     if !s.is_empty() {
    //                         sbuf.push_str(s)?;
    //                     }
    //                 } else {
    //                     sbuf.push_content_slice(&replacement.slice(j0..j))?;
    //                 }
    //             }
    //             StringChar::Char('<') if !named_captures.is_empty() => {
    //                 // $<name> -> names_captures[name]
    //                 if let Some(k) = replacement.index_of_char('>', j as _) {
    //                     let rep = replacement.slice(j..k);
    //                     let name = rep.to_utf8();
    //                     if let Some((_, capture)) = named_captures.iter().find(|(n, _)| name.eq(n))
    //                     {
    //                         sbuf.push_str(capture)?;
    //                     }
    //                     j = k + 1;
    //                 } else {
    //                     sbuf.push_content_slice(&replacement.slice(j0..j))?;
    //                 }
    //             }
    //             _ => {
    //                 sbuf.push_content_slice(&replacement.slice(j0..j))?;
    //             }
    //         }

    //         i = j;
    //     }

    //     sbuf.push_content_slice(&replacement.slice(i..rep_len))?;

    //     Ok(sbuf)
    // }

    /// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
    ///
    /// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
    /// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
    /// 返回: 包含替换后的字符串值
    #[deprecated]
    pub fn substitution_v0(
        &self,
        position: usize,                     // 匹配发生的位置索引
        matched: Cow<str>,                   // 匹配到的子字符串（对应 $&）
        replacement: Cow<str>,               // 替换字符串或替换函数。
        captures: &[Cow<str>],               // 命名捕获组对象。（对应 $1, $2 等）
        named_captures: &[(&str, Cow<str>)], // 命名捕获组。（对应 $<name>）
    ) -> Result<Self, TryReserveError> {
        do_substitution(
            &self.as_slice(),
            position,
            matched,
            replacement,
            captures,
            named_captures,
        )
    }

    pub fn substitution2<'a>(
        &self,
        position: usize,                                       // 匹配发生的位置索引
        matched: &str,                                         // 匹配到的子字符串（对应 $&）
        replacement: &str,                                     // 替换字符串或替换函数。
        captures: impl Fn(usize) -> Option<Cow<'a, str>>,      // 命名捕获组对象。（对应 $1, $2 等）
        named_captures: impl Fn(&str) -> Option<Cow<'a, str>>, // 命名捕获组。（对应 $<name>）
    ) -> Result<StringBuf, TryReserveError> {
        do_substitution2(
            &self.as_slice(),
            position,
            matched,
            replacement,
            captures,
            named_captures,
        )
    }

    pub fn substitution<'a>(
        matched: &str,                                         // 匹配到的子字符串（对应 $&）
        str: &Self,                                            // 原字串
        position: usize,                                       // 匹配发生的位置索引
        replacement: impl Iterator<Item = StringChar>,         // 替换字符串或替换函数。
        captures: impl Fn(usize) -> Option<Cow<'a, str>>,      // 命名捕获组对象。（对应 $1, $2 等）
        named_captures: impl Fn(&str) -> Option<Cow<'a, str>>, // 命名捕获组。（对应 $<name>）
    ) -> Result<StringBuf, TryReserveError> {
        do_substitution3(
            &str.as_slice(),
            position,
            matched,
            replacement,
            captures,
            named_captures,
        )
    }

    pub fn substitution_buf<'a>(
        matched: &Self,     // 匹配到的子字符串（对应 $&）
        str: &Self,         // 原字串
        position: usize,    // 匹配发生的位置索引
        replacement: &Self, // 替换字符串或替换函数。
        captures: impl Fn(usize) -> Result<Option<StringContent>, i32>, // 命名捕获组对象。（对应 $1, $2 等）
        named_captures: impl Fn(&str) -> Result<Option<StringContent>, i32>, // 命名捕获组。（对应 $<name>）
    ) -> Result<StringBuf, Option<TryReserveError>> {
        do_substitution4(
            str,
            position,
            matched,
            replacement,
            captures,
            named_captures,
        )
    }
}

/// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
///
/// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
/// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
/// 返回: 包含替换后的字符串值
#[deprecated]
fn do_substitution(
    text: &StringBufSlice,               // 原始字符串。
    position: usize,                     // 匹配发生的位置索引
    matched: Cow<str>,                   // 匹配到的子字符串（对应 $&）
    replacement: Cow<str>,               // 替换字符串或替换函数。
    captures: &[Cow<str>],               // 命名捕获组对象。（对应 $1, $2 等）
    named_captures: &[(&str, Cow<str>)], // 命名捕获组。（对应 $<name>）
) -> Result<StringBuf, TryReserveError> {
    let matched_len = matched.len();
    let captures_len = captures.len() as u32;
    let rep_len = replacement.len();

    // alloc result string buf
    let mut res = StringBuf::with_capacity(text.buf_len(), text.is_wide())?;

    // escape $-patterns in `replacement`
    let mut i = 0_usize;

    loop {
        let mut j = if let Some(x) = replacement
            .get(i..)
            .and_then(|s| s.find('$'))
            .map(|p| p + i)
        {
            if x + 1 >= rep_len {
                break;
            }
            x as usize
        } else {
            break;
        };
        debug_assert!(j >= i);

        if let Some(ss) = replacement.get(i..j) {
            res.push_str(ss)?;
        }

        let j0 = j;
        j += 1;

        let (p, c) = replacement[j..].char_indices().next().unwrap();
        // let (_, c) = replacement.iter_since(j).next().unwrap();
        j += p;

        match c {
            '$' => {
                // $$ -> $
                res.push_char(c)?;
            }
            '&' => {
                // $& -> matched
                res.push_str(&matched)?;
            }
            '`' => {
                // $` -> str[0..position]
                res.push_content_slice(&text.slice(0..position))?;
            }
            '\'' => {
                // $' -> str[position+matched.len()..]
                res.push_content_slice(&text.slice((position + matched_len)..text.buf_len()))?;
            }
            a @ ('0'..'9') => {
                // $k -> captures[k]
                let mut k = a as u32 - '0' as u32;
                if j < rep_len {
                    if let Some(c1) = replacement[j..].chars().next()
                        && c1.is_ascii_digit()
                    {
                        let k1 = k * 10 + (c1 as u32 - '0' as u32);
                        if k1 >= 1 && k1 < captures_len {
                            k = k1;
                            j += 1;
                        }
                    }
                }

                if (1..captures_len).contains(&k) {
                    let s = &captures[k as usize];
                    if !s.is_empty() {
                        res.push_str(s)?;
                    }
                } else {
                    res.push_str(&replacement[j0..j])?;
                }
            }
            '<' if !named_captures.is_empty() => {
                // $<name> -> names_captures[name]
                if let Some(k) = replacement[j..].find('>').map(|p| p + j) {
                    let name = &replacement[j..k];
                    if let Some((_, capture)) = named_captures.iter().find(|(n, _)| *n == name) {
                        res.push_str(capture)?;
                    }
                    j = k + 1;
                } else {
                    res.push_str(&replacement[j0..j])?;
                }
            }
            _ => {
                res.push_str(&replacement[j0..j])?;
            }
        }

        i = j;
    }

    res.push_str(&replacement[i..rep_len])?;

    Ok(res)
}

/// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
///
/// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
/// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
/// 返回: 包含替换后的字符串值
fn do_substitution2<'a>(
    str: &StringBufSlice,                                  // 原始字符串。
    position: usize,                                       // 匹配发生的位置索引
    matched: &str,                                         // 匹配到的子字符串（对应 $&）
    replacement: &str,                                     // 替换字符串或替换函数。
    captures: impl Fn(usize) -> Option<Cow<'a, str>>,      // 命名捕获组对象。（对应 $1, $2 等）
    named_captures: impl Fn(&str) -> Option<Cow<'a, str>>, // 命名捕获组。（对应 $<name>）
) -> Result<StringBuf, TryReserveError> {
    let matched_len = matched.len();

    // alloc result string buf
    let mut res = StringBuf::with_capacity(str.buf_len(), str.is_wide())?;

    // escape $-patterns in `replacement`
    let mut repl_chars = replacement.char_indices();
    let mut i = 0_usize;

    while let Some((dollar, _)) = repl_chars.find(|(_, c)| *c == '$') {
        if i < dollar {
            res.push_str(&replacement[i..dollar])?;
        }

        i = dollar;

        if let Some((mut j, c)) = repl_chars.next() {
            match c {
                '$' => {
                    // $$ -> $
                    res.push_char(c)?;
                }
                '&' => {
                    // $& -> matched
                    res.push_str(&matched)?;
                }
                '`' => {
                    // $` -> str[0..position]
                    res.push_content_slice(&str.slice(0..position))?;
                }
                '\'' => {
                    // $' -> str[position+matched.len()..]
                    res.push_content_slice(&str.slice((position + matched_len)..str.buf_len()))?;
                }
                a @ ('0'..'9') => {
                    // $k -> captures[k]
                    let mut n = a as u32 - '0' as u32;
                    j += 1;
                    while let Some((p, c1)) = repl_chars.next()
                        && c1.is_ascii_digit()
                    {
                        let k1 = n * 10 + (c1 as u32 - '0' as u32);
                        n = k1;
                        j = p + 1;
                    }
                    if n > 0 {
                        if let Some(s) = captures(n as usize) {
                            res.push_str(&s)?;
                        }
                    } else {
                        res.push_str(&replacement[dollar..j])?;
                    }
                }
                '<' => {
                    // $<name> -> names_captures[name]
                    if let Some((p, _)) = repl_chars.find(|(_, c1)| *c1 == '>') {
                        let name = &replacement[(j + 1)..p];
                        if let Some(s) = named_captures(name) {
                            res.push_str(&s)?;
                        }
                        j = p + 1;
                    } else {
                        res.push_str(&replacement[dollar..j])?;
                    }
                }
                _ => {
                    res.push_str(&replacement[dollar..j])?;
                }
            }

            i = j;
        }
    }

    // remainder of replacement
    res.push_str(&replacement[i..])?;

    Ok(res)
}

/// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
///
/// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
/// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
/// 返回: 包含替换后的字符串值
fn do_substitution3<'a>(
    str: &StringBufSlice,                                  // 原始字符串。
    position: usize,                                       // 匹配发生的位置索引
    matched: &str,                                         // 匹配到的子字符串（对应 $&）
    replacement: impl Iterator<Item = StringChar>,         // 替换字符串或替换函数。
    captures: impl Fn(usize) -> Option<Cow<'a, str>>,      // 命名捕获组对象。（对应 $1, $2 等）
    named_captures: impl Fn(&str) -> Option<Cow<'a, str>>, // 命名捕获组。（对应 $<name>）
) -> Result<StringBuf, TryReserveError> {
    let matched_len = matched.len();

    // alloc result string buf
    let mut res = StringBuf::with_capacity(matched.len(), str.is_wide())?;

    let mut replace_chars = replacement.peekable();

    // escape $-patterns in `replacement`
    while let Some(c) = replace_chars.next() {
        if c == StringChar::Char('$') {
            if let Some(c1) = replace_chars.next() {
                match c1 {
                    StringChar::Char('$') => {
                        // $$ -> $
                        res.push_char('$')?;
                    }
                    StringChar::Char('&') => {
                        // $& -> matched
                        res.push_str(&matched)?;
                    }
                    StringChar::Char('`') => {
                        // $` -> str[0..position]
                        res.push_content_slice(&str.slice(0..position))?;
                    }
                    StringChar::Char('\'') => {
                        // $' -> str[position+matched.len()..]
                        res.push_content_slice(
                            &str.slice((position + matched_len)..str.buf_len()),
                        )?;
                    }
                    StringChar::Char(a @ ('0'..='9')) => {
                        // $k -> captures[k]
                        let mut n = a as u32 - '0' as u32;
                        while let Some(StringChar::Char(c2)) = replace_chars.peek()
                            && c2.is_ascii_digit()
                        {
                            n = n * 10 + (*c2 as u32 - '0' as u32);
                            let _ = replace_chars.next(); // consume digit
                        }

                        if n > 0 {
                            if let Some(s) = captures(n as usize) {
                                res.push_str(&s)?;
                            }
                        } else {
                            res.push_char(c)?; // $ 
                            res.push_char(a)?; // 0
                        }
                    }
                    StringChar::Char('<') => {
                        // $<name> -> names_captures[name]
                        let name_start = res.buf_len();
                        res.push_char('<')?;

                        while let Some(c1) = replace_chars.next() {
                            if c1 == StringChar::Char('>') {
                                // extract name
                                let name = res
                                    .iter_since(name_start)
                                    .map(|(_, c)| match c {
                                        StringChar::Char(c) => c,
                                        StringChar::Code(_) => '\u{FFFD}',
                                    })
                                    .skip(1) // ignore leading <
                                    .collect::<String>();

                                res.drain(name_start..res.buf_len());
                                if let Some(s) = named_captures(&name) {
                                    res.push_str(&s)?;
                                }
                            } else {
                                res.push_char(c1)?;
                            }

                            if res.buf_len() + 8 < res.capacity() {
                                res.try_reserve_exact(32)?;
                            }
                        }
                    }
                    _ => {
                        res.push_char(c)?;
                        res.push_char(c1)?;
                    }
                }
            }
        } else {
            res.push_char(c)?;
        }

        if res.buf_len() + 8 < res.capacity() {
            res.try_reserve_exact(32)?;
        }
    }

    Ok(res)
}

/// 该函数是 JavaScript 正则替换功能的核心实现，处理了替换字符串中的所有特殊模式，确保符合 ECMAScript 规范要求。
///
/// 实现 JavaScript 字符串替换中的特殊替换模式（如 $&, $1, `$`` 等）
/// 对应规范：ECMAScript 的 GetSubstitution 抽象操作.
/// 返回: 包含替换后的字符串值
fn do_substitution4<'a>(
    str: &StringBuf,                                                     // 原始字符串。
    position: usize,                                                     // 匹配发生的位置索引
    matched: &StringBuf,     // 匹配到的子字符串（对应 $&）
    replacement: &StringBuf, // 替换字符串或替换函数。
    captures: impl Fn(usize) -> Result<Option<StringContent>, i32>, // 命名捕获组对象。（对应 $1, $2 等）
    named_captures: impl Fn(&str) -> Result<Option<StringContent>, i32>, // 命名捕获组。（对应 $<name>）
) -> Result<StringBuf, Option<TryReserveError>> {
    // alloc result string buf
    let mut sbuf = StringBuf::with_capacity(matched.buf_len(), str.is_wide())?;
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
                        sbuf.push(matched)?;
                    }
                    StringChar::Char('`') => {
                        // $` -> str[0..position]
                        sbuf.push_content_slice(&str.slice(0..position))?;
                    }
                    StringChar::Char('\'') => {
                        // $' -> str[position+matched.len()..]
                        sbuf.push_content_slice(
                            &str.slice((position + matched.buf_len())..str.buf_len()),
                        )?;
                    }
                    StringChar::Char(a @ ('0'..='9')) => {
                        // $k -> captures[k]
                        let mut n = a as u32 - '0' as u32;
                        while let Some(c2) = replace_chars.peek()
                            && (c2 < 0x80 && (c2 as u8).is_ascii_digit())
                        {
                            n = n * 10 + (c2 as u32 - '0' as u32);
                            let _ = replace_chars.next(); // consume digit
                        }

                        if n > 0 {
                            if let Some(s) = captures(n as usize).map_err(|_| None)? {
                                sbuf.push(&s).map_err(|e| Some(e))?;
                            }
                        } else {
                            sbuf.push_char(c)?; // $ 
                            sbuf.push_char(a)?; // 0
                        }
                    }
                    StringChar::Char('<') => {
                        // $<name> -> names_captures[name]
                        let name_start = sbuf.buf_len();
                        sbuf.push_char('<')?;

                        while let Some((_, c1)) = replace_chars.next() {
                            if c1 == StringChar::Char('>') {
                                // extract name
                                let name = sbuf
                                    .iter_since(name_start)
                                    .map(|(_, c)| match c {
                                        StringChar::Char(c) => c,
                                        StringChar::Code(_) => '\u{FFFD}',
                                    })
                                    .skip(1) // ignore leading <
                                    .collect::<String>();

                                sbuf.drain(name_start..sbuf.buf_len());
                                if let Some(s) = named_captures(&name).map_err(|_| None)? {
                                    sbuf.push(&s)?;
                                }
                            } else {
                                sbuf.push_char(c1)?;
                            }

                            if sbuf.buf_len() + 8 < sbuf.capacity() {
                                sbuf.try_reserve_exact(32)?;
                            }
                        }
                    }
                    _ => {
                        sbuf.push_char(c)?;
                        sbuf.push_char(c1)?;
                    }
                }
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
