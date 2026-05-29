//! `SyncEngine::apply_to_home` tripwire — bead v12.D.2.
//!
//! Seeds a sandbox tool home and a mock Fetcher returning fixed bytes,
//! then verifies that applying a `SyncDirection::ToHome` action lands
//! the bytes at the mapped home path. Re-running the same action is a
//! no-op (idempotent — same content, no error).

use std::path::{Path, PathBuf};

use ainb_skill_core::manifest::{SourceEntry, TargetMapping};
use ainb_skill_core::sync::{
    apply_to_home, ContentFetcher, FetchError, SyncAction, SyncDirection,
};

/// Mock that hands back fixed bytes for a single (ref, repo-rel-path)
/// pair and counts calls so the idempotency test can assert that
/// re-applying does *not* trigger a redundant fetch only when the
/// content has not changed — i.e. that the executor's contract is
/// "write file with these bytes" regardless of cache.
struct MockFetcher {
    expected_ref: String,
    expected_path: PathBuf,
    bytes: Vec<u8>,
    calls: std::cell::Cell<usize>,
}

impl MockFetcher {
    fn new(expected_ref: &str, expected_path: &str, bytes: &[u8]) -> Self {
        Self {
            expected_ref: expected_ref.to_string(),
            expected_path: PathBuf::from(expected_path),
            bytes: bytes.to_vec(),
            calls: std::cell::Cell::new(0),
        }
    }
}

impl ContentFetcher for MockFetcher {
    fn fetch_content(&self, ref_name: &str, repo_path: &Path) -> Result<Vec<u8>, FetchError> {
        self.calls.set(self.calls.get() + 1);
        assert_eq!(ref_name, self.expected_ref, "unexpected ref");
        assert_eq!(repo_path, self.expected_path, "unexpected repo path");
        Ok(self.bytes.clone())
    }
}

fn fake_source() -> SourceEntry {
    SourceEntry {
        name: "skills-src".to_string(),
        kind: Some("gh".to_string()),
        uri: "gh:owner/repo".to_string(),
        r#ref: "main".to_string(),
        enabled: true,
        read_only: false,
        target_layout: vec![TargetMapping {
            glob: "skills/*/SKILL.md".to_string(),
            home: PathBuf::from(".claude/skills"),
            repo: PathBuf::from("skills"),
        }],
    }
}

#[test]
fn apply_to_home_writes_fetched_bytes_to_mapped_path() {
    let tool_home = tempfile::tempdir().expect("tmpdir");
    let bytes = b"---\nname: commit\n---\nbody\n";
    let fetcher = MockFetcher::new("main", "skills/commit/SKILL.md", bytes);

    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::ToHome,
        reason: "repo updated upstream since deploy; home still at deployed sha".into(),
    };
    let source = fake_source();
    let unit_path = PathBuf::from("skills/commit/SKILL.md");

    apply_to_home(&action, tool_home.path(), &source, &unit_path, &fetcher).expect("apply");

    let landed = tool_home.path().join(".claude/skills/commit/SKILL.md");
    assert!(landed.exists(), "file must land at mapped home path");
    let on_disk = std::fs::read(&landed).expect("read landed");
    assert_eq!(on_disk, bytes, "bytes must round-trip");
    assert_eq!(fetcher.calls.get(), 1, "exactly one fetch call");
}

#[test]
fn apply_to_home_is_idempotent_on_unchanged_bytes() {
    let tool_home = tempfile::tempdir().expect("tmpdir");
    let bytes = b"body bytes\n";
    let fetcher = MockFetcher::new("main", "skills/commit/SKILL.md", bytes);

    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::ToHome,
        reason: "fetch new".into(),
    };
    let source = fake_source();
    let unit_path = PathBuf::from("skills/commit/SKILL.md");

    apply_to_home(&action, tool_home.path(), &source, &unit_path, &fetcher).expect("first apply");
    apply_to_home(&action, tool_home.path(), &source, &unit_path, &fetcher).expect("second apply");

    let landed = tool_home.path().join(".claude/skills/commit/SKILL.md");
    let on_disk = std::fs::read(&landed).expect("read landed");
    assert_eq!(on_disk, bytes, "second apply must leave the same bytes");
}

#[test]
fn apply_to_home_skips_non_to_home_directions() {
    let tool_home = tempfile::tempdir().expect("tmpdir");
    let fetcher = MockFetcher::new("main", "skills/commit/SKILL.md", b"data");

    let action = SyncAction {
        unit_name: "commit".into(),
        direction: SyncDirection::NoOp,
        reason: "already in sync".into(),
    };
    let source = fake_source();
    let unit_path = PathBuf::from("skills/commit/SKILL.md");

    apply_to_home(&action, tool_home.path(), &source, &unit_path, &fetcher).expect("noop");

    let landed = tool_home.path().join(".claude/skills/commit/SKILL.md");
    assert!(!landed.exists(), "NoOp must not write anything");
    assert_eq!(fetcher.calls.get(), 0, "NoOp must not call fetcher");
}

#[test]
fn apply_to_home_errors_when_unit_path_not_in_layout() {
    let tool_home = tempfile::tempdir().expect("tmpdir");
    let fetcher = MockFetcher::new("main", "skills/commit/SKILL.md", b"x");

    let action = SyncAction {
        unit_name: "rogue".into(),
        direction: SyncDirection::ToHome,
        reason: "fetch new".into(),
    };
    let source = fake_source();
    let unit_path = PathBuf::from("rogue/path.md"); // not covered by `skills/*/SKILL.md`

    let err = apply_to_home(&action, tool_home.path(), &source, &unit_path, &fetcher).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("layout") || err.to_string().contains("mapping"),
        "got: {err}"
    );
}
