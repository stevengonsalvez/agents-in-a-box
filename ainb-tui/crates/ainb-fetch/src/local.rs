//! Local-path fetcher (`local:` URIs).
//!
//! No copy / clone — the fetched path is the locator itself. The
//! "resolved SHA" is a deterministic pseudo-digest derived from the
//! path + its directory mtime so it can key the cache and let
//! consumers detect "user edited the source dir" without paying for
//! a tree walk.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ainb_skill_core::{SourceType, Uri};

use crate::fetcher::{now_utc_iso8601, FetchError, FetchedRef, Fetcher};

pub struct LocalFetcher;

impl LocalFetcher {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LocalFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher for LocalFetcher {
    fn fetch(
        &self,
        uri: &Uri,
        _source_name: &str,
        _cache_root: &Path,
    ) -> Result<FetchedRef, FetchError> {
        if uri.source_type != SourceType::Local {
            return Err(FetchError::UnsupportedSourceType(
                uri.source_type.as_str().to_string(),
            ));
        }

        let path = PathBuf::from(&uri.locator);
        if !path.exists() {
            return Err(FetchError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("local path does not exist: {}", path.display()),
            )));
        }

        let resolved_sha = pseudo_sha(&path)?;
        Ok(FetchedRef {
            path,
            resolved_sha,
            fetched_at: now_utc_iso8601(),
        })
    }
}

/// Stable digest of `path` + its mtime, formatted as 16 hex chars.
/// Cheap, no cryptographic guarantees — only used as a cache key.
fn pseudo_sha(path: &Path) -> Result<String, FetchError> {
    let meta = std::fs::metadata(path)?;
    let modified = meta
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    modified.hash(&mut hasher);
    Ok(format!("{:016x}", hasher.finish()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetches_existing_local_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.md"), b"# hi").unwrap();

        let uri_str = format!("local:{}", dir.path().display());
        let uri = Uri::parse(&uri_str).unwrap();
        let fetched = LocalFetcher::new()
            .fetch(&uri, "ignored-for-local", Path::new("/tmp/ainb-cache"))
            .unwrap();

        assert_eq!(fetched.path, dir.path());
        assert_eq!(fetched.resolved_sha.len(), 16);
    }

    #[test]
    fn missing_path_errors() {
        let uri = Uri::parse("local:/definitely-not-a-real-path-9f7c1e3").unwrap();
        let err = LocalFetcher::new()
            .fetch(&uri, "x", Path::new("/tmp/ainb-cache"))
            .unwrap_err();
        assert!(matches!(err, FetchError::Io(_)));
    }

    #[test]
    fn rejects_non_local_source_type() {
        let uri = Uri::parse("gh:foo/bar@main").unwrap();
        let err = LocalFetcher::new()
            .fetch(&uri, "x", Path::new("/tmp/ainb-cache"))
            .unwrap_err();
        assert!(matches!(err, FetchError::UnsupportedSourceType(_)));
    }

    #[test]
    fn sha_stable_for_unchanged_dir() {
        let dir = tempfile::tempdir().unwrap();
        let uri_str = format!("local:{}", dir.path().display());
        let uri = Uri::parse(&uri_str).unwrap();
        let a = LocalFetcher::new().fetch(&uri, "x", Path::new("/")).unwrap();
        let b = LocalFetcher::new().fetch(&uri, "x", Path::new("/")).unwrap();
        assert_eq!(a.resolved_sha, b.resolved_sha);
    }
}
