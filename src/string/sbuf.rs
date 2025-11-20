use std::{borrow::Cow, collections::TryReserveError, ops::Range};

use fallible_collections::{FallibleVec, TryClone, TryCollect};

use super::{StringChar, StringData, char_is_space, string::JS_STRING_LEN_MAX};

/// string content buffer, used to build string
#[repr(u8)]
#[derive(Debug, Default)]
pub enum StringBuf {
    #[default]
    Empty,
    Ascii(Vec<u8>), // TODO: upgrade to ISO8859 variant to hold 0..=255 chars
    Utf16(Vec<u16>),
}

impl From<Vec<u8>> for StringBuf {
    fn from(v: Vec<u8>) -> Self {
        if v.is_empty() {
            Self::Empty
        } else {
            Self::Ascii(v)
        }
    }
}

impl From<Vec<u16>> for StringBuf {
    fn from(v: Vec<u16>) -> Self {
        if v.is_empty() {
            Self::Empty
        } else {
            Self::Utf16(v)
        }
    }
}

static EMPTY: StringBuf = StringBuf::Empty;

impl StringBuf {
    /// Creates StringBuf from str
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(str: &str) -> Result<Self, i32> {
        debug_assert!(str.len() < JS_STRING_LEN_MAX);

        if str.is_empty() {
            Ok(Self::Empty)
        } else if str.is_ascii() {
            let mut s = Vec::<u8>::new();
            s.try_reserve_exact(str.len()).map_err(|_| -1)?;
            s.try_extend_from_slice(str.as_bytes()).map_err(|_| -1)?;
            Ok(Self::Ascii(s))
        } else {
            let mut b = Vec::<u16>::new();
            b.try_reserve_exact(str.len()).map_err(|_| -1)?;
            b.extend(str.encode_utf16());
            b.shrink_to_fit();
            Ok(Self::Utf16(b))
        }
    }

    /// Creates StringBuf from UTF-8 encoded bytes
    pub fn from_utf8(data: Vec<u8>) -> Result<Self, i32> {
        if data.is_empty() {
            Ok(Self::Empty)
        } else if data.is_ascii() {
            Ok(Self::Ascii(data))
        } else {
            let mut u = Vec::<u16>::new();
            u.try_reserve_exact(data.len() / 2).map_err(|_| -1)?;
            let s = unsafe { std::str::from_utf8_unchecked(&data) };
            u.extend(s.encode_utf16());
            u.shrink_to_fit();
            Ok(Self::Utf16(u))
        }
    }

    pub fn with_capacity(cap: usize, is_wide: bool) -> Result<Self, TryReserveError> {
        if is_wide {
            let mut d = Vec::<u16>::new();
            d.try_reserve_exact(cap)?;
            Ok(Self::Utf16(d))
        } else {
            let mut d = Vec::<u8>::new();
            d.try_reserve_exact(cap)?;
            Ok(Self::Ascii(d))
        }
    }

    pub fn try_reserve(&mut self, cap: usize) -> Result<(), TryReserveError> {
        match self {
            Self::Empty => {
                let mut d = Vec::<u8>::new();
                d.try_reserve(cap)?;
                *self = Self::Ascii(d);
                Ok(())
            }
            Self::Ascii(s) => s.try_reserve(cap),
            Self::Utf16(s) => s.try_reserve(cap),
        }
    }

    pub fn try_reserve_exact(&mut self, cap: usize) -> Result<(), TryReserveError> {
        match self {
            Self::Empty => {
                let mut d = Vec::<u8>::new();
                d.try_reserve_exact(cap)?;
                *self = Self::Ascii(d);
                Ok(())
            }
            Self::Ascii(s) => s.try_reserve_exact(cap),
            Self::Utf16(s) => s.try_reserve_exact(cap),
        }
    }

    pub fn shrink_to_fit(&mut self) {
        match self {
            Self::Empty => {}
            Self::Ascii(s) => {
                if s.is_empty() {
                    *self = Self::Empty;
                } else {
                    s.shrink_to_fit();
                }
            }
            Self::Utf16(s) => {
                if s.is_empty() {
                    *self = Self::Empty;
                } else {
                    s.shrink_to_fit();
                }
            }
        }
    }

