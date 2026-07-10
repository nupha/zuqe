// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Module name ↔ URL conversion.
//!
//! Per the "module name unified as URL" design, a module's identity
//! (`ModuleDefImpl.module_name`) is always a canonical `file://` URL (except
//! for native modules, which keep their bare name). This module centralizes the
//! bidirectional conversion between OS filesystem paths and `file://` URLs,
//! delegating all the fiddly parts (Windows drive letters, UNC host segments,
//! percent-encoding) to the `url` crate.

use std::path::{Path, PathBuf};

/// Canonicalize a path, stripping the Windows `\\?\` prefix that
/// `canonicalize()` adds, and restoring `\\?\UNC\server\share` → `\\server\share`.
///
/// This is the "Win32 path → URL-able path" pre-cleaning step that feeds
/// `url::Url::from_file_path`; the WHATWG `url` crate does not understand the
/// `\\?\` extended-length prefix.
fn canonicalize_path<P: AsRef<Path>>(path: P) -> std::io::Result<PathBuf> {
    let p = std::fs::canonicalize(path)?;

    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            // UNC paths: `\\?\UNC\server\share\...` → `\\server\share\...`
            if let Some(unc) = rest
                .strip_prefix("UNC\\")
                .or_else(|| rest.strip_prefix("UNC/"))
            {
                return Ok(PathBuf::from(format!("\\{}", unc)));
            }
            // Regular paths: `\\?\C:\...` → `C:\...`
            return Ok(PathBuf::from(rest));
        }
    }

    Ok(p)
}

/// Resolve a filesystem path to an absolute, canonical path string.
///
/// Tries `canonicalize` (resolves symlinks), then `absolute`, then falls back
/// to the original path unchanged (e.g. for a not-yet-existing file).
fn to_absolute_path(path: &str) -> PathBuf {
    if let Ok(p) = canonicalize_path(path) {
        p
    } else if let Ok(p) = std::path::absolute(path) {
        p
    } else {
        PathBuf::from(path)
    }
}

/// Strip a leading Windows `\\?\` extended-length prefix (and its UNC variant),
/// leaving a normal Win32 path. `url::Url::from_file_path` does not understand
/// `\\?\`, so we remove it before handing the path to the URL layer.
fn strip_extended_prefix(path: &str) -> String {
    #[cfg(windows)]
    {
        if let Some(rest) = path.strip_prefix(r"\\?\") {
            if let Some(unc) = rest
                .strip_prefix("UNC\\")
                .or_else(|| rest.strip_prefix("UNC/"))
            {
                return format!("\\{}", unc);
            }
            return rest.to_string();
        }
    }
    path.to_string()
}

/// Convert a filesystem path into a canonical `file://` URL string.
///
/// On Windows, drive letters (`C:\a`) become `file:///C:/a` and UNC shares
/// (`\\server\share`) become `file://server/share`; the URL layer handles
/// percent-encoding (` `, `$`, non-ASCII) for us. Relative paths are made
/// absolute first so the result is always a valid base URL.
///
/// Returns the original string unchanged if it cannot be represented as a path
/// URL (e.g. it is already a URL or otherwise non-path text).
pub fn path_to_url(path: &str) -> String {
    let abs = to_absolute_path(path);
    let cleaned = strip_extended_prefix(&abs.to_string_lossy());
    match url::Url::from_file_path(Path::new(&cleaned)) {
        Ok(u) => u.to_string(),
        Err(()) => format!("file://{}", cleaned),
    }
}

/// Convert a `file://` URL into its absolute filesystem path.
///
/// Returns `Some(path)` only for `file:` URLs; for any other scheme (e.g.
/// `http(s)://`) or an unparseable value, returns `None` because it does not
/// denote a local file. The returned path is always absolute.
pub fn url_to_abspath(url: &str) -> Option<PathBuf> {
    match url::Url::parse(url) {
        Ok(u) if u.scheme() == "file" => u.to_file_path().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn roundtrip_unix() {
        let url = path_to_url("/usr/x.js");
        assert_eq!(url, "file:///usr/x.js");
        assert_eq!(url_to_abspath(&url), Some(PathBuf::from("/usr/x.js")));
    }

    #[cfg(windows)]
    #[test]
    fn local_drive() {
        // C:\a\b.js -> file:///C:/a/b.js (drive letter special-cased by url crate)
        let url = path_to_url("C:\\a\\b.js");
        assert_eq!(url, "file:///C:/a/b.js");
        assert_eq!(url_to_abspath(&url), Some(PathBuf::from("C:\\a\\b.js")));
    }

    #[cfg(windows)]
    #[test]
    fn percent_encoding() {
        let url = path_to_url("C:\\a b.js");
        assert_eq!(url, "file:///C:/a%20b.js");
        assert_eq!(url_to_abspath(&url), Some(PathBuf::from("C:\\a b.js")));
    }

    #[cfg(windows)]
    #[test]
    fn unc_host_segment() {
        // UNC share: server goes into URL host segment.
        let url = path_to_url("\\\\server\\share\\a.js");
        assert_eq!(url, "file://server/share/a.js");
        assert_eq!(
            url_to_abspath(&url),
            Some(PathBuf::from("\\\\server\\share\\a.js"))
        );
    }

    #[test]
    fn non_file_url_is_none() {
        assert_eq!(url_to_abspath("http://example.com/x.js"), None);
        assert_eq!(url_to_abspath("not a url"), None);
    }

    #[cfg(windows)]
    #[test]
    fn extended_prefix_stripped() {
        // The \\?\ extended-length form normalizes to the same URL.
        let plain = path_to_url("C:\\a\\b.js");
        let extended = path_to_url("\\\\?\\C:\\a\\b.js");
        assert_eq!(plain, extended);
    }
}
