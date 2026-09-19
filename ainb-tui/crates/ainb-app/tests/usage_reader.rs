#![allow(missing_docs)]
#![cfg(feature = "test-support")]

// ABOUTME: The usage reader keeps section 21 current from the daemon's
// `fleet/usage_summary` (D3p-e): one read per cadence, `absent` with the reason
// when the daemon does not serve `fleet.usage.read`, the last numbers kept with
// the reason when a read fails, and a scanning summary asked again sooner than
// a ready one, since the daemon refreshes a ready one every fifteen minutes.

use std::time::Duration;

use ainb_app::AppState;
use ainb_app::fleet::agent_status_reader::fake_daemon::{Fake, listen};
use ainb_app::fleet::bridge::daemon::DaemonClient;
use ainb_app::fleet::usage_reader::{Timing, UsageReader};
use ainb_hangar_proto::fleet::FLEET_CAPABILITY_USAGE_READ;
use ainb_hangar_proto::methods::FLEET_USAGE_SUMMARY;
use serde_json::{Value, json};

fn reply(state: &str, input: u64) -> Value {
    json!({"result": {
        "state": state,
        "generated_at": 10,
        "totals": {
            "input_tokens": input, "cache_creation_tokens": 0, "cache_read_tokens": 0,
            "output_tokens": 0, "reasoning_tokens": 0, "call_count": 1,
            "session_count": 1, "project_count": 1
        },
        "daily": [], "providers": [], "models": [], "projects": []
    }})
}

fn serving(answer: impl Fn(&str, usize) -> Value + Send + Sync + 'static) -> Fake {
    Fake {
        capabilities: vec![FLEET_CAPABILITY_USAGE_READ],
        ..Fake::joined(answer)
    }
}

fn fast() -> Timing {
    Timing {
        ready_every: Duration::from_secs(60),
        scanning_every: Duration::from_millis(40),
        // Long beside the 10 ms drain, so the failure is seen before the
        // retry's read replaces it.
        backoff_initial: Duration::from_millis(150),
        backoff_max: Duration::from_millis(300),
    }
}

fn spawn(socket: std::path::PathBuf) -> UsageReader {
    UsageReader::spawn_timed(
        Box::new(move || Ok(DaemonClient::with_parts(socket.clone(), "t".to_string()))),
        fast(),
    )
}

/// Drain until `done` holds for the section's frame, or fail after 5 s.
async fn until(reader: &mut UsageReader, state: &mut AppState, done: impl Fn(&Value) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        reader.drain_into(state);
        let body = ainb_app::wire::section_json(
            state,
            ainb_app::SectionId::Usage,
            &ainb_app::wire::frame::HostId::local(),
        );
        if done(&body) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "section 21 so far: {body}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_serving_daemon_fills_section_21_for_thirty_days() {
    let dir = tempfile::tempdir().unwrap();
    let fake = serving(|_, _| reply("ready", 77));
    let socket = listen(&dir.path().join("d.sock"), {
        let fake = fake.clone();
        move |_| fake.clone()
    });
    let mut reader = spawn(socket);
    let mut state = AppState::new();
    until(&mut reader, &mut state, |body| {
        body["summary"]["totals"]["input_tokens"] == 77
    })
    .await;
    assert_eq!(
        fake.reads_of(FLEET_USAGE_SUMMARY),
        1,
        "one read, then the cadence"
    );
    assert_eq!(fake.calls(), [FLEET_USAGE_SUMMARY]);
}

#[tokio::test]
async fn a_daemon_without_the_capability_leaves_section_21_absent_and_is_not_asked() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake {
        capabilities: Vec::new(),
        ..Fake::joined(|_, _| reply("ready", 1))
    };
    let socket = listen(&dir.path().join("d.sock"), {
        let fake = fake.clone();
        move |_| fake.clone()
    });
    let mut reader = spawn(socket);
    let mut state = AppState::new();
    until(&mut reader, &mut state, |body| {
        body["absent"]
            .as_str()
            .is_some_and(|reason| reason.contains("fleet.usage.read"))
    })
    .await;
    assert_eq!(
        fake.reads_of(FLEET_USAGE_SUMMARY),
        0,
        "a refused read is not tried"
    );
}

#[tokio::test]
async fn a_failed_read_keeps_the_numbers_says_why_and_is_retried() {
    let dir = tempfile::tempdir().unwrap();
    let fake = serving(|_, index| match index {
        1 => reply("scanning", 0),
        2 => json!({"error": {"code": -32000, "message": "store busy"}}),
        _ => reply("ready", 5),
    });
    let socket = listen(&dir.path().join("d.sock"), {
        let fake = fake.clone();
        move |_| fake.clone()
    });
    let mut reader = spawn(socket);
    let mut state = AppState::new();
    until(&mut reader, &mut state, |body| {
        body["summary"]["state"] == "scanning"
    })
    .await;
    until(&mut reader, &mut state, |body| {
        body["failure"].as_str().is_some_and(|reason| reason.contains("store busy"))
    })
    .await;
    until(&mut reader, &mut state, |body| {
        body["summary"]["state"] == "ready" && body["failure"].is_null()
    })
    .await;
    assert_eq!(
        fake.reads_of(FLEET_USAGE_SUMMARY),
        3,
        "scanning is asked again soon"
    );
}

#[tokio::test]
async fn a_daemon_that_is_not_there_is_a_failure_not_a_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut reader = spawn(dir.path().join("missing.sock"));
    let mut state = AppState::new();
    until(&mut reader, &mut state, |body| body["failure"].is_string()).await;
    let body = ainb_app::wire::section_json(
        &state,
        ainb_app::SectionId::Usage,
        &ainb_app::wire::frame::HostId::local(),
    );
    assert!(
        body["summary"].is_null(),
        "no zeros drawn for a missing daemon: {body}"
    );
}
