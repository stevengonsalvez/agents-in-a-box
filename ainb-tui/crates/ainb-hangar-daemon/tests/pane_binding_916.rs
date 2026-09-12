//! Issue #916 — a hook that cannot see `$TMUX_PANE` still lands one attributed
//! row, or says `pane_unbound`, and never leaves a duplicate legacy row.
//!
//! Codex 0.154 attaches its interactive TUI to a shared app-server and runs
//! hooks in THAT process's environment, which predates the pane. The hook
//! therefore has no `$TMUX_PANE` and `ainb fleet atc hook` writes
//! `tmux_target: null` (`atc.rs::current_tmux_identity` returns `None` without
//! the variable). Measured live: 1,215 of 1,215 sampled lines carried a null.
//!
//! The env loss is reproduced exactly as the spec's gate names it — the real
//! `plugins/ainb-hooks/hooks/notify.sh` is executed under `env -i HOME=<fixture>`
//! with a Codex payload — and the managed branch's `ainb` is a fixture binary
//! that appends the canonical line `build_event_line_for_agent` produces in
//! that environment. The daemon-side binding is then exercised through the real
//! ingest path, `AttentionIngest::ingest_once`, not a hand-built observation.

use ainb_fleet_core::types::{
    AttentionState, Capabilities, Confidence, FleetSession, LifecycleState, ManagementState,
    Provider, SessionKey, TransportHealth,
};
use std::collections::BTreeSet;
use ainb_hangar_daemon::attention_ingest::AttentionIngest;
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_proto::fleet::PaneBinding;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::fleet::FleetRepo;

const PROVIDER: &str = "codex";
const SESSION_ID: &str = "cx-916";
const CWD: &str = "/w/app";

/// The repo-root path of the hook script under test.
fn notify_sh() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../plugins/ainb-hooks/hooks/notify.sh")
        .canonicalize()
        .expect("notify.sh is in the repo")
}

/// Stand up a fixture `$HOME` whose `hooks/ainb-bin` is a script that appends
/// the canonical `events.jsonl` line for the hook it was handed.
///
/// The line's `tmux_target` and `process_start_fingerprint` are null, which is
/// what the real command emits with no `$TMUX_PANE` in its environment — that
/// null IS issue #916, so the fixture must not paper over it.
fn plant_fixture_home(home: &std::path::Path) {
    let hooks = home.join(".agents-in-a-box/hooks");
    std::fs::create_dir_all(&hooks).expect("create fixture hooks dir");
    let bin = home.join("fixture-ainb");
    std::fs::write(
        &bin,
        r#"#!/usr/bin/env bash
# Fixture stand-in for `ainb fleet atc hook`, reduced to the one thing this
# test is about: the canonical events.jsonl append, with the null tmux
# identity a hook with no $TMUX_PANE produces.
set -u
session_id=""; cwd=""; event=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --session-id) session_id="$2"; shift 2 ;;
    --cwd) cwd="$2"; shift 2 ;;
    --event) event="$2"; shift 2 ;;
    *) shift ;;
  esac
done
cat > /dev/null
printf '{"event_id":"e-916","ts":1700000000000,"session_id":"%s","cwd":"%s","transcript_path":"","agent":"codex","event_type":"%s","matcher":null,"parent":null,"tmux_target":null,"process_start_fingerprint":null,"payload":{"session_id":"%s","cwd":"%s"}}\n' \
  "$session_id" "$cwd" "$event" "$session_id" "$cwd" \
  >> "${HOME}/.agents-in-a-box/events.jsonl"
"#,
    )
    .expect("write fixture ainb");
    let mut perms = std::fs::metadata(&bin).expect("stat fixture ainb").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&bin, perms).expect("chmod fixture ainb");
    std::fs::write(hooks.join("ainb-bin"), format!("{}\n", bin.display()))
        .expect("record fixture ainb path");
}

