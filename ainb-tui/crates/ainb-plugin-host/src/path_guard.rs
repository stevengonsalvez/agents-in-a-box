//! Allowlist-rooted path canonicalisation + adversarial guards.
//!
//! Plugins ask the host to read files by name; the host must reject any
//! attempt to escape the directories the plugin's capabilities granted.
//! This module is the single chokepoint for that decision so the fs host
//! fns (and the cache layer) share one set of rules:
//!
//! 1. Reject NUL bytes outright (defends against truncation attacks on the
//!    underlying filesystem syscalls).
//! 2. Expand `~/` against the user's home directory so plugins can write
//!    portable paths without the host knowing about home expansion in two
//!    places.
//! 3. Canonicalise the requested path (resolves `..`, `.`, and follows
//!    symlinks).
//! 4. Canonicalise the allowlist root the same way (so a `/private/var`
//!    vs `/var` skew on macOS doesn't reject a legitimate path that just
//!    happens to live behind a system-installed symlink). Both sides
//!    canonicalise twice — once at allowlist construction, once here —
//!    so any pair `(canonical(req), canonical(root))` compare on the same
//!    physical prefix.
//! 5. The canonical request must `starts_with` one of the canonical
//!    allowlist roots. Otherwise → `PathDenied`.
//!
//! Step 3 is what defeats `..` traversal: `canonicalize` returns an
//! absolute, fully-resolved path with no `..` component left, so an
//! attacker who concatenated `~/.claude/projects/../../etc/passwd`
//! ends up with `/etc/passwd`, which fails the `starts_with` check.
//!
//! Symlink defence falls out of the same step: `canonicalize` follows
//! symlinks; a symlink rooted inside the allowlist that points outside
//! resolves to its real target before the prefix check runs.
//!
//! For paths that do not yet exist on disk (read of a missing file, or
//! a `cache_put` to a fresh key), `canonicalize` errors. We fall back
//! to lexical normalisation of the parent directory + the filename, and
//! still apply the prefix check. This is best-effort by design: a TOCTOU
//! attacker who races us by replacing a path component with a symlink
//! between this check and the actual `read`/`write` is a known
//! limitation, accepted because the protected workload is read-only
//! session logs, not security-sensitive secrets.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Reason a path was refused. Surfaces via the host fns as
/// [`HostStatus::NotPermitted`] or [`HostStatus::InvalidArgument`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PathGuardError {
    /// Path string contained an embedded NUL byte. Always refused.
    #[error("path contains NUL byte")]
    NulByte,
    /// Path string was empty after `~/` expansion.
    #[error("path is empty")]
    Empty,
    /// Could not resolve `~/` because we have no home directory.
    #[error("$HOME is unresolvable")]
    NoHome,
    /// Canonical path was not under any allowlist root.
    #[error("path {0:?} is denied — not under any allowlist root")]
    PathDenied(PathBuf),
    /// Path's parent dir does not exist (so we can't even canonicalise
    /// the parent). The plugin asked for a write into a directory the
    /// host has not been told about.
    #[error("path {0:?} parent does not exist")]
    NoParent(PathBuf),
}

/// A canonicalised allowlist root. Constructed once per capability set and
/// reused for every host-fn call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowlistRoot {
    canonical: PathBuf,
    /// The pre-canonicalised path. Kept around for diagnostics and as a
    /// fallback prefix check when a tail path can't be canonicalised
    /// (the file does not yet exist).
    raw: PathBuf,
}

impl AllowlistRoot {
    /// Build from a single root. `path` may contain a leading `~/`; that
    /// gets expanded against the current home dir. If the resulting path
    /// exists, it is canonicalised; otherwise the un-canonical form is
    /// retained so the allowlist still applies to dirs that haven't been
    /// created yet.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Option<Self> {
        let raw = expand_home(path.into())?;
        let canonical = raw.canonicalize().unwrap_or_else(|_| raw.clone());
        Some(Self { canonical, raw })
    }

    /// The fully-resolved root.
    #[must_use]
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }
}

