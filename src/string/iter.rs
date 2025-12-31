use std::{iter::Peekable, ops::Range};

use crate::string::{CharCodeDecoder, Utf8Stream, Utf16Stream};

use super::StringBuf;

/// Represent unichar (for valid unicode) or code point (for invalid unicode)
#[repr(u8)]
#[derive(Clone, Copy, PartialEq)]
pub enum CharCode {
    /// unichar
    Char(char),
    /// code point for invalid unichar
    Code(u32),
}

impl From<char> for CharCode {
    fn from(value: char) -> Self {
        Self::Char(value)
    }
}

impl From<u8> for CharCode {
    fn from(value: u8) -> Self {
        Self::Char(value as char)
    }
}

impl From<u16> for CharCode {
    fn from(value: u16) -> Self {
        char::from_u32(value as u32).map_or_else(|| Self::Code(value as _), Self::Char)
    }
}

impl From<u32> for CharCode {
    fn from(value: u32) -> Self {
        char::from_u32(value).map_or_else(|| Self::Code(value), Self::Char)
    }
}

impl std::fmt::Display for CharCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Char(c) => {
                write!(f, "{c}")
            }
            Self::Code(c) => {
                write!(f, "\\u{{{c:4X}}}")
            }
        }
    }
}

impl std::fmt::Debug for CharCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "'{self}'")
    }
}

impl CharCode {
    pub const fn to_u32(self) -> u32 {
        match self {
            Self::Char(c) => c as u32,
            Self::Code(c) => c,
        }
    }

    /// if is broken code, returns None. otherwise returns the unichar
    pub const fn char(&self) -> Option<char> {
        if let Self::Char(c) = self {
            Some(*c)
        } else {
            None
        }
    }

    #[inline(always)]
    pub fn encode_utf16<'a>(&self, buf: &'a mut [u16; 2]) -> &'a [u16] {
        u32_encode_utf16(self.to_u32(), buf)
    }
}

pub(super) fn u32_encode_utf16(cp: u32, buf: &mut [u16; 2]) -> &[u16] {
    if cp < 0x10000 {
        buf[0] = cp as u16;
        &buf[0..1]
    } else {
        // use surrogate pair
        let h = cp - 0x10000;
        buf[0] = ((h >> 10) + 0xd800) as u16;
        buf[1] = ((h & 0x3ff) + 0xdc00) as u16;
        buf
    }
}

/// In case cp is valid unichar, encode it to utf8;
/// In case cp is invalid unicode or broken surrogate and `allow_invalid` is true, force encode it to utf8;
/// otherwise encode U+FFFD replacement char.
pub(super) fn u32_encode_utf8(cp: u32, buf: &mut [u8; 6], allow_invalid: bool) -> &[u8] {
    if cp < 0x80 {
        // ascii
        buf[0] = cp as u8;
        &buf[0..1]
    } else if let Some(uc) = char::from_u32(cp) {
        // valid unichar
        uc.encode_utf8(buf).as_bytes()
    } else if allow_invalid {
        if cp <= 0xffff {
            // 3-bytes
            buf[0] = 0xE0 | ((cp >> 12) as u8);
            buf[1] = 0x80 | (((cp >> 6) & 0x3F) as u8);
            buf[2] = 0x80 | ((cp & 0x3F) as u8);
            &buf[0..3]
        } else if cp <= 0x10ffffff {
            // 4-bytes
            buf[0] = 0xF0 | ((cp >> 18) as u8);
            buf[1] = 0x80 | (((cp >> 12) & 0x3F) as u8);
            buf[2] = 0x80 | (((cp >> 6) & 0x3F) as u8);
            buf[3] = 0x80 | ((cp & 0x3F) as u8);
            &buf[0..4]
        } else {
            // 6-bytes
            buf[0] = 0xFC | ((cp >> 30) as u8);
            buf[1] = 0x80 | (((cp >> 24) & 0x3F) as u8);
            buf[2] = 0x80 | (((cp >> 18) & 0x3F) as u8);
            buf[3] = 0x80 | (((cp >> 12) & 0x3F) as u8);
            buf[4] = 0x80 | (((cp >> 6) & 0x3F) as u8);
            buf[5] = 0x80 | ((cp & 0x3F) as u8);
            buf
        }
    } else {
        // U+FFFD
        buf[0] = 0xEF;
        buf[1] = 0xBF;
        buf[2] = 0xBD;
        &buf[0..3]
    }
}

#[deprecated(note = "use Utf16Stream instead")]
#[derive(Clone)]
pub struct U16CharIter<'a> {
    data: Peekable<std::slice::Iter<'a, u16>>,
    cursor: usize,
}

