use std::str::{CharIndices, Chars};

use widestring::{
    U16Str,
    ustr::{CharIndicesLossyUtf16, CharsLossyUtf16},
};

use super::StringBuf;

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

impl StringBuf {
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
}
