use std::{ops::Range, ptr::NonNull};

use crate::string::CharCode;

// Maximum UTF-8 character length
pub const UTF8_CHAR_LEN_MAX: usize = 6;

// Unicode code points
pub const CP_LS: u32 = 0x2028; // Line separator
pub const CP_PS: u32 = 0x2029; // Paragraph separator

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

    /// decode char at current position, don't advance cursor.
    /// returns char or code and sequence length
    fn current_char(&self) -> Option<(CharCode, usize)>;

    /// decode char at current cursur, then advance cursor to next char.
    fn next_char(&mut self) -> Option<(Range<usize>, CharCode)>;

    /// get current cursor position.
    fn position(&self) -> Option<usize>;

    /// advance or rewind cursor by offset relative to current position
    fn move_cursor_by(&mut self, offset: i32);

    /// check current cursor is at end of data?
    fn is_end(&self) -> bool;
}

#[derive(Debug)]
pub struct Utf8Stream<'a> {
    data: &'a [u8], // UTF-8 bytes
    index: usize,   // current byte index
}

impl<'a> Utf8Stream<'a> {
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, index: 0 }
    }

    /// load data since current position (inclusive), to given `buf`.
    /// returns actual bytes slice.
    pub fn peek_data<'b>(&self, buf: &'b mut [u8]) -> &'b [u8] {
        if self.index < self.data.len() {
            let cnt = std::cmp::min(self.data.len() - self.index, buf.len());
            unsafe {
                std::ptr::copy(self.data.as_ptr().add(self.index), buf.as_mut_ptr(), cnt);
            }
            &buf[..cnt]
        } else {
            &[]
        }
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

    /// get byte at offset relative to current position
    /// e.g. 0=current index, 1=next of current index, -1=prev of current index
    #[inline]
    pub fn peek_byte_at(&self, offset: i32) -> Option<u8> {
        let i = self.index as i32 + offset;
        if i >= 0 {
            self.data.get(i as usize).copied()
        } else {
            None
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

    /// Get byte at current cursor.
    /// For semantic meaning, this is actually an alias to `::peek_byte_at(0)`
    #[inline(always)]
    pub fn current_byte(&self) -> Option<u8> {
        self.peek_byte_at(0)
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

    fn position(&self) -> Option<usize> {
        if self.index < self.data.len() {
            Some(self.index)
        } else {
            None
        }
    }

    /// advance or rewind cursor by offset relative to current position
    fn move_cursor_by(&mut self, offset: i32) {
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

impl<'a> Utf16Stream<'a> {
    pub const fn new(data: &'a [u16]) -> Self {
        Self { data, index: 0 }
    }

    /// get N data since current position (inclusive), to given `buf`.
    /// returns actual data slice.
    pub fn peek_data<'b>(&self, buf: &'b mut [u16]) -> &'b [u16] {
        if self.index < self.data.len() {
            let cnt = std::cmp::min(self.data.len() - self.index, buf.len());
            unsafe {
                std::ptr::copy(self.data.as_ptr().add(self.index), buf.as_mut_ptr(), cnt);
            }
            &buf[..cnt]
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
    pub fn peek_element_at(&self, offset: i32) -> Option<u16> {
        let i = self.index as i32 + offset;
        if i >= 0 {
            self.data.get(i as usize).copied()
        } else {
            None
        }
    }

    /// data since current position (inclusive) through end
    pub fn remainder(&self) -> &[u16] {
        if self.index < self.data.len() {
            &self.data[self.index..]
        } else {
            &[]
        }
    }

    /// Get element at current cursor.
    /// For semantic meaning, this is actually an alias to `::peek_byte_at(0)`
    #[inline(always)]
    pub fn current_element(&self) -> Option<u16> {
        self.peek_element_at(0)
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

    fn position(&self) -> Option<usize> {
        if self.index < self.data.len() {
            Some(self.index)
        } else {
            None
        }
    }

    fn move_cursor_by(&mut self, offset: i32) {
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
