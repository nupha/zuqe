// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{ops::Range, ptr::NonNull};

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

    /// invalid code point is forced to utf16 too.
    #[inline(always)]
    pub fn encode_utf16<'a>(&self, buf: &'a mut [u16; 2]) -> &'a [u16] {
        u32_encode_utf16(self.to_u32(), buf)
    }
}

/// accept invalid code point too.
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

#[derive(Debug, Clone, PartialEq)]
pub enum DecodeCharError {
    EndOfInput,
    Incomplete,
    InvalidSeq(u32, usize),
    Overlong(u32, usize),
    Surrogate(u32, usize),
    InvalidCodePoint(u32, usize),
}

#[inline(always)]
pub(crate) const fn char_is_space(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '\u{feff}' |  // Zero Width No-Break Space (BOM)
            '\u{200b}' |  // Zero Width Space
            '\u{200c}' |  // Zero Width Non-Joiner
            '\u{200d}' |  // Zero Width Joiner
            '\u{2060}' // Word Joiner
        )
}

pub(crate) trait CharCodeDecoder: Iterator<Item = (Range<usize>, CharCode)> {
    /// Attempt to decode the UTF-8 character at current cursor position
    /// Returns (char, bytes_consumed) or error
    fn decode_char(&self, since: usize) -> Result<(char, usize), DecodeCharError>;

    /// decode char at current position, don't advance position.
    /// returns char or code and sequence length
    fn current_char(&self) -> Option<(CharCode, usize)>;

    /// decode char at current position, then advance cursor to next char.
    fn next_char(&mut self) -> Option<(Range<usize>, CharCode)>;

    /// Set current position (absolute).
    fn set_pos(&mut self, i: usize);

    /// Returns the current position.
    fn pos(&self) -> Option<usize>;

    /// Seeks relative to the current position.
    fn seek_relative(&mut self, offset: i32);

    /// Returns data length
    fn len(&self) -> usize;

    /// check current cursor is at end of data?
    fn is_end(&self) -> bool;

    /// if has invalid code point, returns false.
    /// if all code point is valid, returns true.
    fn is_well_formed(&self) -> bool {
        let len = self.len();
        let mut p = 0;
        while p < len {
            if let Ok((_, n)) = self.decode_char(p) {
                p += n;
            } else {
                return false;
            }
        }

        true
    }
}

#[derive(Debug)]
pub struct Utf8Stream<'a> {
    data: &'a [u8], // UTF-8 bytes
    index: usize,   // current byte index
}

impl<'a> Clone for Utf8Stream<'a> {
    /// current position reset to 0 for cloned stream
    fn clone(&self) -> Self {
        Self {
            data: self.data,
            index: 0,
        }
    }
}

