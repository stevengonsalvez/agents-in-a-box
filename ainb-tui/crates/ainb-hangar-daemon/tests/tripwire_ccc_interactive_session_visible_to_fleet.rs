//! ccc (agents-in-a-box-3n5) — a daemon-spawned INTERACTIVE card run is visible
//! to the fleet: it appears in `ainb list` (the session registry) AND its
//! AskUserQuestion reaches the attention pipeline.
//!
//! The defect (proven in the CH3 / CH6 journey frames): a task session the hangar
//! daemon spawns is invisible to the fleet layer on BOTH axes —
//!   1. `discover_from_ainb()` shells `ainb list`, but the session was never in
//!      ainb's `sessions.json` registry, so auto-standup / `atc broadcast` could
//!      not see or target it; and
//!   2. `ainb fleet atc hook` gates its `events.jsonl` append on fleet membership,
//!      which a card run lacked, so its AskUserQuestion never reached attention.
//!
//! The fix: the interactive runner (a) stamps `AINB_PARENT_SESSION` onto the child
//! env (hook membership resolves) and (b) registers the live `tmux_hangar-<id>`
//! session in `sessions.json` — the SAME store `ainb list` reads.
//!
//! ```text
//!  enqueue interactive task ─▶ daemon claims ─▶ runner spawns REAL tmux session
//!         │                                      + registers it in sessions.json
//!         │  ── tripwire asserts session is LIVE + registered + ASK raised ──
//!         ▼
//!  stub fires the hook ─▶ events.jsonl ─▶ ingest ─▶ attention row
//!  release sentinel ─▶ agent exits ─▶ session reaped ─▶ task done
//! ```
//!
//! The agent stub BLOCKS on a `$HOME/interactive-go` sentinel so the "live +
//! registered" assertions are race-free; the tripwire releases it to finish.
//! SKIPs when tmux or the `ainb` binary is absent. Follows the `tmux-ui-tripwire`
//! HARD RULES: exact-name kills only, deadline-bounded polls, POSITIVE + NEGATIVE
//! assertions.

// Prose-heavy test docs mention capitalised domain nouns (AskUserQuestion, ATC);
// backticking every one hurts readability. Mirrors `tripwire_p4_common.rs`.
#![allow(clippy::doc_markdown)]

mod tripwire_support;

use std::path::Path;
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tripwire_support::{
    DaemonProcess, ainb_bin, fake_claude_interactive_fires_hook, now_ms, plant_ask_transcript,
    read_sessions_json, seed_world, tmux_available, tmux_kill_session, tmux_session_live,
    wait_for_attention_kind, wait_for_db,
};

/// The session id the stub fires the hook under.
const HOOK_SID: &str = "ccc-interactive-sid";
/// The enqueued task id — the interactive tmux session name is derived from it.
const TASK_ID: &str = "task-ccc-int";

#[tokio::test]
async fn interactive_card_run_is_registered_and_raises_attention() {
    if !tmux_available() {
        eprintln!("tmux not available; skipping ccc interactive-visibility tripwire");
        return;
    }
    let Some(ainb) = ainb_bin() else {
        eprintln!("ainb binary not built; skipping ccc interactive-visibility tripwire");
        return;
    };

    let home = tempfile::tempdir().expect("tempdir home");
    let hangar = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar).expect("create hangar dir");
    let db_path = hangar.join("hangar.db");

    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    plant_ask_transcript(home.path());
    let fake = fake_claude_interactive_fires_hook(home.path(), &ainb, HOOK_SID);

    let _daemon = DaemonProcess::spawn(
        home.path(),
        &[
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    // Enqueue an INTERACTIVE card run (the `mode = interactive` D6 path).
    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, mode, created_at) \
         VALUES (?, ?, ?, ?, 'interactive', ?)",
    )
    .bind(TASK_ID)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(now_ms())
    .execute(&pool)
    .await
    .expect("enqueue interactive task");

    // The interactive runner mints `tmux_hangar-<task_id>`; wait for it to be live
    // (proves the interactive path — not the headless one — dispatched this run).
    let session_name = format!("tmux_hangar-{TASK_ID}");
    let live_deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_live = false;
    while Instant::now() < live_deadline {
        if tmux_session_live(&session_name) {
            saw_live = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    assert!(
        saw_live,
        "the interactive runner must spawn a REAL tmux session named `{session_name}`"
    );

    // POSITIVE (mechanism b): the live session is registered in `sessions.json` —
    // the SAME store `ainb list` / fleet discover read — so standup / broadcast can
    // now target it. Poll to absorb the spawn→register ordering.
    let reg_deadline = Instant::now() + Duration::from_secs(15);
    let mut registered = None;
    while Instant::now() < reg_deadline {
        if let Some(store) = read_sessions_json(home.path()) {
            let entry = store["sessions"].get(&session_name).cloned();
            if entry.is_some() {
                registered = entry;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    let entry = registered.unwrap_or_else(|| {
        // Best-effort teardown before the failure surfaces.
        tmux_kill_session(&session_name);
        panic!("the interactive session `{session_name}` was never registered in sessions.json (ainb list would not see it)")
    });
    assert_eq!(
        entry["tmux_session_name"], session_name,
        "the registered entry must carry the interactive session name"
    );
    assert_eq!(
        entry["workspace_name"], ids.workspace_slug,
        "the registered entry must carry the owning workspace slug"
    );

    // POSITIVE (mechanism a): the stub's hook reached the daemon's attention
    // ingest, which classified it ASK and raised a row for the session.
    let kind = wait_for_attention_kind(&pool, HOOK_SID, Duration::from_secs(30)).await;
    let kind = kind.unwrap_or_else(|| {
        tmux_kill_session(&session_name);
        panic!("the interactive card run's AskUserQuestion never reached the attention inbox")
    });
    assert_eq!(
        kind, "ask_user_question",
        "the raised attention row must be an ASK"
    );

    // Release the blocked agent so it exits → the daemon reaps the session +
    // finalizes the task.
    std::fs::write(home.path().join("interactive-go"), "go").expect("write release sentinel");
    let _ = wait_for_db(&pool, TASK_ID, "done", Duration::from_secs(30)).await;

    // The interactive tmux session is reaped once the agent exits.
    let reap_deadline = Instant::now() + Duration::from_secs(15);
    let mut reaped = false;
    while Instant::now() < reap_deadline {
        if !tmux_session_live(&session_name) {
            reaped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // Defensive exact-name teardown in case the daemon was slow to reap.
    tmux_kill_session(&session_name);
    assert!(
        reaped,
        "the interactive tmux session `{session_name}` must be reaped after the agent exits"
    );
}

/// Open a WAL sqlite pool at `db_path` (matches the daemon's connection mode).
async fn open_pool(db_path: &Path) -> SqlitePool {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
    SqlitePoolOptions::new().connect_with(opts).await.expect("open pool")
}
