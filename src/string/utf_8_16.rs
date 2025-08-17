// utf8 and utf16 mapping utils

use std::ops::Range;

use crate::string::U16CharIter;

#[inline]
pub(crate) fn utf8_test_first_byte(b: u8) -> u8 {
    if (b & 0x80) == 0 {
        1
    } else if (b & 0xE0) == 0xC0 {
        2
    } else if (b & 0xF0) == 0xE0 {
        3
    } else if (b & 0xF8) == 0xF0 {
        4
    } else {
        0
    }
}

/// get char starting index for the char at position `pos` of a utf8 encodes sequence.
pub(crate) fn utf8_char_boundary_at(bytes: &[u8], pos: usize) -> Range<usize> {
    debug_assert!(pos < bytes.len());

    let mut index = pos;

    // look at most 4 bytes backward
    for _ in 0..4 {
        let b = bytes[index];

        let n = utf8_test_first_byte(b) as usize;
        if n > 0 {
            // b is starting byte
            if index + n <= bytes.len() {
                return index..index + n;
            } else {
                break;
            };
        } else if index == 0 {
            break;
        } else {
            index -= 1;
        }
    }

    pos..pos
}

#[inline(always)]
const fn is_utf8_char_boundary(u: u8) -> bool {
    // This is bit magic equivalent to: b < 128 || b >= 192
    (u as i8) >= -0x40
}

/// map an index in utf8 to utf16 index
fn map_utf8_to_utf16_pos_floor(s8: &[u8], s16: &[u16], pos: usize) -> usize {
    debug_assert!(unsafe { str::from_utf8_unchecked(s8).is_char_boundary(0) });

    let mut it = U16CharIter::new(s16, 0);
    let utf8_text = unsafe { str::from_utf8_unchecked(s8) };

    let (start_char_floor, start_index) = if pos == 0 {
        (pos, pos)
    } else {
        // detect char boundary at pos
        // let char_bound = utf8_char_boundary_at(s8, pos);
        let char_bound = {
            let lower_bound = pos.saturating_sub(3);
            let new_index = s8[lower_bound..=pos]
                .iter()
                .rposition(|b| is_utf8_char_boundary(*b));

            // SAFETY: we know that the character boundary will be within four bytes
            lower_bound + unsafe { new_index.unwrap_unchecked() }
        };

        let mut idx = if char_bound == 0 {
            0
        } else {
            let ltrim_nc = std::str::from_utf8(&s8[0..char_bound])
                .unwrap()
                .chars()
                .count();
            debug_assert!(ltrim_nc > 0);
            it.nth(ltrim_nc - 1).unwrap().0
        };

        // test if start char is broken by range.start
        match pos - char_bound {
            0 => {
                // not broken
            }
            1 => {
                // could be 2/3/4-bytes utf8, broken
                // in case of 4-byte utf8, corresponding utf16 is surrogate, take high part.
            }
            2 => {
                // could be 2/3/4-bytes utf8, broken.
                // in case of 4-byte utf8, corresponding utf16 is surrogate, take low part.
                if utf8_test_first_byte(s8[char_bound]) == 4 {
                    idx += 1;
                }
            }
            3 => {
                // 4-bytes utf8, broken, corresponding utf16 is surrogate, take low part.
                idx += 1;
            }
            _ => unreachable!(),
        }

        (char_bound, idx)
    };

    start_index
}

/// map an index in utf8 to utf16 index
fn map_utf8_to_utf16_pos_ceil(s8: &[u8], s16: &[u16], pos: usize) -> usize {
    debug_assert!(unsafe { str::from_utf8_unchecked(s8).is_char_boundary(0) });

    let mut it = U16CharIter::new(s16, 0);
    let utf8_text = unsafe { str::from_utf8_unchecked(s8) };

    let end_index = if pos == s8.len() {
        s16.len()
    } else {
        let end_char_floor = {
            let lower_bound = pos.saturating_sub(3);
            let new_index = s8[lower_bound..=pos]
                .iter()
                .rposition(|b| is_utf8_char_boundary(*b));
            lower_bound + unsafe { new_index.unwrap_unchecked() }
        };

        let nc = utf8_text[0..end_char_floor].chars().count();

        let mut idx = it
            .nth(if end_char_floor < pos {
                nc
            } else {
                debug_assert!(nc > 0);
                nc - 1
            })
            .unwrap()
            .0;

        // check char is broken by range.end
        if pos > end_char_floor {
            let nbytes = utf8_test_first_byte(s8[end_char_floor]) as usize;
            match nbytes - (pos - end_char_floor) {
                1 => {}
                2 => {
                    // take surrogate low
                    if nbytes == 4 {
                        idx -= 1;
                    }
                }
                3 => {
                    // take surrogate high
                    idx -= 1;
                }
                _ => unreachable!(),
            }
        }

        idx
    };

    end_index
}

