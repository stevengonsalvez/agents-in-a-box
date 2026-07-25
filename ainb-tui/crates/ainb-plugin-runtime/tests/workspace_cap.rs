//! P5.5 — `host/workspace_*` cap integration: switch state + event emission.
//!
//! These tests exercise the workspace switch logic directly through the
//! testable seams (`set_active_logic`, `set_default_logic`, `get_active_logic`)
//! against a real `state.toml`-backed `StateTomlWorkspaceStore` in a tempdir, so
//! the full write → read-back → broadcast path is covered without a plugin
//! subprocess. The cap gate itself (`workspace:write` → `-32001`) is covered in
//! the protocol/handler unit tests; here we pin the spec's four switch RED
//! tests:
//!
//! 1. `set_active_updates_state_toml` — write → read-back.
//! 2. `set_active_emits_workspace_changed_event` — subscriber gets it <200ms.
//! 3. `default_workspace_used_when_active_not_set`.
//! 4. `set_default_does_not_change_active` — independence.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_proto::events::HangarEvent;
use ainb_plugin_protocol::errors;
use ainb_plugin_protocol::errors::RpcError;
use ainb_plugin_protocol::manifest::CapabilityGrant;
use ainb_plugin_protocol::params::{
    WorkspaceCreateResult, WorkspaceDeleteResult, WorkspaceGetActiveResult, WorkspaceListResult,
};
use ainb_plugin_runtime::workspace_store::{
    StateTomlWorkspaceStore, WorkspaceCatalogueMutator, WorkspaceInfo, create_logic, delete_logic,
    get_active_logic, list_logic, read_switch_state_at, set_active_logic, set_default_logic,
};

/// A granted `workspace:write` cap (bool-true form).
const fn write_grant() -> CapabilityGrant {
    CapabilityGrant::Bool(true)
}

/// Two-workspace catalogue keyed by ULID id (slug is display-only).
fn catalogue() -> Vec<WorkspaceInfo> {
    vec![
        WorkspaceInfo {
            id: "01ID_DEFAULT".into(),
            slug: "default".into(),
            name: "Default".into(),
        },
        WorkspaceInfo {
            id: "01ID_ACME".into(),
            slug: "acme".into(),
            name: "Acme".into(),
        },
    ]
}

fn store_in(dir: &std::path::Path) -> StateTomlWorkspaceStore {
    StateTomlWorkspaceStore::new(dir.join("hangar").join("state.toml"), catalogue())
}

/// RED 1: switching the active workspace persists `active_workspace` to
/// `state.toml` and reads back identically.
#[test]
fn set_active_updates_state_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("hangar").join("state.toml");
    let store = StateTomlWorkspaceStore::new(path.clone(), catalogue());

    set_active_logic(&write_grant(), &store, "01ID_ACME").expect("set_active");

    // Read the file straight back (independent of the store's cache).
    let state = read_switch_state_at(&path).expect("read state.toml");
    assert_eq!(state.active.as_deref(), Some("01ID_ACME"));

    // And the get-active cap reflects it.
    let v = get_active_logic(&store);
    let res: WorkspaceGetActiveResult = serde_json::from_value(v).unwrap();
    assert_eq!(res.workspace_id.as_deref(), Some("01ID_ACME"));
}

/// RED 2: a subscribed plugin receives `WorkspaceChanged { from, to }` within
/// 200ms of the switch.
#[test]
fn set_active_emits_workspace_changed_event() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_in(tmp.path());
    let mut rx = store.subscribe();

    // First activation: no prior active → from is None (default resolves to
    // the first catalogue entry, so `from` = the effective prior active).
    set_active_logic(&write_grant(), &store, "01ID_ACME").expect("set_active");

    let ev = rx
        .try_recv()
        .or_else(|_| {
            // Allow a brief settle for the broadcast to land.
            std::thread::sleep(Duration::from_millis(50));
            rx.try_recv()
        })
        .expect("WorkspaceChanged must arrive within 200ms");
    match ev {
        HangarEvent::WorkspaceChanged { from, to } => {
            // Effective prior active was the first workspace (no explicit set).
            assert_eq!(from.as_deref(), Some("01ID_DEFAULT"));
            assert_eq!(to, "01ID_ACME");
        }
        other => panic!("expected WorkspaceChanged, got {other:?}"),
    }
}

