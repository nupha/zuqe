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
use url::Url;

/// `fs::canonicalize` with the Windows verbatim prefix removed, so the result
/// feeds `url::Url::from_file_path`.
fn canonicalize_path<P: AsRef<Path>>(path: P) -> std::io::Result<PathBuf> {
    let canon = std::fs::canonicalize(path)?;
    #[cfg(windows)]
    let canon = simplified_path(&canon);
    Ok(canon)
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

/// Whether a path names a network share (`\\server\share\...`), including the
/// verbatim form `canonicalize()` returns for one.
///
/// Windows-only: POSIX has no UNC form, and `Url::to_file_path()` refuses a
/// `file:` URL carrying a host everywhere else, so a share cannot reach the
/// filesystem layer as a path there.
#[cfg(windows)]
pub(crate) fn is_unc(path: &Path) -> bool {
    use std::path::{Component, Prefix};

    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
    )
}

/// Strip a leading Windows `\\?\` extended-length prefix (and its UNC variant):
/// `\\?\C:\a\b` becomes `C:\a\b`, `\\?\UNC\srv\share` becomes `\\srv\share`.
///
/// `fs::canonicalize` returns the verbatim form, and `url::Url::from_file_path`
/// does not understand it, so every path crossing that boundary is cleaned here.
#[cfg(windows)]
fn strip_extended_prefix(path: &str) -> String {
    let Some(rest) = path.strip_prefix(r"\\?\") else {
        return path.to_string();
    };
    match rest
        .strip_prefix("UNC\\")
        .or_else(|| rest.strip_prefix("UNC/"))
    {
        // UNC shares keep their two leading slashes.
        Some(unc) => format!("\\\\{}", unc),
        None => rest.to_string(),
    }
}

/// `strip_extended_prefix` for a path `canonicalize()` produced. Windows-only:
/// POSIX has no verbatim form, so its `canonicalize()` result is already final.
#[cfg(windows)]
pub(crate) fn simplified_path(path: &Path) -> PathBuf {
    PathBuf::from(strip_extended_prefix(&path.to_string_lossy()))
}

/// Convert a filesystem path into a canonical `file://` URL.
///
/// On Windows, drive letters (`C:\a`) become `file:///C:/a` and UNC shares
/// (`\\server\share`) become `file://server/share`; the URL layer handles
/// percent-encoding (` `, `$`, non-ASCII) for us. Relative paths are made
/// absolute first so the result is always a valid base URL.
///
/// The return type is `Url` (not `String`) so callers can join/resolve without
/// re-parsing. If the path cannot be represented as a `file:` URL directly
/// (e.g. it is already a URL or otherwise non-path text), falls back to treating
/// it as a `file://`-prefixed URL; `Err` is returned only when even that cannot
/// be parsed.
pub fn path_to_url(path: &str) -> Result<Url, url::ParseError> {
    let abs = to_absolute_path(path);
    let cleaned = {
        #[cfg(windows)]
        {
            strip_extended_prefix(&abs.to_string_lossy())
        }
        #[cfg(not(windows))]
        {
            abs.to_string_lossy().into_owned()
        }
    };

    match Url::from_file_path(Path::new(&cleaned)) {
        Ok(u) => Ok(u),
        Err(()) => Url::parse(&format!("file://{}", cleaned)),
    }
}

