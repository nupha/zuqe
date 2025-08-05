use std::{
    iter::Peekable,
    str::{CharIndices, Chars},
};

use widestring::{
    U16Str,
    ustr::{CharIndicesLossyUtf16, CharsLossyUtf16},
};

use super::StringBuf;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StringCharOrCode {
    Char(char),
    RawCode(u16),
}

pub struct StringBufIter<'a> {
    u16_iter: Peekable<std::slice::Iter<'a, u16>>,
    cur_idx: usize,
}

impl<'a> StringBufIter<'a> {
    /// get current cursor index
    #[inline(always)]
    pub fn cursor(&self) -> usize {
        self.cur_idx
    }
}

impl<'a> Iterator for StringBufIter<'a> {
    type Item = StringCharOrCode;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(n) = self.u16_iter.next() {
            let high = *n;
            self.cur_idx += 1;

            let r = if (0xD800..=0xDBFF).contains(&high) {
                if let Some(low) = self.u16_iter.peek() {
                    if (0xDC00..=0xDFFF).contains(*low) {
                        // 合法代理对 → 组合成有效的 Unicode 字符
                        let code_point =
                            ((high - 0xD800) as u32) << 10 | (**low - 0xDC00) as u32 + 0x10000;
                        if let Some(ch) = char::from_u32(code_point) {
                            self.u16_iter.next(); // 消耗低代理项
                            self.cur_idx += 1;
                            return Some(StringCharOrCode::Char(ch));
                        }
                    }
                }

                // 非法代理对（单独的高代理或无效组合）→ 保留原始 u16
                StringCharOrCode::RawCode(high)
            } else if (0xDC00..=0xDFFF).contains(&high) {
                // 单独的低代理项 → 保留原始 u16
                StringCharOrCode::RawCode(high)
            } else {
                // 普通 BMP 字符（U+0000..=U+FFFF）
                if let Some(ch) = char::from_u32(high as u32) {
                    StringCharOrCode::Char(ch)
                } else {
                    StringCharOrCode::RawCode(high)
                }
            };

            return Some(r);
        }

        None
    }
}

#[deprecated(note = "use StringCharOrCode instead")]
#[derive(Clone)]
pub enum StringChars<'a> {
    Empty,
    Utf8(Chars<'a>),
    Utf16(CharsLossyUtf16<'a>),
}

impl Iterator for StringChars<'_> {
    type Item = char;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty => None,
            Self::Utf8(s) => s.next(),
            Self::Utf16(s) => s.next(),
        }
    }
}

#[deprecated(note = "use StringCharOrCode instead")]
#[derive(Clone)]
pub enum StringCharIndices<'a> {
    Empty,
    Utf8(CharIndices<'a>),
    Utf16(CharIndicesLossyUtf16<'a>),
}

impl Iterator for StringCharIndices<'_> {
    type Item = (usize, char);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty => None,
            Self::Utf8(s) => s.next(),
            Self::Utf16(s) => s.next(),
        }
    }
}

impl StringBuf {
    /// iterate over string char or code
    pub fn iter_since(&self, index: usize) -> StringBufIter<'_> {
        match self {
            Self::Empty | Self::Null => {
                let it = [].iter();
                let it = it.peekable();
                StringBufIter {
                    u16_iter: it,
                    cur_idx: 0,
                }
            }
            Self::Utf8(s) => {
                todo!()
            }
            Self::Utf16(s) => {
                let it = if index < s.len() {
                    s[index..].iter()
                } else {
                    [].iter()
                };
                let it = it.peekable();
                StringBufIter {
                    u16_iter: it,
                    cur_idx: index,
                }
            }
        }
    }

    #[inline(always)]
    pub fn iter(&self) -> StringBufIter<'_> {
        self.iter_since(0)
    }

    /// get chars iterator
    #[deprecated(note = "use StringBuf::iter() instead")]
    pub fn chars(&self) -> StringChars<'_> {
        match self {
            Self::Empty | Self::Null => StringChars::Empty,
            Self::Utf8(s) => StringChars::Utf8(unsafe { str::from_utf8_unchecked(s) }.chars()),
            Self::Utf16(s) => StringChars::Utf16(U16Str::from_slice(s).chars_lossy()),
        }
    }

    /// get chars iterator
    #[deprecated(note = "use StringBuf::iter() instead")]
    pub fn chars_indices(&self) -> StringCharIndices<'_> {
        match self {
            Self::Empty | Self::Null => StringCharIndices::Empty,
            Self::Utf8(s) => {
                StringCharIndices::Utf8(unsafe { str::from_utf8_unchecked(s) }.char_indices())
            }
            Self::Utf16(s) => StringCharIndices::Utf16(U16Str::from_slice(s).char_indices_lossy()),
        }
    }
}