impl<'a> Iterator for U16CharIter<'a> {
    type Item = (usize, CharCode);

    /// returns (next_pos, char)
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(n) = self.data.next() {
            let cp1 = *n as u32;
            self.cursor += 1;

            let r = if cp1 < 0x80 {
                CharCode::Char(cp1 as u8 as _)
            } else if (0xD800..=0xDBFF).contains(&cp1) {
                if let Some(cp2) = self.data.peek()
                    && (0xDC00..=0xDFFF).contains(*cp2)
                {
                    // surrogate pair
                    let cp = ((cp1 - 0xD800) << 10) + (**cp2 as u32 - 0xDC00) + 0x10000;
                    let ch = char::from_u32(cp).unwrap();
                    self.data.next();
                    self.cursor += 1;
                    return Some((self.cursor, CharCode::Char(ch)));
                }

                // single surrogate
                CharCode::Code(cp1 as _)
            } else if (0xDC00..=0xDFFF).contains(&cp1) {
                // single surrogate
                CharCode::Code(cp1 as _)
            } else if let Some(ch) = char::from_u32(cp1) {
                CharCode::Char(ch)
            } else {
                CharCode::Code(cp1 as _)
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

#[derive(Debug)]
pub enum StringCharIter<'a> {
    U8(Utf8Stream<'a>),
    U16(Utf16Stream<'a>),
}

impl<'a> CharCodeDecoder for StringCharIter<'a> {
    fn decode_char(&self, since: usize) -> Result<(char, usize), super::DecodeCharError> {
        match self {
            Self::U8(s) => s.decode_char(since),
            Self::U16(s) => s.decode_char(since),
        }
    }

    fn current_char(&self) -> Option<(CharCode, usize)> {
        match self {
            Self::U8(s) => s.current_char(),
            Self::U16(s) => s.current_char(),
        }
    }

    fn next_char(&mut self) -> Option<(Range<usize>, CharCode)> {
        match self {
            Self::U8(s) => s.next_char(),
            Self::U16(s) => s.next_char(),
        }
    }

    fn position(&self) -> Option<usize> {
        match self {
            Self::U8(s) => s.position(),
            Self::U16(s) => s.position(),
        }
    }

    fn move_cursor_by(&mut self, offset: i32) {
        match self {
            Self::U8(s) => s.move_cursor_by(offset),
            Self::U16(s) => s.move_cursor_by(offset),
        }
    }

    fn is_end(&self) -> bool {
        match self {
            Self::U8(s) => s.is_end(),
            Self::U16(s) => s.is_end(),
        }
    }
}

impl<'a> Iterator for StringCharIter<'a> {
    type Item = (Range<usize>, CharCode);

    fn next(&mut self) -> Option<Self::Item> {
        self.next_char()
    }
}

impl<'a> StringCharIter<'a> {
    pub fn from_u8(slice: &'a [u8], start: usize) -> Self {
        let mut s = Utf8Stream::new(slice);
        s.move_cursor_by(start as _);
        Self::U8(s)
    }

    pub fn from_u16(slice: &'a [u16], start: usize) -> Self {
        let mut s = Utf16Stream::new(slice);
        s.move_cursor_by(start as _);
        Self::U16(s)
    }

    // /// get current cursor
    // pub const fn cursor(&self) -> usize {
    //     match self {
    //         Self::U8 { cursor, .. } => *cursor,
    //         Self::U16(it) => it.cursor,
    //     }
    // }

    // /// peek next code (not char).
    // pub fn peek(&mut self) -> Option<u16> {
    //     match self {
    //         Self::U8 { it, .. } => it.peek().copied().map(|c| *c as _),
    //         Self::U16(it) => it.peek(),
    //     }
    // }
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
    pub fn iter_cp_range(&self, range: Range<usize>) -> StringCodepointIter<'_> {
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
    pub fn iter_cp_since(&self, index: usize) -> StringCodepointIter<'_> {
        self.iter_cp_range(index..self.len())
    }

    /// iterate over string code points. short cut to `::iter_cp_since(0)`.
    #[inline(always)]
    pub fn iter_cp(&self) -> StringCodepointIter<'_> {
        self.iter_cp_since(0)
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
                Self::Ascii(s) => {
                    StringCharIter::from_u8(&s[0..std::cmp::min(range.end, s.len())], range.start)
                }
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
            CharCode::Char(c) => c,
            CharCode::Code(_) => '\u{FFFD}',
        })
    }

    pub fn chars_lossy(&self) -> impl Iterator<Item = char> {
        self.iter().map(|(_, c)| match c {
            CharCode::Char(c) => c,
            CharCode::Code(_) => '\u{FFFD}',
        })
    }
}
