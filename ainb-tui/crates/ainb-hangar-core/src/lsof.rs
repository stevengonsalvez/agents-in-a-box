//! Parsing helpers for `lsof` output, shared by every ownership proof.
//!
//! Three crates prove "this pid holds this home's socket" before signalling it:
//! the `stop` command in `ainb-core`, the codex reaper in `ainb-hangar-daemon`,
//! and the notifyd reaper in `ainb-plugin-notifyd`. Each shells out to
//! `lsof -a -p <pid> -u <uid> -U -F n` and compares the reported NAME to a path.
//!
//! They had three copies of that comparison, and all three carried the same bug,
//! so the fix lives here once instead.

/// Drop the ` type=STREAM` suffix Linux `lsof` appends to a unix socket's NAME.
///
/// macOS emits the bare path; Linux appends the socket type:
///
/// ```text
/// macOS   n/tmp/x/notify.sock
/// Linux   n/tmp/x/notify.sock type=STREAM
/// ```
///
/// Without this the name never equals the expected path on Linux, so every
/// ownership proof answers "not ours" for every pid. The proofs fail closed, so
/// the result is silent: reaping and `stop` degrade to no-ops on Linux rather
/// than erroring, and only their tests notice.
///
/// The suffix is only stripped when the tail is a socket type `lsof` actually
/// emits. Position alone is not enough: a path through a directory named
/// `odd type=dir` would be truncated at the wrong point, yielding a name that
/// matches nothing, which fails closed and spares a pid we own.
///
/// ```
/// use ainb_hangar_core::lsof::strip_type_suffix;
/// assert_eq!(strip_type_suffix("/tmp/x/n.sock type=STREAM"), "/tmp/x/n.sock");
/// assert_eq!(strip_type_suffix("/tmp/x/n.sock"), "/tmp/x/n.sock");
/// ```
#[must_use]
pub fn strip_type_suffix(name: &str) -> &str {
    const SOCKET_TYPES: [&str; 3] = ["STREAM", "DGRAM", "SEQPACKET"];
    match name.rsplit_once(" type=") {
        Some((path, tail)) if SOCKET_TYPES.contains(&tail) => path,
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::strip_type_suffix;

    /// Both platforms' shapes, so the Linux one cannot regress from a macOS-only
    /// machine. The bug this guards: Linux appends ` type=STREAM`, the name never
    /// matched, and because the proofs fail closed, reaping and `stop` degraded
    /// to silent no-ops on every Linux host.
    #[test]
    fn strips_only_the_suffix_lsof_appends() {
        // Linux
        assert_eq!(
            strip_type_suffix("/tmp/x/notify.sock type=STREAM"),
            "/tmp/x/notify.sock"
        );
        // macOS: bare path, untouched
        assert_eq!(
            strip_type_suffix("/tmp/x/notify.sock"),
            "/tmp/x/notify.sock"
        );
        // Other types Linux reports
        assert_eq!(
            strip_type_suffix("/tmp/x/approve.sock type=DGRAM"),
            "/tmp/x/approve.sock"
        );
        assert_eq!(
            strip_type_suffix("/tmp/x/s.sock type=SEQPACKET"),
            "/tmp/x/s.sock"
        );
        // A path containing the marker: strip only the appended suffix
        assert_eq!(
            strip_type_suffix("/tmp/odd type=dir/notify.sock type=STREAM"),
            "/tmp/odd type=dir/notify.sock"
        );
        // Same path with no suffix: leave it whole
        assert_eq!(
            strip_type_suffix("/tmp/odd type=dir/notify.sock"),
            "/tmp/odd type=dir/notify.sock"
        );
        // Not a socket type: not ours to strip
        assert_eq!(
            strip_type_suffix("/tmp/x/notify.sock type=whatever"),
            "/tmp/x/notify.sock type=whatever"
        );
    }
}