/// Run the real hook script with the environment stripped to `HOME`, exactly
/// as a hook forked from a long-lived provider daemon sees it.
fn fire_hook_under_env_i(home: &std::path::Path) {
    let payload = format!(
        r#"{{"type":"request_user_input","session_id":"{SESSION_ID}","cwd":"{CWD}","payload":{{}}}}"#
    );
    let status = std::process::Command::new("env")
        .arg("-i")
        .arg(format!("HOME={}", home.display()))
        .arg("AINB_AGENT=codex")
        .arg("AINB_NOTIFY_DISABLE_LAZY_SPAWN=1")
        .arg("bash")
        .arg(notify_sh())
        .arg(payload)
        .status()
        .expect("run notify.sh");
    assert!(status.success(), "the hook must always exit 0");
    assert!(
        home.join(".agents-in-a-box/events.jsonl").exists(),
        "the managed branch must have appended a canonical line"
    );
    let line = std::fs::read_to_string(home.join(".agents-in-a-box/events.jsonl"))
        .expect("read events.jsonl");
    assert!(
        line.contains(r#""tmux_target":null"#),
        "the premise of #916: no $TMUX_PANE means no target — {line}"
    );
    assert!(
        line.contains(SESSION_ID) && line.contains(CWD),
        "identity survives env loss even though the pane does not — {line}"
    );
}

/// Seed one tier-5 discovered pane row: what the tmux scan writes for a pane it
/// found running `provider` in `cwd`, with no hook behind it.
async fn plant_discovered_pane(store: &Store, target: &str, cwd: &str) {
    let session = FleetSession {
        session_key: SessionKey::legacy(Provider::Codex, target, "pane=%1;pid=1;started=1"),
        provider: Provider::Codex,
        provider_session_id: None,
        cwd: cwd.to_string(),
        exact_tmux_target: Some(target.to_string()),
        pane_pid: Some(1),
        process_start_fingerprint: Some("pane=%1;pid=1;started=1".to_string()),
        lifecycle: LifecycleState::Running,
        attention: AttentionState::None,
        management: ManagementState::Degraded,
        capabilities: Capabilities::default(),
        provenance: BTreeSet::new(),
        confidence: Confidence::Inferred,
        transport_health: TransportHealth::Healthy,
        first_seen_ms: Some(1),
        last_seen_ms: Some(1),
        version: 1,
    };
    ainb_hangar_daemon::fleet::reconcile_discovered_panes(
        store.pool(),
        &EventBroker::new().sink(),
        vec![session],
        1,
        ainb_hangar_daemon::fleet::ReconcilePass::Panes,
    )
    .await
    .expect("seed discovered pane");
}

/// Drive the fixture's `events.jsonl` through the real ingest.
async fn ingest(store: &Store, home: &std::path::Path) {
    let ingest = AttentionIngest::new(
        store.pool().clone(),
        EventBroker::new().sink(),
        home.join(".agents-in-a-box/events.jsonl"),
        home.join(".agents-in-a-box/attention.cursor"),
    );
    ingest.ingest_once(1_700_000_001_000).await;
}

/// Every visible row the store holds, as `(session_key, pane_binding)`.
async fn visible_rows(store: &Store) -> Vec<(String, PaneBinding)> {
    let snapshot = ainb_hangar_daemon::fleet::snapshot_wire(store.pool())
        .await
        .expect("snapshot");
    let mut rows: Vec<_> = snapshot
        .sessions
        .into_iter()
        .map(|s| (s.session_key, s.pane_binding))
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// Exactly one pane matches: the hook row takes that pane, and the discovered
/// row it came from is retired rather than left standing beside it.
#[tokio::test]
async fn one_matching_pane_produces_one_attributed_row() {
    let home = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(home.path()).await.expect("open store");
    plant_fixture_home(home.path());
    plant_discovered_pane(&store, "dev:1.0", CWD).await;

    fire_hook_under_env_i(home.path());
    ingest(&store, home.path()).await;

    let rows = visible_rows(&store).await;
    assert_eq!(rows.len(), 1, "one agent must be one row, got {rows:?}");
    let (key, binding) = &rows[0];
    assert_eq!(key, &format!("{PROVIDER}:{SESSION_ID}"));
    assert_eq!(*binding, PaneBinding::Bound, "the pane must be attributed");
    let row = FleetRepo::get_session(store.pool(), key)
        .await
        .expect("read row")
        .expect("the hook row exists");
    assert_eq!(row.tmux_target.as_deref(), Some("dev:1.0"));
}

/// No pane matches: the row is honest about it rather than binding something.
#[tokio::test]
async fn no_matching_pane_produces_one_pane_unbound_row() {
    let home = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(home.path()).await.expect("open store");
    plant_fixture_home(home.path());
    // A pane for the same provider in a DIFFERENT directory must not be taken.
    plant_discovered_pane(&store, "dev:9.0", "/w/other").await;

    fire_hook_under_env_i(home.path());
    ingest(&store, home.path()).await;

    let rows = visible_rows(&store).await;
    let hook = rows
        .iter()
        .find(|(key, _)| key == &format!("{PROVIDER}:{SESSION_ID}"))
        .expect("the hook row exists");
    assert_eq!(hook.1, PaneBinding::PaneUnbound);
    // The unrelated pane is untouched: it was never a candidate, so nothing
    // retired it and nothing duplicated it.
    assert_eq!(rows.len(), 2, "{rows:?}");
}

/// Two panes match: binding either one would type an answer into an agent that
/// never asked, so neither is taken and no legacy row is retired.
#[tokio::test]
async fn two_matching_panes_produce_one_pane_unbound_row_and_retire_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(home.path()).await.expect("open store");
    plant_fixture_home(home.path());
    plant_discovered_pane(&store, "dev:1.0", CWD).await;
    plant_discovered_pane(&store, "dev:2.0", CWD).await;

    fire_hook_under_env_i(home.path());
    ingest(&store, home.path()).await;

    let rows = visible_rows(&store).await;
    let hook = rows
        .iter()
        .find(|(key, _)| key == &format!("{PROVIDER}:{SESSION_ID}"))
        .expect("the hook row exists");
    assert_eq!(hook.1, PaneBinding::PaneUnbound);
    assert_eq!(
        rows.len(),
        3,
        "an ambiguous binding retires nothing — {rows:?}"
    );
}

/// The same line replayed (a daemon restart re-reading the cursor) must not
/// double-apply: idempotency is what keeps a retired legacy row retired.
#[tokio::test]
async fn replaying_the_same_hook_line_leaves_one_row() {
    let home = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(home.path()).await.expect("open store");
    plant_fixture_home(home.path());
    plant_discovered_pane(&store, "dev:1.0", CWD).await;

    fire_hook_under_env_i(home.path());
    ingest(&store, home.path()).await;
    std::fs::remove_file(home.path().join(".agents-in-a-box/attention.cursor")).ok();
    ingest(&store, home.path()).await;

    let rows = visible_rows(&store).await;
    assert_eq!(rows.len(), 1, "a replay must not fork the row — {rows:?}");
}
