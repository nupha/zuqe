use std::ops::Range;

use super::{CharCode, CharCodeDecoder, StringBuf, Utf8Stream, Utf16Stream};

#[derive(Debug, Clone)]
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

impl StringBuf {
    /// iterate over string chars. `range` is the range of buf index.
    /// if `range` is out of bounds, the intersected range is applied.
    /// for broken surrogate, returns raw code directly, otherwise returns unichar
    pub fn chars_range(&self, range: Range<usize>) -> StringCharIter<'_> {
        if range.is_empty() || range.start >= self.len() {
            StringCharIter::from_u8(&[], range.start)
        } else {
            match self {
                Self::Empty => StringCharIter::from_u8(&[], 0),
                Self::Ascii(s) => {
                    StringCharIter::from_u8(&s[0..std::cmp::min(range.end, s.len())], range.start)
                }
                Self::Utf16(s) => {
                    StringCharIter::from_u16(&s[0..std::cmp::min(range.end, s.len())], range.start)
                }
            }
        }
    }

    /// iterate over string chars. `index` is the starting buf index.
    ///
    /// for broken surrogate, returns raw code directly, otherwise returns unichar
    #[inline(always)]
    pub fn chars_since(&self, index: usize) -> StringCharIter<'_> {
        self.chars_range(index..self.len())
    }

    /// iterate over string chars. short cut to `::iter_since(0)`.
    #[inline(always)]
    pub fn chars(&self) -> StringCharIter<'_> {
        self.chars_range(0..self.len())
    }
}