impl<'a> Utf8Stream<'a> {
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, index: 0 }
    }

    /// test if remainder bytes starts with given pattern.
    ///
    /// this roughly equivelent to `s.remainder_bytes().starts_with(needle)`
    pub fn match_bytes(&self, needle: &[u8]) -> bool {
        let n = self.index + needle.len();
        if n <= self.data.len() {
            self.data[self.index..n].eq(needle)
        } else {
            false
        }
    }

    /// Return byte at offset relative to current position.
    /// e.g. 0=current index, 1=next of current index, -1=prev of current index
    #[inline]
    pub fn peek_relative(&self, offset: i32) -> Option<u8> {
        let i = self.index as i32 + offset;
        if i >= 0 {
            self.data.get(i as usize).copied()
        } else {
            None
        }
    }

    /// Get byte at current position.
    /// This is equivelent to `::peek_relative(0)`
    #[inline(always)]
    pub fn peek(&self) -> Option<u8> {
        self.peek_relative(0)
    }

    /// read data since current position (inclusive), to given `buf`.
    pub fn read<'b>(&self, buf: &'b mut [u8]) -> &'b [u8] {
        if self.index < self.data.len() {
            let n = std::cmp::min(self.data.len() - self.index, buf.len());
            unsafe {
                std::ptr::copy(self.data.as_ptr().add(self.index), buf.as_mut_ptr(), n);
            }
            &buf[..n]
        } else {
            &[]
        }
    }

    /// data slice since current position (inclusive) through end
    pub fn remainder(&self) -> &[u8] {
        if self.index < self.data.len() {
            &self.data[self.index..]
        } else {
            &[]
        }
    }

    /// get raw pointer to current cursor in bytes.
    /// if cursor index out of bound, returns None
    pub const fn current_ptr(&self) -> Option<NonNull<u8>> {
        if self.index < self.data.len() {
            Some(unsafe { NonNull::new_unchecked(self.data.as_ptr().add(self.index) as _) })
        } else {
            None
        }
    }

    #[inline(always)]
    pub const fn start_ptr(&self) -> NonNull<u8> {
        unsafe { NonNull::new_unchecked(self.data.as_ptr() as _) }
    }

    /// return valid utf8 str since current position (inclusive), and move cursor to next byte if have some
    pub fn next_str(&mut self) -> Option<&str> {
        if self.index < self.data.len() {
            match std::str::from_utf8(&self.data[self.index..]) {
                Ok(s) => {
                    self.index += s.len();
                    Some(s)
                }
                Err(e) => {
                    let n = e.valid_up_to();
                    if n > 0 {
                        let s =
                            std::str::from_utf8(&self.data[self.index..self.index + n]).unwrap();
                        self.index += n;
                        Some(s)
                    } else {
                        None
                    }
                }
            }
        } else {
            None
        }
    }
}

impl<'a> CharCodeDecoder for Utf8Stream<'a> {
    fn decode_char(&self, since: usize) -> Result<(char, usize), DecodeCharError> {
        if since < self.data.len() {
            let b = self.data[since];
            if b <= 0x7F {
                // ASCII
                return Ok((b as char, 1));
            }

            // Determine UTF-8 sequence length
            let seq_len = match b {
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                0xF8..=0xFB => 5,
                0xFC..=0xFD => 6,
                _ => return Err(DecodeCharError::InvalidSeq(b as _, 0)),
            };

            // Check if enough bytes are available
            if since + seq_len > self.data.len() {
                return Err(DecodeCharError::Incomplete);
            }

            // Decode UTF-8 sequence
            let mut code_point = match seq_len {
                2 => (b as u32 & 0x1F) << 6,
                3 => (b as u32 & 0x0F) << 12,
                4 => (b as u32 & 0x07) << 18,
                5 => (b as u32 & 0x03) << 24,
                6 => (b as u32 & 0x01) << 30,
                _ => unreachable!(),
            };

            // Decode subsequent bytes
            for i in 1..seq_len {
                let b = self.data[since + i];
                if !(0x80..0xC0).contains(&b) {
                    return Err(DecodeCharError::InvalidSeq(b as _, i));
                }
                code_point |= ((b & 0x3F) as u32) << (6 * (seq_len - i - 1));
            }

            // Check code point validity
            if code_point > 0x10FFFF {
                return Err(DecodeCharError::InvalidCodePoint(code_point, seq_len));
            }

            // Check for overlong encoding (non‑minimal form)
            if (seq_len == 2 && code_point < 0x80)
                || (seq_len == 3 && code_point < 0x800)
                || (seq_len == 4 && code_point < 0x10000)
            {
                return Err(DecodeCharError::Overlong(code_point, seq_len));
            }

            // Check surrogate pair range (U+D800..U+DFFF)
            if (0xD800..=0xDFFF).contains(&code_point) {
                return Err(DecodeCharError::Surrogate(code_point, seq_len));
            }

            // Convert to character
            char::from_u32(code_point)
                .ok_or(DecodeCharError::InvalidCodePoint(code_point, seq_len))
                .map(|c| (c, seq_len))
        } else {
            Err(DecodeCharError::EndOfInput)
        }
    }

