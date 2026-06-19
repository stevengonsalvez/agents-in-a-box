// ABOUTME: Regression test for deleting orphaned sessions whose worktree is gone.
//
// Reproduces the "deleted session still shows in the UI" bug: a sessions.json
// entry whose `by-session/<uuid>` symlink / worktree no longer exists (e.g. a
// shared worktree removed by a sibling session, or an imported record that never
// had a symlink). The old `remove_session` bailed at the worktree step and never
// purged the store record, so the session reappeared on every reload.

use ainb::interactive::session_manager::{
    InteractiveSessionManager, SessionMetadata, SessionStore,
};
use ainb::models::session::SessionAgentType;
use anyhow::Result;
use chrono::Utc;
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

fn tmux_available() -> bool {
    Command::new("tmux")
        .args(["-V"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Deleting a session whose worktree is already gone must still remove its
/// sessions.json record (otherwise it stays visible in the UI forever).
#[tokio::test]
async fn test_remove_orphaned_session_purges_store_record() -> Result<()> {
    if !tmux_available() {
        eprintln!("Skipping test: tmux not available");
        return Ok(());
    }

    // Isolate both the session store and the worktree base under a temp AINB_HOME
    // so we never touch the real ~/.agents-in-a-box. Safe to set process-wide:
    // this is the only test in its binary, and no other test reads AINB_HOME.
    let home = tempfile::tempdir()?;
    std::env::set_var("AINB_HOME", home.path());

    // Seed an orphan: a store entry with NO matching by-session symlink/worktree.
    let orphan_id = Uuid::new_v4();
    let tmux_name = format!("tmux_orphan-{}", &orphan_id.to_string()[..8]);
    let keep_id = Uuid::new_v4();
    let keep_name = format!("tmux_keep-{}", &keep_id.to_string()[..8]);

    let mut store = SessionStore::default();
    store.upsert(SessionMetadata {
        session_id: orphan_id,
        tmux_session_name: tmux_name.clone(),
        worktree_path: home.path().join(".agents-in-a-box/worktrees/gone_worktree"),
        workspace_name: "orphan-workspace".to_string(),
        created_at: Utc::now(),
        agent_type: SessionAgentType::default(),
        headroom_enabled: false,
        rtk_enabled: false,
    });
    store.upsert(SessionMetadata {
        session_id: keep_id,
        tmux_session_name: keep_name.clone(),
        worktree_path: PathBuf::from("/path/keep"),
        workspace_name: "keep-workspace".to_string(),
        created_at: Utc::now(),
        agent_type: SessionAgentType::default(),
        headroom_enabled: false,
        rtk_enabled: false,
    });
    store.save()?;
    assert_eq!(SessionStore::load().sessions().len(), 2);

    // Act: delete the orphan. The worktree lookup will fail (no symlink), which
    // previously aborted the whole operation before the store cleanup.
    let mut manager = InteractiveSessionManager::new()?;
    let result = manager.remove_session(orphan_id).await;
    assert!(
        result.is_ok(),
        "removing an orphaned session should succeed, got: {result:?}"
    );

    // Assert: orphan record gone, sibling untouched.
    let after = SessionStore::load();
    assert_eq!(
        after.sessions().len(),
        1,
        "orphan record must be purged from sessions.json"
    );
    assert!(
        after.find_by_tmux_name(&tmux_name).is_none(),
        "orphan entry should no longer be present"
    );
    assert!(
        after.find_by_tmux_name(&keep_name).is_some(),
        "unrelated session must be preserved"
    );

    std::env::remove_var("AINB_HOME");
    Ok(())
}
