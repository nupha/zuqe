use std::ptr::NonNull;

use crate::{string::CharCode, utils::unlikely};

// Maximum UTF-8 character length
pub const UTF8_CHAR_LEN_MAX: usize = 6;

// Unicode code points
pub const CP_LS: u32 = 0x2028; // Line separator
pub const CP_PS: u32 = 0x2029; // Paragraph separator

// Minimum code point values for UTF-8 encoding
pub const utf8_min_code: [u32; 5] = [0x80, 0x800, 0x10000, 0x00200000, 0x04000000];
// First byte masks for UTF-8 encoding
pub const utf8_first_code_mask: [u8; 5] = [0x1f, 0x0f, 0x07, 0x03, 0x01];

/// Decode utf8 from buf, returns decoded unicode point.
/// Note: may return invalid unicode point.
#[deprecated(note = "use Utf8CharIter instead")]
pub(crate) fn unicode_from_utf8(buf: &[u8]) -> Result<(u32, usize), i32> {
    debug_assert!(!buf.is_empty());

    let c = buf[0];

    // Determine UTF-8 sequence length based on first byte
    let n = match c {
        0x00..=0x7f => {
            return Ok((c as u32, 1)); // ASCII
        }
        0xc0..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf7 => 3,
        0xf8..=0xfb => 4,
        0xfc..=0xfd => 5,
        _ => {
            return Err(-1); // Invalid UTF-8 start byte
        }
    };

    if n > buf.len() - 1 {
        return Err(-1);
    }

    // Apply first byte mask
    let mut c = c as u32;
    c &= utf8_first_code_mask[n - 1] as u32;

    // Decode subsequent bytes
    for i in 0..n {
        let b = buf[i + 1];
        if !(0x80..0xc0).contains(&b) {
            return Err(-1); // Invalid UTF-8 continuation byte
        }
        c = (c << 6) | ((b & 0x3f) as u32);
    }

    // Check if decoded code point is valid
    if unlikely(c < utf8_min_code[n - 1]) {
        Err(-1)
    } else {
        Ok((c, n + 1))
    }
}

// Decode Unicode code point from UTF-8 byte sequence
// Returns -1 on error. *pp is not updated on error. max_len must be >=1.
// Maximum length of UTF-8 byte sequence is 6 bytes (max_len=6).
#[deprecated(note = "use unicode_from_utf8() instead")]
pub(crate) fn unicode_from_utf8_v0(
    mut p: *const u8,
    max_len: i32,
    pp: *mut *const u8,
) -> Result<char, i32> {
    let c = unsafe {
        let c = *p as char;
        p = p.offset(1);
        c
    };

    // ASCII characters returned directly
    if c.is_ascii() {
        unsafe {
            *pp = p;
        }
        return Ok(c);
    }

    // Determine UTF-8 sequence length based on first byte
    let l = match c as u8 {
        0xc0..=0xdf => 1,
        0xe0..=0xef => 2,
        0xf0..=0xf7 => 3,
        0xf8..=0xfb => 4,
        0xfc..=0xfd => 5,
        _ => return Err(-1), // Invalid UTF-8 start byte
    };

    // Check if enough bytes are available
    if l > (max_len - 1) {
        return Err(-1);
    }

    // Apply first byte mask
    let mut c = c as u32;
    c &= utf8_first_code_mask[l as usize - 1] as u32;

    // Decode subsequent bytes
    for _ in 0..l {
        unsafe {
            let b = *p;
            p = p.offset(1);
            if !(0x80..0xc0).contains(&b) {
                return Err(-1); // Invalid UTF-8 continuation byte
            }
            c = (c << 6) | ((b & 0x3f) as u32);
        }
    }

    // Check if decoded code point is valid
    if c < utf8_min_code[l as usize - 1] {
        return Err(-1);
    }

    unsafe {
        *pp = p;
    }
    char::from_u32(c).ok_or(-1)
}