    fn current_char(&self) -> Option<(CharCode, usize)> {
        let pos = self.index;

        match self.decode_char(pos) {
            Ok((c, n)) => Some((CharCode::Char(c), n)),
            Err(DecodeCharError::Surrogate(cp, n) | DecodeCharError::InvalidCodePoint(cp, n)) => {
                Some((CharCode::Code(cp), n))
            }
            Err(DecodeCharError::EndOfInput) => None,
            Err(
                DecodeCharError::Incomplete
                | DecodeCharError::Overlong(..)
                | DecodeCharError::InvalidSeq(..),
            ) => Some((CharCode::Code(self.data[pos] as _), 1)),
        }
    }

    fn next_char(&mut self) -> Option<(Range<usize>, CharCode)> {
        let pos = self.index;
        if pos < self.data.len() {
            match self.current_char() {
                Some((c, n)) => {
                    self.index += n;
                    Some((pos..pos + n, c))
                }
                None => {
                    self.index += 1; // skip one byte when invalid utf8 sequence
                    Some((pos..pos + 1, CharCode::Code(self.data[pos] as _)))
                }
            }
        } else {
            None
        }
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.data.len()
    }

    fn pos(&self) -> Option<usize> {
        if self.index < self.data.len() {
            Some(self.index)
        } else {
            None
        }
    }

    fn set_pos(&mut self, i: usize) {
        self.index = std::cmp::min(i, self.data.len());
    }

    /// advance or rewind cursor by offset relative to current position
    fn seek_relative(&mut self, offset: i32) {
        self.index = if offset >= 0 {
            std::cmp::min(self.index + offset as usize, self.data.len())
        } else {
            (self.index as i32).saturating_add(offset) as usize
        }
    }

    #[inline(always)]
    fn is_end(&self) -> bool {
        self.index >= self.data.len()
    }
}

impl<'a> Iterator for Utf8Stream<'a> {
    type Item = (Range<usize>, CharCode);

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        self.next_char()
    }
}

#[derive(Debug)]
pub struct Utf16Stream<'a> {
    data: &'a [u16], // UTF16 data
    index: usize,    // current index
}

impl<'a> Clone for Utf16Stream<'a> {
    /// current position reset to 0 for cloned stream
    fn clone(&self) -> Self {
        Self {
            data: self.data,
            index: 0,
        }
    }
}

impl<'a> Utf16Stream<'a> {
    pub const fn new(data: &'a [u16]) -> Self {
        Self { data, index: 0 }
    }

    /// read data since current position (inclusive), to given `buf`.
    pub fn read<'b>(&self, buf: &'b mut [u16]) -> &'b [u16] {
        if self.index < self.data.len() {
            let n = std::cmp::min(self.data.len() - self.index, buf.len());
            unsafe {
                std::ptr::copy(self.data.as_ptr().add(self.index), buf.as_mut_ptr(), n);
            }
            &buf[..n]
        } else {
            &[]
        }
    }

    /// test if remainder data starts with given pattern.
    ///
    /// this roughly equivelent to `s.remainder_data().starts_with(needle)`
    pub fn match_data(&self, needle: &[u16]) -> bool {
        let n = self.index + needle.len();
        if n <= self.data.len() {
            self.data[self.index..n].eq(needle)
        } else {
            false
        }
    }

    /// get element at offset relative to current position
    /// e.g. 0=current index, 1=next of current index, -1=prev of current index
    #[inline]
    pub fn peek_relative(&self, offset: i32) -> Option<u16> {
        let i = self.index as i32 + offset;
        if i >= 0 {
            self.data.get(i as usize).copied()
        } else {
            None
        }
    }

    /// Get element at current position.
    /// This is equivelent to `::peek_relative(0)`
    #[inline(always)]
    pub fn peek(&self) -> Option<u16> {
        self.peek_relative(0)
    }

    /// data since current position (inclusive) through end
    pub fn remainder(&self) -> &[u16] {
        if self.index < self.data.len() {
            &self.data[self.index..]
        } else {
            &[]
        }
    }

    /// get raw pointer to current cursor in data.
    /// if cursor index out of bound, returns None
    pub const fn current_ptr(&self) -> Option<NonNull<u16>> {
        if self.index < self.data.len() {
            Some(unsafe { NonNull::new_unchecked(self.data.as_ptr().add(self.index) as _) })
        } else {
            None
        }
    }

    #[inline(always)]
    pub const fn start_ptr(&self) -> NonNull<u16> {
        unsafe { NonNull::new_unchecked(self.data.as_ptr() as _) }
    }
}

