//! Run the external `witr --json <target>` binary and decode its
//! stdout into a [`WitrSnapshot`](crate::model::WitrSnapshot).
//!
//! The exec runs under the plugin's `spawn_subprocess` capability —
//! see `plans/witr-plugin-spec.md` § Security for the threat model.
//! Two guarantees this module owns:
//!
//! 1. **No shell.** We spawn via `tokio::process::Command::new(path)`
//!    with `.arg(target)` — argv array, not `sh -c`. Shell
//!    metacharacters in `target` are passed verbatim to witr, not
//!    interpreted by a shell, so command injection is structurally
//!    impossible.
//!
//! 2. **Defence in depth.** [`validate_target`] still rejects
//!    obviously-pathological inputs (empty, embedded NUL, embedded
//!    newline, length > 256 chars) so a buggy caller can't trigger a
//!    nuisance exec or leak weird strings into logs.
//!
//! The exec is bounded by [`SCAN_TIMEOUT`] (5s, per
//! `plans/witr-plugin-spec.md` § Performance). On timeout the child
//! is dropped — tokio reaps it via `Child::kill()` semantics on
//! drop. No process leak.

use std::path::Path;
use std::time::Duration;

use thiserror::Error;
use tokio::process::Command;
use tokio::time::timeout;

use crate::model::WitrSnapshot;

/// Maximum wall-clock budget for a single `witr --json <target>`
/// exec. Spec § Performance — 5s. Cold-cache `/proc` walks and
/// `libproc` cgo calls on macOS need a generous ceiling; 5s clears
/// the largest reasonable target tree we've measured (~1k procs).
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(5);

/// Soft cap on stdout bytes we'll hold for a parse-error excerpt.
/// Prevents a runaway witr from spilling MBs into our error variant.
const STDOUT_EXCERPT_BYTES: usize = 256;

/// Maximum target length we'll accept. Witr names, container IDs,
/// and file paths fit well under this; longer is almost certainly
/// caller error.
const MAX_TARGET_LEN: usize = 256;

/// Outcome of one `exec_witr_json` call. The render path (cfx.5)
/// matches on these exhaustively — every variant maps to a distinct
/// user-visible state (data / timeout banner / error banner /
/// install hint).
#[derive(Debug)]
pub enum ExecResult {
    /// Witr exited 0 and stdout decoded into a [`WitrSnapshot`].
    ///
    /// Boxed to keep the enum's stack footprint small — the snapshot
    /// carries ~25 fields including several `Vec`/`String`s, so the
    /// `Ok` variant would otherwise dominate the union and force
    /// every error path to pay the same allocation cost.
    Ok(Box<WitrSnapshot>),
    /// `witr --json` ran past [`SCAN_TIMEOUT`] and was reaped.
    Timeout,
    /// Witr exited non-zero. Most common case: target not found
    /// (witr exits 1 with a friendly stderr line).
    NonZero {
        /// Exit code, or `None` if the process was killed by a signal.
        code: Option<i32>,
        /// Trimmed stderr — the render path surfaces this verbatim
        /// in the per-tab error banner.
        stderr: String,
    },
    /// Spawn / I/O error before we could collect stdout (e.g.
    /// permission denied on the witr binary). The diagnostic carries
    /// the underlying error message.
    SpawnFailed(String),
    /// Witr exited 0 but stdout was not valid JSON or didn't match
    /// our [`WitrSnapshot`] schema.
    ParseError {
        /// `serde_json::Error` rendered to text.
        error: String,
        /// First [`STDOUT_EXCERPT_BYTES`] bytes of stdout, for logs.
        raw_stdout_excerpt: String,
    },
}

/// Why a target string was rejected before we tried to spawn witr.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TargetValidationError {
    /// Caller passed an empty string.
    #[error("target is empty")]
    Empty,
    /// Caller passed a string containing a NUL byte. POSIX `argv` is
    /// NUL-terminated so this would silently truncate the arg even
    /// if we passed it through.
    #[error("target contains a NUL byte at offset {0}")]
    EmbeddedNul(usize),
    /// Caller passed a string with an embedded newline. Witr
    /// shouldn't see one; logs would be smeared.
    #[error("target contains a control character (newline / CR / tab)")]
    EmbeddedControl,
    /// Caller passed a string above the [`MAX_TARGET_LEN`] cap.
    #[error("target length {0} exceeds max {1}")]
    TooLong(usize, usize),
}