    pub fn push_str<S: AsRef<str> + ?Sized>(&mut self, str: &S) -> Result<(), TryReserveError> {
        let s = str.as_ref();
        if !s.is_empty() {
            match self {
                Self::Empty => {
                    let cc = if s.is_ascii() {
                        let mut d = Vec::<u8>::new();
                        d.try_reserve_exact(s.len())?;
                        d.try_extend_from_slice(s.as_bytes())?;
                        Self::Ascii(d)
                    } else {
                        let d = s.encode_utf16().try_collect::<Vec<_>>()?;
                        Self::Utf16(d)
                    };
                    let _ = std::mem::replace(self, cc);
                }
                Self::Ascii(d) => {
                    if s.is_ascii() {
                        d.try_extend_from_slice(s.as_bytes())?;
                    } else {
                        let mut w = Vec::<u16>::new();
                        w.try_reserve_exact(s.len() + d.len())?;
                        let a =
                            unsafe { std::str::from_utf8_unchecked(d.as_slice()) }.encode_utf16();
                        let b = s.encode_utf16();
                        for c in a.chain(b) {
                            w.push(c);
                        }
                        w.shrink_to_fit();
                        let _ = std::mem::replace(self, Self::Utf16(w));
                    }
                }
                Self::Utf16(w) => {
                    w.try_reserve(s.len())?;
                    for c in s.encode_utf16() {
                        w.push(c);
                    }
                    w.shrink_to_fit();
                }
            }
        }

        Ok(())
    }

    /// concat other string content to self content, returns a new result string content.
    pub fn push_buf_slice(&mut self, source: &StringBufSlice) -> Result<(), TryReserveError> {
        // use a temp var to hold current self content
        let orig_content = std::mem::replace(self, Self::Empty);

        let new_content: Self = match (orig_content, source.src) {
            (s, Self::Empty) => s,
            (Self::Empty, _) => source.to_owned()?,

            // same type
            (Self::Utf16(mut s1), Self::Utf16(s2)) => {
                if let Err(e) = s1.try_reserve_exact(s2.len()) {
                    *self = Self::Utf16(s1);
                    return Err(e);
                } else {
                    s1.extend_from_slice(&s2.as_slice()[source.buf_ranges.clone()]);
                    Self::Utf16(s1)
                }
            }
            (Self::Ascii(mut s1), Self::Ascii(s2)) => {
                if let Err(e) = s1.try_reserve_exact(s2.len()) {
                    *self = Self::Ascii(s1);
                    return Err(e);
                } else {
                    s1.extend_from_slice(&s2.as_slice()[source.buf_ranges.clone()]);
                    Self::Ascii(s1)
                }
            }

            (Self::Utf16(mut s1), Self::Ascii(s2)) => {
                if let Err(e) = s1.try_reserve(s2.len()) {
                    *self = Self::Utf16(s1);
                    return Err(e);
                }
                for c in unsafe {
                    std::str::from_utf8_unchecked(&s2.as_slice()[source.buf_ranges.clone()])
                }
                .encode_utf16()
                {
                    s1.push(c);
                }
                s1.shrink_to_fit();
                Self::Utf16(s1)
            }

            (Self::Ascii(s1), Self::Utf16(s2)) => {
                let mut buf = Vec::<u16>::new();
                if let Err(e) = buf.try_reserve_exact(s1.len() + s2.len()) {
                    *self = Self::Ascii(s1); // restore original
                    return Err(e);
                }

                let a = unsafe { std::str::from_utf8_unchecked(s1.as_slice()) }.encode_utf16();
                let b = s2.as_slice()[source.buf_ranges.clone()].iter().copied();
                buf.extend(a.chain(b));

                if !buf.iter().any(|c| *c >= 0x80) {
                    // ascii
                    Self::Ascii(unsafe { s16_into_s8(buf) })
                } else {
                    Self::Utf16(buf)
                }
            }
        };

        *self = new_content;
        Ok(())
    }

    /// concat other string buf to `self`, returns a new string buf.
    pub fn push(&mut self, other: &Self) -> Result<(), TryReserveError> {
        self.push_buf_slice(&other.slice(0..other.len()))
    }

    pub fn push_u8_slice(&mut self, s8: &[u8]) -> Result<(), TryReserveError> {
        match self {
            Self::Empty => {
                let d = s8.iter().copied().try_collect::<Vec<_>>()?;
                let _ = std::mem::replace(self, Self::Ascii(d));
            }
            Self::Ascii(d) => {
                d.try_reserve(d.len())?;
                d.extend_from_slice(s8);
            }
            Self::Utf16(w) => {
                w.try_reserve(s8.len())?;
                w.extend(s8.iter().map(|x| *x as u16));
            }
        }
        Ok(())
    }

