// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#[inline(always)]
pub(crate) const fn unlikely(expr: bool) -> bool {
    if expr {
        #[cold]
        const fn cold_path() -> bool {
            true
        }
        cold_path()
    } else {
        false
    }
}
