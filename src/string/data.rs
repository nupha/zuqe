use std::{borrow::Cow, cmp::Ordering, collections::TryReserveError, ops::Range};

use fallible_collections::TryCollect;

use crate::{
    number::{parse_i64, parse_u32},
    string::{Utf8Stream, Utf16Stream, escape_str, is_all_chars_same},
    utils::unlikely,
};

use super::{CharCode, CharCodeDecoder, StringBuf, StringCharIter, s16_into_s8, u32_encode_utf8};

/// read-only string content.
#[repr(u8)]
#[derive(Default, Clone)]
pub enum StringData {
    #[default]
    Empty,

    Ascii(Box<[u8]>), // TODO: upgrade to ISO8859 variant to hold 0..=255 chars
    Utf16(Box<[u16]>),

    /// the static ascii str
    AsciiStatic(&'static str),

    /// identical content. used for anonymous Symbol atom.
    Ident,
}

#[cfg(debug_assertions)]
impl Drop for StringData {
    fn drop(&mut self) {
        log::trace!("StringData::drop() {}", self.encode_utf8(false));
    }
}

impl PartialEq for StringData {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            // same kind
            (Self::Ascii(s1), Self::Ascii(s2)) => s1.as_ref() == s2.as_ref(),
            (Self::Utf16(s1), Self::Utf16(s2)) => s1.as_ref() == s2.as_ref(),
            (Self::AsciiStatic(s1), Self::AsciiStatic(s2)) => s1.eq(s2),
            (Self::Empty, Self::Empty) => true,
            (Self::Ident, Self::Ident) => false, // idents are never equal

            // ascii vs utf16
            (Self::Ascii(s8), Self::Utf16(s16)) | (Self::Utf16(s16), Self::Ascii(s8)) => {
                is_all_chars_same(Utf8Stream::new(s8), Utf16Stream::new(s16))
            }

            // static vs ascii, utf16
            (Self::Ascii(s8), Self::AsciiStatic(ss)) | (Self::AsciiStatic(ss), Self::Ascii(s8)) => {
                ss.as_bytes().eq(s8.as_ref())
            }
            (Self::AsciiStatic(ss), Self::Utf16(s16))
            | (Self::Utf16(s16), Self::AsciiStatic(ss)) => {
                is_all_chars_same(Utf8Stream::new(ss.as_bytes()), Utf16Stream::new(s16))
            }

            #[cfg(debug_assertions)]
            (Self::Empty, s) | (s, Self::Empty) => {
                debug_assert!(!s.is_empty(), "ascii or utf16 string cannot be empty");
                false
            }

            _ => false,
        }
    }
}

impl Eq for StringData {}

impl Ord for StringData {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            // for same kind.
            (StringData::Ascii(s1), StringData::Ascii(s2)) => s1.as_ref().cmp(s2.as_ref()),
            (StringData::Utf16(s1), StringData::Utf16(s2)) => s1.as_ref().cmp(s2.as_ref()),
            (StringData::Empty, StringData::Empty) | (StringData::Ident, StringData::Ident) => {
                Ordering::Equal
            }

            // utf8 vs utf16 cmp
            (StringData::Ascii(s1), StringData::Utf16(s2)) => {
                let mut it1 = s1.as_ref().iter();
                let mut it2 = s2.as_ref().iter();
                loop {
                    match (it1.next(), it2.next()) {
                        (Some(a), Some(b)) => {
                            let a = *a as u16;
                            if a != *b {
                                return a.cmp(b);
                            }
                        }
                        (None, None) => {
                            return Ordering::Equal;
                        }
                        (Some(_), None) => {
                            return Ordering::Greater;
                        }
                        (None, Some(_)) => {
                            return Ordering::Less;
                        }
                    }
                }
            }
            (StringData::Utf16(s1), StringData::Ascii(s2)) => {
                let mut it1 = s1.as_ref().iter();
                let mut it2 = s2.as_ref().iter();
                loop {
                    match (it1.next(), it2.next()) {
                        (Some(a), Some(b)) => {
                            let b = *b as u16;
                            if *a != b {
                                return a.cmp(&b);
                            }
                        }
                        (None, None) => {
                            return Ordering::Equal;
                        }
                        (Some(_), None) => {
                            return Ordering::Greater;
                        }
                        (None, Some(_)) => {
                            return Ordering::Less;
                        }
                    }
                }
            }