    pub fn push_u16_slice(&mut self, source: &[u16]) -> Result<(), TryReserveError> {
        match self {
            Self::Empty => {
                if source.iter().any(|x| *x >= 0x80) {
                    // utf16
                    let d16 = source.iter().copied().try_collect::<Vec<_>>()?;
                    *self = Self::Utf16(d16);
                } else {
                    // ascii
                    let d8 = source.iter().map(|c| *c as u8).try_collect::<Vec<_>>()?;
                    *self = Self::Ascii(d8);
                }
            }
            Self::Ascii(d) => {
                if source.iter().any(|&u| u >= 0x80) {
                    // migrate to utf16
                    let mut d16 = Vec::<u16>::new();
                    d16.try_reserve_exact(d.len() + source.len())?;
                    d16.extend(unsafe { std::str::from_utf8_unchecked(d) }.encode_utf16());
                    d16.try_extend_from_slice(source)?;
                    *self = Self::Utf16(d16);
                } else {
                    d.try_reserve_exact(source.len())?;
                    d.extend(source.iter().map(|c| *c as u8));
                }
            }
            Self::Utf16(d) => {
                d.try_reserve_exact(d.len())?;
                d.extend_from_slice(source);
            }
        }

        Ok(())
    }

    pub fn push_u16_iter(
        &mut self,
        source: impl Iterator<Item = u16>,
    ) -> Result<(), TryReserveError> {
        match self {
            Self::Empty => {
                let d16 = source.try_collect::<Vec<_>>()?;
                if d16.iter().any(|x| *x >= 0x80) {
                    // utf16
                    *self = Self::Utf16(d16);
                } else {
                    // ascii
                    let mut d8 = unsafe { s16_into_s8(d16) };
                    d8.shrink_to_fit();
                    *self = Self::Ascii(d8);
                }
            }
            Self::Ascii(d8) => {
                let mut s16: Option<Vec<u16>> = None;
                for c in source {
                    if let Some(lst) = s16.as_mut() {
                        if lst.capacity() == lst.len() {
                            lst.try_reserve(32)?;
                        }
                        lst.push(c)
                    } else if c < 0x80 {
                        if d8.capacity() == d8.len() {
                            d8.try_reserve(32)?;
                        }
                        d8.push(c as u8);
                    } else {
                        // migrate to utf16
                        let mut w = Vec::<u16>::new();
                        w.try_reserve(d8.len() * 3 / 2)?;
                        w.extend(unsafe { std::str::from_utf8_unchecked(d8) }.encode_utf16());
                        w.push(c);
                        s16.replace(w);
                    }
                }

                if let Some(d16) = s16.take() {
                    *self = Self::Utf16(d16);
                }
            }
            Self::Utf16(d16) => {
                for c in source {
                    if d16.capacity() == d16.len() {
                        d16.try_reserve(32)?;
                    }
                    d16.push(c);
                }
            }
        }

        Ok(())
    }

    #[inline(always)]
    pub fn push_u8(&mut self, c: u8) -> Result<(), TryReserveError> {
        self.push_u8_slice(&[c])
    }

    #[inline]
    pub fn push_u16(&mut self, c: u16) -> Result<(), TryReserveError> {
        if c < 0x80 {
            self.push_u8_slice(&[c as u8])
        } else {
            self.push_u16_slice(&[c])
        }
    }

    pub fn push_u32(&mut self, c: u32) -> Result<(), TryReserveError> {
        if c >= 0x10000 {
            // push utf16 surrogate pair
            let h = c - 0x10000;
            self.push_u16_slice(&[((h >> 10) + 0xd800) as u16, ((h & 0x3ff) + 0xdc00) as u16])
        } else if c < 0x80 {
            self.push_u8(c as u8)
        } else {
            self.push_u16(c as u16)
        }
    }

    pub fn push_char<C: Into<StringChar>>(&mut self, c: C) -> Result<(), TryReserveError> {
        let c: StringChar = c.into();
        match c {
            StringChar::Char(c) => {
                if c.is_ascii() {
                    self.push_u8(c as u8)
                } else {
                    self.push_u16_slice(c.encode_utf16(&mut [0; 2]))
                }
            }
            StringChar::Code(c) => self.push_u16(c),
        }
    }

