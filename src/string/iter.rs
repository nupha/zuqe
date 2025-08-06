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
pub enum StringChar {
    Char(char),
    Broken(u16),
}

impl StringChar {
    pub fn to_u32(self) -> u32 {
        match self {
            Self::Char(c) => c as u32,
            Self::Broken(c) => c as u32,
        }
    }
}

pub enum StringBufIter<'a> {
    U16(Peekable<std::slice::Iter<'a, u16>>),
    U8(Peekable<std::slice::Iter<'a, u8>>),
}

impl<'a> Iterator for StringBufIter<'a> {
    type Item = StringChar;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::U16(it) => Self::next_u16(it),
            Self::U8(it) => Self::next_u8(it),
        }
    }
}

impl<'a> StringBufIter<'a> {
    fn from_u16(slice: &'a [u16]) -> Self {
        let it = slice.iter();
        let it = it.peekable();
        Self::U16(it)
    }

    fn from_u8(slice: &'a [u8]) -> Self {
        let it = slice.iter();
        let it = it.peekable();
        Self::U8(it)
    }

    #[inline]
    fn next_u8(it: &mut Peekable<std::slice::Iter<'_, u8>>) -> Option<StringChar> {
        it.next().map(|c| StringChar::Char(*c as char))
    }

    #[inline]
    fn next_u16(it: &mut Peekable<std::slice::Iter<'_, u16>>) -> Option<StringChar> {
        while let Some(n) = it.next() {
            let high = *n;

            let r = if (0xD800..=0xDBFF).contains(&high) {
                if let Some(low) = it.peek() {
                    if (0xDC00..=0xDFFF).contains(*low) {
                        // 合法代理对 → 组合成有效的 Unicode 字符
                        let code_point =
                            ((high - 0xD800) as u32) << 10 | (**low - 0xDC00) as u32 + 0x10000;
                        if let Some(ch) = char::from_u32(code_point) {
                            it.next(); // 消耗低代理项
                            return Some(StringChar::Char(ch));
                        }
                    }
                }

                // 非法代理对（单独的高代理或无效组合）→ 保留原始 u16
                StringChar::Broken(high)
            } else if (0xDC00..=0xDFFF).contains(&high) {
                // 单独的低代理项 → 保留原始 u16
                StringChar::Broken(high)
            } else {
                // 普通 BMP 字符（U+0000..=U+FFFF）
                if let Some(ch) = char::from_u32(high as u32) {
                    StringChar::Char(ch)
                } else {
                    StringChar::Broken(high)
                }
            };

            return Some(r);
        }

        None
    }
}

#[deprecated(note = "use StringChar instead")]
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

#[deprecated(note = "use StringChar instead")]
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
            Self::Empty | Self::Null => StringBufIter::from_u8(&[]),
            Self::Utf8(s) => {
                StringBufIter::from_u8(if index < s.len() { &s[index..] } else { &[] })
            }
            Self::Utf16(s) => {
                StringBufIter::from_u16(if index < s.len() { &s[index..] } else { &[] })
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
