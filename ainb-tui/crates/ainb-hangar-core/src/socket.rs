//! Resolving the Hangar daemon socket a client should dial (D17, amendment 21).
//!
//! The daemon binds `hangar.sock` and publishes `hangar-v<N>.sock` beside it as
//! a SYMLINK, so a client that knows the protocol version can say so and both
//! names still reach one listener, one flock singleton, one inode.
//!
//! ## Why the alias is verified rather than trusted
//!
//! The alias is created best-effort at bind and never re-checked by the daemon.
//! The directory is the operator's own `$HOME`, so every process running as
//! that user can write there, including an agent this daemon itself spawned.
//! Any of them can unlink the alias and listen on the path instead, and the
//! very first frame a client sends to it is the daemon bearer token.
//!
//! So the preference is conditional, and the condition is cheap:
//!
//! ```text
//!   hangar-v<N>.sock is a SYMLINK  and  it resolves to hangar.sock
//!   in the SAME directory                     ──▶ dial the alias
//!   anything else (a socket, a regular file,
//!   a link pointing elsewhere, missing)       ──▶ dial hangar.sock
//! ```
//!
//! This is not a security boundary on its own, a same-uid attacker can also
//! replace `hangar.sock`, and it is not meant to be. It removes the EXTRA
//! surface the versioned alias would otherwise add for free: without the check,
//! a path the daemon may legitimately have failed to create is a path clients
//! prefer, which is the easiest possible thing to squat.

use std::path::{Path, PathBuf};

/// The unversioned socket the daemon always binds.
#[must_use]
pub fn plain_socket_in(home: &Path) -> PathBuf {
    home.join("hangar.sock")
}

/// The versioned alias for `version`.
#[must_use]
pub fn versioned_socket_in(home: &Path, version: u32) -> PathBuf {
    home.join(format!("hangar-v{version}.sock"))
}

/// The socket a client of `version` should dial inside `home`.
///
/// Prefers the versioned alias only when it is a symlink resolving to
/// `hangar.sock` in the same directory; falls back to the plain path otherwise,
/// which is what every client dialled before the alias existed.
#[must_use]
pub fn dial_path_in(home: &Path, version: u32) -> PathBuf {
    let plain = plain_socket_in(home);
    let alias = versioned_socket_in(home, version);
    if alias_is_trustworthy(&alias, &plain) {
        return alias;
    }
    plain
}

/// Whether `alias` is the daemon's own symlink to `plain`.
///
/// `read_link` rather than `canonicalize`: the question is what the link SAYS,
/// and a canonicalising read would follow a chain of links the daemon never
/// wrote. A relative target (which is what the daemon writes) is resolved
/// against the alias's own directory, so a target that climbs out of the home
/// fails the comparison rather than escaping it.
#[must_use]
pub fn alias_is_trustworthy(alias: &Path, plain: &Path) -> bool {
    let Ok(target) = std::fs::read_link(alias) else {
        // Missing, or present but NOT a symlink: a socket or a regular file at
        // that path is precisely the squat this check exists to refuse.
        return false;
    };
    let resolved = if target.is_absolute() {
        target
    } else {
        match alias.parent() {
            Some(dir) => dir.join(target),
            None => return false,
        }
    };
    resolved == plain
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// The daemon's own alias is preferred.
    #[test]
    fn a_symlink_to_the_plain_socket_is_dialled() {
        let dir = tempfile::tempdir().unwrap();
        let plain = plain_socket_in(dir.path());
        std::fs::write(&plain, b"").unwrap();
        let alias = versioned_socket_in(dir.path(), 1);
        std::os::unix::fs::symlink("hangar.sock", &alias).unwrap();
        assert_eq!(dial_path_in(dir.path(), 1), alias);
    }

    /// A real socket squatting the alias path is NOT dialled: that is the
    /// process that would otherwise be handed the daemon token.
    #[test]
    fn a_squatted_alias_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let plain = plain_socket_in(dir.path());
        std::fs::write(&plain, b"").unwrap();
        let alias = versioned_socket_in(dir.path(), 1);
        let _squatter = UnixListener::bind(&alias).unwrap();
        assert_eq!(dial_path_in(dir.path(), 1), plain);
    }

    /// A symlink pointing somewhere else is refused too: the target is what
    /// makes the alias the daemon's, not the fact that it is a link.
    #[test]
    fn an_alias_pointing_elsewhere_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let plain = plain_socket_in(dir.path());
        std::fs::write(&plain, b"").unwrap();
        let alias = versioned_socket_in(dir.path(), 1);
        std::os::unix::fs::symlink("/tmp/somebody-elses.sock", &alias).unwrap();
        assert_eq!(dial_path_in(dir.path(), 1), plain);

        // Including one that climbs out of the home by a relative path.
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink("../elsewhere/hangar.sock", &alias).unwrap();
        assert_eq!(dial_path_in(dir.path(), 1), plain);
    }

    /// No alias at all is the ordinary case on a daemon that could not create
    /// one, and on every daemon that predates them.
    #[test]
    fn a_missing_alias_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let plain = plain_socket_in(dir.path());
        std::fs::write(&plain, b"").unwrap();
        assert_eq!(dial_path_in(dir.path(), 1), plain);
    }
}
