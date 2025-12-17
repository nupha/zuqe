use std::{iter::Peekable, ops::Range};

use super::StringBuf;

#[repr(u8)]
#[derive(Clone, Copy, PartialEq)]
pub enum StringChar {
    /// unichar
    Char(char),
    /// invalid code
    Code(u16),
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
        if matches!(value, 0xD800..=0xDFFF) {
            // invalid high or low surrogate.
            Self::Code(value)
        } else {
            Self::Char(char::from_u32(value as u32).unwrap())
        }
    }
}

impl std::fmt::Display for StringChar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Char(c) => {
                write!(f, "{c}")
            }
            Self::Code(c) => {
                write!(f, "\\u{{{:4X}}}", *c)
            }
        }
    }
}

impl std::fmt::Debug for StringChar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "'{self}'")
    }
}

impl StringChar {
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::Char(c) => c as u32,
            Self::Code(c) => c as u32,
        }
    }

    /// if is broken code, returns None. otherwise returns the char
    pub const fn char(&self) -> Option<char> {
        if let Self::Char(c) = self {
            Some(*c)
        } else {
            None
        }
    }
}

#[derive(Clone)]
pub struct U16CharIter<'a> {
    data: Peekable<std::slice::Iter<'a, u16>>,
    cursor: usize,
}

impl<'a> Iterator for U16CharIter<'a> {
    type Item = (usize, StringChar);

    /// returns (next_pos, char)
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(n) = self.data.next() {
            let high = *n as u32;
            self.cursor += 1;

            let r = if (0xD800..=0xDBFF).contains(&high) {
                if let Some(low) = self.data.peek()
                    && (0xDC00..=0xDFFF).contains(*low)
                {
                    // 合法代理对 → 组合成有效的 Unicode 字符
                    let code_point = ((high - 0xD800) << 10) + (**low as u32 - 0xDC00) + 0x10000;

                    if let Some(ch) = char::from_u32(code_point) {
                        self.data.next(); // 消耗低代理项
                        self.cursor += 1;
                        return Some((self.cursor, StringChar::Char(ch)));
                    }
                }

                // 非法代理对（单独的高代理或无效组合）→ 保留原始 u16
                StringChar::Code(high as _)
            } else if (0xDC00..=0xDFFF).contains(&high) {
                // 单独的低代理项 → 保留原始 u16
                StringChar::Code(high as _)
            } else {
                // 普通 BMP 字符（U+0000..=U+FFFF）
                if let Some(ch) = char::from_u32(high) {
                    StringChar::Char(ch)
                } else {
                    StringChar::Code(high as _)
                }
            };

            Some((self.cursor, r))
        } else {
            None
        }
    }
}

impl<'a> U16CharIter<'a> {
    pub fn new(slice: &'a [u16], base_index: usize) -> Self {
        let it = slice.iter();
        let it = it.peekable();
        Self {
            data: it,
            cursor: base_index,
        }
    }

    /// peek next code (not char).
    pub fn peek(&mut self) -> Option<u16> {
        self.data.peek().copied().copied()
    }
}

#[derive(Clone)]
pub enum StringCharIter<'a> {
    U16(U16CharIter<'a>),
    U8 {
        it: Peekable<std::slice::Iter<'a, u8>>,
        cursor: usize,
    },
}

impl<'a> Iterator for StringCharIter<'a> {
    type Item = (usize, StringChar);

    /// returns (next_pos, char)
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::U16(it) => it.next(),
            Self::U8 { it, cursor } => it.next().map(|c| {
                *cursor += 1;
                (*cursor, StringChar::Char(*c as char))
            }),
        }
    }
}

impl<'a> StringCharIter<'a> {
    pub fn from_u16(slice: &'a [u16], base_index: usize) -> Self {
        Self::U16(U16CharIter::new(slice, base_index))
    }

    pub fn from_u8(slice: &'a [u8], base_index: usize) -> Self {
        Self::U8 {
            it: slice.iter().peekable(),
            cursor: base_index,
        }
    }

    /// get current cursor
    pub const fn cursor(&self) -> usize {
        match self {
            Self::U8 { cursor, .. } => *cursor,
            Self::U16(it) => it.cursor,
        }
    }

