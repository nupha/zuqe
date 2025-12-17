use crate::{string::StringChar, utils::unlikely};

// UTF-8字符最大长度
pub const UTF8_CHAR_LEN_MAX: usize = 6;

// Unicode代码点
pub const CP_LS: u32 = 0x2028; // 行分隔符
pub const CP_PS: u32 = 0x2029; // 段落分隔符

// UTF-8编码的最小代码点值
pub const utf8_min_code: [u32; 5] = [0x80, 0x800, 0x10000, 0x00200000, 0x04000000];
// UTF-8编码的首字节掩码
pub const utf8_first_code_mask: [u8; 5] = [0x1f, 0x0f, 0x07, 0x03, 0x01];

/// dececode utf8 from buf, returns decoded unicode point.
/// Note: may return invalid unicode point.
#[deprecated(note = "use Utf8CharIter instead")]
pub(crate) fn unicode_from_utf8(buf: &[u8]) -> Result<(u32, usize), i32> {
    debug_assert!(!buf.is_empty());

    let c = buf[0];

    // 根据首字节确定UTF-8序列长度
    let n = match c {
        0x00..=0x7f => {
            // ascii
            return Ok((c as u32, 1));
        }
        0xc0..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf7 => 3,
        0xf8..=0xfb => 4,
        0xfc..=0xfd => 5,
        _ => return Err(-1), // 无效的UTF-8首字节
    };

    if n > buf.len() - 1 {
        return Err(-1);
    }

    // 应用首字节掩码
    let mut c = c as u32;
    c &= utf8_first_code_mask[n - 1] as u32;

    // 解码后续字节
    for i in 0..n {
        let b = buf[i + 1];
        if !(0x80..0xc0).contains(&b) {
            return Err(-1); // 无效的UTF-8后续字节
        }
        c = (c << 6) | ((b & 0x3f) as u32);
    }

    // 检查解码后的代码点是否有效
    if unlikely(c < utf8_min_code[n - 1]) {
        Err(-1)
    } else {
        Ok((c, n + 1))
    }
}

// 从UTF-8字节序列解码Unicode代码点
// 返回-1表示错误。*pp在错误情况下不会被更新。max_len必须>=1。
// UTF-8字节序列的最大长度为6字节 (max_len=6)。
#[deprecated(note = "use unicode_from_utf8() instead")]
pub(crate) fn unicode_from_utf8_v0(
    mut p: *const u8,
    max_len: i32,
    pp: *mut *const u8,
) -> Result<char, i32> {
    let c = unsafe {
        let c = *p as char;
        p = p.offset(1);
        c
    };

    // ASCII字符直接返回
    if c.is_ascii() {
        unsafe {
            *pp = p;
        }
        return Ok(c);
    }

    // 根据首字节确定UTF-8序列长度
    let l = match c as u8 {
        0xc0..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf7 => 3,
        0xf8..=0xfb => 4,
        0xfc..=0xfd => 5,
        _ => return Err(-1), // 无效的UTF-8首字节
    };

    // 检查是否有足够的字节
    if l > (max_len - 1) {
        return Err(-1);
    }

    // 应用首字节掩码
    let mut c = c as u32;
    c &= utf8_first_code_mask[l as usize - 1] as u32;

    // 解码后续字节
    for _ in 0..l {
        unsafe {
            let b = *p;
            p = p.offset(1);
            if !(0x80..0xc0).contains(&b) {
                return Err(-1); // 无效的UTF-8后续字节
            }
            c = (c << 6) | ((b & 0x3f) as u32);
        }
    }

    // 检查解码后的代码点是否有效
    if c < utf8_min_code[l as usize - 1] {
        return Err(-1);
    }

    unsafe {
        *pp = p;
    }
    char::from_u32(c).ok_or(-1)
}

// UTF-8到Unicode-16的迭代器
pub struct Utf8ToUnicode16Iter<'a> {
    pub bytes: &'a [u8],           // UTF-8字节序列
    pub size: usize,               // 字节序列长度
    pub index: usize,              // 当前索引
    pub next_unichar: Option<u16>, // 用于存储代理对中的第二个字符
}