/// Convert a `file://` URL into its absolute filesystem path.
///
/// Takes the already-parsed `Url` (the module name at the host boundary).
/// Returns `Some(path)` only for `file:` URLs; for any other scheme (e.g.
/// `http(s)://`) returns `None` because it does not denote a local file.
/// The returned path is always absolute.
pub fn url_to_abspath(url: &Url) -> Option<PathBuf> {
    if url.scheme() == "file" {
        url.to_file_path().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn roundtrip_unix() {
        let url = path_to_url("/usr/x.js").unwrap();
        assert_eq!(url.as_str(), "file:///usr/x.js");
        assert_eq!(url_to_abspath(&url), Some(PathBuf::from("/usr/x.js")));
    }

    #[cfg(windows)]
    #[test]
    fn local_drive() {
        // C:\a\b.js -> file:///C:/a/b.js (drive letter special-cased by url crate)
        let url = path_to_url("C:\\a\\b.js").unwrap();
        assert_eq!(url.as_str(), "file:///C:/a/b.js");
        assert_eq!(url_to_abspath(&url), Some(PathBuf::from("C:\\a\\b.js")));
    }

    #[cfg(windows)]
    #[test]
    fn percent_encoding() {
        let url = path_to_url("C:\\a b.js").unwrap();
        assert_eq!(url.as_str(), "file:///C:/a%20b.js");
        assert_eq!(url_to_abspath(&url), Some(PathBuf::from("C:\\a b.js")));
    }

    #[cfg(windows)]
    #[test]
    fn unc_host_segment() {
        // UNC share: server goes into URL host segment.
        let url = path_to_url("\\\\server\\share\\a.js").unwrap();
        assert_eq!(url.as_str(), "file://server/share/a.js");
        assert_eq!(
            url_to_abspath(&url),
            Some(PathBuf::from("\\\\server\\share\\a.js"))
        );
    }

    #[test]
    fn non_file_url_is_none() {
        assert_eq!(
            url_to_abspath(&Url::parse("http://example.com/x.js").unwrap()),
            None
        );
        assert_eq!(
            Url::parse("not a url").ok().as_ref().map(url_to_abspath),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn extended_prefix_stripped() {
        // The \\?\ extended-length form normalizes to the same URL.
        let plain = path_to_url("C:\\a\\b.js").unwrap();
        let extended = path_to_url("\\\\?\\C:\\a\\b.js").unwrap();
        assert_eq!(plain, extended);
    }

    #[cfg(windows)]
    #[test]
    fn simplified_path_of_real_canonicalized_file() {
        // `fs::canonicalize` really does hand back `\\?\...` on Windows; the
        // simplified form must not leak that prefix to users.
        let f = format!("{}\\Cargo.toml", env!("CARGO_MANIFEST_DIR"));
        let canon = std::fs::canonicalize(&f).unwrap();
        let plain = simplified_path(&canon);
        let s = plain.to_string_lossy();
        assert!(!s.starts_with("\\\\?\\"), "still verbatim: {s}");
        assert!(!s.contains('?'), "still verbatim: {s}");
        assert!(std::path::Path::new(s.as_ref()).exists());
    }

    #[cfg(windows)]
    #[test]
    fn simplified_path_drops_verbatim_prefix() {
        assert_eq!(
            simplified_path(Path::new(r"\\?\C:\a\b.dll")),
            PathBuf::from(r"C:\a\b.dll")
        );
        assert_eq!(
            simplified_path(Path::new(r"\\?\UNC\server\share\a.dll")),
            PathBuf::from(r"\\server\share\a.dll")
        );
        // A plain path is untouched.
        assert_eq!(
            simplified_path(Path::new(r"C:\a\b.dll")),
            PathBuf::from(r"C:\a\b.dll")
        );
    }

    #[cfg(windows)]
    #[test]
    fn only_unc_prefixes_count_as_network() {
        // Both the plain and the verbatim spelling name a share; `canonicalize()`
        // produces the latter.
        assert!(is_unc(Path::new(r"\\server\share\a.dll")));
        assert!(is_unc(Path::new(r"\\server\share\sub\a.dll")));
        assert!(is_unc(Path::new(r"\\?\UNC\server\share\a.dll")));

        // A drive letter is a local path even when it is mapped to a share; the
        // syntax alone cannot tell, so it is the caller's policy call.
        assert!(!is_unc(Path::new(r"C:\a\b.dll")));
        assert!(!is_unc(Path::new(r"Z:\a\b.dll")));
        // Verbatim drive paths and device paths are not shares.
        assert!(!is_unc(Path::new(r"\\?\C:\a\b.dll")));
        assert!(!is_unc(Path::new(r"\\.\PhysicalDrive0")));
    }
}
