use crate::branch_hints::unlikely;

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
pub(crate) fn unicode_from_utf8(buf: &[u8]) -> Result<(u32, usize), i32> {
    debug_assert!(!buf.is_empty());

    let c = buf[0];

    // 根据首字节确定UTF-8序列长度
    let n = match c as u8 {
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
    c &= utf8_first_code_mask[n as usize - 1] as u32;

    // 解码后续字节
    for i in 0..n {
        let b = buf[i + 1];
        if !(0x80..0xc0).contains(&b) {
            return Err(-1); // 无效的UTF-8后续字节
        }
        c = (c << 6) | ((b & 0x3f) as u32);
    }

    // 检查解码后的代码点是否有效
    if unlikely(c < utf8_min_code[n as usize - 1]) {
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
            return None;
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

// 将Unicode代码点编码为UTF-8字节序列
// 最多编码31位。最多输出UTF8_CHAR_LEN_MAX字节。
#[deprecated]
pub unsafe fn unicode_to_utf8_v0(buf: *mut u8, c: u32) -> i32 {
    let mut q = buf;

    if c < 0x80 {
        *q = c as u8;
        q = q.offset(1);
    } else {
        if c < 0x800 {
            *q = ((c >> 6) as u8) | 0xc0;
            q = q.offset(1);
        } else {
            if c < 0x10000 {
                *q = ((c >> 12) as u8) | 0xe0;
                q = q.offset(1);
            } else {
                if c < 0x00200000 {
                    *q = ((c >> 18) as u8) | 0xf0;
                    q = q.offset(1);
                } else {
                    if c < 0x04000000 {
                        *q = ((c >> 24) as u8) | 0xf8;
                        q = q.offset(1);
                    } else if c < 0x80000000 {
                        *q = ((c >> 30) as u8) | 0xfc;
                        q = q.offset(1);
                        *q = ((c >> 24) as u8 & 0x3f) | 0x80;
                        q = q.offset(1);
                    } else {
                        return 0;
                    }
                    *q = ((c >> 18) as u8 & 0x3f) | 0x80;
                    q = q.offset(1);
                }
                *q = ((c >> 12) as u8 & 0x3f) | 0x80;
                q = q.offset(1);
            }
            *q = ((c >> 6) as u8 & 0x3f) | 0x80;
            q = q.offset(1);
        }
        *q = (c as u8 & 0x3f) | 0x80;
        q = q.offset(1);
    }
    q.offset_from(buf) as i32
}