    /// peek next code (not char).
    pub fn peek(&mut self) -> Option<u16> {
        match self {
            Self::U8 { it, .. } => it.peek().copied().map(|c| *c as _),
            Self::U16(it) => it.peek(),
        }
    }
}

#[derive(Clone)]
pub enum StringCodepointIter<'a> {
    U16(Peekable<std::slice::Iter<'a, u16>>),
    U8(Peekable<std::slice::Iter<'a, u8>>),
}

impl<'a> Iterator for StringCodepointIter<'a> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::U16(it) => it.next().copied(),
            Self::U8(it) => it.next().map(|c| *c as u16),
        }
    }
}

impl<'a> StringCodepointIter<'a> {
    pub fn peek(&mut self) -> Option<u16> {
        match self {
            Self::U16(it) => it.peek().map(|c| **c),
            Self::U8(it) => it.peek().map(|c| **c as u16),
        }
    }
}

impl StringBuf {
    /// iterate over string code points. `range` is the range of buf index.
    /// if `range` has out of bound, range is intersected with buf.
    pub fn iter_codepoint_range(&self, range: Range<usize>) -> StringCodepointIter<'_> {
        match self {
            Self::Empty => StringCodepointIter::U8([].iter().peekable()),
            Self::Ascii(s) => {
                if let Some(slice) = s.get(range) {
                    StringCodepointIter::U8(slice.iter().peekable())
                } else {
                    StringCodepointIter::U8([].iter().peekable())
                }
            }
            Self::Utf16(s) => {
                if let Some(slice) = s.get(range) {
                    StringCodepointIter::U16(slice.iter().peekable())
                } else {
                    StringCodepointIter::U16([].iter().peekable())
                }
            }
        }
    }

    /// iterate over string code points. `index` is the starting buf index.
    #[inline(always)]
    pub fn iter_codepoint_since(&self, index: usize) -> StringCodepointIter<'_> {
        self.iter_codepoint_range(index..self.len())
    }

    /// iterate over string code points. short cut to `::iter_codepoint_since(0)`.
    #[inline(always)]
    pub fn iter_codepoint(&self) -> StringCodepointIter<'_> {
        self.iter_codepoint_since(0)
    }

    /// iterate over string chars. `range` is the range of buf index.
    /// if `range` is out of bounds, the intersected range is applied.
    /// for broken surrogate, returns raw code directly, otherwise returns unichar
    pub fn iter_range(&self, range: Range<usize>) -> StringCharIter<'_> {
        if range.is_empty() || range.start >= self.len() {
            StringCharIter::from_u8(&[], range.start)
        } else {
            match self {
                Self::Empty => StringCharIter::from_u8(&[], 0),
                Self::Ascii(s) => StringCharIter::from_u8(
                    &s[range.start..std::cmp::min(range.end, s.len())],
                    range.start,
                ),
                Self::Utf16(s) => StringCharIter::from_u16(
                    &s[range.start..std::cmp::min(range.end, s.len())],
                    range.start,
                ),
            }
        }
    }

    /// iterate over string chars. `index` is the starting buf index.
    ///
    /// for broken surrogate, returns raw code directly, otherwise returns unichar
    #[inline(always)]
    pub fn iter_since(&self, index: usize) -> StringCharIter<'_> {
        self.iter_range(index..self.len())
    }

    /// iterate over string chars. short cut to `::iter_since(0)`.
    #[inline(always)]
    pub fn iter(&self) -> StringCharIter<'_> {
        self.iter_range(0..self.len())
    }

    pub fn chars_range_lossy(&self, range: Range<usize>) -> impl Iterator<Item = char> {
        self.iter_range(range).map(|(_, c)| match c {
            StringChar::Char(c) => c,
            StringChar::Code(_) => '\u{FFFD}',
        })
    }

    pub fn chars_lossy(&self) -> impl Iterator<Item = char> {
        self.iter().map(|(_, c)| match c {
            StringChar::Char(c) => c,
            StringChar::Code(_) => '\u{FFFD}',
        })
    }
}
