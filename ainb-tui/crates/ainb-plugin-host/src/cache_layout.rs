//! On-disk layout for installed plugins + a global flock for concurrent
//! install protection.
//!
//! Layout (rooted at `~/.agents-in-a-box/plugins/`):
//!
//! ```text
//! plugins/
//! ├── .lock                            # fs2 advisory lock — one install at a time
//! ├── installed.toml                   # see marketplace::Lockfile
//! ├── marketplaces/
//! │   └── <marketplace>/marketplace.json
//! ├── cache/
//! │   └── <marketplace>/<plugin>/<version>/
//! │       ├── plugin.toml
//! │       └── plugin.wasm
//! └── data/
//!     └── <plugin>/                    # plugin-owned writable storage
//! ```
//!
//! `$AINB_HOME` overrides the home root for tests/CI; everything below is
//! computed relative to it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs2::FileExt;

/// Resolve the install root. Honours `$AINB_HOME` for tests; otherwise uses
/// `<user-home>/.agents-in-a-box`. Returns `None` only when neither is
/// available — practically never on a real machine, but lets tests assert
/// on the missing-home path without panicking.
#[must_use]
pub fn ainb_home() -> Option<PathBuf> {
    if let Ok(env) = std::env::var("AINB_HOME") {
        return Some(PathBuf::from(env));
    }
    dirs::home_dir().map(|h| h.join(".agents-in-a-box"))
}

/// Plugins root: `<ainb_home>/plugins/`.
#[must_use]
pub fn plugins_root() -> Option<PathBuf> {
    ainb_home().map(|h| h.join("plugins"))
}

/// Path to the marketplaces dir.
#[must_use]
pub fn marketplaces_dir() -> Option<PathBuf> {
    plugins_root().map(|p| p.join("marketplaces"))
}

/// Path to the cache dir (where installed `plugin.wasm` files live).
#[must_use]
pub fn cache_dir() -> Option<PathBuf> {
    plugins_root().map(|p| p.join("cache"))
}

/// Path to the per-plugin writable data dir.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    plugins_root().map(|p| p.join("data"))
}

/// Path to the lockfile (`installed.toml`).
#[must_use]
pub fn lockfile_path() -> Option<PathBuf> {
    plugins_root().map(|p| p.join("installed.toml"))
}

/// Path inside the marketplaces dir for a given marketplace.
#[must_use]
pub fn marketplace_dir(name: &str) -> Option<PathBuf> {
    marketplaces_dir().map(|p| p.join(name))
}

/// Concrete on-disk path for a given marketplace's catalog.
#[must_use]
pub fn marketplace_json(name: &str) -> Option<PathBuf> {
    marketplace_dir(name).map(|p| p.join("marketplace.json"))
}

/// Cache dir path for a specific plugin@version installed from a
/// marketplace.
#[must_use]
pub fn plugin_cache_dir(marketplace: &str, plugin: &str, version: &str) -> Option<PathBuf> {
    cache_dir().map(|p| p.join(marketplace).join(plugin).join(version))
}

/// Per-plugin data dir.
#[must_use]
pub fn plugin_data_dir(plugin: &str) -> Option<PathBuf> {
    data_dir().map(|p| p.join(plugin))
}

/// Make sure every dir on the layout exists. Idempotent. Errors only on
/// real filesystem failure (read-only mount, permission denied, …).
pub fn ensure_layout() -> Result<PathBuf> {
    let root =
        plugins_root().context("cannot resolve $AINB_HOME or user home — refusing install")?;
    fs::create_dir_all(&root).with_context(|| format!("create_dir_all {}", root.display()))?;
    for sub in ["marketplaces", "cache", "data"] {
        let d = root.join(sub);
        fs::create_dir_all(&d).with_context(|| format!("create_dir_all {}", d.display()))?;
    }
    Ok(root)
}

/// RAII handle to the install-time global lock. Drop releases the lock.
///
/// The host opens `<plugins-root>/.lock` (creating it if needed) and takes
/// an exclusive flock. Two concurrent `ainb plugin install` invocations
/// will serialise here so the lockfile + cache writes never race.
pub struct InstallLock {
    file: fs::File,
    path: PathBuf,
}

impl InstallLock {
    /// Acquire the global install lock — blocks the current thread until
    /// the lock is granted. For tests / CI use [`try_acquire`].
    pub fn acquire() -> Result<Self> {
        let root = ensure_layout()?;
        Self::acquire_in(&root)
    }

    /// Try once to acquire the global install lock. Returns `Ok(None)` if
    /// another process holds it; `Ok(Some(_))` on success.
    pub fn try_acquire() -> Result<Option<Self>> {
        let root = ensure_layout()?;
        Self::try_acquire_in(&root)
    }