            // str vs ascii, str
            (StringData::AsciiStatic(t1), StringData::AsciiStatic(t2)) => t1.cmp(t2),
            (StringData::Ascii(a), StringData::AsciiStatic(s)) => a.as_ref().cmp(s.as_bytes()),
            (StringData::AsciiStatic(s), StringData::Ascii(a)) => s.as_bytes().cmp(a.as_ref()),

            // str vs utf16
            (StringData::AsciiStatic(s), StringData::Utf16(w)) => {
                let mut it1 = s.as_bytes().iter();
                let mut it2 = w.as_ref().iter();
                loop {
                    match (it1.next(), it2.next()) {
                        (Some(a), Some(b)) => {
                            let a = *a as u16;
                            if a != *b {
                                return a.cmp(b);
                            }
                        }
                        (None, None) => {
                            return Ordering::Equal;
                        }
                        (Some(_), None) => {
                            return Ordering::Greater;
                        }
                        (None, Some(_)) => {
                            return Ordering::Less;
                        }
                    }
                }
            }
            (StringData::Utf16(w), StringData::AsciiStatic(s)) => {
                let mut it1 = w.as_ref().iter();
                let mut it2 = s.as_bytes().iter();
                loop {
                    match (it1.next(), it2.next()) {
                        (Some(a), Some(b)) => {
                            let b = *b as u16;
                            if *a != b {
                                return a.cmp(&b);
                            }
                        }
                        (None, None) => {
                            return Ordering::Equal;
                        }
                        (Some(_), None) => {
                            return Ordering::Greater;
                        }
                        (None, Some(_)) => {
                            return Ordering::Less;
                        }
                    }
                }
            }

            // ident is the greatest.
            (StringData::Ident, _) => Ordering::Greater,
            (_, StringData::Ident) => Ordering::Less,

            // empty is the smallest
            (StringData::Empty, _) => Ordering::Less,
            (_, StringData::Empty) => Ordering::Greater,
        }
    }
}

impl PartialOrd for StringData {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Debug for StringData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ascii(arg0) => f.debug_tuple("StringData::Ascii").field(arg0).finish(),
            Self::Utf16(arg0) => f.debug_tuple("StringData::Utf16").field(arg0).finish(),
            Self::AsciiStatic(s) => f.debug_tuple("StringData::AsciiStatic").field(s).finish(),
            Self::Empty => write!(f, "StringData::Empty"),
            Self::Ident => write!(f, "StringData::Ident"),
        }
    }
}

impl StringData {
    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// is identical content
    #[inline(always)]
    pub const fn is_ident(&self) -> bool {
        matches!(self, Self::Ident)
    }

    #[inline(always)]
    pub const fn is_wide(&self) -> bool {
        matches!(self, Self::Utf16(_))
    }

    /// get u8 slice if content is ascii only
    pub fn ascii(&self) -> Option<&[u8]> {
        match self {
            Self::Ascii(s) => Some(s.as_ref()),
            Self::AsciiStatic(s) => Some(s.as_bytes()),
            Self::Empty | Self::Ident => Some(&[]),
            Self::Utf16(_) => None,
        }
    }

    /// get u16 slice if content is utf16
    pub fn utf16(&self) -> Option<&[u16]> {
        if let Self::Utf16(s) = self {
            Some(s.as_ref())
        } else {
            None
        }
    }

