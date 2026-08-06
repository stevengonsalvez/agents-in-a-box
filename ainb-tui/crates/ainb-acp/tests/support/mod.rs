//! Shared harness for the fake-adapter tests.
//!
//! DISCLOSURE: everything here drives the SCRIPTED FIXTURE adapter
//! (`src/bin/fake_acp_adapter.rs`), never a real one. The real-adapter probes
//! live in `tests/real_adapter.rs` behind `#[ignore]` and an env gate.

use std::path::{Path, PathBuf};

use ainb_acp::config::{AdapterConfig, CLAUDE_ADAPTER};

/// Path to the compiled fixture adapter.
pub fn fake_adapter() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake_acp_adapter"))
}

/// Repo path of a checked-in ndjson fixture.
#[allow(dead_code)] // used by store_writer_fake_adapter only
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// A config pointing at the fixture adapter with `mode` pinned.
pub fn fake_config(mode: &str, script_env: Vec<(String, String)>) -> AdapterConfig {
    AdapterConfig::new(CLAUDE_ADAPTER, mode)
        .command(fake_adapter())
        .extra_env(script_env)
}

/// `("NAME", "value")` pairs, spelled once.
pub fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

/// Collect every notification the adapter has emitted, waiting out the
/// dispatch loop rather than racing it with `try_recv`.
pub async fn drain_notifications(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<
        agent_client_protocol::schema::v1::SessionNotification,
    >,
) -> Vec<agent_client_protocol::schema::v1::SessionNotification> {
    let mut collected = Vec::new();
    while let Ok(Some(notification)) =
        tokio::time::timeout(std::time::Duration::from_millis(250), rx.recv()).await
    {
        collected.push(notification);
    }
    collected
}