    /// Acquire the install lock under an explicit root. Used by tests so
    /// they don't have to mutate the global `$AINB_HOME` to exercise the
    /// flock semantics.
    pub fn acquire_in(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)
            .with_context(|| format!("create_dir_all {}", root.display()))?;
        let path = root.join(".lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open lockfile {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("flock(EX) {}", path.display()))?;
        Ok(Self { file, path })
    }

    /// Try-acquire variant of [`acquire_in`].
    pub fn try_acquire_in(root: &Path) -> Result<Option<Self>> {
        fs::create_dir_all(root)
            .with_context(|| format!("create_dir_all {}", root.display()))?;
        let path = root.join(".lock");
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("open lockfile {}", path.display()))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Some(Self { file, path })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(anyhow::Error::new(e)),
        }
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        // Best-effort unlock; failures here are unrecoverable for the
        // caller and only matter to the OS, so swallow.
        let _ = self.file.unlock();
        let _ = self.path; // suppress unused warning when feature gates flip
    }
}

/// Substitute `{owner}` / `{repo}` / `{tag}` / `{plugin}` placeholders in
/// a `release_url` template. `repo` may be a full HTTPS URL or `owner/name`
/// — the helper extracts the trailing two segments.
#[must_use]
pub fn fill_release_url(template: &str, repo: &str, tag: &str, plugin: &str) -> String {
    let (owner, repo_name) = parse_owner_repo(repo);
    template
        .replace("{owner}", owner)
        .replace("{repo}", repo_name)
        .replace("{tag}", tag)
        .replace("{plugin}", plugin)
}

fn parse_owner_repo(repo: &str) -> (&str, &str) {
    // Strip a trailing `.git` so `https://github.com/foo/bar.git` round-trips.
    let trimmed = repo.strip_suffix(".git").unwrap_or(repo);
    let segments: Vec<&str> = trimmed
        .trim_end_matches('/')
        .rsplit('/')
        .take(2)
        .collect();
    match segments.as_slice() {
        [name, owner] => (owner, name),
        _ => ("", trimmed),
    }
}

/// Default release-URL template: GitHub releases at `<repo>/<tag>/<plugin>.wasm`.
/// Used when a marketplace entry doesn't carry an explicit `release_url`.
#[must_use]
pub fn default_release_template() -> &'static str {
    "https://github.com/{owner}/{repo}/releases/download/{tag}/{plugin}.wasm"
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn with_home<F: FnOnce(&Path) -> R, R>(f: F) -> R {
        let dir = TempDir::new().expect("tmp");
        std::env::set_var("AINB_HOME", dir.path());
        let r = f(dir.path());
        std::env::remove_var("AINB_HOME");
        r
    }

    #[test]
    fn ensure_layout_creates_subdirs() {
        with_home(|home| {
            ensure_layout().expect("ok");
            assert!(home.join("plugins").is_dir());
            assert!(home.join("plugins/marketplaces").is_dir());
            assert!(home.join("plugins/cache").is_dir());
            assert!(home.join("plugins/data").is_dir());
        });
    }

    #[test]
    fn install_lock_blocks_second_acquire() {
        // Use an explicit root so parallel tests can't trample each other
        // via $AINB_HOME (std::env is process-global).
        let dir = TempDir::new().expect("tmp");
        let _held = InstallLock::acquire_in(dir.path()).expect("first acquires");
        let second = InstallLock::try_acquire_in(dir.path()).expect("call ok");
        assert!(second.is_none(), "second try must observe contention");
    }

    #[test]
    fn install_lock_releases_on_drop() {
        let dir = TempDir::new().expect("tmp");
        {
            let _held = InstallLock::acquire_in(dir.path()).expect("first");
        }
        let after = InstallLock::try_acquire_in(dir.path()).expect("call ok");
        assert!(after.is_some(), "drop should have unlocked");
    }

    #[test]
    fn fill_release_url_substitutes_all_placeholders() {
        let s = fill_release_url(
            default_release_template(),
            "https://github.com/example/repo",
            "v0.2.0",
            "burndown",
        );
        assert_eq!(
            s,
            "https://github.com/example/repo/releases/download/v0.2.0/burndown.wasm"
        );
    }

    #[test]
    fn parse_owner_repo_strips_dot_git() {
        let (o, r) = parse_owner_repo("https://github.com/example/repo.git");
        assert_eq!((o, r), ("example", "repo"));
    }

    #[test]
    fn cache_paths_are_layered_correctly() {
        with_home(|home| {
            let p = plugin_cache_dir("ainb-plugins", "burndown", "0.1.0").expect("home");
            assert_eq!(
                p,
                home.join("plugins/cache/ainb-plugins/burndown/0.1.0")
            );
        });
    }
}
