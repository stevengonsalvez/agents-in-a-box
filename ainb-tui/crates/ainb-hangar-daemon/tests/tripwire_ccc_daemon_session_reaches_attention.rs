//! ccc (agents-in-a-box-3n5) — a daemon-spawned HEADLESS card run's
//! AskUserQuestion reaches the attention pipeline.
//!
//! The defect (proven in the CH3 journey frames): a task session the hangar
//! daemon spawns is INVISIBLE to the fleet layer. `ainb fleet atc hook` gates its
//! `events.jsonl` append on fleet membership (a resolvable parent, a non-empty
//! inbox, or an ATC cwd), and a card run has NONE — so its AskUserQuestion never
//! reaches the daemon's attention ingest, and the control centre shows "0 need
//! you" with the picker open (INV-3 / D11). The fix stamps `AINB_PARENT_SESSION`
//! onto the spawned session's env so the hook's membership gate resolves.
//!
//! ```text
//!  enqueue headless task ─▶ daemon claims ─▶ runner spawns stub `claude`
//!         │                                   (env carries AINB_PARENT_SESSION)
//!         ▼
//!  stub fires `ainb fleet atc hook --event Notification` (as notify.sh does)
//!         │  hook gate resolves ─▶ appends to events.jsonl
//!         ▼
//!  daemon attention ingest tails events.jsonl ─▶ classifies ASK ─▶ attention row
//! ```
//!
//! This tripwire is a genuine regression guard: BEFORE the fix the hook's gate
//! does not resolve for a card run, so nothing is appended and no row is raised —
//! the poll below times out and the test fails. SKIPs cleanly when tmux or the
//! `ainb` binary is absent (the hook can't be fired without it). Follows the
//! `tmux-ui-tripwire` HARD RULES: exact-name kills only, deadline-bounded polls,
//! POSITIVE + NEGATIVE assertions.

// Prose-heavy test docs mention capitalised domain nouns (AskUserQuestion, ATC);
// backticking every one hurts readability. Mirrors `tripwire_p4_common.rs`.
#![allow(clippy::doc_markdown)]

mod tripwire_support;

use std::path::Path;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use tripwire_support::{
    DaemonProcess, ainb_bin, fake_claude_fires_hook, now_ms, plant_ask_transcript, seed_world,
    tmux_available, wait_for_attention_kind, wait_for_db,
};

/// The session id the stub fires the hook under — the attention row's session id.
const HOOK_SID: &str = "ccc-headless-sid";

#[tokio::test]
async fn headless_card_run_hook_raises_an_attention_row() {
    if !tmux_available() {
        eprintln!("tmux not available; skipping ccc attention tripwire");
        return;
    }
    let Some(ainb) = ainb_bin() else {
        eprintln!("ainb binary not built; skipping ccc attention tripwire");
        return;
    };

    // Isolated home: HOME/.agents-in-a-box holds hangar.db + events.jsonl so the
    // daemon's ingest, the hook, and this test all resolve to the same tree.
    let home = tempfile::tempdir().expect("tempdir home");
    let hangar = home.path().join(".agents-in-a-box");
    std::fs::create_dir_all(&hangar).expect("create hangar dir");
    let db_path = hangar.join("hangar.db");

    let pool = open_pool(&db_path).await;
    ainb_hangar_store::apply_migrations(&pool).await.expect("migrate");
    let ids = seed_world(&pool).await;

    // Plant the ASK transcript the stub's hook points the ingest at.
    plant_ask_transcript(home.path());
    let fake = fake_claude_fires_hook(home.path(), &ainb, HOOK_SID);

    let _daemon = DaemonProcess::spawn(
        home.path(),
        &[
            ("HANGAR_DAEMON_RUNTIME_ID", &ids.runtime_id),
            ("HANGAR_CLAUDE_PATH", fake.to_str().unwrap()),
            ("HANGAR_DAEMON_POLL_MS", "200"),
        ],
    );

    // Enqueue a HEADLESS card run (no `mode` → the default headless path).
    let task_id = "task-ccc-headless";
    sqlx::query(
        "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, created_at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(task_id)
    .bind(&ids.workspace_id)
    .bind(&ids.runtime_id)
    .bind(&ids.agent_id)
    .bind(now_ms())
    .execute(&pool)
    .await
    .expect("enqueue task");

    // The run reaches done (the stub exits 0 after firing the hook).
    let _ = wait_for_db(&pool, task_id, "done", Duration::from_secs(30)).await;

    // POSITIVE: the hook's events.jsonl append — resolvable ONLY because the fix
    // stamps AINB_PARENT_SESSION onto the child env — reached the daemon's
    // attention ingest, which classified it ASK and raised a row for the session.
    let kind = wait_for_attention_kind(&pool, HOOK_SID, Duration::from_secs(30)).await;

    let kind = kind.unwrap_or_else(|| {
        panic!(
            "a daemon-spawned headless card run's AskUserQuestion never reached the attention \
             inbox — the hook's fleet-membership gate did not resolve (AINB_PARENT_SESSION missing)"
        )
    });
    assert_eq!(
        kind, "ask_user_question",
        "the raised attention row must be an ASK"
    );

    // NEGATIVE: no row exists for a session that never fired a hook — the gate did
    // not spuriously append for an unrelated session.
    let stray: Option<String> =
        sqlx::query_scalar("SELECT kind FROM attention WHERE session_id = ?")
            .bind("never-fired")
            .fetch_optional(&pool)
            .await
            .expect("query stray");
    assert!(
        stray.is_none(),
        "no attention row must exist for a session that never fired a hook"
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
