//! Path resolution for the on-disk state under `$AINB_HOME`.
//!
//! Resolution order for [`default_ainb_home`]:
//!
//! 1. `$AINB_HOME` (explicit override).
//! 2. `$XDG_CONFIG_HOME/agents-in-a-box`.
//! 3. `$HOME/.agents-in-a-box`.
//! 4. `./.agents-in-a-box` (last-ditch fallback when no home dir is set).
//!
//! Tests should prefer the path-explicit helpers (`*_in`) so they can
//! point at a tempdir without mutating the process environment.

use std::path::{Path, PathBuf};

/// Resolve the canonical `$AINB_HOME` directory using env-var precedence.
pub fn default_ainb_home() -> PathBuf {
    if let Ok(v) = std::env::var("AINB_HOME") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("agents-in-a-box");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home).join(".agents-in-a-box");
        }
    }
    PathBuf::from(".agents-in-a-box")
}

/// Manifest path for an explicit ainb home directory.
pub fn manifest_path_in(home: &Path) -> PathBuf {
    home.join("manifest.yaml")
}

/// Lockfile path for an explicit ainb home directory.
pub fn lockfile_path_in(home: &Path) -> PathBuf {
    home.join("lock.yaml")
}

/// Cache directory for an explicit ainb home directory.
pub fn cache_dir_in(home: &Path) -> PathBuf {
    home.join("cache")
}

/// Default manifest path (uses [`default_ainb_home`]).
pub fn default_manifest_path() -> PathBuf {
    manifest_path_in(&default_ainb_home())
}

/// Default lockfile path (uses [`default_ainb_home`]).
pub fn default_lockfile_path() -> PathBuf {
    lockfile_path_in(&default_ainb_home())
}

/// Default cache directory (uses [`default_ainb_home`]).
pub fn default_cache_dir() -> PathBuf {
    cache_dir_in(&default_ainb_home())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_helpers_compose_from_home() {
        let home = Path::new("/tmp/fake-ainb-home");
        assert_eq!(
            manifest_path_in(home),
            PathBuf::from("/tmp/fake-ainb-home/manifest.yaml")
        );
        assert_eq!(
            lockfile_path_in(home),
            PathBuf::from("/tmp/fake-ainb-home/lock.yaml")
        );
        assert_eq!(
            cache_dir_in(home),
            PathBuf::from("/tmp/fake-ainb-home/cache")
        );
    }
}