    /// codepoint length.
    pub fn len(&self) -> usize {
        match self {
            Self::Empty | Self::Ident => 0,
            Self::Ascii(s) => s.as_ref().len(),
            Self::Utf16(s) => s.as_ref().len(),
            Self::AsciiStatic(s) => s.len(),
        }
    }

    /// create string buf from content.
    /// if range is out of content length, take the rest content till end.
    pub fn to_buf_range(&self, range: Range<usize>) -> Result<StringBuf, TryReserveError> {
        if range.is_empty() || range.start >= self.len() {
            Ok(StringBuf::Empty)
        } else {
            match self {
                Self::Empty | Self::Ident => Ok(StringBuf::Empty),
                Self::Ascii(s) => {
                    let lst = s
                        .iter()
                        .skip(range.start)
                        .take(range.len())
                        .copied()
                        .try_collect::<Vec<_>>()?;
                    Ok(StringBuf::Ascii(lst))
                }
                Self::Utf16(s) => {
                    let it = s.iter().skip(range.start).take(range.len());
                    if it.clone().any(|c| *c >= 0x80) {
                        // wide string
                        let lst = it.copied().try_collect::<Vec<_>>()?;
                        Ok(StringBuf::Utf16(lst))
                    } else {
                        // ascii string
                        let lst = it.map(|c| *c as u8).try_collect::<Vec<_>>()?;
                        Ok(StringBuf::Ascii(lst))
                    }
                }
                Self::AsciiStatic(s) => {
                    let lst = s
                        .as_bytes()
                        .iter()
                        .skip(range.start)
                        .take(range.len())
                        .copied()
                        .try_collect::<Vec<_>>()?;
                    Ok(StringBuf::Ascii(lst))
                }
            }
        }
    }

    #[inline(always)]
    pub fn to_buf(&self) -> Result<StringBuf, TryReserveError> {
        self.to_buf_range(0..self.len())
    }