impl<'a> Iterator for Utf8ToUnicode16Iter<'a> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(uc) = self.next_unichar.take() {
            Some(uc)
        } else if self.index == self.size {
            None
        } else {
            let b1 = self.bytes[self.index];
            self.index += 1;

            match b1 {
                0x00..=0x7F => Some(b1 as u16), // ASCII字符
                0x80..=0xBF => Some(0xFFFD),    // 无效的UTF-8后续字节
                0xC0..=0xDF => {
                    // 2字节UTF-8序列
                    if self.index < self.size {
                        //same to: self.index < self.size && self.size > self.index
                        let b2 = self.bytes[self.index];
                        self.index += 1;

                        if b2 & 0xC0 == 0x80 {
                            let code_point = ((b1 as u32 & 0x1F) << 6) | (b2 as u32 & 0x3F);
                            if code_point >= 0x80 {
                                Some(code_point as u16)
                            } else {
                                Some(0xFFFD)
                            }
                        } else {
                            self.index -= 1; // 回退无效的后续字节
                            Some(0xFFFD)
                        }
                    } else {
                        self.index = self.size;
                        Some(0xFFFD)
                    }
                }
                0xE0..=0xEF => {
                    // 3字节UTF-8序列
                    if self.index < self.size - 2 && self.size >= self.index + 2 {
                        let b2 = self.bytes[self.index];
                        let b3 = self.bytes[self.index + 1];
                        self.index += 2;

                        if b2 & 0xC0 == 0x80 && b3 & 0xC0 == 0x80 {
                            let code_point = ((b1 as u32 & 0x0F) << 12)
                                | ((b2 as u32 & 0x3F) << 6)
                                | (b3 as u32 & 0x3F);
                            if (0x800..=0xFFFF).contains(&code_point)
                                || (0xD800..=0xDFFF).contains(&code_point)
                            {
                                Some(code_point as u16)
                            } else {
                                Some(0xFFFD)
                            }
                        } else {
                            self.index -= 2; // 回退无效的后续字节
                            Some(0xFFFD)
                        }
                    } else {
                        self.index = self.size;
                        Some(0xFFFD)
                    }
                }
                0xF0..=0xF7 => {
                    // 4字节UTF-8序列
                    if self.index < self.size - 3 && self.size >= self.index + 3 {
                        let b2 = self.bytes[self.index];
                        let b3 = self.bytes[self.index + 1];
                        let b4 = self.bytes[self.index + 2];
                        self.index += 3;

                        if b2 & 0xC0 == 0x80 && b3 & 0xC0 == 0x80 && b4 & 0xC0 == 0x80 {
                            let code_point = ((b1 as u32 & 0x07) << 18)
                                | ((b2 as u32 & 0x3F) << 12)
                                | ((b3 as u32 & 0x3F) << 6)
                                | (b4 as u32 & 0x3F);
                            if (0x10000..=0x10FFFF).contains(&code_point) {
                                let high_surrogate = ((code_point - 0x10000) >> 10) + 0xD800;
                                let low_surrogate = ((code_point - 0x10000) & 0x3FF) + 0xDC00;
                                self.next_unichar.replace(low_surrogate as u16);
                                Some(high_surrogate as u16)
                            } else {
                                Some(0xFFFD)
                            }
                        } else {
                            self.index -= 3; // 回退无效的后续字节
                            Some(0xFFFD)
                        }
                    } else {
                        self.index = self.size;
                        Some(0xFFFD)
                    }
                }
                _ => Some(0xFFFD), // 无效的UTF-8首字节
            }
        }
    }
}

// 将Unicode代码点编码为UTF-8字节序列
// 最多编码31位。最多输出UTF8_CHAR_LEN_MAX字节。
pub(crate) fn unicode_to_utf8(buf: &mut Vec<u8>, c: u32) {
    if c < 0x80 {
        buf.push(c as u8);
    } else {
        if c < 0x800 {
            buf.push(((c >> 6) as u8) | 0xc0);
        } else {
            if c < 0x10000 {
                buf.push(((c >> 12) as u8) | 0xe0);
            } else {
                if c < 0x00200000 {
                    buf.push(((c >> 18) as u8) | 0xf0);
                } else {
                    if c < 0x04000000 {
                        buf.push(((c >> 24) as u8) | 0xf8);
                    } else if c < 0x80000000 {
                        buf.push(((c >> 30) as u8) | 0xfc);
                        buf.push(((c >> 24) as u8 & 0x3f) | 0x80);
                    } else {
                        return;
                    }
                    buf.push(((c >> 18) as u8 & 0x3f) | 0x80);
                }
                buf.push(((c >> 12) as u8 & 0x3f) | 0x80);
            }
            buf.push(((c >> 6) as u8 & 0x3f) | 0x80);
        }
        buf.push((c as u8 & 0x3f) | 0x80);
    }
}