impl<'a> CharCodeDecoder for Utf16Stream<'a> {
    fn decode_char(&self, since: usize) -> Result<(char, usize), DecodeCharError> {
        if since < self.data.len() {
            let cp1 = self.data[since] as u32;

            if cp1 <= 0x7F {
                return Ok((cp1 as u8 as char, 1));
            }

            let mut len = 1;
            let mut code_point = cp1;

            if (0xD800..=0xDBFF).contains(&cp1)
                && let Some(cp2) = self.data.get(since + 1)
                && (0xDC00..=0xDFFF).contains(cp2)
            {
                // surrogate pair
                len += 1;
                code_point = ((cp1 - 0xD800) << 10) + (*cp2 as u32 - 0xDC00) + 0x10000;
            }

            // Check surrogate pair range (U+D800..U+DFFF)
            if (0xD800..=0xDFFF).contains(&code_point) {
                return Err(DecodeCharError::Surrogate(code_point, len));
            }
            // Check code point validity
            if code_point > 0x10FFFF {
                return Err(DecodeCharError::InvalidCodePoint(code_point, len));
            }

            // Convert to character
            char::from_u32(code_point)
                .ok_or(DecodeCharError::InvalidCodePoint(code_point, len))
                .map(|c| (c, len))
        } else {
            Err(DecodeCharError::EndOfInput)
        }
    }

    fn current_char(&self) -> Option<(CharCode, usize)> {
        let pos = self.index;

        match self.decode_char(pos) {
            Ok((c, n)) => Some((CharCode::Char(c), n)),
            Err(DecodeCharError::Surrogate(cp, n) | DecodeCharError::InvalidCodePoint(cp, n)) => {
                Some((CharCode::Code(cp), n))
            }
            Err(DecodeCharError::EndOfInput) => None,
            Err(
                DecodeCharError::Incomplete
                | DecodeCharError::Overlong(..)
                | DecodeCharError::InvalidSeq(..),
            ) => Some((CharCode::Code(self.data[pos] as _), 1)),
        }
    }

    fn next_char(&mut self) -> Option<(Range<usize>, CharCode)> {
        let pos = self.index;
        if pos < self.data.len() {
            match self.current_char() {
                Some((c, n)) => {
                    self.index += n;
                    Some((pos..pos + n, c))
                }
                None => {
                    self.index += 1; // skip one element when invalid utf8 sequence
                    Some((pos..pos + 1, CharCode::Code(self.data[pos] as _)))
                }
            }
        } else {
            None
        }
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.data.len()
    }

    fn pos(&self) -> Option<usize> {
        if self.index < self.data.len() {
            Some(self.index)
        } else {
            None
        }
    }

    fn set_pos(&mut self, i: usize) {
        self.index = std::cmp::min(i, self.data.len());
    }

    fn seek_relative(&mut self, offset: i32) {
        self.index = if offset >= 0 {
            std::cmp::min(self.index + offset as usize, self.data.len())
        } else {
            (self.index as i32).saturating_add(offset) as usize
        }
    }

    #[inline(always)]
    fn is_end(&self) -> bool {
        self.index >= self.data.len()
    }
}

impl<'a> Iterator for Utf16Stream<'a> {
    type Item = (Range<usize>, CharCode);

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        self.next_char()
    }
}

pub(crate) fn is_all_chars_same<S1: CharCodeDecoder, S2: CharCodeDecoder>(
    mut s1: S1,
    mut s2: S2,
) -> bool {
    loop {
        match (s1.next(), s2.next()) {
            (Some((_, a)), Some((_, b))) if a.to_u32() == b.to_u32() => {
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
