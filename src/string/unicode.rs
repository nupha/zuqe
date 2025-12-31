use std::ptr::NonNull;

use crate::string::CharCode;

// Maximum UTF-8 character length
pub const UTF8_CHAR_LEN_MAX: usize = 6;

// Unicode code points
pub const CP_LS: u32 = 0x2028; // Line separator
pub const CP_PS: u32 = 0x2029; // Paragraph separator

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

pub(crate) struct Utf8Stream<'a> {
    bytes: &'a [u8], // UTF-8 bytes
    index: usize,    // current byte index
}

impl<'a> Utf8Stream<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, index: 0 }
    }

    /// Attempt to decode the UTF-8 character at current cursor position
    /// Returns (char, bytes_consumed) or error
    pub fn decode_char(&self, since: usize) -> Result<(char, usize), CharDecodeError> {
        if since >= self.bytes.len() {
            return Err(CharDecodeError::EndOfInput);
        }

        let b = self.bytes[self.index];

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
            _ => return Err(CharDecodeError::InvalidByte(b, 0)),
        };

        // Check if enough bytes are available
        if since + seq_len > self.bytes.len() {
            return Err(CharDecodeError::Incomplete);
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
                return Err(CharDecodeError::InvalidByte(b, i));
            }
            code_point |= ((b & 0x3F) as u32) << (6 * (seq_len - i - 1));
        }

        // Check code point validity
        if code_point > 0x10FFFF {
            return Err(CharDecodeError::CodePointOutOfRange(code_point, seq_len));
        }

        // Check for overlong encoding (non‑minimal form)
        if (seq_len == 2 && code_point < 0x80)
            || (seq_len == 3 && code_point < 0x800)
            || (seq_len == 4 && code_point < 0x10000)
        {
            return Err(CharDecodeError::Overlong(code_point, seq_len));
        }

        // Check surrogate pair range (U+D800..U+DFFF)
        if (0xD800..=0xDFFF).contains(&code_point) {
            return Err(CharDecodeError::Surrogate(code_point, seq_len));
        }

        // Convert to character
        char::from_u32(code_point)
            .ok_or(CharDecodeError::InvalidCodePoint(code_point, seq_len))
            .map(|c| (c, seq_len))
    }

    /// decode char at current position, don't advance cursor.
    /// returns char or code and sequence len
    pub fn current_char(&self) -> Option<(CharCode, usize)> {
        let pos = self.index;

        match self.decode_char(pos) {
            Ok((c, n)) => Some((CharCode::Char(c), n)),
            Err(CharDecodeError::Surrogate(c, n)) => Some((CharCode::Code(c), n)),
            Err(CharDecodeError::EndOfInput) => None,
            Err(
                CharDecodeError::Incomplete
                | CharDecodeError::Overlong(..)
                | CharDecodeError::InvalidByte(..)
                | CharDecodeError::CodePointOutOfRange(..)
                | CharDecodeError::InvalidCodePoint(..),
            ) => Some((CharCode::Code(self.bytes[pos] as _), 1)),
        }
    }

    /// decode char at current cursur, then advance cursor to next char.
    pub fn next_char(&mut self) -> Option<(usize, CharCode)> {
        if self.index < self.bytes.len() {
            let pos = self.index;
            match self.current_char() {
                Some((c, n)) => {
                    self.index += n;
                    Some((pos, c))
                }
                None => {
                    self.index += 1; // skip one byte when invalid utf8 sequence
                    Some((pos, CharCode::Code(self.bytes[pos] as _)))
                }
            }
        } else {
            None
        }
    }

    /// get N bytes since current position (inclusive), to given `buf`.
    /// returns actual bytes slice.
    pub fn peek_bytes<'b>(&self, buf: &'b mut [u8]) -> &'b [u8] {
        if self.index < self.bytes.len() {
            let cnt = std::cmp::min(self.bytes.len() - self.index, buf.len());
            unsafe {
                std::ptr::copy(self.bytes.as_ptr().add(self.index), buf.as_mut_ptr(), cnt);
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

    /// all bytes
    #[inline(always)]
    pub const fn bytes(&self) -> &[u8] {
        self.bytes
    }

    /// bytes since current position (inclusive) through end
    pub fn remainder_bytes(&self) -> &[u8] {
        if self.index < self.bytes.len() {
            &self.bytes[self.index..]
        } else {
            &[]
        }
    }

    /// get current index.
    pub const fn current_index(&self) -> Option<usize> {
        if self.index < self.bytes.len() {
            Some(self.index)
        } else {
            None
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
}

#[derive(Debug, Clone, PartialEq)]
pub enum CharDecodeError {
    EndOfInput,
    Incomplete,
    InvalidByte(u8, usize),
    Overlong(u32, usize),
    Surrogate(u32, usize),
    CodePointOutOfRange(u32, usize),
    InvalidCodePoint(u32, usize),
}

impl<'a> Iterator for Utf8Stream<'a> {
    type Item = (usize, CharCode); // (byte_index, char_or_code)

    #[inline(always)]
    fn next(&mut self) -> Option<Self::Item> {
        self.next_char()
    }
}
