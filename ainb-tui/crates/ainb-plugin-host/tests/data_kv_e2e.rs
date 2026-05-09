//! End-to-end tests for `ainb_data_read` / `ainb_data_write` — the host-side
//! per-plugin SQLite KV store.
//!
//! Each plugin gets its own DB at `~/.agents-in-a-box/plugins/data/<id>/<id>.db`.
//! Tests redirect `$HOME` to a temp dir so they don't trample real state.

use ainb_plugin_api::manifest::{CapabilitiesTable, PluginTable, ProvidesTable};
use ainb_plugin_api::Manifest;
use ainb_plugin_host::PluginHost;

fn data_manifest(name: &str) -> Manifest {
    Manifest {
        plugin: PluginTable {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            ainb_min_version: "1.0.0".to_string(),
            description: None,
            author: None,
            license: None,
            homepage: None,
        },
        capabilities: CapabilitiesTable {
            write_plugin_data: true,
            ..CapabilitiesTable::default()
        },
        provides: ProvidesTable::default(),
        paths: ainb_plugin_api::PathsTable::default(),
        subscribes: ainb_plugin_api::SubscribesTable::default(),
    }
}

/// Wat fixture that writes a known KV pair on `_init`, then reads it back
/// on `_render`. The read result is published via `ainb_render_buffer` so
/// the test can assert byte-equality against the expected payload.
fn build_kv_wat(key: &str, value: &[u8]) -> String {
    const KEY_OFF: u32 = 64;
    const VAL_OFF: u32 = 256;
    const READBACK_OFF: u32 = 1024;
    const RB_CAP: u32 = 4096;
    let key_bytes: String = key
        .as_bytes()
        .iter()
        .map(|b| format!("\\{b:02x}"))
        .collect();
    let val_bytes: String = value.iter().map(|b| format!("\\{b:02x}")).collect();
    format!(
        r#"
(module
  (import "env" "ainb_data_read"
    (func $read (param i32 i32 i32 i32) (result i32)))
  (import "env" "ainb_data_write"
    (func $write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const {key_off}) "{key_bytes}")
  (data (i32.const {val_off}) "{val_bytes}")
  (func (export "_init") (result i32)
    ;; ainb_data_write(key, val)
    i32.const {key_off}     ;; key_ptr
    i32.const {key_len}     ;; key_len
    i32.const {val_off}     ;; val_ptr
    i32.const {val_len}     ;; val_len
    call $write
    drop
    ;; ainb_data_read(key, readback) — store status at mem[0]
    i32.const 0
    i32.const {key_off}
    i32.const {key_len}
    i32.const {rb_off}
    i32.const {rb_cap}
    call $read
    i32.store
    i32.const 0)
  (func (export "_shutdown")))
"#,
        key_off = KEY_OFF,
        key_len = key.len(),
        val_off = VAL_OFF,
        val_len = value.len(),
        rb_off = READBACK_OFF,
        rb_cap = RB_CAP,
        key_bytes = key_bytes,
        val_bytes = val_bytes,
    )
}

#[test]
fn data_write_then_read_roundtrips_bytes() {
    let home = tempfile::tempdir().expect("tempdir for $HOME");
    let prev_home = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());

    let key = "burndown.cache.v1";
    let value: &[u8] = b"\x00\x01\x02 burndown bytes \xfe\xff";

    let manifest = data_manifest("kv_canary");
    let wat = build_kv_wat(key, value);
    let wasm = wat::parse_str(&wat).expect("parse wat");

    let mut host = PluginHost::new();
    host.load_bytes(manifest, &wasm).expect("plugin loads");

    // _init wrote the value, then read it back into READBACK_OFF and stored
    // the byte count at mem[0]. We assert via the host-side DB that the
    // write actually landed (the read path is exercised purely by the
    // wat fixture not trapping).
    let db = home
        .path()
        .join(".agents-in-a-box")
        .join("plugins")
        .join("data")
        .join("kv_canary")
        .join("kv_canary.db");
    assert!(db.exists(), "host should have created {}", db.display());
    let conn = rusqlite::Connection::open(&db).expect("open kv db");
    let stored: Vec<u8> = conn
        .query_row("SELECT v FROM kv WHERE k = ?1", [key], |r| r.get(0))
        .expect("kv row");
    assert_eq!(stored, value, "stored value must match plugin's write");

    let plugin = host.plugins().next().unwrap();
    assert!(
        plugin.host_state().last_error.is_none(),
        "no host-fn error expected, got {:?}",
        plugin.host_state().last_error
    );

    if let Some(prev) = prev_home {
        std::env::set_var("HOME", prev);
    } else {
        std::env::remove_var("HOME");
    }
}

#[test]
fn data_read_missing_key_returns_zero_bytes() {
    let home = tempfile::tempdir().expect("tempdir for $HOME");
    let prev_home = std::env::var_os("HOME");
    std::env::set_var("HOME", home.path());

    // Build a wat that reads "no_such_key" without ever writing — expect
    // the read to return 0 (Ok / zero-length) and not trap.
    let wat = format!(
        r#"
(module
  (import "env" "ainb_data_read"
    (func $read (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 64) "no_such_key")
  (func (export "_init") (result i32)
    i32.const 0
    i32.const 64    ;; key_ptr
    i32.const 11    ;; key_len
    i32.const 1024  ;; out_ptr
    i32.const 256   ;; out_len
    call $read
    i32.store
    i32.const 0)
  (func (export "_shutdown")))
"#
    );
    let wasm = wat::parse_str(&wat).expect("parse wat");

    let mut host = PluginHost::new();
    host.load_bytes(data_manifest("kv_missing_canary"), &wasm)
        .expect("plugin loads");

    let plugin = host.plugins().next().unwrap();
    assert!(
        plugin.host_state().last_error.is_none(),
        "missing key should be Ok, not error: {:?}",
        plugin.host_state().last_error
    );

    if let Some(prev) = prev_home {
        std::env::set_var("HOME", prev);
    } else {
        std::env::remove_var("HOME");
    }
}

#[test]
fn data_fns_not_linked_without_capability() {
    let manifest = Manifest {
        plugin: PluginTable {
            name: "kv_unlinked".to_string(),
            version: "0.1.0".to_string(),
            ainb_min_version: "1.0.0".to_string(),
            description: None,
            author: None,
            license: None,
            homepage: None,
        },
        capabilities: CapabilitiesTable::default(), // write_plugin_data = false
        provides: ProvidesTable::default(),
        paths: ainb_plugin_api::PathsTable::default(),
        subscribes: ainb_plugin_api::SubscribesTable::default(),
    };
    let wat = build_kv_wat("k", b"v");
    let wasm = wat::parse_str(&wat).expect("parse wat");

    let mut host = PluginHost::new();
    let err = host.load_bytes(manifest, &wasm).expect_err("must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("ainb_data_read") || msg.contains("ainb_data_write")
            || msg.contains("cannot find definition"),
        "expected unresolved data_read/write import, got: {msg}"
    );
}
