use std::{borrow::Cow, cmp::Ordering, collections::TryReserveError, ops::Range};

use fallible_collections::TryCollect;
use widestring::U16Str;

use crate::number::{parse_i64, parse_u32};

use super::{
    StringBuf, StringChar, StringCharIter, StringCodepointIter, U16CharIter, s16_into_s8,
    unicode_to_utf8,
};

/// read-only string content.
#[repr(u8)]
#[derive(Clone, Default)]
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
            (Self::Ascii(s1), Self::Utf16(s2)) => {
                let mut it1 = unsafe { std::str::from_utf8_unchecked(s1.as_ref()) }.chars();
                let mut it2 = U16Str::from_slice(s2.as_ref()).chars_lossy();
                loop {
                    match (it1.next(), it2.next()) {
                        (Some(a), Some(b)) => {
                            if a != b {
                                return false;
                            }
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
            (Self::Utf16(s1), Self::Ascii(s2)) => {
                let mut it1 = U16Str::from_slice(s1.as_ref()).chars_lossy();
                let mut it2 = unsafe { std::str::from_utf8_unchecked(s2.as_ref()) }.chars();
                loop {
                    match (it1.next(), it2.next()) {
                        (Some(a), Some(b)) => {
                            if a != b {
                                return false;
                            }
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

            // static str vs ascii, utf16
            (Self::Ascii(sa), Self::AsciiStatic(ss)) | (Self::AsciiStatic(ss), Self::Ascii(sa)) => {
                ss.as_bytes().eq(sa.as_ref())
            }
            (Self::AsciiStatic(ss), Self::Utf16(us)) | (Self::Utf16(us), Self::AsciiStatic(ss)) => {
                let mut it1 = U16Str::from_slice(us.as_ref()).chars_lossy();
                let mut it2 = ss.chars();
                loop {
                    match (it1.next(), it2.next()) {
                        (Some(a), Some(b)) => {
                            if a != b {
                                return false;
                            }
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
            Self::Empty => write!(f, "Empty"),
            Self::Ascii(arg0) => f.debug_tuple("Ascii").field(arg0).finish(),
            Self::Utf16(arg0) => f.debug_tuple("Utf16").field(arg0).finish(),
            Self::AsciiStatic(s) => write!(f, "AsciiStatic({:?})", s),
            Self::Ident => write!(f, "Ident"),
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

    /// get u8 slice if content is ascii
    pub fn ascii(&self) -> Option<&[u8]> {
        if let Self::Ascii(s) = self {
            Some(s.as_ref())
        } else {
            None
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

    /// encode content to utf8 bytes.
    ///
    /// Note: if has invalid utf16 code points, encode to invalid utf8 bytes too.
    pub fn encode_utf8_bytes_range(&self, range: Range<usize>) -> Cow<'_, [u8]> {
        if range.is_empty() || range.start >= self.len() {
            Cow::Borrowed(&[])
        } else {
            let range = range.start..std::cmp::min(range.end, self.len());
            match self {
                Self::Empty | Self::Ident => Cow::Borrowed(&[]),
                Self::Ascii(s) => Cow::Borrowed(s),
                Self::Utf16(s) => {
                    let slice = &s[range];
                    let len = slice
                        .iter()
                        .map(|x| if *x >= 0x80 { 4 } else { 1 })
                        .sum::<usize>();
                    let mut out = Vec::<u8>::with_capacity(len);

                    let mut it = slice.iter().peekable();
                    while let Some(&c) = it.next() {
                        if c < 0x80 {
                            out.push(c as u8);
                        } else {
                            let mut c = c as u32;
                            if (0xd800..0xdc00).contains(&c)
                                && let Some(c1) = it.peek()
                            {
                                let c1 = (**c1) as u32;
                                if (0xdc00..0xe000).contains(&c1) {
                                    // surrogate pair
                                    c = (((c & 0x3ff) << 10) | (c1 & 0x3ff)) + 0x10000;
                                    it.next();
                                }
                            }
                            unicode_to_utf8(&mut out, c)
                        }
                    }

                    Cow::Owned(out)
                }
                Self::AsciiStatic(s) => Cow::Borrowed(s.as_bytes()),
            }
        }
    }

    /// Encode string content to utf8 bytes.
    ///
    /// Note: If content has invalid utf16 code points, encode it as invalid utf8 bytes too.
    #[inline(always)]
    pub fn encode_utf8_bytes(&self) -> Cow<'_, [u8]> {
        self.encode_utf8_bytes_range(0..self.len())
    }

    /// encode content to utf8 str.
    /// if `escape` is true, invalid code points will be escaped as '\uXXXX',
    /// otherwise invalid codes will be replaced with 'U+FFFD'.
    pub fn to_utf8_range(&self, range: Range<usize>, escape: bool) -> Cow<'_, str> {
        if range.is_empty() || range.start >= self.len() {
            Cow::Borrowed("")
        } else {
            let range = range.start..std::cmp::min(range.end, self.len());
            match self {
                Self::Empty | Self::Ident => Cow::Borrowed(""),
                Self::Ascii(s) => {
                    if !escape {
                        Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&s[range]) })
                    } else {
                        let cnt_bs = s[range.clone()].iter().filter(|x| **x == b'\\').count();
                        if cnt_bs == 0 {
                            Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&s[range]) })
                        } else {
                            let mut b = String::with_capacity(s.len() + cnt_bs);
                            for c in &s[range] {
                                if *c == b'\\' {
                                    b.push('\\');
                                    b.push('\\');
                                } else {
                                    b.push(*c as char);
                                }
                            }
                            Cow::Owned(b)
                        }
                    }
                }
                Self::Utf16(s) => {
                    let slice = &s[range.clone()];

                    let out = if !escape {
                        String::from_utf16_lossy(slice)
                    } else {
                        let mut out = String::with_capacity(slice.len() * 4);
                        for (_, c) in U16CharIter::new(slice, 0) {
                            match c {
                                StringChar::Char(c) => {
                                    if c == '\\' {
                                        out.push('\\');
                                        out.push('\\');
                                    } else {
                                        out.push(c)
                                    }
                                }
                                StringChar::Code(c) => {
                                    out.push_str(&format!("\\u{c:04x}"));
                                }
                            }
                        }
                        out.shrink_to_fit();
                        out
                    };

                    Cow::Owned(out)
                }
                Self::AsciiStatic(s) => {
                    let bb = s.as_bytes();

                    if !escape {
                        Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&bb[range]) })
                    } else {
                        let cnt_bs = bb[range.clone()].iter().filter(|x| **x == b'\\').count();
                        if cnt_bs == 0 {
                            Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&bb[range]) })
                        } else {
                            let mut b = String::with_capacity(bb.len() + cnt_bs);
                            for c in &bb[range] {
                                if *c == b'\\' {
                                    b.push('\\');
                                    b.push('\\');
                                } else {
                                    b.push(*c as char);
                                }
                            }
                            Cow::Owned(b)
                        }
                    }
                }
            }
        }
    }

    /// encode content to utf8 str.
    /// if `escape` is true, invalid char codes will be escaped as '\uXXXX',
    /// otherwise invalid char codes will be replaced with 'U+FFFD'.
    #[inline(always)]
    pub fn to_utf8(&self, escape: bool) -> Cow<'_, str> {
        self.to_utf8_range(0..self.len(), escape)
    }

    /// get codepoint at index.
    /// returns None if index is out of bound.
    pub fn get(&self, index: usize) -> Option<u16> {
        match self {
            Self::Empty | Self::Ident => None,
            Self::Ascii(lst) => lst.as_ref().get(index).map(|n| *n as u16),
            Self::Utf16(lst) => lst.as_ref().get(index).copied(),
            Self::AsciiStatic(s) => s.as_bytes().get(index).map(|n| *n as u16),
        }
    }

    /// iterate over string code points. `range` is the range of buf index.
    /// if `range` has out of bound, range is intersected with buf.
    pub fn iter_codepoint_range(&self, range: Range<usize>) -> StringCodepointIter<'_> {
        match self {
            Self::Empty | Self::Ident => StringCodepointIter::U8([].iter().peekable()),
            Self::Ascii(s) => {
                if let Some(slice) = { s.as_ref().get(range) } {
                    StringCodepointIter::U8(slice.iter().peekable())
                } else {
                    StringCodepointIter::U8([].iter().peekable())
                }
            }
            Self::Utf16(s) => {
                if let Some(slice) = { s.as_ref().get(range) } {
                    StringCodepointIter::U16(slice.iter().peekable())
                } else {
                    StringCodepointIter::U16([].iter().peekable())
                }
            }
            Self::AsciiStatic(s) => {
                if let Some(slice) = s.as_bytes().get(range) {
                    StringCodepointIter::U8(slice.iter().peekable())
                } else {
                    StringCodepointIter::U8([].iter().peekable())
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
                Self::Empty | Self::Ident => StringCharIter::from_u8(&[], 0),
                Self::Ascii(s) => StringCharIter::from_u8(
                    &s[range.start..std::cmp::min(range.end, s.len())],
                    range.start,
                ),
                Self::Utf16(s) => StringCharIter::from_u16(
                    &s[range.start..std::cmp::min(range.end, s.len())],
                    range.start,
                ),
                Self::AsciiStatic(s) => StringCharIter::from_u8(
                    &s.as_bytes()[range.start..std::cmp::min(range.end, s.len())],
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

    /// get iterator over string chars, invalid code points will be replaced with char 'U+FFFD'.
    pub fn chars_lossy(&self) -> impl Iterator<Item = char> {
        self.chars_range_lossy(0..self.len())
    }

    /// count string chars, include broken invalid code.
    pub fn chars_count(&self) -> usize {
        match self {
            Self::Empty | Self::Ident => 0,
            Self::Ascii(s) => s.as_ref().len(),
            Self::Utf16(_) => self.iter().count(),
            Self::AsciiStatic(s) => s.len(),
        }
    }

    /// find index of code occurrence.
    pub fn index_of_code(&self, code: u16, from_index: usize) -> Option<usize> {
        if !(0..self.len()).contains(&from_index) {
            return None;
        }

        match self {
            Self::Empty | Self::Ident => None,
            Self::Ascii(s) => {
                if code > 0x7F {
                    None
                } else {
                    let c1 = code as u8;

                    s[from_index..]
                        .iter()
                        .position(|&c| c == c1)
                        .map(|n| n + from_index)
                }
            }
            Self::Utf16(s) => s[from_index..]
                .iter()
                .position(|&c| c == code)
                .map(|n| n + from_index),
            Self::AsciiStatic(s) => {
                if code > 0x7F {
                    None
                } else {
                    let c1 = code as u8;
                    s.as_bytes()[from_index..]
                        .iter()
                        .position(|&c| c == c1)
                        .map(|n| n + from_index)
                }
            }
        }
    }

    /// find index of string buf occurrence in haystack `self`, starting at `from_index`.
    pub fn index_of(&self, needle: &Self, from_index: usize) -> Option<usize> {
        let len = self.len();
        let needle_len = needle.len();

        if from_index + needle_len > len {
            // range out of bound
            None
        } else if needle.is_empty() {
            Some(from_index)
        } else {
            let first = needle.get(0).unwrap();
            let it2 = needle.iter();
            let mut cursor = from_index;

            while let Some(i) = self.index_of_code(first, cursor) {
                debug_assert!(cursor + needle_len <= len, "{i} + {needle_len} <= {len}");

                if i + needle_len <= len
                    && self
                        .iter_range(i..(i + needle_len))
                        .zip(it2.clone())
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

    /// find index of char occurrence in self, starting at from.
    pub fn index_of_char<C: Into<StringChar>>(&self, ch: C, from_index: usize) -> Option<usize> {
        let len = self.len();
        if !(0..len).contains(&from_index) {
            None
        } else {
            let ch: StringChar = ch.into();
            self.iter_since(from_index)
                .find(|(_, c)| ch.eq(c))
                .map(|(i, _)| i + from_index)
        }
    }

    /// get index of next char or code nearest to `pos` .
    pub fn index_of_next(&self, pos: usize, must_char: bool) -> Option<usize> {
        if pos < self.len() {
            if must_char && self.is_wide() {
                for (i, c) in self.iter_since(pos) {
                    if matches!(c, StringChar::Char(_)) {
                        return Some(i);
                    }
                }
            } else {
                return Some(pos + 1);
            }
        }

        None
    }

    /// compare content with str
    pub fn eq_str(&self, other: &str) -> bool {
        match self {
            Self::Empty => other.is_empty(),
            Self::Ascii(s) => s.len() == other.len() && s.as_ref() == other.as_bytes(),
            Self::Utf16(s) => {
                let mut a = U16Str::from_slice(s.as_ref()).chars_lossy();
                let mut b = other.chars();
                loop {
                    match (a.next(), b.next()) {
                        (Some(a), Some(b)) => {
                            if a != b {
                                return false;
                            }
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
            Self::AsciiStatic(s) => s.len() == other.len() && s.as_bytes() == other.as_bytes(),

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
}

// /// calculates hash of a sequence of chars (code points)
// fn hash_codepoints(s: impl Iterator<Item = StringChar>) -> u32 {
//     let mut h = 0; // init hash
//     s.for_each(|c| {
//         h = hash_u32_from_base(h, c.to_u32());
//     });
//     h
// }

impl StringData {
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
}

#[cfg(feature = "string_intern")]
impl Drop for StringData {
    fn drop(&mut self) {
        match self {
            Self::Ascii(s) => {
                if Rc::strong_count(s) == 1 {
                    Self::try_unintern(self);
                }
            }
            Self::Utf16(s) => {
                if Rc::strong_count(s) == 1 {
                    Self::try_unintern(self);
                }
            }
            _ => {}
        }
    }
}

#[cfg(feature = "string_intern")]
impl StringData {
    thread_local! {
        static STRING_CONTENT_INTERN_DICT: RefCell<HashMap<u32, Vec<StringDataWeakref>>> = RefCell::new(HashMap::new());
    }

    /// get or make a string content interned
    fn intern(buf: StringBuf) -> StringData {
        if buf.is_empty() {
            return Self::Empty;
        }

        let hash = hash_codepoints(buf.iter().map(|(_, c)| c));
        Self::STRING_CONTENT_INTERN_DICT.with_borrow_mut(|lut| {
            if let Some(lst) = lut.get_mut(&hash) {
                if let Some(s) = lst.iter().find_map(|w| w.upgrade()) {
                    if s.eq_buf(&buf) {
                        s
                    } else {
                        let s = StringData::from_buf(buf);
                        lst.push(s.downgrade());
                        s
                    }
                } else {
                    let s = StringData::from_buf(buf);
                    lst.push(s.downgrade());
                    s
                }
            } else {
                let s = StringData::from_buf(buf);
                lut.insert(hash, vec![s.downgrade()]);
                s
            }
        })
    }

    /// remove a string content from the intern pool.
    /// this is called when last reference of string content is dropped.
    ///
    /// See `StringData::drop()`
    fn try_unintern(cc: &StringData) {
        let hash = hash_codepoints(cc.iter().map(|(_, c)| c));

        let r = Self::STRING_CONTENT_INTERN_DICT.try_with(|lut| {
            let mut lut = lut.borrow_mut();

            if let Some(lst) = lut.get_mut(&hash) {
                lst.retain(|w| {
                    if let Some(w) = w.upgrade() {
                        !w.eq(cc)
                    } else {
                        false
                    }
                });
                if lst.is_empty() {
                    lut.remove(&hash);
                }
            }
        });

        #[cfg(debug_assertions)]
        if r.is_err() {
            println!("ACCESS STRING INTERN AFTER THREAD EXIT: {:?}", cc);
        }
    }

    fn downgrade(&self) -> StringDataWeakref {
        match self {
            StringData::Ascii(s) => StringDataWeakref::Ascii(Rc::downgrade(s)),
            StringData::Utf16(s) => StringDataWeakref::Utf16(Rc::downgrade(s)),
            StringData::Empty => StringDataWeakref::Empty,
            StringData::AsciiStatic(s) => StringDataWeakref::AsciiStr(s),
            StringData::Ident => StringDataWeakref::Ident,
        }
    }
}

#[cfg(feature = "string_intern")]
#[derive(Debug, Clone)]
#[repr(u8)]
enum StringDataWeakref {
    Empty,
    Ascii(Weak<[u8]>),
    Utf16(Weak<[u16]>),
    AsciiStr(&'static str),
    Ident,
}

#[cfg(feature = "string_intern")]
impl StringDataWeakref {
    pub fn upgrade(&self) -> Option<StringData> {
        match self {
            StringDataWeakref::Ascii(s) => s.upgrade().map(StringData::Ascii),
            StringDataWeakref::Utf16(s) => s.upgrade().map(StringData::Utf16),
            StringDataWeakref::Empty => Some(StringData::Empty),
            StringDataWeakref::AsciiStr(s) => Some(StringData::AsciiStatic(s)),
            StringDataWeakref::Ident => Some(StringData::Ident),
        }
    }
}