/// RED 3: with no explicit active set, the default workspace is used as the
/// effective active.
#[test]
fn default_workspace_used_when_active_not_set() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_in(tmp.path());

    set_default_logic(&write_grant(), &store, "01ID_ACME").expect("set_default");

    // No active was ever set → effective active is the default.
    let v = get_active_logic(&store);
    let res: WorkspaceGetActiveResult = serde_json::from_value(v).unwrap();
    assert_eq!(res.workspace_id.as_deref(), Some("01ID_ACME"));

    // And the list marks acme active + default, default-row not active.
    let lv = list_logic(&store);
    let list: WorkspaceListResult = serde_json::from_value(lv).unwrap();
    let acme = list.workspaces.iter().find(|w| w.id == "01ID_ACME").unwrap();
    assert!(acme.active, "acme is the effective active via default");
    assert!(acme.default, "acme is the default");
    let def = list.workspaces.iter().find(|w| w.id == "01ID_DEFAULT").unwrap();
    assert!(!def.active);
    assert!(!def.default);
}

/// RED 4: setting the default never changes the active workspace.
#[test]
fn set_default_does_not_change_active() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("hangar").join("state.toml");
    let store = StateTomlWorkspaceStore::new(path.clone(), catalogue());

    // Make acme the explicit active.
    set_active_logic(&write_grant(), &store, "01ID_ACME").expect("set_active");
    // Now set the DEFAULT to the other workspace.
    set_default_logic(&write_grant(), &store, "01ID_DEFAULT").expect("set_default");

    let state = read_switch_state_at(&path).unwrap();
    assert_eq!(
        state.active.as_deref(),
        Some("01ID_ACME"),
        "active must be untouched by a default change"
    );
    assert_eq!(state.default.as_deref(), Some("01ID_DEFAULT"));

    // Effective active is still the explicit active, not the new default.
    let v = get_active_logic(&store);
    let res: WorkspaceGetActiveResult = serde_json::from_value(v).unwrap();
    assert_eq!(res.workspace_id.as_deref(), Some("01ID_ACME"));
}

/// `state.toml` foreign sections survive a switch write (atomic, merge-on-save).
#[test]
fn set_active_preserves_foreign_state_toml_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("hangar");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("state.toml");
    // Pre-seed a foreign section + a foreign top-level key.
    std::fs::write(
        &path,
        "warnings_ack = [\"danger-full-access\"]\n\n[other_plugin]\nfoo = \"bar\"\n",
    )
    .unwrap();

    let store = StateTomlWorkspaceStore::new(path.clone(), catalogue());
    set_active_logic(&write_grant(), &store, "01ID_ACME").expect("set_active");

    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("active_workspace"),
        "active key written:\n{raw}"
    );
    assert!(
        raw.contains("warnings_ack"),
        "foreign top-level key preserved:\n{raw}"
    );
    assert!(
        raw.contains("[other_plugin]") && raw.contains("foo"),
        "foreign section preserved:\n{raw}"
    );
}

/// The `workspace:write` cap gate denies an ungranted plugin with `-32001`
/// BEFORE the store is written (no `state.toml` side effect).
#[test]
fn set_active_without_capability_gets_32001() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("hangar").join("state.toml");
    let store = StateTomlWorkspaceStore::new(path.clone(), catalogue());

    let denied = CapabilityGrant::default(); // Bool(false)
    let err = set_active_logic(&denied, &store, "01ID_ACME")
        .expect_err("ungranted set_active must be denied");
    assert_eq!(err.code, errors::CAPABILITY_DENIED);
    // No file was written (the gate ran before any IO).
    assert!(!path.exists(), "denied switch must not write state.toml");

    // set_default is gated identically.
    let err = set_default_logic(&denied, &store, "01ID_ACME")
        .expect_err("ungranted set_default must be denied");
    assert_eq!(err.code, errors::CAPABILITY_DENIED);
}

/// An unknown workspace id is rejected with `-32602` (after the cap gate).
#[test]
fn set_active_unknown_id_is_invalid_params() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_in(tmp.path());
    let err = set_active_logic(&write_grant(), &store, "01ID_GHOST")
        .expect_err("unknown id must be rejected");
    assert_eq!(err.code, errors::INVALID_PARAMS);
}

// ---- create / delete cap logic (multica gap #4) ----

/// An in-memory `WorkspaceCatalogueMutator` double: mints deterministic ids,
/// rejects a slug it already knows with `-32602` (mirrors the store's `SlugTaken`),
/// and tracks created/deleted ids so a test can assert the delegation.
#[derive(Default)]
struct FakeMutator {
    known_slugs: Mutex<Vec<String>>,
}

