use std::{borrow::Cow, cmp::Ordering, collections::TryReserveError, ops::Range, rc::Rc};

use fallible_collections::TryCollect;
use widestring::U16Str;

use crate::{
    context::JSContext,
    number::{ATOD_ACCEPT_BIN_OCT, js_atof, parse_i64, parse_u32},
    string::{
        StringBuf, StringChar, StringCharIter, StringCodepointIter, is_char_visible, s16_into_s8,
    },
    value::JSValue,
};

/// read-only, ref-counted bytes that represents a string.
#[derive(Clone)]
#[repr(u8)]
pub enum StringContent {
    Empty,
    Ascii(Rc<[u8]>),
    Utf16(Rc<[u16]>),
    /// identical content. used for anonymous Symbol atom.
    Ident,
}

impl Default for StringContent {
    fn default() -> Self {
        StringContent::Empty
    }
}

impl PartialEq for StringContent {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            // same kind
            (Self::Ascii(s1), Self::Ascii(s2)) => s1.as_ref() == s2.as_ref(),
            (Self::Utf16(s1), Self::Utf16(s2)) => s1.as_ref() == s2.as_ref(),
            (Self::Empty, Self::Empty) => true,
            (Self::Ident, Self::Ident) => false, // idents are never equal

            // ascii vs utf16
            (Self::Ascii(s1), Self::Utf16(s2)) => {
                let mut it1 = unsafe { std::str::from_utf8_unchecked(s1) }.chars();
                let mut it2 = U16Str::from_slice(s2).chars_lossy();
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
                let mut it1 = U16Str::from_slice(s1).chars_lossy();
                let mut it2 = unsafe { std::str::from_utf8_unchecked(s2) }.chars();
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

impl Eq for StringContent {}

impl Ord for StringContent {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            // for same kind.
            (StringContent::Ascii(s1), StringContent::Ascii(s2)) => s1.cmp(s2),
            (StringContent::Utf16(s1), StringContent::Utf16(s2)) => s1.cmp(s2),
            (StringContent::Empty, StringContent::Empty)
            | (StringContent::Ident, StringContent::Ident) => Ordering::Equal,

            // utf8 vs utf16 cmp
            (StringContent::Ascii(s1), StringContent::Utf16(s2)) => {
                let mut it1 = s1.iter();
                let mut it2 = s2.iter();
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
            (StringContent::Utf16(s1), StringContent::Ascii(s2)) => {
                let mut it1 = s1.iter();
                let mut it2 = s2.iter();
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
            (StringContent::Ident, _) => Ordering::Greater,
            (_, StringContent::Ident) => Ordering::Less,

            // empty is the smallest
            (StringContent::Empty, _) => Ordering::Less,
            (_, StringContent::Empty) => Ordering::Greater,
        }
    }
}

impl PartialOrd for StringContent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Debug for StringContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "\"\""),
            Self::Ident => write!(f, "<ident>"),
            _ => {
                let sep = '\"';
                write!(f, "{sep}")?;

                for (_, c) in self.iter() {
                    match c {
                        StringChar::Char(c) if c == sep => {
                            write!(f, "\\{c}")?;
                        }
                        StringChar::Char('\n') => {
                            write!(f, "\\n")?;
                        }
                        StringChar::Char('\r') => {
                            write!(f, "\\r")?;
                        }
                        StringChar::Char('\t') => {
                            write!(f, "\\t")?;
                        }
                        StringChar::Char('\\') => {
                            write!(f, "\\\\")?;
                        }
                        StringChar::Char(c) => {
                            if !is_char_visible(c) {
                                write!(f, "\\u{:04X}", c as u32)?;
                            } else {
                                write!(f, "{c}")?;
                            }
                        }
                        StringChar::Code(c) => {
                            write!(f, "\\u{c:04X}")?;
                        }
                    }
                }

                write!(f, "{sep}")
            }
        }
    }
}

