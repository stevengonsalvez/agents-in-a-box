//! Manifest sanity checks the host runs before instantiation.
//!
//! Validation runs *after* deserialisation succeeds — the `serde` step already
//! enforces the schema; this module catches semantic errors:
//! * version-string syntax (must parse as semver-ish `MAJOR.MINOR.PATCH`)
//! * `ainb_min_version` not greater than the host's own version
//! * empty `name` / `version` strings
//! * filesystem allowlist patterns aren't absolute (`/etc/...`) escapes

use ainb_plugin_api::Manifest;
use thiserror::Error;

/// The version of the host this build of `ainb-plugin-host` ships against.
///
/// Bumped when the host-fn ABI changes incompatibly. Plugins whose
/// `ainb_min_version` is greater than this string are rejected at load.
///
/// Phase 6 ABI bump: 1.1.0 → 1.2.0 to advertise the new host fns
/// (`ainb_fs_read_dir`, `ainb_fs_read_file`, `ainb_cache_get`,
/// `ainb_cache_put`, `ainb_request_data`). Plugins that need the new
/// surface declare `ainb_min_version = "1.2.0"`. Older plugins
/// (existing CTS canaries) keep `1.1.0` and stay compatible.
pub const HOST_VERSION: &str = "1.2.0";

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ValidateError {
    #[error("plugin name is empty")]
    EmptyName,
    #[error("plugin version is empty")]
    EmptyVersion,
    #[error("plugin version {0:?} is not in MAJOR.MINOR.PATCH form")]
    BadVersion(String),
    #[error("ainb_min_version {0:?} is not in MAJOR.MINOR.PATCH form")]
    BadMinVersion(String),
    #[error("plugin requires host >= {required:?} but this host is {host:?}")]
    HostTooOld { required: String, host: String },
    #[error("filesystem allowlist contains escape path: {0:?}")]
    EscapingFsPath(String),
}

pub fn validate(m: &Manifest) -> Result<(), ValidateError> {
    if m.plugin.name.trim().is_empty() {
        return Err(ValidateError::EmptyName);
    }
    if m.plugin.version.trim().is_empty() {
        return Err(ValidateError::EmptyVersion);
    }
    let plugin_v = parse_semver(&m.plugin.version)
        .ok_or_else(|| ValidateError::BadVersion(m.plugin.version.clone()))?;
    let _ = plugin_v;

    let min_v = parse_semver(&m.plugin.ainb_min_version)
        .ok_or_else(|| ValidateError::BadMinVersion(m.plugin.ainb_min_version.clone()))?;
    let host_v = parse_semver(HOST_VERSION).expect("HOST_VERSION is always valid");
    if min_v > host_v {
        return Err(ValidateError::HostTooOld {
            required: m.plugin.ainb_min_version.clone(),
            host: HOST_VERSION.to_string(),
        });
    }

    for pat in &m.capabilities.filesystem {
        if pat.contains("..") {
            return Err(ValidateError::EscapingFsPath(pat.clone()));
        }
    }
    Ok(())
}

/// Tiny semver parser. We only care about ordering for `ainb_min_version`
/// vs `HOST_VERSION`, so prerelease/build tags are ignored.
fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let core = s.split('-').next()?;
    let core = core.split('+').next()?;
    let mut it = core.split('.');
    let maj = it.next()?.parse().ok()?;
    let min = it.next()?.parse().ok()?;
    let pat = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((maj, min, pat))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_api::manifest::{CapabilitiesTable, PluginTable, ProvidesTable};

    fn mf(version: &str, min: &str) -> Manifest {
        Manifest {
            plugin: PluginTable {
                name: "x".into(),
                version: version.into(),
                ainb_min_version: min.into(),
                description: None,
                author: None,
                license: None,
                homepage: None,
            },
            capabilities: CapabilitiesTable::default(),
            provides: ProvidesTable::default(),
        }
    }

    #[test]
    fn accepts_a_well_formed_manifest() {
        let m = mf("0.1.0", "1.0.0");
        assert!(validate(&m).is_ok());
    }

    #[test]
    fn rejects_empty_name() {
        let mut m = mf("0.1.0", "1.0.0");
        m.plugin.name = String::new();
        assert_eq!(validate(&m), Err(ValidateError::EmptyName));
    }

    #[test]
    fn rejects_bad_version() {
        let m = mf("not-a-version", "1.0.0");
        assert!(matches!(validate(&m), Err(ValidateError::BadVersion(_))));
    }

    #[test]
    fn rejects_when_host_too_old() {
        let m = mf("0.1.0", "999.0.0");
        assert!(matches!(validate(&m), Err(ValidateError::HostTooOld { .. })));
    }

    #[test]
    fn rejects_escaping_fs_path() {
        let mut m = mf("0.1.0", "1.0.0");
        m.capabilities.filesystem = vec!["~/foo/../bar".into()];
        assert!(matches!(validate(&m), Err(ValidateError::EscapingFsPath(_))));
    }
}
