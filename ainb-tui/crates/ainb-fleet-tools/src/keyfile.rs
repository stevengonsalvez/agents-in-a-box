//! Where the daemon token comes from, and where it must never come from.
//!
//! This process is a CHILD of an ACP adapter, which is a child of the daemon.
//! Its argv is visible to every process on the box (`ps`), and its environment
//! is inherited by anything it spawns and printed by half the crash reporters in
//! existence. So the token arrives exactly one way: read at startup from a
//! private file the daemon already writes at `0600`
//! ([`ainb_hangar_proto::auth::default_token_file`]).
//!
//! The env vars this module reads carry PATHS, never secrets. A token found in
//! the environment is a hard error rather than a fallback: silently ignoring it
//! would leave a real token sitting in a real environment, which is the bug.

use std::path::{Path, PathBuf};

use ainb_hangar_client::DaemonClient;
use ainb_hangar_proto::auth;

/// Path override for the daemon socket (a PATH, not a secret).
pub const SOCKET_ENV: &str = "AINB_FLEET_TOOLS_SOCKET";

/// Path override for the daemon token file (a PATH, not a secret).
pub const TOKEN_FILE_ENV: &str = "AINB_FLEET_TOOLS_TOKEN_FILE";

/// Environment variables that would carry the token ITSELF. Their presence is
/// refused, never used.
pub const BANNED_TOKEN_ENV: &[&str] = &[
    "AINB_FLEET_TOOLS_TOKEN",
    "AINB_HANGAR_TOKEN",
    "AINB_DAEMON_TOKEN",
];

/// Why this process cannot talk to the daemon.
#[derive(Debug, thiserror::Error)]
pub enum KeyfileError {
    /// The hangar home (and so the socket / token path) is unresolvable.
    #[error("hangar home is not resolvable; set {SOCKET_ENV} and {TOKEN_FILE_ENV}")]
    NoHome,
    /// The token file could not be read.
    #[error("token file {path}: {source}")]
    Unreadable {
        /// The path that failed.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The token file is readable by somebody other than its owner.
    #[error("token file {path} is mode {mode:04o}; it must be 0600 (owner-only)")]
    Permissive {
        /// The offending path.
        path: String,
        /// Its permission bits.
        mode: u32,
    },
    /// The token file exists but holds nothing usable.
    #[error("token file {path} is empty")]
    Empty {
        /// The offending path.
        path: String,
    },
    /// A token was handed to this process through its environment.
    #[error("{name} is set: the daemon token must arrive in a 0600 file, never the environment")]
    TokenInEnvironment {
        /// The offending variable.
        name: &'static str,
    },
    /// A token was (or might have been) handed to this process through argv.
    #[error("this server takes no arguments: the daemon token must never reach argv")]
    Arguments,
}

/// Resolve socket + token from the environment, then build the ONE daemon
/// client the tools use.
pub fn client_from_env() -> Result<DaemonClient, KeyfileError> {
    if let Some(name) = banned_token_env(|name| std::env::var_os(name).is_some()) {
        return Err(KeyfileError::TokenInEnvironment { name });
    }
    let socket = match std::env::var_os(SOCKET_ENV) {
        Some(path) => PathBuf::from(path),
        None => ainb_hangar_client::socket_path().ok_or(KeyfileError::NoHome)?,
    };
    let token_file = match std::env::var_os(TOKEN_FILE_ENV) {
        Some(path) => PathBuf::from(path),
        None => auth::default_token_file().ok_or(KeyfileError::NoHome)?,
    };
    Ok(DaemonClient::with_parts(socket, read_token(&token_file)?))
}

/// The first banned variable `is_set` reports, if any.
///
/// Split out from [`client_from_env`] so the rule is testable without mutating
/// the process environment.
pub(crate) fn banned_token_env<F: Fn(&str) -> bool>(is_set: F) -> Option<&'static str> {
    BANNED_TOKEN_ENV.iter().copied().find(|name| is_set(name))
}

/// Read a token from a private file, refusing anything a second user could read.
pub fn read_token(path: &Path) -> Result<String, KeyfileError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = std::fs::metadata(path).map_err(|source| KeyfileError::Unreadable {
            path: path.display().to_string(),
            source,
        })?;
        let mode = metadata.permissions().mode() & 0o777;
        // Group or other bits set means the secret is not private any more.
        // Refusing beats repairing: whoever loosened it should find out.
        if mode & 0o077 != 0 {
            return Err(KeyfileError::Permissive {
                path: path.display().to_string(),
                mode,
            });
        }
    }
    let token = std::fs::read_to_string(path)
        .map_err(|source| KeyfileError::Unreadable {
            path: path.display().to_string(),
            source,
        })?
        .trim()
        .to_string();
    if token.is_empty() {
        return Err(KeyfileError::Empty {
            path: path.display().to_string(),
        });
    }
    Ok(token)
}

/// Refuse any argv beyond the program name.
///
/// The server is configured entirely by the two PATH variables above, so an
/// argument can only be someone trying to pass a token (or a typo). Both are
/// worth failing loudly.
pub fn reject_arguments<I: IntoIterator<Item = String>>(args: I) -> Result<(), KeyfileError> {
    if args.into_iter().nth(1).is_some() {
        return Err(KeyfileError::Arguments);
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn token_file(mode: u32, contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("daemon.token");
        std::fs::write(&path, contents).expect("write token");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            .expect("set token mode");
        (dir, path)
    }

    #[test]
    fn a_private_token_file_is_read_and_trimmed() {
        let (_dir, path) = token_file(0o600, "mdt_SECRET\n");
        assert_eq!(read_token(&path).expect("read"), "mdt_SECRET");
    }

    #[test]
    fn a_group_readable_token_file_is_refused() {
        let (_dir, path) = token_file(0o640, "mdt_SECRET");
        assert!(matches!(
            read_token(&path),
            Err(KeyfileError::Permissive { mode: 0o640, .. })
        ));
    }

    #[test]
    fn a_world_readable_token_file_is_refused() {
        let (_dir, path) = token_file(0o644, "mdt_SECRET");
        assert!(matches!(
            read_token(&path),
            Err(KeyfileError::Permissive { .. })
        ));
    }

    #[test]
    fn an_empty_token_file_is_refused() {
        let (_dir, path) = token_file(0o600, "   \n");
        assert!(matches!(read_token(&path), Err(KeyfileError::Empty { .. })));
    }

    #[test]
    fn a_token_in_the_environment_is_refused_not_used() {
        assert_eq!(
            banned_token_env(|name| name == "AINB_HANGAR_TOKEN"),
            Some("AINB_HANGAR_TOKEN")
        );
        assert_eq!(banned_token_env(|_| false), None);
    }

    #[test]
    fn a_token_on_argv_is_refused() {
        assert!(matches!(
            reject_arguments(["ainb-fleet-tools".to_string(), "mdt_SECRET".to_string()]),
            Err(KeyfileError::Arguments)
        ));
        assert!(reject_arguments(["ainb-fleet-tools".to_string()]).is_ok());
    }
}