    /// Encode range of content to utf8 bytes.
    ///
    /// For invalid code point or broken surrogate, when `allow_invalid` is true, force encode it to utf8;
    /// otherwise replaced with U+FFFD.
    pub(crate) fn encode_utf8_bytes_internal(
        &self,
        range: Range<usize>,
        allow_invalid: bool,
        keep_surrogate_pair: bool,
    ) -> Cow<'_, [u8]> {
        if range.is_empty() || range.start >= self.len() {
            Cow::Borrowed(&[])
        } else {
            let range = range.start..std::cmp::min(range.end, self.len());
            if unlikely(range.is_empty()) {
                Cow::Borrowed(&[])
            } else {
                match self {
                    Self::Empty | Self::Ident => Cow::Borrowed(&[]),
                    Self::Ascii(s) => Cow::Borrowed(s),
                    Self::Utf16(s) => {
                        let src = &s[range];

                        let mut buf = Vec::<u8>::with_capacity(
                            src.iter()
                                .map(|x| if *x >= 0x80 { 4 } else { 1 })
                                .sum::<usize>(),
                        );
                        let mut it = src.iter().peekable();

                        while let Some(&n) = it.next() {
                            if n < 0x80 {
                                buf.push(n as u8);
                            } else {
                                let mut cp = n as u32;
                                let is_surrogate = if keep_surrogate_pair {
                                    (0xd800..0xe000).contains(&cp)
                                } else {
                                    if (0xd800..0xdc00).contains(&cp) // high
                                    && !keep_surrogate_pair
                                    && let Some(c1) = it.peek()
                                    && (0xdc00..0xe000).contains(*c1)
                                    {
                                        // valid surrogate pair
                                        let lo = **c1 as u32;
                                        cp = (((cp & 0x3ff) << 10) | (lo & 0x3ff)) + 0x10000;
                                        it.next();
                                    }
                                    false
                                };

                                buf.extend_from_slice(u32_encode_utf8(
                                    cp,
                                    &mut [0; 6],
                                    allow_invalid || is_surrogate,
                                ));
                            }
                        }

                        Cow::Owned(buf)
                    }
                    Self::AsciiStatic(s) => Cow::Borrowed(s.as_bytes()),
                }
            }
        }
    }

    /// Encode string content to utf8 bytes.
    ///
    /// For invalid code point or broken surrogate, when `allow_invalid` is true, force encode it to utf8;
    /// otherwise encode U+FFFD instead.
    #[inline(always)]
    pub fn encode_utf8_bytes(&self, allow_invalid: bool) -> Cow<'_, [u8]> {
        self.encode_utf8_bytes_internal(0..self.len(), allow_invalid, false)
    }

    /// Encode content to utf8 str.
    ///
    /// For invalid code points:
    /// * if `escape` is true, it will be escaped as '\uXXXX',
    /// * if `escape` is false, it will be encoded as U+FFFD.
    ///
    /// If `escape` is true, char '\' will be represented by "\\""
    pub fn encode_utf8_range(&self, range: Range<usize>, escape: bool) -> Cow<'_, str> {
        if unlikely(range.is_empty() || range.start >= self.len()) {
            Cow::Borrowed("")
        } else {
            let range = range.start..std::cmp::min(range.end, self.len());
            if unlikely(range.is_empty()) {
                Cow::Borrowed("")
            } else {
                match self {
                    Self::Ascii(s) => {
                        let t = unsafe { std::str::from_utf8_unchecked(&s[range]) };
                        if !escape {
                            Cow::Borrowed(t)
                        } else {
                            escape_str(t)
                        }
                    }
                    Self::AsciiStatic(t) => {
                        let t = &t[range];
                        if !escape {
                            Cow::Borrowed(t)
                        } else {
                            escape_str(t)
                        }
                    }
                    Self::Utf16(s) => {
                        let slice = &s[range];
                        let out = if !escape {
                            String::from_utf16_lossy(slice)
                        } else {
                            let mut buf = String::with_capacity(slice.len() * 4);
                            let mut it = Utf16Stream::new(slice);

                            while let Some((_, cc)) = it.next_char() {
                                match cc {
                                    CharCode::Char('\\') => {
                                        buf.push_str(r"\\");
                                    }
                                    // CharCode::Char('"') => {
                                    //     buf.push_str("\\\"");
                                    // }
                                    CharCode::Char(c) => {
                                        buf.push(c);
                                    }
                                    CharCode::Code(cp) => {
                                        buf.push_str(&format!("\\u{cp:04x}"));
                                    }
                                }
                            }

                            buf.shrink_to_fit();
                            buf
                        };

                        Cow::Owned(out)
                    }
                    Self::Empty | Self::Ident => Cow::Borrowed(""),
                }
            }
        }
    }

    /// encode content to utf8 str.
    /// if `escape` is true, invalid char codes will be escaped as '\uXXXX',
    /// otherwise invalid char codes will be replaced with 'U+FFFD'.
    #[inline(always)]
    pub fn encode_utf8(&self, escape: bool) -> Cow<'_, str> {
        self.encode_utf8_range(0..self.len(), escape)
    }

    /// get code point at index.
    /// returns None if index is out of bound.
    pub fn get(&self, index: usize) -> Option<u16> {
        match self {
            Self::Empty | Self::Ident => None,
            Self::Ascii(lst) => lst.as_ref().get(index).map(|n| *n as u16),
            Self::Utf16(lst) => lst.as_ref().get(index).copied(),
            Self::AsciiStatic(s) => s.as_bytes().get(index).map(|n| *n as u16),
        }
    }

    /// iterate over string chars. `range` is the range of buf index.
    /// if `range` is out of bounds, the intersected range is applied.
    /// for broken surrogate, returns raw code directly, otherwise returns unichar
    pub fn chars_range(&self, range: Range<usize>) -> StringCharIter<'_> {
        if unlikely(range.is_empty() || range.start >= self.len()) {
            StringCharIter::from_u8(&[], 0)
        } else {
            match self {
                Self::Ascii(s) => {
                    StringCharIter::from_u8(&s[0..std::cmp::min(range.end, s.len())], range.start)
                }
                Self::AsciiStatic(s) => StringCharIter::from_u8(
                    &s.as_bytes()[0..std::cmp::min(range.end, s.len())],
                    range.start,
                ),
                Self::Utf16(s) => {
                    StringCharIter::from_u16(&s[0..std::cmp::min(range.end, s.len())], range.start)
                }
                Self::Empty | Self::Ident => StringCharIter::from_u8(&[], 0),
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

    pub fn chars_range_lossy(&self, range: Range<usize>) -> impl Iterator<Item = char> {
        self.chars_range(range).map(|(_, c)| match c {
            CharCode::Char(c) => c,
            CharCode::Code(_) => '\u{FFFD}',
        })
    }

    /// get iterator over string chars, invalid code points will be replaced with char 'U+FFFD'.
    pub fn chars_lossy(&self) -> impl Iterator<Item = char> {
        self.chars_range_lossy(0..self.len())
    }

    /// count string chars, include broken invalid code.
    pub fn chars_count(&self) -> usize {
        match self {
            Self::Empty | Self::Ident => 0,
            Self::Ascii(s) => s.len(),
            Self::Utf16(_) => self.chars().count(),
            Self::AsciiStatic(s) => s.len(),
        }
    }

    /// find index of code occurrence.
    pub fn index_of_code(&self, code: u16, start: usize) -> Option<usize> {
        if (0..self.len()).contains(&start) {
            match self {
                Self::Empty | Self::Ident => None,
                Self::Ascii(s) => {
                    if code > 0x7F {
                        None
                    } else {
                        let c1 = code as u8;
                        s[start..].iter().position(|&c| c == c1).map(|n| n + start)
                    }
                }
                Self::Utf16(s) => s[start..]
                    .iter()
                    .position(|&c| c == code)
                    .map(|n| n + start),
                Self::AsciiStatic(s) => {
                    if code > 0x7F {
                        None
                    } else {
                        let c1 = code as u8;
                        s.as_bytes()[start..]
                            .iter()
                            .position(|&c| c == c1)
                            .map(|n| n + start)
                    }
                }
            }
        } else {
            None
        }
    }

    /// find index of string buf occurrence in haystack `self`, starting since `start`.
    pub fn index_of(&self, needle: &Self, start: usize) -> Option<usize> {
        let len = self.len();
        let needle_len = needle.len();

        if start + needle_len > len {
            // range out of bound
            None
        } else if needle.is_empty() {
            Some(start)
        } else {
            let first = needle.get(0).unwrap();
            let mut cursor = start;

            while let Some(i) = self.index_of_code(first, cursor) {
                debug_assert!(cursor + needle_len <= len, "{i} + {needle_len} <= {len}");

                if i + needle_len <= len
                    && self
                        .chars_range(i..(i + needle_len))
                        .zip(needle.chars())
                        .all(|((_, c1), (_, c2))| c1 == c2)
                {
                    return Some(i); // found at cursor
                }

                // forward for next occurrence
                cursor = i + 1;
                if cursor + needle_len > len {
                    break;
                }
            }

            None
        }
    }

    /// find index of char occurrence in self, starting since `start`.
    pub fn index_of_char<C: Into<CharCode>>(&self, ch: C, start: usize) -> Option<usize> {
        let len = self.len();
        if !(0..len).contains(&start) {
            None
        } else {
            let ch: CharCode = ch.into();
            self.chars_since(start)
                .find(|(_, c)| ch.eq(c))
                .map(|(r, _)| r.end + start)
        }
    }

    /// get index of next char or code nearest to `pos` .
    pub fn index_of_next(&self, pos: usize, must_char: bool) -> Option<usize> {
        if pos < self.len() {
            if must_char && self.is_wide() {
                for (span, c) in self.chars_since(pos) {
                    if matches!(c, CharCode::Char(_)) {
                        return Some(span.end);
                    }
                }
            } else {
                return Some(pos + 1);
            }
        }

        None
    }

    pub(crate) fn index_of_next_i64(&self, pos: i64, must_char: bool) -> Option<i64> {
        if (0..self.len() as i64).contains(&pos) {
            #[cfg(target_pointer_width = "64")]
            let next_index = self
                .index_of_next(pos as usize, must_char)
                .map(|x| x as i64);

            #[cfg(target_pointer_width = "32")]
            let next_index = str.index_of_next(pos as usize, must_char).map(|x| x as i64);

            next_index
        } else {
            None
        }
    }

    /// Compare string content with `str` char by char
    pub fn eq_str(&self, other: &str) -> bool {
        match self {
            Self::Ascii(s) => s.len() == other.len() && s.as_ref() == other.as_bytes(),
            Self::AsciiStatic(s) => s.len() == other.len() && s.as_bytes() == other.as_bytes(),
            Self::Utf16(s) => {
                let mut it1 = Utf16Stream::new(s);
                let mut it2 = other.chars();
                loop {
                    match (it1.next(), it2.next()) {
                        (Some((_, CharCode::Char(a))), Some(b)) if a == b => {
                            continue;
                        }
                        (None, None) => {
                            return true;
                        }
                        _ => {
                            return false;
                        }
                    }
                }
            }
            Self::Empty => other.is_empty(),
            Self::Ident => false,
        }
    }

    /// compare content with stringbuf
    pub fn eq_buf(&self, other: &StringBuf) -> bool {
        match (self, other) {
            (Self::Empty, b) => b.is_empty(),
            (Self::Ascii(s), StringBuf::Ascii(b)) => s.as_ref() == b.as_slice(),
            (Self::Utf16(s), StringBuf::Utf16(b)) => s.as_ref() == b.as_slice(),

            (Self::Utf16(s), StringBuf::Ascii(b)) => {
                if s.len() == b.len() {
                    for (a, b) in s.iter().zip(b.iter().map(|x| *x as u16)) {
                        if *a != b {
                            return false;
                        }
                    }
                    true
                } else {
                    false
                }
            }
            (Self::Ascii(s), StringBuf::Utf16(b)) => {
                if s.len() == b.len() {
                    for (a, b) in s.iter().map(|x| *x as u16).zip(b.iter()) {
                        if a != *b {
                            return false;
                        }
                    }
                    true
                } else {
                    false
                }
            }

            _ => false,
        }
    }

    /// parse content as u32.
    pub fn parse_u32(&self) -> Option<u32> {
        if let Self::Ascii(s) = self {
            parse_u32(std::str::from_utf8(s).unwrap())
        } else {
            None
        }
    }

    /// parse content as i64.
    pub fn parse_i64(&self) -> Option<i64> {
        if let Self::Ascii(buf) = self {
            let s = std::str::from_utf8(buf).unwrap();
            parse_i64(s)
        } else {
            None
        }
    }

    pub fn from_buf(buf: StringBuf) -> Self {
        match buf {
            StringBuf::Empty => Self::Empty,
            StringBuf::Ascii(s) => {
                if s.is_empty() {
                    Self::Empty
                } else {
                    debug_assert!(s.is_ascii());
                    Self::Ascii(s.into_boxed_slice())
                }
            }
            StringBuf::Utf16(s) => {
                if s.is_empty() {
                    Self::Empty
                } else if !s.iter().any(|c| *c >= 0x80) {
                    // ascii
                    let data = unsafe { s16_into_s8(s) };
                    Self::Ascii(data.into_boxed_slice())
                } else {
                    Self::Utf16(s.into_boxed_slice())
                }
            }
        }
    }

    /// static str `s` must be ascii str.
    #[inline]
    pub fn from_static(s: &'static str) -> Self {
        if s.is_empty() {
            Self::Empty
        } else {
            debug_assert!(s.is_ascii());
            Self::AsciiStatic(s)
        }
    }
}