impl StringContent {
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// is identical content
    #[inline(always)]
    pub fn is_ident(&self) -> bool {
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
            Self::Ascii(s) => s.len(),
            Self::Utf16(s) => s.len(),
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
            }
        }
    }

    #[inline(always)]
    pub fn to_buf(&self) -> Result<StringBuf, TryReserveError> {
        self.to_buf_range(0..self.len())
    }

    /// encode content to utf8
    pub fn to_utf8_range(&self, range: Range<usize>) -> Cow<'_, str> {
        if range.is_empty() || range.start >= self.len() {
            Cow::Borrowed("")
        } else {
            let range = range.start..std::cmp::min(range.end, self.len());
            match self {
                Self::Empty | Self::Ident => Cow::Borrowed(""),
                Self::Ascii(s) => {
                    Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(&s[range]) })
                }
                Self::Utf16(s) => Cow::Owned(String::from_utf16_lossy(&s[range])),
            }
        }
    }

    /// encode content to utf8
    #[inline(always)]
    pub fn to_utf8(&self) -> Cow<'_, str> {
        self.to_utf8_range(0..self.len())
    }

    /// get codepoint at index.
    /// returns None if index is out of bound.
    pub fn get(&self, index: usize) -> Option<u16> {
        match self {
            Self::Empty | Self::Ident => None,
            Self::Ascii(lst) => lst.get(index).map(|n| *n as u16),
            Self::Utf16(lst) => lst.get(index).map(|n| *n),
        }
    }

    /// iterate over string code points. `range` is the range of buf index.
    /// if `range` has out of bound, range is intersected with buf.
    pub fn iter_codepoint_range(&self, range: Range<usize>) -> StringCodepointIter<'_> {
        match self {
            Self::Empty | Self::Ident => StringCodepointIter::U8([].iter().peekable()),
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
                Self::Empty | Self::Ident => StringCharIter::from_u8(&[], 0),
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

    /// count string chars, include broken invalid code.
    pub fn chars_count(&self) -> usize {
        match self {
            Self::Empty | Self::Ident => 0,
            Self::Ascii(s) => s.len(),
            Self::Utf16(_) => self.iter().count(),
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
            let first = needle.get(0).unwrap().clone();
            let it2 = needle.iter();
            let mut cursor = from_index;

            while let Some(i) = self.index_of_code(first, cursor) {
                debug_assert!(cursor + needle_len <= len, "{i} + {needle_len} <= {len}");

                if i + needle_len <= len {
                    if self
                        .iter_range(i..(i + needle_len))
                        .zip(it2.clone())
                        .all(|((_, c1), (_, c2))| c1 == c2)
                    {
                        return Some(i); // found at cursor
                    }
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
                let mut a = U16Str::from_slice(s).chars_lossy();
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
            Self::Ident => false,
        }
    }

    /// parse content as u32.
    pub fn parse_u32(&self) -> Result<u32, i32> {
        if let Self::Ascii(s) = self {
            parse_u32(std::str::from_utf8(s).map_err(|_| -1)?)
        } else {
            Err(-1) // can't parse utf16, empty, null to u32
        }
    }

    /// parse content as i64.
    pub fn parse_i64(&self) -> Result<i64, i32> {
        if let Self::Ascii(s) = self {
            parse_i64(std::str::from_utf8(s).map_err(|_| -1)?)
        } else {
            Err(-1)
        }
    }

    /// parse content as f64.
    pub fn parse_f64(&self, ctx: &JSContext) -> Option<f64> {
        let str = self.to_utf8();
        let str = str.as_ref().trim();

        if str.is_empty() {
            Some(0.0)
        } else {
            let mut p = str.as_ptr();
            if let Ok(num) = unsafe { js_atof(ctx, p.cast(), &mut p, 0, ATOD_ACCEPT_BIN_OCT) } {
                if unsafe { p.offset_from(str.as_ptr()) != str.len() as isize } {
                    // `str` has remaining characters that are not part of the number.
                    // This is not a valid number.
                    None
                } else {
                    Some(match num {
                        JSValue::Int(i) => i as f64,
                        JSValue::Float64(f) => f,
                        _ => unreachable!(),
                    })
                }
            } else {
                None
            }
        }
    }
}

impl From<StringBuf> for StringContent {
    fn from(value: StringBuf) -> Self {
        match value {
            StringBuf::Empty => Self::Empty,
            StringBuf::Ascii(s) => {
                if s.is_empty() {
                    Self::Empty
                } else {
                    debug_assert!(s.is_ascii());
                    Self::Ascii(s.into())
                }
            }
            StringBuf::Utf16(s) => {
                if s.is_empty() {
                    Self::Empty
                } else {
                    // debug_assert!(s.iter().any(|c| *c >= 0x80));

                    if !s.iter().any(|c| *c >= 0x80) {
                        // ascii
                        Self::Ascii(unsafe { s16_into_s8(s).into() })
                    } else {
                        Self::Utf16(s.into())
                    }
                }
            }
        }
    }
}

impl StringBuf {
    /// concat other string content to self content, returns a new result string content.
    /// if `range` is out of bounds, the intersectioned range will be applied.
    pub fn push_content_range(
        &mut self,
        content: &StringContent,
        range: Range<usize>,
    ) -> Result<(), TryReserveError> {
        if range.is_empty() || range.start >= content.len() {
            return Ok(());
        }

        // intersect range
        let range = range.start..std::cmp::min(range.end, content.len());

        // use a temp var to hold current self content
        let orig_data = std::mem::replace(self, Self::Empty);

        let new_data: StringBuf = match (orig_data, content) {
            (s, StringContent::Empty | StringContent::Ident) => s,
            (Self::Empty, _) => content.to_buf_range(range)?,

            // same type
            (Self::Utf16(mut s1), StringContent::Utf16(s2)) => {
                if let Err(e) = s1.try_reserve_exact(s2.len()) {
                    *self = Self::Utf16(s1);
                    return Err(e);
                } else {
                    s1.extend_from_slice(&s2[range.clone()]);
                    Self::Utf16(s1)
                }
            }
            (Self::Ascii(mut s1), StringContent::Ascii(s2)) => {
                if let Err(e) = s1.try_reserve_exact(s2.len()) {
                    *self = Self::Ascii(s1);
                    return Err(e);
                } else {
                    s1.extend_from_slice(&s2[range.clone()]);
                    Self::Ascii(s1)
                }
            }

            (Self::Utf16(mut s1), StringContent::Ascii(s2)) => {
                if let Err(e) = s1.try_reserve(s2.len()) {
                    *self = Self::Utf16(s1);
                    return Err(e);
                }
                for c in unsafe { std::str::from_utf8_unchecked(&s2[range.clone()]) }.encode_utf16()
                {
                    s1.push(c);
                }
                Self::Utf16(s1)
            }
            (Self::Ascii(s1), StringContent::Utf16(s2)) => {
                let mut buf = Vec::<u16>::new();
                if let Err(e) = buf.try_reserve_exact(s1.len() + s2.len()) {
                    *self = Self::Ascii(s1); // restore original
                    return Err(e);
                }

                let a = unsafe { std::str::from_utf8_unchecked(s1.as_slice()) }.encode_utf16();
                let b = s2[range.clone()].iter().copied();
                buf.extend(a.chain(b));

                if !buf.iter().any(|c| *c >= 0x80) {
                    // is ascii
                    Self::Ascii(unsafe { s16_into_s8(buf) })
                } else {
                    Self::Utf16(buf)
                }
            }
        };

        *self = new_data;
        Ok(())
    }

    #[inline(always)]
    pub fn push_content(&mut self, content: &StringContent) -> Result<(), TryReserveError> {
        self.push_content_range(content, 0..content.len())
    }
}
