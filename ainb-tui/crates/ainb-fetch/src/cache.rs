//! Cache layout helpers (spec §6.5).
//!
//! Each fetched source materializes into
//! `<cache_root>/<source_name>/<short_sha>/`. Shortening to 8 chars
//! keeps directory names readable while still being unique in practice
//! across one source's history.

use std::path::{Path, PathBuf};

/// Truncate a SHA-ish string to 8 chars (or the full string if shorter).
pub fn short_sha(sha: &str) -> &str {
    let cut = sha.char_indices().nth(8).map(|(i, _)| i).unwrap_or(sha.len());
    &sha[..cut]
}

/// Build the canonical cache path for `(source_name, sha)` under a
/// given cache root. The directory is NOT created by this function;
/// fetchers create it after they have content to write.
pub fn cache_path_for(cache_root: &Path, source_name: &str, sha: &str) -> PathBuf {
    cache_root.join(source_name).join(short_sha(sha))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_sha_truncates_long_hex() {
        assert_eq!(short_sha("0e9bc28a1b2c3d4e5f"), "0e9bc28a");
    }

    #[test]
    fn short_sha_preserves_short() {
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn cache_path_layout() {
        let p = cache_path_for(Path::new("/tmp/ainb/cache"), "toolkit", "0e9bc28a1b2c3d4e");
        assert_eq!(p, PathBuf::from("/tmp/ainb/cache/toolkit/0e9bc28a"));
    }
}