/// map index range in utf8 to utf16 correspondant.
/// assume `s8` and `s16` have same content in utf8 and utf16 encoding respectively.
pub(crate) fn map_utf8_to_utf16_range(
    s8: &[u8],
    s16: &[u16],
    s8_range: Range<usize>,
) -> Range<usize> {
    if s8_range.start == 0 && s8_range.len() == s8.len() {
        0..s16.len() // all content
    } else {
        map_utf8_to_utf16_pos_floor(s8, s16, s8_range.start)
            ..map_utf8_to_utf16_pos_ceil(s8, s16, s8_range.end)
    }
}

/// map an index in utf8 to utf16 index
pub(crate) fn map_utf8_to_utf16_range_v1(
    s8: &[u8],
    s16: &[u16],
    range: Range<usize>,
) -> Range<usize> {
    debug_assert!(unsafe { str::from_utf8_unchecked(s8).is_char_boundary(0) });

    let mut it = U16CharIter::new(s16, 0);
    let utf8_text = unsafe { str::from_utf8_unchecked(s8) };

    let (start_char_floor, start_index) = if range.start == 0 {
        (range.start, range.start)
    } else {
        // detect char boundary at pos
        // let char_bound = utf8_char_boundary_at(s8, pos);
        let char_bound = {
            let lower_bound = range.start.saturating_sub(3);
            let new_index = s8[lower_bound..=range.start]
                .iter()
                .rposition(|b| is_utf8_char_boundary(*b));

            // SAFETY: we know that the character boundary will be within four bytes
            lower_bound + unsafe { new_index.unwrap_unchecked() }
        };

        let mut idx = if char_bound == 0 {
            0
        } else {
            let ltrim_nc = std::str::from_utf8(&s8[0..char_bound])
                .unwrap()
                .chars()
                .count();
            debug_assert!(ltrim_nc > 0);
            it.nth(ltrim_nc - 1).unwrap().0
        };

        // test if start char is broken by range.start
        match range.start - char_bound {
            0 => {
                // not broken
            }
            1 => {
                // could be 2/3/4-bytes utf8, broken
                // in case of 4-byte utf8, corresponding utf16 is surrogate, take high part.
            }
            2 => {
                // could be 2/3/4-bytes utf8, broken.
                // in case of 4-byte utf8, corresponding utf16 is surrogate, take low part.
                if utf8_test_first_byte(s8[char_bound]) == 4 {
                    idx += 1;
                }
            }
            3 => {
                // 4-bytes utf8, broken, corresponding utf16 is surrogate, take low part.
                idx += 1;
            }
            _ => unreachable!(),
        }

        (char_bound, idx)
    };

    // calc end index to utf16
    let end_index = if range.end == s8.len() {
        s16.len()
    } else {
        let end_char_floor = {
            let lower_bound = range.start.saturating_sub(3);
            let new_index = s8[lower_bound..range.end]
                .iter()
                .rposition(|b| is_utf8_char_boundary(*b));
            lower_bound + unsafe { new_index.unwrap_unchecked() }
        };
        let end_char_ceil = {
            let new_index = s8[range.end..std::cmp::min(range.end + 4, s8.len())]
                .iter()
                .position(|b| is_utf8_char_boundary(*b));
            // SAFETY: we know that the character boundary will be within four bytes
            range.end + unsafe { new_index.unwrap_unchecked() }
        };

        if end_char_floor == start_char_floor {
            // broken surrogate
            debug_assert!(matches!(range.len(), 1 | 2 | 3));
            start_index + 1
        } else {
            let match_nc = utf8_text[start_char_floor..end_char_ceil].chars().count();
            let mut idx = it.nth(match_nc - 1).unwrap().0;

            // check char is broken by range.end
            match end_char_ceil - range.end {
                0 => {
                    // not broken
                }
                1 => {}
                2 => {
                    // take surrogate low
                    if utf8_test_first_byte(s8[end_char_floor]) == 4 {
                        idx -= 1;
                    }
                }
                3 => {
                    // take surrogate high
                    idx -= 1;
                }
                _ => unreachable!(),
            }

            idx
        }
    };

    start_index..end_index
}