// UTF-8 to Unicode-16 iterator
pub(crate) struct Utf8ToUnicode16Iter<'a> {
    pub bytes: &'a [u8],           // UTF-8 byte sequence
    pub size: usize,               // Length of byte sequence
    pub index: usize,              // Current index
    pub next_unichar: Option<u16>, // Stores the second character of a surrogate pair
}

impl<'a> Iterator for Utf8ToUnicode16Iter<'a> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(uc) = self.next_unichar.take() {
            Some(uc)
        } else if self.index == self.size {
            None
        } else {
            let b1 = self.bytes[self.index];
            self.index += 1;

            match b1 {
                0x00..=0x7F => Some(b1 as u16), // ASCII character
                0x80..=0xBF => Some(0xFFFD),    // Invalid UTF-8 continuation byte
                0xC0..=0xDF => {
                    // 2-byte UTF-8 sequence
                    if self.index < self.size {
                        //same to: self.index < self.size && self.size > self.index
                        let b2 = self.bytes[self.index];
                        self.index += 1;

                        if b2 & 0xC0 == 0x80 {
                            let code_point = ((b1 as u32 & 0x1F) << 6) | (b2 as u32 & 0x3F);
                            if code_point >= 0x80 {
                                Some(code_point as u16)
                            } else {
                                Some(0xFFFD)
                            }
                        } else {
                            self.index -= 1; // Roll back invalid continuation byte
                            Some(0xFFFD)
                        }
                    } else {
                        self.index = self.size;
                        Some(0xFFFD)
                    }
                }
                0xE0..=0xEF => {
                    // 3-byte UTF-8 sequence
                    if self.index < self.size - 2 && self.size >= self.index + 2 {
                        let b2 = self.bytes[self.index];
                        let b3 = self.bytes[self.index + 1];
                        self.index += 2;

                        if b2 & 0xC0 == 0x80 && b3 & 0xC0 == 0x80 {
                            let code_point = ((b1 as u32 & 0x0F) << 12)
                                | ((b2 as u32 & 0x3F) << 6)
                                | (b3 as u32 & 0x3F);
                            if (0x800..=0xFFFF).contains(&code_point)
                                || (0xD800..=0xDFFF).contains(&code_point)
                            {
                                Some(code_point as u16)
                            } else {
                                Some(0xFFFD)
                            }
                        } else {
                            self.index -= 2; // Roll back invalid continuation byte
                            Some(0xFFFD)
                        }
                    } else {
                        self.index = self.size;
                        Some(0xFFFD)
                    }
                }
                0xF0..=0xF7 => {
                    // 4-byte UTF-8 sequence
                    if self.index < self.size - 3 && self.size >= self.index + 3 {
                        let b2 = self.bytes[self.index];
                        let b3 = self.bytes[self.index + 1];
                        let b4 = self.bytes[self.index + 2];
                        self.index += 3;

                        if b2 & 0xC0 == 0x80 && b3 & 0xC0 == 0x80 && b4 & 0xC0 == 0x80 {
                            let code_point = ((b1 as u32 & 0x07) << 18)
                                | ((b2 as u32 & 0x3F) << 12)
                                | ((b3 as u32 & 0x3F) << 6)
                                | (b4 as u32 & 0x3F);
                            if (0x10000..=0x10FFFF).contains(&code_point) {
                                let high_surrogate = ((code_point - 0x10000) >> 10) + 0xD800;
                                let low_surrogate = ((code_point - 0x10000) & 0x3FF) + 0xDC00;
                                self.next_unichar.replace(low_surrogate as u16);
                                Some(high_surrogate as u16)
                            } else {
                                Some(0xFFFD)
                            }
                        } else {
                            self.index -= 3; // Roll back invalid continuation byte
                            Some(0xFFFD)
                        }
                    } else {
                        self.index = self.size;
                        Some(0xFFFD)
                    }
                }
                _ => Some(0xFFFD), // Invalid UTF-8 start byte
            }
        }
    }
}