/// Defence-in-depth validation for the `target` argument we'll pass
/// to `witr`. Argv-array spawning already prevents shell injection
/// (see module docs); this just catches obvious-bug inputs.
pub fn validate_target(target: &str) -> Result<(), TargetValidationError> {
    if target.is_empty() {
        return Err(TargetValidationError::Empty);
    }
    if target.len() > MAX_TARGET_LEN {
        return Err(TargetValidationError::TooLong(target.len(), MAX_TARGET_LEN));
    }
    if let Some(idx) = target.find('\0') {
        return Err(TargetValidationError::EmbeddedNul(idx));
    }
    if target.chars().any(|c| matches!(c, '\n' | '\r' | '\t')) {
        return Err(TargetValidationError::EmbeddedControl);
    }
    Ok(())
}

/// Truncate `s` to at most [`STDOUT_EXCERPT_BYTES`] bytes at a UTF-8
/// char boundary. Used to bound the diagnostic in
/// [`ExecResult::ParseError`] so a runaway witr can't bloat our
/// errors with megabytes of garbage. Only appends `…` when
/// truncation actually fired — an exact-length input that lands on
/// a char boundary returns unchanged.
fn excerpt(s: &str) -> String {
    if s.len() <= STDOUT_EXCERPT_BYTES {
        return s.to_string();
    }
    let mut end = STDOUT_EXCERPT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

/// Run `witr --json <target>` against the binary at `path` and
/// decode its stdout into a [`WitrSnapshot`].
///
/// `path` must come from [`crate::detect::DetectResult::Ready.path`]
/// — we don't re-resolve via `which` here so a manifest-renamed witr
/// can't slip past the detect gate.
///
/// `target` is validated via [`validate_target`] before any process
/// is spawned. Returns [`ExecResult::SpawnFailed`] on a validation
/// error (carrying the validation error message) rather than
/// surfacing a separate error type — the renderer matches one enum.
pub async fn exec_witr_json(path: &Path, target: &str) -> ExecResult {
    if let Err(e) = validate_target(target) {
        return ExecResult::SpawnFailed(format!("invalid target: {e}"));
    }

    // `--` before the target defangs a leading `-` — without it,
    // `witr --json -foo` would have witr's clap parser treat `-foo`
    // as an option flag. Combined with `validate_target`'s rejection
    // of control chars, this nails the last reasonable attack surface
    // a malicious target string could exploit.
    let exec = Command::new(path)
        .arg("--json")
        .arg("--")
        .arg(target)
        .stdin(std::process::Stdio::null())
        .output();

    let output = match timeout(SCAN_TIMEOUT, exec).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::warn!(?target, error = %e, "witr --json spawn failed");
            return ExecResult::SpawnFailed(e.to_string());
        }
        Err(_) => {
            tracing::warn!(
                ?target,
                timeout_secs = SCAN_TIMEOUT.as_secs(),
                "witr --json timed out",
            );
            return ExecResult::Timeout;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let code = output.status.code();
        // `debug!` not `warn!` — "target not found" is witr's normal
        // exit-1 path and a user will hit it constantly. The render
        // banner surfaces the stderr text; the runtime log shouldn't
        // shout about it.
        tracing::debug!(?target, code, %stderr, "witr --json non-zero exit");
        return ExecResult::NonZero { code, stderr };
    }

    // Decode straight from bytes so non-UTF-8 stdout surfaces as a
    // serde error rather than being silently smoothed over by
    // `from_utf8_lossy`. Only the error branch pays for the lossy
    // string conversion (for the bounded excerpt).
    match serde_json::from_slice::<WitrSnapshot>(&output.stdout) {
        Ok(snap) => ExecResult::Ok(Box::new(snap)),
        Err(e) => {
            tracing::warn!(?target, error = %e, "witr --json output failed to decode");
            let lossy = String::from_utf8_lossy(&output.stdout);
            ExecResult::ParseError {
                error: e.to_string(),
                raw_stdout_excerpt: excerpt(&lossy),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_target_accepts_ordinary_names() {
        assert!(validate_target("nginx").is_ok());
        assert!(validate_target("1234").is_ok());
        assert!(validate_target("5432").is_ok());
        assert!(validate_target("/var/run/foo.pid").is_ok());
        assert!(validate_target("container-name_v2.0").is_ok());
    }

    #[test]
    fn validate_target_rejects_empty() {
        assert_eq!(validate_target(""), Err(TargetValidationError::Empty));
    }

    #[test]
    fn validate_target_rejects_embedded_nul() {
        let with_nul = "foo\0bar";
        match validate_target(with_nul) {
            Err(TargetValidationError::EmbeddedNul(3)) => {}
            other => panic!("expected EmbeddedNul(3), got {other:?}"),
        }
    }

    #[test]
    fn validate_target_rejects_newline() {
        assert_eq!(
            validate_target("foo\nbar"),
            Err(TargetValidationError::EmbeddedControl),
        );
        assert_eq!(
            validate_target("foo\rbar"),
            Err(TargetValidationError::EmbeddedControl),
        );
        assert_eq!(
            validate_target("foo\tbar"),
            Err(TargetValidationError::EmbeddedControl),
        );
    }

    #[test]
    fn validate_target_rejects_overlong() {
        let long = "x".repeat(MAX_TARGET_LEN + 1);
        match validate_target(&long) {
            Err(TargetValidationError::TooLong(len, max)) => {
                assert_eq!(len, MAX_TARGET_LEN + 1);
                assert_eq!(max, MAX_TARGET_LEN);
            }
            other => panic!("expected TooLong, got {other:?}"),
        }
    }

    #[test]
    fn validate_target_accepts_at_length_cap() {
        let at_cap = "x".repeat(MAX_TARGET_LEN);
        assert!(validate_target(&at_cap).is_ok());
    }

    #[test]
    fn validate_target_argv_safe_metacharacters_pass() {
        // Shell-meaningful chars are passed through unchanged — the
        // argv-array spawn means there's no shell to interpret them.
        // We rely on `Command::arg()` for safety; validation is
        // defense in depth, not the load-bearing check.
        assert!(validate_target("foo;bar").is_ok());
        assert!(validate_target("foo|bar").is_ok());
        assert!(validate_target("foo`bar").is_ok());
        assert!(validate_target("foo$bar").is_ok());
        assert!(validate_target("foo&bar").is_ok());
    }

    #[test]
    fn excerpt_truncates_at_char_boundary() {
        let huge = "x".repeat(1_000);
        let e = excerpt(&huge);
        assert!(
            e.len() <= STDOUT_EXCERPT_BYTES + '…'.len_utf8(),
            "len {}",
            e.len(),
        );
        assert!(e.ends_with('…'));
    }

    #[test]
    fn excerpt_passes_short_input_unchanged() {
        assert_eq!(excerpt("hello"), "hello");
    }

    #[test]
    fn excerpt_at_exact_cap_does_not_append_ellipsis() {
        let at_cap = "x".repeat(STDOUT_EXCERPT_BYTES);
        let e = excerpt(&at_cap);
        assert_eq!(e, at_cap, "exact-length input must not be truncated");
        assert!(!e.ends_with('…'));
    }

    #[test]
    fn excerpt_does_not_falsely_truncate_multibyte_input_below_cap() {
        // 3-byte glyph repeated to land BELOW the cap. The boundary
        // walk on `STDOUT_EXCERPT_BYTES` could otherwise shrink `end`
        // past the input length and append `…` to data we didn't
        // actually truncate.
        let s: String = "é".repeat(STDOUT_EXCERPT_BYTES / 4);
        assert!(s.len() < STDOUT_EXCERPT_BYTES);
        let e = excerpt(&s);
        assert_eq!(e, s, "below-cap multi-byte input must not be truncated");
        assert!(!e.ends_with('…'));
    }
}
