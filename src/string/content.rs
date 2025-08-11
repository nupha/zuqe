use std::collections::VecDeque;
use std::{borrow::Cow, cmp::Ordering, collections::TryReserveError, ops::Range, str::FromStr};
use {
    fallible_collections::{FallibleVec, TryClone, TryCollect},
    widestring::U16Str,
};

use crate::{atom::AtomTypeHint, string::StringChar};

use super::string::JS_STRING_LEN_MAX;

/// string content buffer
#[derive(Debug)]
pub enum StringBuf {
    /// string with no content
    Empty,

    /// null string - behaves like empty, but is for semantically differ only.
    /// only used when creating an anonymous Symbol,
    Null,

    /// utf8 encoded bytes
    Ascii(Vec<u8>),
    /// utf16 wide-chars
    Utf16(Vec<u16>),
}

impl FromStr for StringBuf {
    type Err = i32;

    fn from_str(str: &str) -> Result<Self, Self::Err> {
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
            for el in str.encode_utf16() {
                b.push(el);
            }
            b.shrink_to_fit();
            Ok(Self::Utf16(b))
        }
    }
}

impl StringBuf {
    /// Creates StringBuf from UTF-8 encoded bytes
    ///
    /// Handles both ASCII and non-ASCII UTF-8 content:
    /// - ASCII content is stored as-is in UTF-8 format
    /// - Non-ASCII content is converted to UTF-16 for better Unicode support
    pub fn from_utf8(data: Vec<u8>) -> Result<Self, i32> {
        debug_assert!(!data.is_empty());

        if data.is_empty() {
            Ok(Self::Empty)
        } else if data.is_ascii() {
            Ok(Self::Ascii(data))
        } else {
            let mut u = Vec::<u16>::new();
            u.try_reserve_exact(data.len() / 2).map_err(|_| -1)?;

            let s = unsafe { String::from_utf8_unchecked(data) };
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

    pub fn reserve(&mut self, cap: usize) -> Result<(), TryReserveError> {
        match self {
            Self::Empty | Self::Null => {
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
            Self::Empty | Self::Null => {
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
            Self::Empty | Self::Null => {}
            Self::Ascii(s) => s.shrink_to_fit(),
            Self::Utf16(s) => s.shrink_to_fit(),
        }
    }

    /// create a new string content by coping content.
    pub fn copy_to_new(&self) -> Result<Self, TryReserveError> {
        match self {
            Self::Empty => Ok(Self::Empty),
            Self::Null => Ok(Self::Null),
            Self::Ascii(s) => {
                let s2 = s.try_clone()?;
                Ok(Self::Ascii(s2))
            }
            Self::Utf16(s) => {
                let s2 = s.try_clone()?;
                Ok(Self::Utf16(s2))
            }
        }
    }

    pub fn push_str<S: AsRef<str> + ?Sized>(&mut self, str: &S) -> Result<(), TryReserveError> {
        let s = str.as_ref();
        if !s.is_empty() {
            match self {
                Self::Empty | Self::Null => {
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
    pub fn push_content_slice(&mut self, source: &StringBufSlice) -> Result<(), TryReserveError> {
        // use a temp var to hold current self content
        let orig_content = std::mem::replace(self, Self::Empty);

        let new_content: Self = match (orig_content, source.src) {
            (s, Self::Empty | Self::Null) => s,
            (Self::Empty | Self::Null, _) => source.to_owned()?,

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
                buf.shrink_to_fit();
                Self::Utf16(buf)
            }
        };

        *self = new_content;
        Ok(())
    }

    /// concat other string buf to `self`, returns a new string buf.
    pub fn push(&mut self, other: &Self) -> Result<(), TryReserveError> {
        self.push_content_slice(&other.slice(0..other.buf_len()))
    }

    pub fn push_u8_slice(&mut self, s8: &[u8]) -> Result<(), TryReserveError> {
        match self {
            Self::Empty | Self::Null => {
                let d = s8.iter().copied().try_collect::<Vec<_>>()?;
                let _ = std::mem::replace(self, Self::Ascii(d));
            }
            Self::Ascii(d) => {
                d.try_reserve(d.len())?;
                d.extend_from_slice(s8);
            }
            Self::Utf16(w) => {
                w.try_reserve(s8.len())?;
                w.extend(unsafe { std::str::from_utf8_unchecked(s8) }.encode_utf16());
            }
        }
        Ok(())
    }

    /// inplace convert Vec<u16> to Vec<u8>
    unsafe fn wide_to_ascii(mut lst: Vec<u16>) -> Vec<u8> {
        debug_assert!(!lst.iter().any(|c| *c >= 0x80), "must be ascii content");

        let mut ptr16 = lst.as_mut_ptr();
        let mut ptr8 = ptr16.cast::<u8>();
        let ptr = ptr8;
        let len = lst.len();
        let cap = lst.capacity();

        for _ in 0..len {
            *ptr8 = *ptr16 as u8;
            ptr8 = ptr8.add(1);
            ptr16 = ptr16.add(1);
        }

        // IMPORTANT: don't let lst drop
        std::mem::forget(lst);
        // re-interpret lst as Vec<u8>
        let mut a = Vec::from_raw_parts(ptr, len, cap * 2);
        a.shrink_to_fit();
        a
    }

    pub fn push_u16_slice(&mut self, source: &[u16]) -> Result<(), TryReserveError> {
        match self {
            Self::Empty | Self::Null => {
                let d = source.iter().copied().try_collect::<Vec<_>>()?;
                let _ = std::mem::replace(self, Self::Utf16(d));
            }
            Self::Ascii(d) => {
                if source.iter().any(|&u| u >= 0x80) {
                    // is wide string, migrate to utf16
                    let mut w = Vec::<u16>::new();
                    w.try_reserve(d.len() + source.len())?;
                    w.extend(unsafe { std::str::from_utf8_unchecked(d) }.encode_utf16());
                    w.try_extend_from_slice(source)?;
                    *self = Self::Utf16(w);
                } else {
                    // is ascii string
                    d.try_reserve_exact(source.len())?;
                    d.extend(source.iter().map(|c| *c as u8));
                }
            }
            Self::Utf16(d) => {
                d.try_reserve(d.len())?;
                d.extend_from_slice(source);
            }
        }
        Ok(())
    }

    #[inline(always)]
    pub fn push_u8(&mut self, c: u8) -> Result<(), TryReserveError> {
        self.push_u8_slice(&[c])
    }

    #[inline(always)]
    pub fn push_u16(&mut self, c: u16) -> Result<(), TryReserveError> {
        self.push_u16_slice(&[c])
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
            StringChar::BrokenCode(c) => self.push_u16(c as u16),
        }
    }

    pub fn push_chars_iter(
        &mut self,
        chars: impl Iterator<Item = StringChar>,
    ) -> Result<(), TryReserveError> {
        let sz = chars.size_hint();
        let len = std::cmp::max(sz.0, sz.1.unwrap_or(1));

        match self {
            Self::Empty | Self::Null => {
                let mut lst = Vec::<u16>::new(); // assume wide string at first.
                lst.try_reserve(len)?;

                for c in chars {
                    let mut a = [0u16; 2];
                    lst.try_extend_from_slice(match c {
                        StringChar::Char(c) => c.encode_utf16(&mut a),
                        StringChar::BrokenCode(c) => {
                            a[0] = c as _;
                            &a[..1]
                        }
                    })?;
                }

                if lst.iter().any(|n| *n >= 0x80) {
                    // is utf16 content
                    let _ = std::mem::replace(self, Self::Utf16(lst));
                } else {
                    // is ascii content
                    let s8 = unsafe { Self::wide_to_ascii(lst) };
                    let _ = std::mem::replace(self, Self::Ascii(s8));
                }
            }
            Self::Ascii(lst) => {
                let mut s8: Option<Vec<u8>> = Some(lst.try_clone()?);
                let mut s16: Option<Vec<u16>> = None;

                for c in chars {
                    match c {
                        StringChar::Char(c) => {
                            let mut a = [0u16; 2];
                            let bb = c.encode_utf16(&mut a);

                            if let Some(s8) = &mut s8
                                && bb[0] > 0x80
                            {
                                s8.try_push(bb[0] as u8)?;
                            } else {
                                if let Some(s16) = &mut s16 {
                                    s16.try_extend_from_slice(bb)?;
                                } else {
                                    let s8 = s8.take().unwrap();
                                    let mut lst = Vec::<u16>::new();
                                    lst.try_reserve(s8.len() + 2)?;
                                    lst.extend(
                                        s8.iter()
                                            .copied()
                                            .map(|x| x as u16)
                                            .chain(bb.iter().copied()),
                                    );
                                    s16.replace(lst);
                                }
                            }
                        }
                        StringChar::BrokenCode(c) => {
                            if let Some(s16) = &mut s16 {
                                s16.try_push(c as u16)?;
                            } else {
                                let s8 = s8.take().unwrap();
                                let mut lst = Vec::<u16>::new();
                                lst.try_reserve(s8.len() + 2)?;
                                lst.extend(
                                    s8.iter()
                                        .copied()
                                        .map(|x| x as u16)
                                        .chain([c as u16].iter().copied()),
                                );
                                s16.replace(lst);
                            }
                        }
                    }
                }

                if let Some(s8) = s8 {
                    let _ = std::mem::replace(self, Self::Ascii(s8));
                } else if let Some(s16) = s16 {
                    let _ = std::mem::replace(self, Self::Utf16(s16));
                }
            }
            Self::Utf16(lst) => {
                lst.try_reserve(len)?;
                for c in chars {
                    match c {
                        StringChar::Char(c) => {
                            lst.try_extend_from_slice(c.encode_utf16(&mut [0; 2]))?;
                        }
                        StringChar::BrokenCode(c) => {
                            lst.try_push(c as u16)?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    pub const fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Empty | Self::Null => &[],
            Self::Ascii(s) => s.as_slice(),
            Self::Utf16(s) => unsafe {
                std::slice::from_raw_parts(s.as_ptr().cast::<u8>(), s.len() * 2)
            },
        }
    }

    /// encode content to utf8 str
    pub fn to_utf8(&self) -> Cow<'_, str> {
        match self {
            Self::Empty | Self::Null => Cow::Borrowed(""),
            Self::Ascii(s) => Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(s) }),
            Self::Utf16(s) => Cow::Owned(U16Str::from_slice(s).to_string_lossy()),
        }
    }

    /// buffer length.
    pub fn buf_len(&self) -> usize {
        match self {
            Self::Empty | Self::Null => 0,
            Self::Ascii(s) => s.len(),
            Self::Utf16(s) => s.len(),
        }
    }

    /// count string chars (include broken surrogate code).
    pub fn char_count(&self) -> usize {
        match self {
            Self::Empty | Self::Null => 0,
            Self::Ascii(s) => s.len(),
            Self::Utf16(slice) => {
                self.iter().count()

                // alternative code:
                // let mut count = 0_usize;
                // let mut i = 0;
                // while i < slice.len() {
                //     let u = slice[i];
                //     if (0xD800..=0xDBFF).contains(&u) && i + 1 < slice.len() {
                //         let next = slice[i + 1];
                //         if (0xDC00..=0xDFFF).contains(&next) {
                //             count += 1;
                //             i += 2;
                //             continue;
                //         }
                //     }
                //     count += 1;
                //     i += 1;
                // }
                // count
            }
        }
    }

    /// get code at buffer index.
    /// returns None if index is out of bound.
    pub fn get(&self, buf_index: usize) -> Option<u16> {
        match self {
            Self::Empty | Self::Null => None,
            Self::Ascii(lst) => lst.get(buf_index).map(|n| *n as u16),
            Self::Utf16(lst) => lst.get(buf_index).map(|n| *n),
        }
    }

    /// buffer capacity
    pub const fn capacity(&self) -> usize {
        match self {
            Self::Empty | Self::Null => 0,
            Self::Ascii(s) => s.capacity(),
            Self::Utf16(s) => s.capacity(),
        }
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    #[inline(always)]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    #[inline(always)]
    pub fn is_empty_or_null(&self) -> bool {
        matches!(self, Self::Empty | Self::Null)
    }

    #[inline(always)]
    pub const fn is_wide(&self) -> bool {
        matches!(self, Self::Utf16(_))
    }

    pub fn trim(&mut self, left: bool, right: bool) {
        match self {
            Self::Ascii(s) => {
                if right {
                    if let Some(n) = unsafe { str::from_utf8_unchecked(s) }
                        .chars()
                        .rev()
                        .position(|c| !c.is_whitespace())
                    {
                        s.truncate(s.len() - n);
                    }
                }
                if left {
                    if let Some(n) = unsafe { str::from_utf8_unchecked(s) }
                        .chars()
                        .position(|c| !c.is_whitespace())
                    {
                        s.drain(0..n);
                    }
                }
                s.shrink_to_fit();
            }
            Self::Utf16(ws) => {
                if right {
                    if let Some(n) = U16Str::from_slice(ws)
                        .chars_lossy()
                        .rev()
                        .position(|c| !c.is_whitespace())
                    {
                        let len = ws.len();
                        ws.truncate(len - n);
                    }
                }
                if left {
                    if let Some(n) = U16Str::from_slice(ws)
                        .chars_lossy()
                        .position(|c| !c.is_whitespace())
                    {
                        ws.drain(0..n);
                    }
                }
            }
            Self::Empty | Self::Null => {}
        }
    }

    /// compare string content with str
    pub fn eq_str(&self, other: &str) -> bool {
        match self {
            Self::Empty => other.is_empty(),
            Self::Null => false,
            Self::Ascii(s) => s.len() == other.len() && s.as_slice() == other.as_bytes(),
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
        }
    }

    /// calc hash for string content
    #[inline]
    pub fn calc_hash(&self, atom_type: AtomTypeHint) -> u32 {
        let mut h = atom_type as u32;

        match &self {
            Self::Empty | Self::Null => h,
            Self::Ascii(s) => {
                for c in s {
                    h = hash_char((*c) as u32, h);
                }
                h
            }
            Self::Utf16(s) => {
                for c in s.as_slice() {
                    h = hash_char((*c) as u32, h);
                }
                h
            }
        }
    }

    /// calc hash value for str
    #[inline(always)]
    pub(crate) fn calc_hash_for_str(str: &str, atom_type: AtomTypeHint) -> u32 {
        let mut h = atom_type as u32;
        for c in str.encode_utf16() {
            h = hash_char(c as u32, h);
        }
        h
    }

    /// if string is correct representation of u32, convert to u32. otherwise, returns Err
    pub fn parse_u32_radix(&self, radix: u8) -> Result<u32, i32> {
        let len = self.buf_len();
        if len == 0 || len > 10 {
            return Err(-1);
        }

        let mut ret: u32 = 0;

        for (i, c) in self.iter() {
            let ch = c.char().ok_or(-1)?; // broken code

            if ch.is_ascii_digit() {
                if i == 0 && ch == '0' {
                    return if len != 1 { Err(-1) } else { Ok(0) };
                }

                let n = ch as u64 - '0' as u64;
                if n < radix as u64 {
                    let n64: u64 = (ret as u64) * 10 + n;
                    if n64 >> 32 != 0 {
                        return Err(-1); // u32 overflow
                    }
                    ret = n64 as u32;
                } else {
                    return Err(-1);
                }
            } else if i > 0 && ch == '_' {
                //skip '_'
            } else {
                return Err(-1);
            }
        }

        Ok(ret)
    }

    /// if string is correct representation of u32, convert to u32. otherwise, returns Err
    #[inline(always)]
    pub fn parse_u32(&self) -> Result<u32, i32> {
        self.parse_u32_radix(10)
    }

    /// find index of code occurrence.
    pub fn index_of_code(&self, code: u16, from_index: usize) -> Option<usize> {
        if !(0..self.buf_len()).contains(&from_index) {
            return None;
        }

        match self {
            Self::Empty | Self::Null => None,
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
        let len = self.buf_len();
        let len1 = needle.buf_len();

        if from_index + len1 > len {
            // range out of bound
            None
        } else if needle.is_empty() {
            Some(from_index)
        } else {
            let first = needle.get(0).unwrap().clone();
            let it2 = needle.iter();
            let mut cursor = from_index;

            while let Some(i) = self.index_of_code(first, cursor) {
                debug_assert!(cursor + len1 <= len, "{i} + {len1} <= {len}");

                if i + len1 <= len {
                    if self
                        .iter_range(i..(i + len1))
                        .zip(it2.clone())
                        .all(|((_, c1), (_, c2))| c1 == c2)
                    {
                        return Some(i); // found at cursor
                    }
                }

                // forward for next occurrence
                cursor = i + 1;
                if cursor + len1 > len {
                    break;
                }
            }

            None
        }
    }

    /// find index of char occurrence in self, starting at from.
    pub fn index_of_char<C: Into<StringChar>>(&self, ch: C, from_index: usize) -> Option<usize> {
        let len = self.buf_len();
        if !(0..len).contains(&from_index) {
            None
        } else {
            let ch: StringChar = ch.into();
            self.iter_since(from_index)
                .find(|(_, c)| ch.eq(c))
                .map(|(i, _)| i + from_index)
        }
    }

    /// Note: `range` is about buffer index, not char position.
    pub fn slice(&self, range: Range<usize>) -> StringBufSlice {
        let len = self.buf_len();

        if !(0..len).contains(&range.start) {
            StringBufSlice {
                src: self,
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

    pub fn dump(&self) {
        let sep = '\"';
        print!("{sep}");

        for c in self.chars() {
            match c {
                '\'' | '"' if c == sep => print!("\\{c}"),
                '\n' => print!("\\n"),
                '\r' => print!("\\r"),
                '\t' => print!("\\t"),
                '\\' => print!("\\\\"),
                ' '..='~' => print!("{c}"),
                _ => print!("\\u{:04x}", c as u32),
            }
        }

        print!("{sep}");
    }
}

impl PartialEq for StringBuf {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Empty | Self::Null, _) | (_, Self::Empty | Self::Null) => {
                // empty eq empty, null eq null, empty eq null
                self.buf_len() == other.buf_len()
            }

            (Self::Ascii(s1), Self::Ascii(s2)) => s1 == s2,
            (Self::Utf16(s1), Self::Utf16(s2)) => s1 == s2,
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
        }
    }
}

impl Eq for StringBuf {}

impl Ord for StringBuf {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            // same kind cmp.
            (Self::Empty, Self::Empty) => Ordering::Equal,
            (Self::Null, Self::Null) => Ordering::Equal,
            (Self::Ascii(s1), Self::Ascii(s2)) => s1.cmp(s2),
            (Self::Utf16(s1), Self::Utf16(s2)) => s1.cmp(s2),

            // null is the smallest.
            (Self::Null, _) => Ordering::Less,
            (_, Self::Null) => Ordering::Greater,
            // empty is greater than null only.
            (Self::Empty, _) => Ordering::Less,
            (_, Self::Empty) => Ordering::Greater,

            // utf8 vs utf16 cmp
            (Self::Ascii(s1), Self::Utf16(s2)) => {
                let mut it1 = unsafe { std::str::from_utf8_unchecked(s1) }.chars();
                let mut it2 = U16Str::from_slice(s2).chars_lossy();
                loop {
                    match (it1.next(), it2.next()) {
                        (Some(a), Some(b)) => {
                            if a != b {
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
            (Self::Utf16(s1), Self::Ascii(s2)) => {
                let mut it1 = U16Str::from_slice(s1).chars_lossy();
                let mut it2 = unsafe { std::str::from_utf8_unchecked(s2) }.chars();
                loop {
                    match (it1.next(), it2.next()) {
                        (Some(a), Some(b)) => {
                            if a != b {
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
        }
    }
}

impl PartialOrd for StringBuf {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone)]
pub struct StringBufSlice<'a> {
    src: &'a StringBuf,
    buf_ranges: Range<usize>,
}

impl std::fmt::Debug for StringBufSlice<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.src {
            StringBuf::Empty => write!(f, "<StringBufSlice Empty>"),
            StringBuf::Null => write!(f, "<StringBufSlice Null>"),
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
    pub fn buf_len(&self) -> usize {
        self.buf_ranges.len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.buf_len() == 0
    }

    pub fn to_owned(&self) -> Result<StringBuf, TryReserveError> {
        let len = self.buf_len();
        if len == 0 {
            Ok(StringBuf::Empty)
        } else {
            let mut res = StringBuf::with_capacity(len, self.src.is_wide())?;
            match self.src {
                StringBuf::Ascii(src) => {
                    let _ = res.push_u8_slice(&src[self.buf_ranges.clone()]);
                }
                StringBuf::Utf16(src) => {
                    let _ = res.push_u16_slice(&src[self.buf_ranges.clone()]);
                }
                _ => unreachable!(),
            }
            Ok(res)
        }
    }
}

#[inline(always)]
const fn hash_char(c: u32, h: u32) -> u32 {
    h.wrapping_mul(263).wrapping_add(c)
}