// Encode Unicode code point to UTF-8 byte sequence
// Encodes up to 31 bits. Outputs at most UTF8_CHAR_LEN_MAX bytes.
pub(crate) fn unicode_to_utf8(buf: &mut Vec<u8>, c: u32) {
    if c < 0x80 {
        buf.push(c as u8);
    } else {
        if c < 0x800 {
            buf.push(((c >> 6) as u8) | 0xc0);
        } else {
            if c < 0x10000 {
                buf.push(((c >> 12) as u8) | 0xe0);
            } else {
                if c < 0x00200000 {
                    buf.push(((c >> 18) as u8) | 0xf0);
                } else {
                    if c < 0x04000000 {
                        buf.push(((c >> 24) as u8) | 0xf8);
                    } else if c < 0x80000000 {
                        buf.push(((c >> 30) as u8) | 0xfc);
                        buf.push(((c >> 24) as u8 & 0x3f) | 0x80);
                    } else {
                        return;
                    }
                    buf.push(((c >> 18) as u8 & 0x3f) | 0x80);
                }
                buf.push(((c >> 12) as u8 & 0x3f) | 0x80);
            }
            buf.push(((c >> 6) as u8 & 0x3f) | 0x80);
        }
        buf.push((c as u8 & 0x3f) | 0x80);
    }
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

/// UTF-8 character iterator
pub(crate) struct Utf8CharIter<'a> {
    bytes: &'a [u8], // UTF-8 bytes
    index: usize,    // current byte index
}

