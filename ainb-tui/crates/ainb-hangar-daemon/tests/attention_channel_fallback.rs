//! Integration: legacy attention rows (no stamped channels) resolve their notify
//! routing at READ time (holistic tcp T5 review).
//!
//! Migration 0037 stamps each attention row's resolved push channels once at emit
//! time. A row raised BEFORE 0037 persists the empty default (`''`); treating that
//! as "no channels" would drop an in-flight ASK from before the upgrade off EVERY
//! push channel. `attention_list` resolves an empty stamp against the live rules
//! now: a legacy ASK regains its phone/web/os routing, a genuinely board-only
//! `waiting` row stays empty, and a row that DOES carry a stamp keeps it verbatim.

use ainb_hangar_core::channel::{Channel, ChannelSet};
use ainb_hangar_daemon::rpc::snapshots::attention_list;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};

async fn seed_workspace(pool: &sqlx::SqlitePool, ws: &str) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(ws)
        .bind(ws)
        .bind(ws)
        .bind(1_000_i64)
        .execute(pool)
        .await
        .expect("seed workspace");
}

/// A legacy (pre-0037) attention row: unstamped channels — exactly the empty
/// `''` the migration's `ADD COLUMN ... DEFAULT ''` left on every pre-existing row.
fn legacy_row(id: &str, kind: AttentionKind, ws: &str) -> NewAttention {
    NewAttention {
        id: id.into(),
        session_id: format!("sess-{id}"),
        cwd: "/w".into(),
        workspace_id: Some(ws.into()),
        kind,
        payload: "{}".into(),
        degraded: false,
        created_at: 1_000,
        raise_transcript: None,
        channels: ChannelSet::NONE,
    }
}

#[tokio::test]
async fn legacy_unstamped_rows_resolve_channels_at_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    seed_workspace(pool, "ws-a").await;

    // A legacy ASK row (unstamped) — before the upgrade this paged the phone.
    AttentionRepo::insert(pool, &legacy_row("ask", AttentionKind::AskUserQuestion, "ws-a"))
        .await
        .unwrap();
    // A legacy waiting row (unstamped) — genuinely board-only, no push.
    AttentionRepo::insert(pool, &legacy_row("wait", AttentionKind::Waiting, "ws-a"))
        .await
        .unwrap();
    // A STAMPED ASK row (phone only) — its stamp must be honoured verbatim, never
    // re-resolved to the fuller default.
    let mut stamped = legacy_row("stamped", AttentionKind::AskUserQuestion, "ws-a");
    stamped.channels = ChannelSet::from_channels([Channel::Phone]);
    AttentionRepo::insert(pool, &stamped).await.unwrap();

    let rows = attention_list(pool, Some("ws-a"), false).await.unwrap();

    let ask = rows.iter().find(|r| r.id == "ask").expect("legacy ask present");
    assert_eq!(
        ask.channels,
        ChannelSet::from_channels([Channel::Phone, Channel::Web, Channel::Os, Channel::Atc]),
        "a legacy ASK resolves its real push channels at read (regains phone, gains atc per 0040)"
    );

    let wait = rows.iter().find(|r| r.id == "wait").expect("legacy waiting present");
    assert!(
        wait.channels.is_empty(),
        "a legacy waiting row stays board-only after the read-time resolve"
    );

    let stamped_row = rows.iter().find(|r| r.id == "stamped").expect("stamped present");
    assert_eq!(
        stamped_row.channels,
        ChannelSet::from_channels([Channel::Phone]),
        "a row WITH a stamp is used verbatim, never re-resolved"
    );
}
