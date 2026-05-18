//! HTTP fetcher (`https:` URIs).
//!
//! Downloads a single file via reqwest::blocking into
//! `<cache_root>/<source_name>/<sha>/<basename>`. The SHA is a hash of
//! the URL string (deterministic; doesn't require reading the content
//! first), so re-fetches of the same URL hit the cached file.
//!
//! Tests for the network path are gated behind
//! `AINB_TEST_NETWORK=1` + `#[ignore]` so the default suite stays
//! offline.

use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::Path;

use ainb_skill_core::{SourceType, Uri};

use crate::cache::cache_path_for;
use crate::fetcher::{now_utc_iso8601, FetchError, FetchedRef, Fetcher};

pub struct HttpFetcher {
    client: reqwest::blocking::Client,
}

impl HttpFetcher {
    pub fn new() -> Self {
        Self {
            client: reqwest::blocking::Client::builder()
                .user_agent(concat!("ainb-fetch/", env!("CARGO_PKG_VERSION")))
                .build()
                .expect("reqwest client"),
        }
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher for HttpFetcher {
    fn fetch(
        &self,
        uri: &Uri,
        source_name: &str,
        cache_root: &Path,
    ) -> Result<FetchedRef, FetchError> {
        if uri.source_type != SourceType::Https {
            return Err(FetchError::UnsupportedSourceType(
                uri.source_type.as_str().to_string(),
            ));
        }

        let url = format!("https:{}", uri.locator);
        let sha = url_pseudo_sha(&url);
        let final_dir = cache_path_for(cache_root, source_name, &sha);
        let filename = basename_from_url(&url);
        let final_file = final_dir.join(&filename);

        if final_file.exists() {
            return Ok(FetchedRef {
                path: final_file,
                resolved_sha: sha,
                fetched_at: now_utc_iso8601(),
            });
        }

        let resp = self
            .client
            .get(&url)
            .send()
            .map_err(|e| FetchError::Http(format!("GET {url}: {e}")))?;
        if !resp.status().is_success() {
            return Err(FetchError::Http(format!(
                "GET {url}: status {}",
                resp.status()
            )));
        }
        let bytes = resp
            .bytes()
            .map_err(|e| FetchError::Http(format!("read body for {url}: {e}")))?;

        fs::create_dir_all(&final_dir)?;
        let tmp_file = final_dir.join(format!("{filename}.tmp"));
        {
            let mut f = fs::File::create(&tmp_file)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp_file, &final_file)?;

        Ok(FetchedRef {
            path: final_file,
            resolved_sha: sha,
            fetched_at: now_utc_iso8601(),
        })
    }
}

fn url_pseudo_sha(url: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn basename_from_url(url: &str) -> String {
    url.rsplit_once('/')
        .map(|(_, name)| name)
        .filter(|n| !n.is_empty())
        .map(|n| n.split('?').next().unwrap_or(n).to_string())
        .unwrap_or_else(|| "download".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pseudo_sha_is_deterministic() {
        let a = url_pseudo_sha("https://example.com/x");
        let b = url_pseudo_sha("https://example.com/x");
        assert_eq!(a, b);
        let c = url_pseudo_sha("https://example.com/y");
        assert_ne!(a, c);
    }

    #[test]
    fn basename_strips_query() {
        assert_eq!(
            basename_from_url("https://example.com/a/skill.md?v=1"),
            "skill.md"
        );
    }

    #[test]
    fn basename_falls_back_when_path_empty() {
        assert_eq!(basename_from_url("https://example.com/"), "download");
    }

    #[test]
    fn rejects_non_https_source_type() {
        let uri = Uri::parse("gh:foo/bar@main").unwrap();
        let cache = tempfile::tempdir().unwrap();
        let err = HttpFetcher::new()
            .fetch(&uri, "x", cache.path())
            .unwrap_err();
        assert!(matches!(err, FetchError::UnsupportedSourceType(_)));
    }

    #[test]
    #[ignore = "needs network: set AINB_TEST_NETWORK=1 and use --ignored"]
    fn downloads_small_file() {
        if std::env::var("AINB_TEST_NETWORK").as_deref() != Ok("1") {
            return;
        }
        let cache = tempfile::tempdir().unwrap();
        // Stable, tiny endpoint that just echoes bytes.
        let uri = Uri::parse("https://httpbin.org/bytes/64").unwrap();
        let fetched = HttpFetcher::new()
            .fetch(&uri, "httpbin", cache.path())
            .unwrap();
        assert!(fetched.path.exists());
        assert_eq!(std::fs::metadata(&fetched.path).unwrap().len(), 64);
    }
}
