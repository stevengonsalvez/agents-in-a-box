// ABOUTME: One-time clone of a remote-only favorite into a managed dir (bead
// pv8). A favorite migrated to a remote indicator carries no local checkout
// path, so the card-create @ roster cannot provision a worktree from it. This
// module clones such a remote ONCE into `~/.agents-in-a-box/clones/<dir>` and
// returns the local path, which the existing worktree provisioning then treats
// like any other local repo. Idempotent: an existing clone is reused as-is.
//
// COLLISION SAFETY: the clone dir is derived from the remote itself — a readable
// tail plus a blake3 fingerprint of the full remote — so two DIFFERENT remotes
// that happen to share a tail (`acme/widget` vs `evil/widget`) never collide on
// the same checkout. CONCURRENCY: a per-clone advisory file lock serialises two
// cards racing to clone the same remote, so neither wipes the other's partial.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use fs2::FileExt;

/// The managed directory holding one-time clones of remote-only favorites:
/// `<ainb_dir>/clones`.
#[must_use]
pub fn clones_dir(ainb_dir: &Path) -> PathBuf {
    ainb_dir.join("clones")
}

/// The collision-safe local checkout directory for `remote` under `ainb_dir`.
///
/// `<clones_dir>/<tail>-<fingerprint>` where `tail` is the remote's last path
/// segment (sanitised, for human readability) and `fingerprint` is the first 12
/// hex of `blake3(canonical_remote)` — so two remotes sharing a tail get distinct
/// dirs (bead pv8 / trap: slug collision across remotes).
#[must_use]
pub fn clone_path(ainb_dir: &Path, remote: &str) -> PathBuf {
    let canonical = canonical_remote(remote);
    let tail = sanitized_tail(&canonical);
    let fingerprint = &blake3::hash(canonical.as_bytes()).to_hex()[..12];
    clones_dir(ainb_dir).join(format!("{tail}-{fingerprint}"))
}

/// Expand a favorite's `source` remote into a git-clonable URL.
///
/// - `owner/repo` shorthand → `https://github.com/owner/repo.git`.
/// - anything already qualified (a `://` scheme, a `git@…` scp URL, or an
///   absolute / relative local path) passes through verbatim — so a test can
///   point at a local bare repo and production honours a full clone URL.
fn canonical_remote(remote: &str) -> String {
    let r = remote.trim();
    let qualified =
        r.contains("://") || r.starts_with("git@") || r.starts_with('/') || r.starts_with('.');
    if qualified {
        return r.to_string();
    }
    format!("https://github.com/{}.git", r.trim_end_matches('/'))
}

