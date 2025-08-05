use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::TryReserveError,
    ops::Range,
    str::{self, CharIndices, Chars, FromStr},
};

use fallible_collections::{FallibleVec, TryClone, TryCollect};
use widestring::{
    U16Str,
    ustr::{CharIndicesLossyUtf16, CharsLossyUtf16},
};

use crate::atom::AtomTypeHint;

use super::string::JS_STRING_LEN_MAX;

/// string content buffer
#[derive(Debug)]
pub enum StringBuf {
    /// string with no content
    Empty,

    /// null string - behaves like empty, but is for sementically differ only.
    /// only used when creating an anonymous Symbol,
    Null,

    /// utf8 encoded bytes
    Utf8(Vec<u8>),
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
            Ok(Self::Utf8(s))
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
            Ok(Self::Utf8(data))
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
            Ok(Self::Utf16(Vec::<u16>::try_with_capacity(cap)?))
        } else {
            Ok(Self::Utf8(Vec::<u8>::try_with_capacity(cap)?))
        }
    }

    pub fn reserve(&mut self, cap: usize) -> Result<(), TryReserveError> {
        match self {
            Self::Empty | Self::Null => {
                let _ = std::mem::replace(self, Self::Utf8(Vec::<u8>::try_with_capacity(cap)?));
                Ok(())
            }
            Self::Utf8(s) => s.try_reserve(cap),
            Self::Utf16(s) => s.try_reserve(cap),
        }
    }

    pub fn try_reserve_exact(&mut self, cap: usize) -> Result<(), TryReserveError> {
        match self {
            Self::Empty | Self::Null => {
                let _ = std::mem::replace(self, Self::Utf8(Vec::<u8>::try_with_capacity(cap)?));
                Ok(())
            }
            Self::Utf8(s) => s.try_reserve_exact(cap),
            Self::Utf16(s) => s.try_reserve_exact(cap),
        }
    }

    pub fn shrink_to_fit(&mut self) {
        match self {
            Self::Empty | Self::Null => {}
            Self::Utf8(s) => s.shrink_to_fit(),
            Self::Utf16(s) => s.shrink_to_fit(),
        }
    }

    /// create a new string content by coping content.
    pub fn copy_to_new(&self) -> Result<Self, TryReserveError> {
        match self {
            Self::Empty => Ok(Self::Empty),
            Self::Null => Ok(Self::Null),
            Self::Utf8(s) => {
                let s2 = s.try_clone()?;
                Ok(Self::Utf8(s2))
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
                        let mut d = Vec::<u8>::try_with_capacity(s.len())?;
                        d.try_extend_from_slice(s.as_bytes())?;
                        Self::Utf8(d)
                    } else {
                        let d = s.encode_utf16().try_collect::<Vec<_>>()?;
                        Self::Utf16(d)
                    };
                    let _ = std::mem::replace(self, cc);
                }
                Self::Utf8(d) => {
                    if s.is_ascii() {
                        d.try_extend_from_slice(s.as_bytes())?;
                    } else {
                        let mut w = Vec::<u16>::try_with_capacity(s.len() + d.len())?;
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
    pub fn push_content_slice(&mut self, sref: &StringBufSlice) -> Result<(), TryReserveError> {
        // 使用临时变量存储self的内容,避免借用冲突
        let orig_content = std::mem::replace(self, Self::Empty);

        let new_content: Self = match (orig_content, sref.src) {
            (s, Self::Empty | Self::Null) => s,
            (Self::Empty | Self::Null, _) => sref.to_owned()?,
            (Self::Utf16(mut s1), Self::Utf16(s2)) => {
                if let Err(e) = s1.try_reserve_exact(s2.len()) {
                    let _ = std::mem::replace(self, Self::Utf16(s1));
                    return Err(e);
                } else {
                    s1.extend_from_slice(&s2.as_slice()[sref.ranges.clone()]);
                    Self::Utf16(s1)
                }
            }
            (Self::Utf8(mut s1), Self::Utf8(s2)) => {
                if let Err(e) = s1.try_reserve_exact(s2.len()) {
                    let _ = std::mem::replace(self, Self::Utf8(s1));
                    return Err(e);
                } else {
                    s1.extend_from_slice(&s2.as_slice()[sref.ranges.clone()]);
                    Self::Utf8(s1)
                }
            }
            (Self::Utf16(mut s1), Self::Utf8(s2)) => {
                if let Err(e) = s1.try_reserve(s2.len()) {
                    let _ = std::mem::replace(self, Self::Utf16(s1));
                    return Err(e);
                }

                for c in
                    unsafe { std::str::from_utf8_unchecked(&s2.as_slice()[sref.ranges.clone()]) }
                        .encode_utf16()
                {
                    s1.push(c);
                }
                s1.shrink_to_fit();
                Self::Utf16(s1)
            }
            (Self::Utf8(s1), Self::Utf16(s2)) => {
                match Vec::<u16>::try_with_capacity(s1.len() + s2.len()) {
                    Ok(mut buf) => {
                        let a =
                            unsafe { std::str::from_utf8_unchecked(s1.as_slice()) }.encode_utf16();
                        let b = s2.as_slice()[sref.ranges.clone()].iter().copied();
                        buf.extend(a.chain(b));
                        buf.shrink_to_fit();
                        Self::Utf16(buf)
                    }
                    Err(e) => {
                        let _ = std::mem::replace(self, Self::Utf8(s1));
                        return Err(e);
                    }
                }
            }
        };

        let _ = std::mem::replace(self, new_content);
        Ok(())
    }

    /// concat other string content to self content, returns a new result string content.
    pub fn push(&mut self, other: &Self) -> Result<(), TryReserveError> {
        self.push_content_slice(&other.slice(0..other.len()))
    }

    pub fn push_u8_slice(&mut self, s8: &[u8]) -> Result<(), TryReserveError> {
        match self {
            Self::Empty | Self::Null => {
                let d = s8.iter().copied().try_collect::<Vec<_>>()?;
                let _ = std::mem::replace(self, Self::Utf8(d));
            }
            Self::Utf8(d) => {
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

    pub fn push_u16_slice(&mut self, s16: &[u16]) -> Result<(), TryReserveError> {
        match self {
            Self::Empty | Self::Null => {
                let d = s16.iter().copied().try_collect::<Vec<_>>()?;
                let _ = std::mem::replace(self, Self::Utf16(d));
            }
            Self::Utf8(d) => {
                if s16.iter().any(|&u| u >= 0x80) {
                    // s is wide string, migrate to utf16
                    let mut w = Vec::<u16>::try_with_capacity(d.len() + s16.len())?;
                    w.extend(unsafe { std::str::from_utf8_unchecked(d) }.encode_utf16());
                    w.extend_from_slice(s16);
                    let _ = std::mem::replace(self, Self::Utf16(w));
                } else {
                    // s is ascii string
                    d.try_reserve(s16.len())?;
                    d.extend(s16.iter().map(|c| *c as u8));
                }
            }
            Self::Utf16(d) => {
                d.try_reserve(d.len())?;
                d.extend_from_slice(s16);
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

    pub fn push_char(&mut self, c: char) -> Result<(), TryReserveError> {
        if c.is_ascii() {
            self.push_u8(c as u8)
        } else {
            self.push_u16_slice(c.encode_utf16(&mut [0; 2]))
        }
    }

    pub fn push_chars_iter(
        &mut self,
        chars: impl Iterator<Item = char>,
    ) -> Result<(), TryReserveError> {
        let sz = chars.size_hint();
        let len = std::cmp::max(sz.0, sz.1.unwrap_or(1));

        match self {
            Self::Empty | Self::Null => {
                let mut d = Vec::<u16>::try_with_capacity(len)?;
                for c in chars {
                    d.try_extend_from_slice(c.encode_utf16(&mut [0; 4]))?;
                }
                if !d.iter().any(|n| *n >= 0x80) {
                    // is ascii
                    let mut ptr16 = d.as_mut_ptr();
                    let mut ptr8 = ptr16.cast::<u8>();
                    let ptr = ptr8;
                    let len = d.len();
                    let cap = d.capacity();

                    unsafe {
                        for _ in 0..len {
                            *ptr8 = *ptr16 as u8;
                            ptr8 = ptr8.add(1);
                            ptr16 = ptr16.add(1);
                        }

                        let mut a = Vec::from_raw_parts(ptr, len, cap * 2);
                        a.shrink_to_fit();
                        let _ = std::mem::replace(self, Self::Utf8(a));
                    }
                } else {
                    let _ = std::mem::replace(self, Self::Utf16(d));
                }
            }
            Self::Utf8(s) => {
                s.try_reserve(len * 3)?;
                for c in chars {
                    s.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                    if s.capacity() - s.len() < 4 {
                        s.try_reserve(len / 2)?;
                    }
                }

                if s.iter().any(|b| *b >= 0x80) {
                    // non-ascii, convert to U16
                    let mut w = Vec::<u16>::try_with_capacity(s.len() / 2)?;
                    for c in unsafe { std::str::from_utf8_unchecked(s) }.encode_utf16() {
                        w.push(c);
                    }
                    let _ = std::mem::replace(self, Self::Utf16(w));
                }
            }
            Self::Utf16(w) => {
                w.try_reserve(len)?;
                for c in chars {
                    w.extend_from_slice(c.encode_utf16(&mut [0; 2]));
                    if w.capacity() - w.len() <= 2 {
                        w.try_reserve(len / 2)?;
                    }
                }
            }
        }

        Ok(())
    }

    pub const fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Empty | Self::Null => &[],
            Self::Utf8(s) => s.as_slice(),
            Self::Utf16(s) => unsafe {
                std::slice::from_raw_parts(s.as_ptr().cast::<u8>(), s.len() * 2)
            },
        }
    }

    /// encode content to utf8 str
    pub fn to_str(&self) -> Cow<'_, str> {
        match self {
            Self::Empty | Self::Null => Cow::Borrowed(""),
            Self::Utf8(s) => Cow::Borrowed(unsafe { std::str::from_utf8_unchecked(s) }),
            Self::Utf16(s) => Cow::Owned(U16Str::from_slice(s).to_string_lossy()),
        }
    }

    /// string length
    pub fn len(&self) -> usize {
        match self {
            Self::Empty | Self::Null => 0,
            Self::Utf8(s) => s.len(),
            Self::Utf16(slice) => {
                // count unichars
                let mut count = 0_usize;
                let mut i = 0;
                while i < slice.len() {
                    let u = slice[i];
                    if (0xD800..=0xDBFF).contains(&u) && i + 1 < slice.len() {
                        let next = slice[i + 1];
                        if (0xDC00..=0xDFFF).contains(&next) {
                            count += 1;
                            i += 2;
                            continue;
                        }
                    }
                    count += 1;
                    i += 1;
                }
                count
            }
        }
    }

    /// capacity
    pub const fn capacity(&self) -> usize {
        match self {
            Self::Empty | Self::Null => 0,
            Self::Utf8(s) => s.capacity(),
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
            Self::Utf8(s) => {
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

    /// get chars iterator
    pub fn chars(&self) -> StringChars<'_> {
        match self {
            Self::Empty | Self::Null => StringChars::Empty,
            Self::Utf8(s) => StringChars::Utf8(unsafe { str::from_utf8_unchecked(s) }.chars()),
            Self::Utf16(s) => StringChars::Utf16(U16Str::from_slice(s).chars_lossy()),
        }
    }

    /// get chars iterator
    pub fn chars_indices(&self) -> StringCharIndices<'_> {
        match self {
            Self::Empty | Self::Null => StringCharIndices::Empty,
            Self::Utf8(s) => {
                StringCharIndices::Utf8(unsafe { str::from_utf8_unchecked(s) }.char_indices())
            }
            Self::Utf16(s) => StringCharIndices::Utf16(U16Str::from_slice(s).char_indices_lossy()),
        }
    }

    /// compare string content with str
    pub fn eq_str(&self, other: &str) -> bool {
        match self {
            Self::Empty => other.is_empty(),
            Self::Null => false,
            Self::Utf8(s) => s.len() == other.len() && s.as_slice() == other.as_bytes(),
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
            Self::Utf8(s) => {
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
        let len = self.len();
        if len == 0 || len > 10 {
            return Err(-1);
        }

        let mut ret: u32 = 0;

        for (i, c) in self.chars().enumerate() {
            if c.is_ascii_digit() {
                if i == 0 && c == '0' {
                    return if len != 1 { Err(-1) } else { Ok(0) };
                }

                let n = c as u64 - '0' as u64;
                if n < radix as u64 {
                    let n64: u64 = (ret as u64) * 10 + n;
                    if n64 >> 32 != 0 {
                        return Err(-1); // u32 overflow
                    }
                    ret = n64 as u32;
                } else {
                    return Err(-1);
                }
            } else if i > 0 && c == '_' {
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

    /// find index of `needle` occurance in haystack `self`, starting at index `from`.
    pub fn find(&self, needle: &Self, from: usize) -> Option<usize> {
        let len1 = self.len();
        if !(0..len1).contains(&from) {
            return None;
        }

        let needle_chars: Vec<char> = needle.chars().collect();
        let needle_len = needle_chars.len();
        if needle_len == 0 {
            return Some(from);
        }

        let mut window = Vec::<char>::with_capacity(needle_len);
        let haystack = self.chars().enumerate().skip(from);
        for (i, c) in haystack {
            window.push(c);
            if window.len() == needle_len {
                if window.as_slice() == needle_chars.as_slice() {
                    return Some(i + 1 - needle_len);
                } else {
                    window.remove(0);
                }
            }
        }

        None
    }

    /// find index of ch occurance in self, starting at from.
    pub fn find_char(&self, ch: char, from: usize) -> Option<usize> {
        let len1 = self.len();
        if !(0..len1).contains(&from) {
            None
        } else {
            self.chars().skip(from as usize).position(|c| c == ch)
        }
    }

    pub fn slice(&self, range: Range<usize>) -> StringBufSlice {
        let len = self.len();

        if !(0..len).contains(&range.start) {
            StringBufSlice {
                src: self,
                ranges: 0..0,
            }
        } else {
            let end = range.end.min(len);
            StringBufSlice {
                src: self,
                ranges: range.start..end,
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
                self.len() == other.len()
            }

            (Self::Utf8(s1), Self::Utf8(s2)) => s1 == s2,
            (Self::Utf16(s1), Self::Utf16(s2)) => s1 == s2,
            (Self::Utf8(s1), Self::Utf16(s2)) => {
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
            (Self::Utf16(s1), Self::Utf8(s2)) => {
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
            (Self::Utf8(s1), Self::Utf8(s2)) => s1.cmp(s2),
            (Self::Utf16(s1), Self::Utf16(s2)) => s1.cmp(s2),

            // null is the smallest.
            (Self::Null, _) => Ordering::Less,
            (_, Self::Null) => Ordering::Greater,
            // empty is greater than null only.
            (Self::Empty, _) => Ordering::Less,
            (_, Self::Empty) => Ordering::Greater,

            // utf8 vs utf16 cmp
            (Self::Utf8(s1), Self::Utf16(s2)) => {
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
            (Self::Utf16(s1), Self::Utf8(s2)) => {
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

#[derive(Clone)]
pub struct StringBufSlice<'a> {
    src: &'a StringBuf,
    ranges: Range<usize>,
}

impl StringBufSlice<'_> {
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn to_owned(&self) -> Result<StringBuf, TryReserveError> {
        let len = self.len();
        if len == 0 {
            Ok(StringBuf::Empty)
        } else {
            let mut res = StringBuf::with_capacity(len, self.src.is_wide())?;
            match self.src {
                StringBuf::Utf8(src) => {
                    let _ = res.push_u8_slice(&src[self.ranges.clone()]);
                }
                StringBuf::Utf16(src) => {
                    let _ = res.push_u16_slice(&src[self.ranges.clone()]);
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
