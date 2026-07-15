// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod chars;
mod data;
mod iter;
mod rope;
mod sbuf;
mod string;
mod sub;

pub use {data::*, iter::*, rope::RopeImpl, sbuf::*, string::*};

pub(crate) use chars::*;

/// escape quotes and back-slashes in str
pub(super) fn escape_str(s: &str) -> std::borrow::Cow<'_, str> {
    let n = s.chars().filter(|x| matches!(*x, '\\' | '"')).count();
    if n == 0 {
        std::borrow::Cow::Borrowed(s)
    } else {
        let mut buf = String::with_capacity(s.len() + n);
        for c in s.chars() {
            match c {
                '\\' => {
                    buf.push_str(r"\\");
                }
                // '"' => {
                //     buf.push_str("\\\"");
                // }
                _ => {
                    buf.push(c);
                }
            }
        }

        std::borrow::Cow::Owned(buf)
    }
}