    pub fn push_chars_iter(
        &mut self,
        chars: impl Iterator<Item = StringChar>,
    ) -> Result<(), TryReserveError> {
        let sz = chars.size_hint();
        let len = std::cmp::max(sz.0, sz.1.unwrap_or(1));

        match self {
            Self::Empty => {
                let mut lst = Vec::<u16>::new(); // assume wide string at first.
                lst.try_reserve(len)?;

                for c in chars {
                    let mut a = [0u16; 2];
                    lst.try_extend_from_slice(match c {
                        StringChar::Char(c) => c.encode_utf16(&mut a),
                        StringChar::Code(c) => {
                            a[0] = c;
                            &a[..1]
                        }
                    })?;
                }

                if lst.iter().any(|n| *n >= 0x80) {
                    // is wide string
                    *self = Self::Utf16(lst);
                } else {
                    // is ascii string
                    let s8 = unsafe { s16_into_s8(lst) };
                    *self = Self::Ascii(s8);
                }
            }
            Self::Ascii(lst) => {
                let mut s16: Option<Vec<u16>> = None;

                for c in chars {
                    match c {
                        StringChar::Char(c) => {
                            if let Some(d16) = s16.as_mut() {
                                d16.try_extend_from_slice(c.encode_utf16(&mut [0u16; 2]))?;
                            } else if c.is_ascii() {
                                lst.try_push(c as u8)?;
                            } else {
                                let mut d16 = Vec::<u16>::with_capacity(lst.len() + 16);
                                d16.extend(lst.iter().map(|c| *c as u16));
                                d16.extend_from_slice(c.encode_utf16(&mut [0u16; 2]));
                                s16.replace(d16);
                            }
                        }
                        StringChar::Code(c) => {
                            debug_assert!(c >= 0x80);

                            if let Some(s16) = &mut s16 {
                                s16.try_push(c)?;
                            } else {
                                let mut d16 = Vec::<u16>::new();
                                d16.try_reserve(lst.len() + 16)?;
                                d16.extend(lst.iter().map(|x| *x as u16));
                                d16.push(c);
                                s16.replace(d16);
                            }
                        }
                    }
                }

                if let Some(d16) = s16.take() {
                    *self = Self::Utf16(d16);
                }
            }
            Self::Utf16(lst) => {
                lst.try_reserve(len)?;
                for c in chars {
                    match c {
                        StringChar::Char(c) => {
                            lst.try_extend_from_slice(c.encode_utf16(&mut [0; 2]))?;
                        }
                        StringChar::Code(c) => {
                            lst.try_push(c)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// concat other string content to self content, returns a new result string content.
    /// if `range` is out of bounds, the intersectioned range will be applied.
    pub fn push_content_range(
        &mut self,
        content: &StringData,
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
            (s, StringData::Empty | StringData::Ident) => s,
            (Self::Empty, _) => content.to_buf_range(range)?,

            // same type
            (Self::Utf16(mut s1), StringData::Utf16(s2)) => {
                if let Err(e) = s1.try_reserve_exact(s2.len()) {
                    *self = Self::Utf16(s1);
                    return Err(e);
                } else {
                    s1.extend_from_slice(&s2[range.clone()]);
                    Self::Utf16(s1)
                }
            }
            (Self::Ascii(mut s1), StringData::Ascii(s2)) => {
                if let Err(e) = s1.try_reserve_exact(s2.len()) {
                    *self = Self::Ascii(s1);
                    return Err(e);
                } else {
                    s1.extend_from_slice(&s2[range.clone()]);
                    Self::Ascii(s1)
                }
            }

            (Self::Utf16(mut s1), StringData::Ascii(s2)) => {
                if let Err(e) = s1.try_reserve(s2.len()) {
                    *self = Self::Utf16(s1);
                    return Err(e);
                } else {
                    s1.extend(s2[range.clone()].iter().map(|x| *x as u16));
                    Self::Utf16(s1)
                }
            }
            (Self::Ascii(s1), StringData::Utf16(s2)) => {
                let mut buf = Vec::<u16>::new();
                if let Err(e) = buf.try_reserve_exact(s1.len() + s2.len()) {
                    *self = Self::Ascii(s1); // restore original
                    return Err(e);
                } else {
                    let a = s1.iter().map(|x| *x as u16);
                    let b = s2[range.clone()].iter().copied();
                    buf.extend(a.chain(b));
                    Self::Utf16(buf)
                }
            }

            // static str
            (Self::Utf16(mut s1), StringData::AsciiStatic(s2)) => {
                if let Err(e) = s1.try_reserve(s2.len()) {
                    *self = Self::Utf16(s1);
                    return Err(e);
                } else {
                    s1.extend(s2.as_bytes()[range.clone()].iter().map(|x| *x as u16));
                    Self::Utf16(s1)
                }
            }
            (Self::Ascii(mut s1), StringData::AsciiStatic(s2)) => {
                if let Err(e) = s1.try_reserve_exact(s2.len()) {
                    *self = Self::Ascii(s1);
                    return Err(e);
                } else {
                    s1.extend_from_slice(&s2.as_bytes()[range.clone()]);
                    Self::Ascii(s1)
                }
            }
        };

        *self = new_data;
        Ok(())
    }

    #[inline(always)]
    pub fn push_content(&mut self, content: &StringData) -> Result<(), TryReserveError> {
        self.push_content_range(content, 0..content.len())
    }

    /// buffer capacity
    pub const fn capacity(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Ascii(s) => s.capacity(),
            Self::Utf16(s) => s.capacity(),
        }
    }

    /// buffer length, count of codepoints.
    pub const fn len(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Ascii(s) => s.len(),
            Self::Utf16(s) => s.len(),
        }
    }

    /// count chars (include broken surrogate codepoint).
    pub fn chars_count(&self) -> usize {
        match self {
            Self::Empty => 0,
            Self::Ascii(s) => s.len(),
            Self::Utf16(_) => self.iter().count(),
        }
    }

    /// get code at buffer index.
    /// returns None if index is out of bound.
    pub fn get(&self, index: usize) -> Option<u16> {
        match self {
            Self::Empty => None,
            Self::Ascii(lst) => lst.get(index).map(|n| *n as u16),
            Self::Utf16(lst) => lst.get(index).copied(),
        }
    }

    #[inline(always)]
    pub const fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    #[inline(always)]
    pub const fn is_wide(&self) -> bool {
        matches!(self, Self::Utf16(_))
    }

    pub fn trim(&mut self, left: bool, right: bool) {
        let len = self.len();
        if len > 0 {
            let mut start: Option<usize> = if left { None } else { Some(0) };
            let mut end: usize = if right { 0 } else { len };

            let mut pos = 0;
            for (i, c) in self.iter() {
                if let StringChar::Char(ch) = c
                    && !char_is_space(ch)
                {
                    if start.is_none() {
                        start.replace(pos);
                    }
                    if i > end {
                        end = i;
                    }
                }
                pos = i;
            }

            let start = start.unwrap_or(0);
            if start > 0 || end < len {
                match self {
                    Self::Ascii(s) => {
                        if (start..end).is_empty() {
                            s.clear();
                        } else {
                            if end < len {
                                s.truncate(end);
                            }
                            if start > 0 {
                                s.drain(0..start);
                            }
                        }
                    }
                    Self::Utf16(s) => {
                        if (start..end).is_empty() {
                            *self = StringBuf::Empty;
                        } else {
                            if end < len {
                                s.truncate(end);
                            }
                            if start > 0 {
                                s.drain(0..start);
                            }
                        }
                    }
                    Self::Empty => {}
                }
            }
        }
    }

    /// remove elements from buf in given range
    pub fn drain(&mut self, range: Range<usize>) {
        match self {
            Self::Ascii(b) => {
                b.drain(range);
            }
            Self::Utf16(b) => {
                b.drain(range);
            }
            _ => {}
        }
    }

    /// Note: `range` is about buffer index, not char position.
    pub(super) fn slice(&self, range: Range<usize>) -> StringBufSlice<'_> {
        let len = self.len();
        if !(0..len).contains(&range.start) {
            StringBufSlice {
                src: &EMPTY,
                buf_ranges: 0..0,
            }
        } else {
            let end = range.end.min(len);
            StringBufSlice {
                src: self,
                buf_ranges: range.start..end,
            }
        }
    }

    pub(super) const fn as_slice(&self) -> StringBufSlice<'_> {
        StringBufSlice {
            src: self,
            buf_ranges: 0..self.len(),
        }
    }

    pub fn to_utf8(&self) -> Cow<'_, str> {
        match self {
            StringBuf::Empty => Cow::Borrowed(""),
            StringBuf::Ascii(s) => {
                Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(s.as_slice()) })
            }
            StringBuf::Utf16(s) => Cow::Owned(String::from_utf16_lossy(s.as_slice())),
        }
    }
}