/// Validate `requested` against `roots`. Returns the canonicalised path on
/// success — callers should use *that* path for the actual filesystem
/// operation, not the raw input, so the read/write hits the resolved
/// target rather than re-resolving (and re-racing) via the kernel.
///
/// `requested` may contain a leading `~/`.
///
/// # Errors
///
/// Returns [`PathGuardError`] if the path:
/// * contains a NUL byte,
/// * is empty after `~/` expansion,
/// * resolves outside every supplied root, or
/// * has no canonicalisable parent (its parent dir doesn't exist).
pub fn check_under_allowlist(
    requested: &str,
    roots: &[AllowlistRoot],
) -> Result<PathBuf, PathGuardError> {
    if requested.as_bytes().contains(&0) {
        return Err(PathGuardError::NulByte);
    }
    let raw = PathBuf::from(requested);
    let expanded = expand_home(raw).ok_or(PathGuardError::NoHome)?;
    if expanded.as_os_str().is_empty() {
        return Err(PathGuardError::Empty);
    }

    let canonical = match expanded.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            // The leaf doesn't exist yet (e.g. cache_put to a new key, or
            // probing for absence). Resolve the parent + tail manually so
            // we still get a usable absolute path for the prefix check.
            let parent = expanded
                .parent()
                .ok_or_else(|| PathGuardError::NoParent(expanded.clone()))?;
            let parent_canon = parent
                .canonicalize()
                .map_err(|_| PathGuardError::NoParent(expanded.clone()))?;
            let file = expanded
                .file_name()
                .ok_or_else(|| PathGuardError::NoParent(expanded.clone()))?;
            parent_canon.join(file)
        }
    };

    let permitted = roots
        .iter()
        .any(|root| canonical.starts_with(&root.canonical) || canonical.starts_with(&root.raw));
    if !permitted {
        return Err(PathGuardError::PathDenied(canonical));
    }
    Ok(canonical)
}

