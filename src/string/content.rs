use std::ops::Deref;

use super::{StringBuf, StringData};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct StringContent(StringData);

impl Deref for StringContent {
    type Target = StringData;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<StringBuf> for StringContent {
    #[inline(always)]
    fn from(buf: StringBuf) -> Self {
        Self(StringData::from(buf))
    }
}

impl From<StringData> for StringContent {
    #[inline(always)]
    fn from(data: StringData) -> Self {
        Self(data)
    }
}

impl std::fmt::Debug for StringContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl StringContent {
    #[inline(always)]
    pub fn data(&self) -> &StringData {
        &self.0
    }

    #[inline(always)]
    pub fn empty() -> Self {
        StringContent(StringData::Empty)
    }

    #[inline(always)]
    pub fn ident() -> Self {
        Self(StringData::Ident)
    }
}
