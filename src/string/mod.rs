mod data;
mod iter;
mod sbuf;
mod string;
mod sub;
mod unicode;
mod utf_8_16;

pub use {data::*, iter::*, sbuf::*, string::*};

pub(crate) use {unicode::*, utf_8_16::*};

/// escape quotes and back-slashes in str
pub(super) fn escape_str(s: &str) -> std::borrow::Cow<'_, str> {
    let n = s.chars().filter(|x| matches!(*x, '\\' | '"')).count();
    if n == 0 {
        std::borrow::Cow::Borrowed(s)
    } else {
        let mut b = String::with_capacity(s.len() + n);
        for c in s.chars() {
            match c {
                '\\' => {
                    b.push('\\');
                    b.push('\\');
                }
                '"' => {
                    b.push('\\');
                    b.push('"');
                }
                _ => {
                    b.push(c);
                }
            }
        }

        std::borrow::Cow::Owned(b)
    }
}