fn expand_home(p: PathBuf) -> Option<PathBuf> {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/") {
        dirs::home_dir().map(|h| h.join(rest))
    } else if s == "~" {
        dirs::home_dir()
    } else {
        Some(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn root(p: &Path) -> AllowlistRoot {
        AllowlistRoot::new(p.to_path_buf()).expect("root")
    }

    #[test]
    fn happy_path_inside_allowlist_returns_canonical() {
        let dir = TempDir::new().unwrap();
        let leaf = dir.path().join("hello.jsonl");
        fs::write(&leaf, b"x").unwrap();
        let canon = check_under_allowlist(&leaf.to_string_lossy(), &[root(dir.path())])
            .expect("permitted");
        // canonical may differ from input on macOS (/private/var prefix);
        // the contract is just "it ends with our basename".
        assert_eq!(canon.file_name().unwrap(), "hello.jsonl");
    }

    #[test]
    fn dot_dot_traversal_rejected() {
        let dir = TempDir::new().unwrap();
        let inside = dir.path().join("a");
        fs::create_dir_all(&inside).unwrap();
        // Build a path that uses .. to climb out of the allowlist before
        // re-entering /etc/passwd-style territory. Either a `PathDenied`
        // (we resolved up to /etc/passwd) or `NoParent` (we never resolved
        // because the climbed-to dir is not a real path on this host) is a
        // valid refusal — both keep the plugin out.
        let evil = dir.path().join("a/../../../../etc/passwd");
        let err = check_under_allowlist(&evil.to_string_lossy(), &[root(dir.path())]);
        assert!(
            matches!(
                err,
                Err(PathGuardError::PathDenied(_)) | Err(PathGuardError::NoParent(_))
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn dot_dot_traversal_to_real_target_rejected() {
        // Stronger variant: ../ that resolves to a real path *outside*
        // the allowlist (e.g. /etc which exists on every unix). This is
        // the canonical "passes the parent.canonicalize() step but fails
        // the prefix check" case.
        #[cfg(unix)]
        {
            let dir = TempDir::new().unwrap();
            // dir.path() is something like /var/folders/.../tmp...; we
            // climb out enough levels and back into /etc.
            let evil = dir.path().join("../../../../../../../../etc/hosts");
            let err = check_under_allowlist(&evil.to_string_lossy(), &[root(dir.path())]);
            assert!(matches!(err, Err(PathGuardError::PathDenied(_))), "got {err:?}");
        }
    }

    #[test]
    fn nul_byte_rejected() {
        let dir = TempDir::new().unwrap();
        let evil = format!("{}/foo\0bar", dir.path().display());
        assert_eq!(
            check_under_allowlist(&evil, &[root(dir.path())]),
            Err(PathGuardError::NulByte)
        );
    }

    #[test]
    fn symlink_pointing_outside_allowlist_rejected() {
        let outside = TempDir::new().unwrap();
        let secret = outside.path().join("secret.txt");
        fs::write(&secret, b"hush").unwrap();

        let allow = TempDir::new().unwrap();
        let link = allow.path().join("portal");
        // unix: create symlink portal → outside/secret.txt
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, &link).unwrap();
        // windows: skip (test will compile but won't enforce; CI is unix).
        #[cfg(not(unix))]
        return;

        let err = check_under_allowlist(&link.to_string_lossy(), &[root(allow.path())]);
        // Canonicalisation followed the symlink → outside the allowlist.
        assert!(matches!(err, Err(PathGuardError::PathDenied(_))), "got {err:?}");
    }

    #[test]
    fn macos_private_var_doesnt_break_match() {
        // On macOS, `std::env::temp_dir()` returns something like
        // /var/folders/... but canonicalize() resolves to
        // /private/var/folders/... — both sides of the comparison must
        // canonicalise so the prefix check still holds.
        let dir = TempDir::new().unwrap();
        let leaf = dir.path().join("a.txt");
        fs::write(&leaf, b"x").unwrap();
        // Build the root from the *raw* path; check uses the canonical.
        // This mirrors how the caller wires it: the allowlist comes from
        // expanding `~/.claude/projects/`, the request path comes from the
        // plugin verbatim.
        let r = root(dir.path());
        let canon = check_under_allowlist(&leaf.to_string_lossy(), &[r])
            .expect("must canonicalise both sides");
        assert!(canon.is_absolute());
    }

    #[test]
    fn missing_leaf_with_existing_parent_is_allowed() {
        let dir = TempDir::new().unwrap();
        // Path's leaf doesn't exist yet — this is the cache_put case.
        let leaf = dir.path().join("not-yet-written.bin");
        let canon = check_under_allowlist(&leaf.to_string_lossy(), &[root(dir.path())])
            .expect("missing leaf is fine if parent + prefix check pass");
        assert_eq!(canon.file_name().unwrap(), "not-yet-written.bin");
    }

    #[test]
    fn missing_parent_dir_rejected() {
        let dir = TempDir::new().unwrap();
        // Neither parent nor leaf exists — the plugin invented a directory
        // outside the allowlist tree we'd never create.
        let evil = dir.path().join("nonexistent-parent/not-yet-written.bin");
        let err = check_under_allowlist(&evil.to_string_lossy(), &[root(dir.path())]);
        assert!(matches!(err, Err(PathGuardError::NoParent(_))), "got {err:?}");
    }

    #[test]
    fn empty_path_rejected() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            check_under_allowlist("", &[root(dir.path())]),
            Err(PathGuardError::Empty)
        );
    }

    #[test]
    fn home_expansion_resolves_tilde() {
        // We don't have a real allowlist that contains $HOME, but the
        // expansion itself shouldn't error. Reuse a tempdir that we
        // pretend lives under home.
        let p = expand_home(PathBuf::from("~/some/leaf"));
        assert!(p.is_some(), "tilde expansion should produce a path");
        let p = p.unwrap();
        assert!(p.is_absolute(), "expanded ~/ must be absolute, got {p:?}");
        assert!(p.ends_with("some/leaf"));
    }
}
