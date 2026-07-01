//! Git fetcher (handles `gh:`, `git:`, and `gist:` URIs).
//!
//! Clones the resolved remote URL into a staging directory, resolves
//! the requested ref to a commit SHA, and renames the staging dir into
//! `<cache_root>/<source_name>/<short_sha>/`. The rename is the
//! atomicity boundary: an interrupted clone leaves only the staging
//! dir behind, which is removed on the next attempt.

use std::fs;
use std::path::Path;

use ainb_skill_core::{SourceType, Uri};

use crate::cache::cache_path_for;
use crate::fetcher::{FetchError, FetchedRef, Fetcher, now_utc_iso8601};

const STAGING_DIR: &str = ".staging";

pub struct GitFetcher;

impl GitFetcher {
    pub fn new() -> Self {
        Self
    }
}

impl Default for GitFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Fetcher for GitFetcher {
    fn fetch(
        &self,
        uri: &Uri,
        source_name: &str,
        cache_root: &Path,
    ) -> Result<FetchedRef, FetchError> {
        let remote_url = git_remote_url(uri)?;
        let ref_name = uri.ref_.as_deref().unwrap_or("main");

        let source_root = cache_root.join(source_name);
        fs::create_dir_all(&source_root)?;
        let staging = source_root.join(STAGING_DIR);
        // Clear any stale staging from a previous failed run.
        if staging.exists() {
            fs::remove_dir_all(&staging).ok();
        }

        let repo = git2::Repository::clone(&remote_url, &staging)
            .map_err(|e| FetchError::Git(format!("clone `{remote_url}`: {e}")))?;

        let sha = resolve_ref_to_sha(&repo, ref_name)
            .map_err(|e| FetchError::Git(format!("resolve `{ref_name}`: {e}")))?;
        checkout_sha(&repo, &sha).map_err(|e| FetchError::Git(format!("checkout `{sha}`: {e}")))?;

        // Drop the Repository before renaming so handles are closed.
        drop(repo);

        let final_path = cache_path_for(cache_root, source_name, &sha);
        if final_path.exists() {
            // Already cached at this exact SHA — drop the redundant clone.
            fs::remove_dir_all(&staging).ok();
        } else {
            if let Some(parent) = final_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&staging, &final_path).map_err(|e| {
                FetchError::Io(std::io::Error::new(
                    e.kind(),
                    format!("rename staging→cache: {e}"),
                ))
            })?;
        }

        Ok(FetchedRef {
            path: final_path,
            resolved_sha: sha,
            fetched_at: now_utc_iso8601(),
        })
    }
}

/// Compute the HTTPS clone URL for a [`Uri`] this fetcher understands.
///
/// `marketplace:` URIs share the github-shaped locator convention with
/// `gh:` URIs — the `marketplace` prefix is just a hint to the source
/// adapter, not a different transport.
pub fn git_remote_url(uri: &Uri) -> Result<String, FetchError> {
    match uri.source_type {
        SourceType::Gh | SourceType::Marketplace => {
            Ok(format!("https://github.com/{}.git", uri.locator))
        }
        SourceType::Gist => Ok(format!("https://gist.github.com/{}.git", uri.locator)),
        SourceType::Git => Ok(uri.locator.clone()),
        other => Err(FetchError::UnsupportedSourceType(
            other.as_str().to_string(),
        )),
    }
}

fn resolve_ref_to_sha(repo: &git2::Repository, ref_name: &str) -> Result<String, git2::Error> {
    // Try remote branch first (refs/remotes/origin/<name>), then tag,
    // then raw revspec (catches full or short SHAs).
    let candidates = [
        format!("refs/remotes/origin/{ref_name}"),
        format!("refs/tags/{ref_name}"),
        ref_name.to_string(),
    ];
    let mut last_err: Option<git2::Error> = None;
    for c in &candidates {
        match repo.revparse_single(c) {
            Ok(obj) => return Ok(obj.peel_to_commit()?.id().to_string()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| {
        git2::Error::from_str(&format!("no candidates resolved for `{ref_name}`"))
    }))
}

fn checkout_sha(repo: &git2::Repository, sha: &str) -> Result<(), git2::Error> {
    let obj = repo.revparse_single(sha)?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_tree(&obj, Some(&mut checkout))?;
    repo.set_head_detached(obj.id())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_remote_url_gh() {
        let uri = Uri::parse("gh:stevengonsalvez/my-skills@main").unwrap();
        assert_eq!(
            git_remote_url(&uri).unwrap(),
            "https://github.com/stevengonsalvez/my-skills.git"
        );
    }

    #[test]
    fn git_remote_url_gist() {
        let uri = Uri::parse("gist:abc123def@HEAD").unwrap();
        assert_eq!(
            git_remote_url(&uri).unwrap(),
            "https://gist.github.com/abc123def.git"
        );
    }

    #[test]
    fn git_remote_url_git_pass_through() {
        let uri = Uri::parse("git:https://gitlab.example.com/x/y@v1").unwrap();
        assert_eq!(
            git_remote_url(&uri).unwrap(),
            "https://gitlab.example.com/x/y"
        );
    }

    #[test]
    fn git_remote_url_rejects_local() {
        let uri = Uri::parse("local:/tmp/foo@HEAD").unwrap();
        assert!(matches!(
            git_remote_url(&uri),
            Err(FetchError::UnsupportedSourceType(_))
        ));
    }

    #[test]
    fn git_remote_url_marketplace_shares_gh_shape() {
        let uri = Uri::parse("marketplace:anthropic/claude-marketplace@latest").unwrap();
        assert_eq!(
            git_remote_url(&uri).unwrap(),
            "https://github.com/anthropic/claude-marketplace.git"
        );
    }
}