#[inline(always)]
pub(crate) const fn char_is_space(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '\u{feff}' |  // Zero Width No-Break Space (BOM)
            '\u{200b}' |  // Zero Width Space
            '\u{200c}' |  // Zero Width Non-Joiner
            '\u{200d}' |  // Zero Width Joiner
            '\u{2060}' // Word Joiner
        )
}

/// UTF-8 字符迭代器
pub struct Utf8CharIter<'a> {
    bytes: &'a [u8], // UTF-8 bytes
    index: usize,    // current byte index
}

impl<'a> Utf8CharIter<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, index: 0 }
    }

    /// 尝试解码当前位置的 UTF-8 字符
    /// 返回 (字符, 消耗的字节数) 或错误
    fn try_decode_current(&self) -> Result<(char, usize), DecodeError> {
        if self.index >= self.bytes.len() {
            return Err(DecodeError::EndOfInput);
        }

        let b = self.bytes[self.index];

        // ASCII 字符 (0x00-0x7F)
        if b <= 0x7F {
            return Ok((b as char, 1));
        }

        // 确定 UTF-8 序列长度
        let seq_len = match b {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            0xF8..=0xFB => 5,
            0xFC..=0xFD => 6,
            _ => return Err(DecodeError::InvalidStartByte(b)),
        };

        // 检查是否有足够的字节
        if self.index + seq_len > self.bytes.len() {
            return Err(DecodeError::IncompleteSequence);
        }

        // 解码 UTF-8 序列
        let mut code_point = match seq_len {
            2 => (b as u32 & 0x1F) << 6,
            3 => (b as u32 & 0x0F) << 12,
            4 => (b as u32 & 0x07) << 18,
            5 => (b as u32 & 0x03) << 24,
            6 => (b as u32 & 0x01) << 30,
            _ => unreachable!(),
        };

        // 解码后续字节
        for i in 1..seq_len {
            let b = self.bytes[self.index + i];
            if !(0x80..0xC0).contains(&b) {
                return Err(DecodeError::InvalidContinuationByte(b, i));
            }
            code_point |= ((b & 0x3F) as u32) << (6 * (seq_len - i - 1));
        }

        // 检查代码点有效性
        if code_point > 0x10FFFF {
            return Err(DecodeError::CodePointOutOfRange(code_point));
        }

        // 检查过短编码（非最小形式）
        if (seq_len == 2 && code_point < 0x80)
            || (seq_len == 3 && code_point < 0x800)
            || (seq_len == 4 && code_point < 0x10000)
        {
            return Err(DecodeError::OverlongEncoding);
        }

        // 检查代理对范围（U+D800..U+DFFF）
        if (0xD800..=0xDFFF).contains(&code_point) {
            return Err(DecodeError::SurrogateCodePoint(code_point));
        }

        // 转换为字符
        char::from_u32(code_point)
            .ok_or(DecodeError::InvalidCodePoint(code_point))
            .map(|c| (c, seq_len))
    }
}

#[derive(Debug, Clone, PartialEq)]
enum DecodeError {
    EndOfInput,
    InvalidStartByte(u8),
    InvalidContinuationByte(u8, usize),
    IncompleteSequence,
    OverlongEncoding,
    CodePointOutOfRange(u32),
    SurrogateCodePoint(u32),
    InvalidCodePoint(u32),
}

impl<'a> Iterator for Utf8CharIter<'a> {
    type Item = (usize, StringChar); // (字节偏移量, 字符)

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.bytes.len() {
            let pos = self.index;

            match self.try_decode_current() {
                Ok((ch, consumed)) => {
                    self.index += consumed;
                    Some((pos, StringChar::Char(ch)))
                }
                Err(DecodeError::EndOfInput) => None,
                Err(
                    DecodeError::InvalidStartByte(_)
                    | DecodeError::InvalidContinuationByte(_, _)
                    | DecodeError::IncompleteSequence
                    | DecodeError::OverlongEncoding
                    | DecodeError::CodePointOutOfRange(_)
                    | DecodeError::SurrogateCodePoint(_)
                    | DecodeError::InvalidCodePoint(_),
                ) => {
                    let bad_index = self.index;
                    self.index += 1; // skip one byte when invalid utf8 sequence
                    Some((pos, StringChar::Code(self.bytes[bad_index] as u16)))
                }
            }
        } else {
            None
        }
    }
}