/// The remote's final path segment, stripped of a `.git` suffix and reduced to
/// `[A-Za-z0-9._-]` (other chars → `-`), for a readable clone-dir prefix. Empty
/// or all-stripped input falls back to `repo`.
fn sanitized_tail(canonical: &str) -> String {
    let raw = canonical
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("repo")
        .strip_suffix(".git")
        .unwrap_or_else(|| canonical.rsplit(['/', ':']).next().unwrap_or("repo"));
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "repo".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Whether `dir` is a usable git checkout (carries a `.git` entry).
fn is_git_checkout(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Ensure a one-time clone of `remote` exists at [`clone_path`], returning its
/// local path (bead pv8).
///
/// Idempotent: an existing git checkout at the derived destination is reused
/// verbatim (no fetch, no re-clone), so a repeated pick / rerun is cheap and
/// offline-safe. A destination that exists but is NOT a git checkout — a partial
/// clone left by an earlier failure — is removed and re-cloned. On a clone
/// failure the partial destination is cleaned up before the error returns, so a
/// retry starts fresh and a worktree is never provisioned from a half-clone.
///
/// Concurrency-safe: the check-and-clone runs under a per-clone advisory file
/// lock, so two cards racing to clone the SAME remote serialise — the second sees
/// the first's finished checkout and reuses it rather than wiping a partial.
///
/// # Errors
///
/// Returns an [`io::Error`] if the managed directory cannot be created, the lock
/// cannot be taken, the `git clone` subprocess cannot spawn, or the clone exits
/// non-zero.
pub fn ensure_clone(ainb_dir: &Path, remote: &str) -> io::Result<PathBuf> {
    let dest = clone_path(ainb_dir, remote);
    std::fs::create_dir_all(clones_dir(ainb_dir))?;

    // Serialise concurrent clones of the same remote on a sibling lock file so a
    // racing card can't delete-then-overwrite this clone's partial checkout.
    let lock_path = dest.with_extension("lock");
    let lock = std::fs::OpenOptions::new().create(true).write(true).open(&lock_path)?;
    lock.lock_exclusive()?;
    let result = clone_under_lock(&dest, remote);
    let _ = FileExt::unlock(&lock);
    result
}

/// The clone body, run while the caller holds the per-clone advisory lock.
fn clone_under_lock(dest: &Path, remote: &str) -> io::Result<PathBuf> {
    if is_git_checkout(dest) {
        return Ok(dest.to_path_buf());
    }
    if dest.exists() {
        // A partial / non-git leftover from an aborted clone: clear it so the
        // fresh clone below never fails on a non-empty target directory.
        std::fs::remove_dir_all(dest)?;
    }
    let url = canonical_remote(remote);
    let out = Command::new("git").arg("clone").arg("--").arg(&url).arg(dest).output()?;
    if out.status.success() {
        return Ok(dest.to_path_buf());
    }
    // A failed clone can leave a partial checkout — remove it so a retry is clean.
    let _ = std::fs::remove_dir_all(dest);
    Err(io::Error::other(format!(
        "git clone {url:?} → {} failed ({}): {}",
        dest.display(),
        out.status,
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Build a local bare repo with one commit and return its path — a stand-in
    /// for a "remote" that `git clone` can clone over the filesystem, so the test
    /// never touches the network.
    fn make_bare_remote(root: &Path, name: &str) -> PathBuf {
        let work = root.join(format!("src-{name}"));
        std::fs::create_dir_all(&work).unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(&work)
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t.t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(work.join("README.md"), format!("hello {name}")).unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "init"]);
        let bare = root.join(format!("{name}.git"));
        let ok = Command::new("git")
            .args(["clone", "--bare", "-q"])
            .arg(&work)
            .arg(&bare)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "bare clone failed");
        bare
    }

    #[test]
    fn owner_repo_shorthand_expands_to_github_https() {
        assert_eq!(
            canonical_remote("acme/widget"),
            "https://github.com/acme/widget.git"
        );
    }

    #[test]
    fn qualified_remotes_pass_through() {
        assert_eq!(
            canonical_remote("https://example.com/x/y.git"),
            "https://example.com/x/y.git"
        );
        assert_eq!(
            canonical_remote("git@github.com:x/y.git"),
            "git@github.com:x/y.git"
        );
        assert_eq!(
            canonical_remote("/tmp/local/bare.git"),
            "/tmp/local/bare.git"
        );
    }

    /// Two DIFFERENT remotes sharing a tail get DISTINCT clone dirs (trap: slug
    /// collision) — the readable prefix is the same, the fingerprint differs.
    #[test]
    fn distinct_remotes_sharing_a_tail_never_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let ainb = tmp.path().join(".agents-in-a-box");
        let a = clone_path(&ainb, "acme/widget");
        let b = clone_path(&ainb, "evil/widget");
        assert_ne!(a, b, "same tail, different remote → different dir");
        assert!(a.file_name().unwrap().to_str().unwrap().starts_with("widget-"));
        assert!(b.file_name().unwrap().to_str().unwrap().starts_with("widget-"));
    }

    #[test]
    fn ensure_clone_creates_then_reuses_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let ainb = tmp.path().join(".agents-in-a-box");
        let remote = make_bare_remote(tmp.path(), "widget");
        let remote_str = remote.to_str().unwrap();

        // First pick clones …
        let first = ensure_clone(&ainb, remote_str).unwrap();
        assert_eq!(first, clone_path(&ainb, remote_str));
        assert!(first.join(".git").exists(), "a real checkout landed");
        assert!(
            first.join("README.md").exists(),
            "the remote's content is present"
        );

        // … a second pick reuses the SAME checkout (idempotent — no error, no
        // re-clone). Drop a marker and confirm it survives.
        let marker = first.join("REUSED_MARKER");
        std::fs::write(&marker, "x").unwrap();
        let second = ensure_clone(&ainb, remote_str).unwrap();
        assert_eq!(second, first);
        assert!(
            marker.exists(),
            "existing clone reused as-is, not wiped + re-cloned"
        );
    }

    #[test]
    fn ensure_clone_wipes_a_non_git_leftover_before_cloning() {
        let tmp = tempfile::tempdir().unwrap();
        let ainb = tmp.path().join(".agents-in-a-box");
        let remote = make_bare_remote(tmp.path(), "widget");
        let remote_str = remote.to_str().unwrap();

        // A partial leftover: the dest exists but is not a git checkout.
        let dest = clone_path(&ainb, remote_str);
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("garbage.txt"), "junk").unwrap();

        let path = ensure_clone(&ainb, remote_str).unwrap();
        assert!(
            path.join(".git").exists(),
            "leftover wiped, clean clone landed"
        );
        assert!(
            !path.join("garbage.txt").exists(),
            "the non-git leftover was removed"
        );
    }

    #[test]
    fn ensure_clone_errors_and_leaves_no_partial_on_bad_remote() {
        let tmp = tempfile::tempdir().unwrap();
        let ainb = tmp.path().join(".agents-in-a-box");
        // A local path that is not a repo → git clone fails.
        let bad = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&bad).unwrap();
        let bad_str = bad.to_str().unwrap();

        let err = ensure_clone(&ainb, bad_str);
        assert!(err.is_err(), "cloning a non-repo fails");
        assert!(
            !clone_path(&ainb, bad_str).exists(),
            "a failed clone leaves NO partial checkout behind (retry starts clean)"
        );
    }
}
