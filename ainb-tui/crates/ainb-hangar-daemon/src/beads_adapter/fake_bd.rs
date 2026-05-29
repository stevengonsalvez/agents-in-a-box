//! Test fixtures: a fake `bd` binary as a shell script (P2.2 REFACTOR).
//!
//! The P2.2 adapter tests — and the outbound/inbound sync tests in P2.3+ — drive
//! [`super::BdClient`] against a tiny shell-script stand-in for the real `bd`
//! rather than the live binary, so the suite is hermetic and fast. This module
//! centralises the script writer so every test file uses one source of truth for
//! the fake's wire shape.
//!
//! # Wire shape fidelity
//!
//! The fixtures mirror `bd 0.49.0`'s **per-verb** JSON shape, because the adapter
//! parses each shape differently:
//!
//! - `bd create` emits a bare JSON **object** (`{...}`), with no `labels` field.
//! - `bd close`/`bd list`/`bd show` emit a JSON **array** (`[{...}]`).
//!
//! A fixture that wrapped `create` output in `[...]` (as an earlier revision did)
//! would mask a guaranteed runtime parse failure against the real binary, so the
//! object/array split here is load-bearing — keep it aligned with reality.
//!
//! Gated on `#[cfg(any(test, feature = "test-support"))]`: available to this
//! crate's own unit tests, and to downstream integration tests that enable the
//! `test-support` feature (integration tests do not inherit `#[cfg(test)]`).

use std::fs;
use std::path::{Path, PathBuf};

/// Write an executable `#!/bin/sh` script at `dir/name` and return its path.
pub fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake-bd script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path).expect("stat script").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod script");
    }
    path
}

/// One issue as a bare JSON **object**, mirroring `bd create --json`.
///
/// `bd create` does not echo labels back, so this object omits the `labels`
/// field (the adapter defaults it to empty). It carries `owner` (the bd account)
/// alongside `assignee` to mirror the real payload — the adapter ignores `owner`.
#[must_use]
pub fn one_issue_object(id: &str, title: &str, status: &str) -> String {
    format!(
        r#"{{"id":"{id}","title":"{title}","status":"{status}","assignee":"stevie","owner":"steven.gonsalvez@gmail.com","updated_at":"2026-05-29T12:00:00Z"}}"#
    )
}

/// One issue wrapped in a single-element JSON **array**, mirroring
/// `bd close`/`bd list`/`bd show --json` (which echo `labels`).
#[must_use]
pub fn one_issue_array(id: &str, title: &str, status: &str) -> String {
    format!("[{}]", one_issue_with_labels(id, title, status))
}

/// One issue object including `labels`, as `list`/`show`/`close` emit it.
#[must_use]
pub fn one_issue_with_labels(id: &str, title: &str, status: &str) -> String {
    one_issue_with_labels_at(id, title, status, "2026-05-29T12:00:00Z")
}

/// One issue object including `labels`, with an explicit `updated_at`.
///
/// The reconcile conflict resolver (P2.5) keys last-writer-wins on the `bd`-side
/// `updated_at` versus the mapping's `last_synced`, so its tests need to control
/// that timestamp per issue rather than relying on the fixed default.
#[must_use]
pub fn one_issue_with_labels_at(id: &str, title: &str, status: &str, updated_at: &str) -> String {
    format!(
        r#"{{"id":"{id}","title":"{title}","status":"{status}","assignee":"stevie","owner":"steven.gonsalvez@gmail.com","labels":["foo","bar"],"updated_at":"{updated_at}"}}"#
    )
}

/// Write a fake `bd` that ignores its args and echoes one happy-path issue as a
/// bare **object** (the `bd create` shape), exit 0. Returns the script path to
/// pass to [`super::BdClient::new`].
pub fn happy(dir: &Path, id: &str, title: &str, status: &str) -> PathBuf {
    write_script(
        dir,
        "fake-bd.sh",
        &format!(
            "cat <<'JSON'\n{}\nJSON\nexit 0",
            one_issue_object(id, title, status)
        ),
    )
}

/// Write a fake `bd` that ignores its args and echoes the given issues as a JSON
/// **array** (the `bd list`/`show`/`close` shape), exit 0.
///
/// Each tuple is `(id, title, status)`; an empty slice yields `[]`. This is the
/// fixture the inbound-sync tests (P2.4) use to model `bd list --json`: the poll
/// loop reads whatever the array says and reconciles the mapped Hangar issues.
pub fn listing(dir: &Path, issues: &[(&str, &str, &str)]) -> PathBuf {
    let body: Vec<String> = issues
        .iter()
        .map(|(id, title, status)| one_issue_with_labels(id, title, status))
        .collect();
    let json = format!("[{}]", body.join(","));
    write_script(
        dir,
        "fake-bd.sh",
        &format!("cat <<'JSON'\n{json}\nJSON\nexit 0"),
    )
}

/// Write a fake `bd` whose `list` echoes the given issues — each carrying an
/// explicit `updated_at` — as a JSON **array**, exit 0.
///
/// Each tuple is `(id, title, status, updated_at)`; an empty slice yields `[]`.
/// This is the fixture the reconcile tests (P2.5) use: the conflict resolver
/// compares each issue's `updated_at` against the mapping's `last_synced` to
/// decide the last writer.
pub fn listing_with_updated(dir: &Path, issues: &[(&str, &str, &str, &str)]) -> PathBuf {
    let body: Vec<String> = issues
        .iter()
        .map(|(id, title, status, updated)| {
            one_issue_with_labels_at(id, title, status, updated)
        })
        .collect();
    let json = format!("[{}]", body.join(","));
    write_script(
        dir,
        "fake-bd.sh",
        &format!("cat <<'JSON'\n{json}\nJSON\nexit 0"),
    )
}

/// Write a fake `bd` that prints `stderr_msg` to standard error and exits 1,
/// modelling a non-zero `bd` exit (the adapter maps this to [`super::BdError::Cli`]).
///
/// Used by the outbound-sync failure-mode test (P2.3) to prove a `bd` failure is
/// surfaced as a non-fatal sync error without corrupting Hangar state.
pub fn failing(dir: &Path, stderr_msg: &str) -> PathBuf {
    write_script(dir, "fake-bd.sh", &format!("echo '{stderr_msg}' >&2\nexit 1"))
}

/// Write a fake `bd` that records every argument it received (one per line) to
/// `argv_probe`, then echoes a happy-path **object**, exit 0.
///
/// This is the fixture the argv round-trip tests use: a passing test then asserts
/// `argv_probe` contains the verb, flags, and values the adapter was supposed to
/// emit — proving the command construction reaches `bd`, not merely that JSON
/// parses.
pub fn capturing(dir: &Path, argv_probe: &Path, id: &str, title: &str, status: &str) -> PathBuf {
    write_script(
        dir,
        "fake-bd.sh",
        &format!(
            "printf '%s\\n' \"$@\" > '{probe}'\ncat <<'JSON'\n{json}\nJSON\nexit 0",
            probe = argv_probe.display(),
            json = one_issue_object(id, title, status)
        ),
    )
}