#[deprecated(note = "not used")]
#[derive(Clone)]
pub struct StringBufSlice<'a> {
    src: &'a StringBuf,
    buf_ranges: Range<usize>,
}

impl<'a> From<&'a StringBuf> for StringBufSlice<'a> {
    fn from(value: &'a StringBuf) -> Self {
        value.as_slice()
    }
}

impl std::fmt::Debug for StringBufSlice<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.src {
            StringBuf::Empty => write!(f, "<StringBufSlice Empty>"),
            StringBuf::Ascii(s) => {
                write!(
                    f,
                    "<StringBufSlice ascii {:?}>",
                    &s[self.buf_ranges.clone()]
                )
            }
            StringBuf::Utf16(s) => {
                write!(
                    f,
                    "<StringBufSlice utf16 {:?}>",
                    &s[self.buf_ranges.clone()]
                )
            }
        }
    }
}

impl StringBufSlice<'_> {
    #[inline(always)]
    pub const fn is_wide(&self) -> bool {
        self.src.is_wide()
    }

    #[inline(always)]
    pub fn buf_len(&self) -> usize {
        self.buf_ranges.len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.buf_len() == 0
    }

    /// encode slice content to utf8 str
    pub fn to_utf8(&self) -> Cow<'_, str> {
        if self.is_empty() {
            Cow::Borrowed("")
        } else {
            match self.src {
                StringBuf::Empty => Cow::Borrowed(""),
                StringBuf::Ascii(s) => Cow::Borrowed(unsafe {
                    std::str::from_utf8_unchecked(&s[self.buf_ranges.clone()])
                }),
                StringBuf::Utf16(s) => {
                    Cow::Owned(String::from_utf16_lossy(&s[self.buf_ranges.clone()]))
                }
            }
        }
    }

    pub fn to_owned(&self) -> Result<StringBuf, TryReserveError> {
        if self.is_empty() {
            Ok(StringBuf::Empty)
        } else {
            match self.src {
                StringBuf::Ascii(src) => Ok(StringBuf::Ascii(src.try_clone()?)),
                StringBuf::Utf16(src) => Ok(StringBuf::Utf16(src.try_clone()?)),
                _ => unreachable!(),
            }
        }
    }

    /// slice of slice
    pub fn slice(&self, range: Range<usize>) -> StringBufSlice<'_> {
        let len = self.buf_len();

        if !(0..len).contains(&range.start) {
            StringBufSlice {
                src: &EMPTY,
                buf_ranges: 0..0,
            }
        } else {
            let end = range.end.min(len);
            let base = self.buf_ranges.start;
            StringBufSlice {
                src: self.src,
                buf_ranges: base + range.start..base + end,
            }
        }
    }
}

