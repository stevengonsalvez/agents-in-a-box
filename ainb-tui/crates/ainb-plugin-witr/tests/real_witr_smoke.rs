//! Real-binary smoke: exec the actual installed `witr` (not a stub)
//! against PID 1 and prove our [`WitrSnapshot`] model parses whatever
//! the real `witr --json` emits.
//!
//! This is the highest-fidelity test — it catches `witr --json` schema
//! drift that a canned stub never could (e.g. the typed-target
//! addressing bug found during cfx.7, where a bare positional is a
//! *name* and PIDs need `--pid`).
//!
//! **Skips gracefully** when witr isn't on PATH (CI runners without
//! it), so it's safe in `cargo test --workspace`. Run it on a dev
//! machine with `brew install witr` to exercise the real contract.
//!
//! Targets the test's *own* process by PID — always running, always
//! introspectable without elevated privileges (it's our own process),
//! so the real binary reliably returns a parseable snapshot.

#![cfg(unix)]

use ainb_plugin_witr::exec::{ExecResult, WitrTarget, exec_witr_json};

#[tokio::test]
async fn real_witr_self_pid_parses_into_model() {
    let Ok(path) = which::which("witr") else {
        eprintln!("witr not on PATH — skipping real-binary smoke");
        return;
    };

    let self_pid = std::process::id();
    let result = exec_witr_json(&path, &WitrTarget::Pid(self_pid.to_string())).await;

    match result {
        ExecResult::Ok(snap) => {
            // The decisive assertion: real `witr --json` output decoded
            // into our model with the stable-6 fields intact.
            assert_eq!(
                snap.process.pid as u32, self_pid,
                "self PID round-trips through real witr + our model"
            );
            assert!(
                !snap.process.command.is_empty(),
                "self process has a command"
            );
            assert!(
                !snap.resolved_target.is_empty(),
                "resolved_target populated"
            );
            // Our own ancestry should be non-trivial (cargo/test harness
            // → shell → ...), proving Vec<Process> decodes too.
            eprintln!(
                "real witr OK: pid={} cmd={} ancestry_depth={}",
                snap.process.pid,
                snap.process.command,
                snap.ancestry.len()
            );
        }
        // Once the binary is present (we skip above otherwise), scanning
        // our OWN live PID must succeed — `witr --pid <self>` always has a
        // valid target. A non-zero exit, parse failure, or spawn failure
        // here signals a real regression (witr CLI contract drift, schema
        // change, or our exec breaking), so fail loudly instead of staying
        // green. (The skip-when-absent guard already covers hosts without
        // witr; this branch is only reached with the binary installed.)
        other => panic!(
            "real witr --pid {self_pid} (our own live PID) must decode to ExecResult::Ok, got {other:?}"
        ),
    }
}