impl FakeMutator {
    fn with_existing(slugs: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            known_slugs: Mutex::new(slugs.iter().map(|s| (*s).to_string()).collect()),
        })
    }
}

impl WorkspaceCatalogueMutator for FakeMutator {
    fn create(&self, slug: &str, name: &str) -> Result<WorkspaceInfo, RpcError> {
        let mut known = self.known_slugs.lock().unwrap();
        if known.iter().any(|s| s == slug) {
            return Err(RpcError::invalid_params("slug taken"));
        }
        known.push(slug.to_string());
        Ok(WorkspaceInfo {
            id: format!("01ID_{}", slug.to_uppercase()),
            slug: slug.to_string(),
            name: name.to_string(),
        })
    }

    fn delete(&self, id: &str) -> Result<(), RpcError> {
        // Map the deterministic id back to a slug and forget it.
        self.known_slugs
            .lock()
            .unwrap()
            .retain(|s| format!("01ID_{}", s.to_uppercase()) != id);
        Ok(())
    }
}

fn store_with_mutator(dir: &std::path::Path, mutator: Arc<FakeMutator>) -> StateTomlWorkspaceStore {
    StateTomlWorkspaceStore::new(dir.join("hangar").join("state.toml"), catalogue())
        .with_mutator(mutator)
}

/// A denied `workspace:write` grant blocks create/delete with `-32001` before any
/// store hit.
#[test]
fn create_delete_denied_without_capability() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_mutator(tmp.path(), FakeMutator::with_existing(&["default", "acme"]));
    let denied = CapabilityGrant::default();

    let err = create_logic(&denied, &store, "beta", "Beta").expect_err("create denied");
    assert_eq!(err.code, errors::CAPABILITY_DENIED);
    let err = delete_logic(&denied, &store, "01ID_ACME").expect_err("delete denied");
    assert_eq!(err.code, errors::CAPABILITY_DENIED);
}

/// Create appends the new workspace to the catalogue; `list_logic` shows it with
/// `active`/`default` false.
#[test]
fn create_appends_and_lists_new_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_mutator(tmp.path(), FakeMutator::with_existing(&["default", "acme"]));

    let v = create_logic(&write_grant(), &store, "beta", "Beta").expect("create");
    let res: WorkspaceCreateResult = serde_json::from_value(v).unwrap();
    assert_eq!(res.workspace.slug, "beta");
    assert!(!res.workspace.active, "a fresh workspace is never active");
    assert!(!res.workspace.default);

    let list: WorkspaceListResult = serde_json::from_value(list_logic(&store)).unwrap();
    assert!(
        list.workspaces.iter().any(|w| w.slug == "beta"),
        "the new workspace shows in the list"
    );
}

/// A duplicate slug surfaces the store's rejection as `-32602`.
#[test]
fn create_duplicate_slug_surfaces_invalid_params() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_mutator(tmp.path(), FakeMutator::with_existing(&["default", "acme"]));
    let err = create_logic(&write_grant(), &store, "acme", "Acme II")
        .expect_err("duplicate slug rejected");
    assert_eq!(err.code, errors::INVALID_PARAMS);
}

/// Delete removes the workspace from the catalogue.
#[test]
fn delete_removes_workspace_from_catalogue() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_mutator(tmp.path(), FakeMutator::with_existing(&["default", "acme"]));

    // acme is not the effective-active (default is first), so it can be deleted.
    let v = delete_logic(&write_grant(), &store, "01ID_ACME").expect("delete");
    let _: WorkspaceDeleteResult = serde_json::from_value(v).unwrap();

    let list: WorkspaceListResult = serde_json::from_value(list_logic(&store)).unwrap();
    assert!(
        !list.workspaces.iter().any(|w| w.id == "01ID_ACME"),
        "the deleted workspace is gone from the list"
    );
}

/// Deleting the effective-active workspace is refused with `-32602` (switch away
/// first), before any store mutation.
#[test]
fn delete_active_workspace_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_with_mutator(tmp.path(), FakeMutator::with_existing(&["default", "acme"]));

    // No explicit active → effective active is the first workspace (default).
    let err = delete_logic(&write_grant(), &store, "01ID_DEFAULT")
        .expect_err("deleting the active workspace must be refused");
    assert_eq!(err.code, errors::INVALID_PARAMS);
    // Still present — nothing was mutated.
    let list: WorkspaceListResult = serde_json::from_value(list_logic(&store)).unwrap();
    assert!(list.workspaces.iter().any(|w| w.id == "01ID_DEFAULT"));
}