impl<'a> Utf8CharIter<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, index: 0 }
    }

    /// Attempt to decode the UTF-8 character since starting position
    /// Returns (character, bytes consumed) or error
    fn decode_char(&self, since: usize) -> Result<(char, usize), DecodeError> {
        if since >= self.bytes.len() {
            return Err(DecodeError::EndOfInput);
        }

        let b = self.bytes[self.index];

        // ASCII character (0x00-0x7F)
        if b <= 0x7F {
            return Ok((b as char, 1));
        }

        // Determine UTF-8 sequence length
        let seq_len = match b {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            0xF8..=0xFB => 5,
            0xFC..=0xFD => 6,
            _ => return Err(DecodeError::InvalidStartByte(b)),
        };

        // Check if enough bytes are available
        if since + seq_len > self.bytes.len() {
            return Err(DecodeError::IncompleteSequence);
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
            let b = self.bytes[since + i];
            if !(0x80..0xC0).contains(&b) {
                return Err(DecodeError::InvalidContinuationByte(b, i));
            }
            code_point |= ((b & 0x3F) as u32) << (6 * (seq_len - i - 1));
        }

        // Check code point validity
        if code_point > 0x10FFFF {
            return Err(DecodeError::CodePointOutOfRange(code_point));
        }

        // Check for overlong encoding (non‑minimal form)
        if (seq_len == 2 && code_point < 0x80)
            || (seq_len == 3 && code_point < 0x800)
            || (seq_len == 4 && code_point < 0x10000)
        {
            return Err(DecodeError::OverlongEncoding);
        }

        // Check surrogate pair range (U+D800..U+DFFF)
        if (0xD800..=0xDFFF).contains(&code_point) {
            return Err(DecodeError::SurrogateCodePoint(code_point));
        }

        // Convert to character
        char::from_u32(code_point)
            .ok_or(DecodeError::InvalidCodePoint(code_point))
            .map(|c| (c, seq_len))
    }

    /// get current index.
    pub const fn current_index(&self) -> Option<usize> {
        if self.index < self.bytes.len() {
            Some(self.index)
        } else {
            None
        }
    }

    /// get raw pointer to current cursor in bytes.
    /// if cursor index out of bound, returns None
    pub const fn current_ptr(&self) -> Option<NonNull<u8>> {
        if self.index < self.bytes.len() {
            Some(unsafe { NonNull::new_unchecked(self.bytes.as_ptr().add(self.index) as _) })
        } else {
            None
        }
    }

    #[inline(always)]
    pub const fn start_ptr(&self) -> NonNull<u8> {
        unsafe { NonNull::new_unchecked(self.bytes.as_ptr() as _) }
    }

    /// decode char at current position, don't advance cursor.
    pub fn peek(&self) -> Option<(usize, CharCode)> {
        let pos = self.index;

        match self.decode_char(pos) {
            Ok((c, _)) => Some((pos, CharCode::Char(c))),
            Err(DecodeError::EndOfInput) => None,
            Err(
                DecodeError::InvalidStartByte(_)
                | DecodeError::InvalidContinuationByte(_, _)
                | DecodeError::IncompleteSequence
                | DecodeError::OverlongEncoding
                | DecodeError::CodePointOutOfRange(_)
                | DecodeError::SurrogateCodePoint(_)
                | DecodeError::InvalidCodePoint(_),
            ) => Some((pos, CharCode::Code(self.bytes[pos] as _))),
        }
    }

    /// get bytes since current position (inclusive) through end
    pub fn remainder_bytes(&self) -> &[u8] {
        if self.index < self.bytes.len() {
            &self.bytes[self.index..]
        } else {
            &[]
        }
    }

    /// get N bytes since current position (inclusive), to given `buf`.
    /// returns actual bytes count.
    pub fn peek_bytes(&self, buf: &mut [u8]) -> usize {
        if self.index < self.bytes.len() {
            let cnt = std::cmp::min(self.bytes.len() - self.index, buf.len());
            unsafe {
                std::ptr::copy(self.bytes.as_ptr().add(self.index), buf.as_mut_ptr(), cnt);
            }
            cnt
        } else {
            0
        }
    }

    /// test if remainder bytes starts with given pattern.
    ///
    /// this roughly equivelent to `s.remainder_bytes().starts_with(needle)`
    pub fn match_bytes(&self, needle: &[u8]) -> bool {
        let n = self.index + needle.len();
        if n <= self.bytes.len() {
            self.bytes[self.index..n].eq(needle)
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
            self.bytes.get(i as usize).copied()
        } else {
            None
        }
    }

    /// advance or rewind cursor by offset relative to current position
    pub fn move_cursor_by(&mut self, offset: i32) {
        self.index = if offset >= 0 {
            std::cmp::min(self.index + offset as usize, self.bytes.len())
        } else {
            (self.index as i32).saturating_add(offset) as usize
        }
    }

    /// move cursor to end
    #[inline(always)]
    pub const fn is_end(&self) -> bool {
        self.index >= self.bytes.len()
    }

    /// move cursor to end
    #[inline(always)]
    pub const fn end(&mut self) {
        self.index = self.bytes.len()
    }
}

#[derive(Debug, Clone, PartialEq)]
enum DecodeError {
    EndOfInput,
    InvalidStartByte(u8),
    InvalidContinuationByte(u8, usize),
    IncompleteSequence,
    OverlongEncoding,
    CodePointOutOfRange(u32),
    SurrogateCodePoint(u32),
    InvalidCodePoint(u32),
}

impl<'a> Iterator for Utf8CharIter<'a> {
    type Item = (usize, CharCode); // (byte_index, character)

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.bytes.len() {
            let pos = self.index;

            match self.decode_char(pos) {
                Ok((ch, consumed)) => {
                    self.index += consumed;
                    Some((pos, CharCode::Char(ch)))
                }
                Err(DecodeError::EndOfInput) => None,
                Err(
                    DecodeError::InvalidStartByte(_)
                    | DecodeError::InvalidContinuationByte(_, _)
                    | DecodeError::IncompleteSequence
                    | DecodeError::OverlongEncoding
                    | DecodeError::CodePointOutOfRange(_)
                    | DecodeError::SurrogateCodePoint(_)
                    | DecodeError::InvalidCodePoint(_),
                ) => {
                    self.index += 1; // skip one byte when invalid utf8 sequence
                    Some((pos, CharCode::Code(self.bytes[pos] as u16)))
                }
            }
        } else {
            None
        }
    }
}