/// inplace convert Vec<u16> to Vec<u8>
pub(super) unsafe fn s16_into_s8(mut d16: Vec<u16>) -> Vec<u8> {
    debug_assert!(!d16.iter().any(|c| *c >= 0x80), "must be ascii content");

    let mut ptr16 = d16.as_mut_ptr();
    let mut ptr8 = ptr16.cast::<u8>();
    let ptr = ptr8;
    let len = d16.len();
    let cap = d16.capacity();

    for _ in 0..len {
        *ptr8 = *ptr16 as u8;
        ptr8 = ptr8.add(1);
        ptr16 = ptr16.add(1);
    }

    // IMPORTANT: don't drop vec automatically
    std::mem::forget(d16);
    // re-interpret vec as Vec<u8>
    unsafe { Vec::from_raw_parts(ptr, len, cap * 2) }
}

pub(super) fn is_char_visible(ch: char) -> bool {
    let code_point = ch as u32;

    // 明确的不可见字符范围
    match code_point {
        // 控制字符
        0x0000..=0x001F | 0x007F..=0x009F => false,

        // 零宽和格式字符
        0x200B..=0x200F |  // 零宽字符
        0x2028..=0x202F |  // 分隔符和格式
        0x2060..=0x206F |  // 格式字符
        0xFEFF |           // 零宽空格 (BOM)
        0xFFF9..=0xFFFB |  // 注解字符
        0x1D159..=0x1D172 | // 音乐符号格式
        0xE0000..=0xE0FFF   // 标签字符
        => false,

        // 特殊情况：空格类字符的处理
        0x0020 | 0x00A0 | 0x1680 | 0x2000..=0x200A | 0x2028..=0x2029 | 0x202F..=0x205F | 0x3000 => {
            // 空格字符，根据需求决定是否算作可见
            true // 这里认为空格是"可见"的（虽然显示为空白）
        },

        // 其他字符
        _ => !ch.is_control()
    }
}
