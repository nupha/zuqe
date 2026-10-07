// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod eval;
mod export;
mod import;
mod mdef;
mod reqres;
mod resolve;
mod url;

pub use {
    export::*,
    import::*,
    mdef::*,
    reqres::*,
    url::{path_to_url, url_to_abspath},
};

// Windows-only helpers, re-exported for the zapi loader (the sole caller, which
// is behind the `zapi` feature). `url` itself keeps them private otherwise.
#[cfg(all(windows, feature = "zapi"))]
pub(crate) use url::{is_unc, simplified_path};
