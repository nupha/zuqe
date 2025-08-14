use std::{
    iter::Peekable,
    ops::Range,
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
    BrokenCode(u32),
}

impl From<char> for StringChar {
    fn from(value: char) -> Self {
        Self::Char(value)
    }
}

impl From<u8> for StringChar {
    fn from(value: u8) -> Self {
        Self::Char(value as char)
    }
}

impl From<u16> for StringChar {
    fn from(value: u16) -> Self {
        if matches!(value, 0xD800..=0xDBFF | 0xDC00..=0xDFFF) {
            // invalid high or low surrogate.
            Self::BrokenCode(value as u32)
        } else {
            Self::Char(char::from_u32(value as u32).unwrap())
        }
    }
}

impl StringChar {
    pub fn to_u32(self) -> u32 {
        match self {
            Self::Char(c) => c as u32,
            Self::BrokenCode(c) => c as u32,
        }
    }

    /// if is broken code, returns None. otherwise returns the char
    pub fn char(&self) -> Option<char> {
        if let Self::Char(c) = self {
            Some(*c)
        } else {
            None
        }
    }
}

#[derive(Clone)]
pub enum StringBufIter<'a> {
    U16 {
        it: Peekable<std::slice::Iter<'a, u16>>,
        cursor: usize,
    },
    U8 {
        it: Peekable<std::slice::Iter<'a, u8>>,
        cursor: usize,
    },
}

impl<'a> Iterator for StringBufIter<'a> {
    type Item = (usize, StringChar);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::U16 { it, cursor } => Self::next_u16(it, cursor).map(|c| (*cursor, c)),
            Self::U8 { it, cursor } => Self::next_u8(it, cursor).map(|c| (*cursor, c)),
        }
    }
}

impl<'a> StringBufIter<'a> {
    fn from_u16(slice: &'a [u16]) -> Self {
        let it = slice.iter();
        let it = it.peekable();
        Self::U16 { it, cursor: 0 }
    }

    fn from_u8(slice: &'a [u8]) -> Self {
        let it = slice.iter();
        let it = it.peekable();
        Self::U8 { it, cursor: 0 }
    }

    #[inline]
    fn next_u8(
        it: &mut Peekable<std::slice::Iter<'_, u8>>,
        cursor: &mut usize,
    ) -> Option<StringChar> {
        it.next().map(|c| {
            *cursor += 1;
            StringChar::Char(*c as char)
        })
    }

    #[inline]
    fn next_u16(
        it: &mut Peekable<std::slice::Iter<'_, u16>>,
        cursor: &mut usize,
    ) -> Option<StringChar> {
        while let Some(n) = it.next() {
            let high = *n as u32;
            *cursor += 1;

            let r = if (0xD800..=0xDBFF).contains(&high) {
                if let Some(low) = it.peek() {
                    if (0xDC00..=0xDFFF).contains(*low) {
                        // 合法代理对 → 组合成有效的 Unicode 字符
                        let code_point =
                            ((high - 0xD800) << 10) + (**low as u32 - 0xDC00) + 0x10000;

                        if let Some(ch) = char::from_u32(code_point) {
                            it.next(); // 消耗低代理项
                            *cursor += 1;
                            return Some(StringChar::Char(ch));
                        }
                    }
                }

                // 非法代理对（单独的高代理或无效组合）→ 保留原始 u16
                StringChar::BrokenCode(high as u32)
            } else if (0xDC00..=0xDFFF).contains(&high) {
                // 单独的低代理项 → 保留原始 u16
                StringChar::BrokenCode(high as u32)
            } else {
                // 普通 BMP 字符（U+0000..=U+FFFF）
                if let Some(ch) = char::from_u32(high as u32) {
                    StringChar::Char(ch)
                } else {
                    StringChar::BrokenCode(high as u32)
                }
            };

            return Some(r);
        }

        None
    }

    /// get current cursor
    pub fn cursor(&self) -> usize {
        match self {
            Self::U8 { cursor, .. } => *cursor,
            Self::U16 { cursor, .. } => *cursor,
        }
    }

    /// peek next code (not char).
    pub fn peek(&mut self) -> Option<u16> {
        match self {
            Self::U8 { it, .. } => it.peek().copied().map(|c| *c as _),
            Self::U16 { it, .. } => it.peek().copied().map(|c| *c),
        }
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
    /// iterate over string chars. `range` is the range of buf index.
    /// if `range` has out of bound, range is intersected with buf.
    /// for broken surrogate, returns raw code directly, otherwise returns unichar
    pub fn iter_range(&self, range: Range<usize>) -> StringBufIter<'_> {
        match self {
            Self::Empty | Self::Null => StringBufIter::from_u8(&[]),
            Self::Ascii(s) => {
                if let Some(slice) = s.get(range) {
                    StringBufIter::from_u8(slice)
                } else {
                    StringBufIter::from_u8(&[])
                }
            }
            Self::Utf16(s) => {
                if let Some(slice) = s.get(range) {
                    StringBufIter::from_u16(slice)
                } else {
                    StringBufIter::from_u16(&[])
                }
            }
        }
    }

    /// iterate over string chars. `index` is the starting buf index.
    /// for broken surrogate, returns raw code directly, otherwise returns unichar
    #[inline(always)]
    pub fn iter_since(&self, index: usize) -> StringBufIter<'_> {
        self.iter_range(index..self.buf_len())
    }

    /// iterate over string chars. short cut to `::iter_since(0)`.
    #[inline(always)]
    pub fn iter(&self) -> StringBufIter<'_> {
        self.iter_range(0..self.buf_len())
    }

    /// get chars iterator
    #[deprecated(note = "use StringBuf::iter() instead")]
    pub fn chars(&self) -> StringChars<'_> {
        match self {
            Self::Empty | Self::Null => StringChars::Empty,
            Self::Ascii(s) => StringChars::Utf8(unsafe { str::from_utf8_unchecked(s) }.chars()),
            Self::Utf16(s) => StringChars::Utf16(U16Str::from_slice(s).chars_lossy()),
        }
    }

    /// get chars iterator
    #[deprecated(note = "use StringBuf::iter() instead")]
    pub fn chars_indices(&self) -> StringCharIndices<'_> {
        match self {
            Self::Empty | Self::Null => StringCharIndices::Empty,
            Self::Ascii(s) => {
                StringCharIndices::Utf8(unsafe { str::from_utf8_unchecked(s) }.char_indices())
            }
            Self::Utf16(s) => StringCharIndices::Utf16(U16Str::from_slice(s).char_indices_lossy()),
        }
    }
}
