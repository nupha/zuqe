use std::{iter::Peekable, ops::Range};

use crate::string::{CharCode, CharCodeDecoder, Utf8Stream, Utf16Stream};

use super::StringBuf;

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

    fn len(&self) -> usize {
        match self {
            Self::U8(s) => s.len(),
            Self::U16(s) => s.len(),
        }
    }

    fn set_pos(&mut self, i: usize) {
        match self {
            Self::U8(s) => s.set_pos(i),
            Self::U16(s) => s.set_pos(i),
        }
    }

    fn pos(&self) -> Option<usize> {
        match self {
            Self::U8(s) => s.pos(),
            Self::U16(s) => s.pos(),
        }
    }

    fn seek_relative(&mut self, offset: i32) {
        match self {
            Self::U8(s) => s.seek_relative(offset),
            Self::U16(s) => s.seek_relative(offset),
        }
    }

    fn is_end(&self) -> bool {
        match self {
            Self::U8(s) => s.is_end(),
            Self::U16(s) => s.is_end(),
        }
    }

    fn is_well_formed(&self) -> bool {
        match self {
            Self::U8(s) => s.is_well_formed(),
            Self::U16(s) => s.is_well_formed(),
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
        s.seek_relative(start as _);
        Self::U8(s)
    }

    pub fn from_u16(slice: &'a [u16], start: usize) -> Self {
        let mut s = Utf16Stream::new(slice);
        s.seek_relative(start as _);
        Self::U16(s)
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
