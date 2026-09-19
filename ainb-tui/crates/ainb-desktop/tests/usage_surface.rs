//! Section 21, which the stats tab draws (D3p-e): the desktop reads the
//! daemon's `fleet/usage_summary` through the shared usage reader, started on
//! the first subscription that names `usage`, so a renderer that does not
//! subscribe to it costs the daemon no read. Driven against a fake daemon on a
//! scratch socket.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use ainb_app::config::AppConfig;
use ainb_app::fleet::agent_status_reader::Dialer;
use ainb_app::fleet::agent_status_reader::fake_daemon::{Fake, listen};
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Keymap, SectionId};
use ainb_desktop::host::DesktopHost;
use ainb_hangar_proto::fleet::FLEET_CAPABILITY_USAGE_READ;
use ainb_hangar_proto::methods::FLEET_USAGE_SUMMARY;

mod support;

fn summary() -> serde_json::Value {
    serde_json::json!({"result": {
        "state": "ready",
        "totals": {
            "input_tokens": 9, "cache_creation_tokens": 0, "cache_read_tokens": 0,
            "output_tokens": 0, "reasoning_tokens": 0, "call_count": 1,
            "session_count": 1, "project_count": 1
        },
        "daily": [], "providers": [], "models": [], "projects": []
    }})
}

fn dialer(socket: std::path::PathBuf) -> Dialer {
    Box::new(move || {
        Ok(ainb_app::fleet::bridge::daemon::DaemonClient::with_parts(
            socket.clone(),
            "t".to_string(),
        ))
    })
}

#[test]
fn the_reader_starts_on_the_first_usage_subscription_and_the_tick_frames_it() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let _runtime = runtime.enter();
    support::isolated_home();
    let dir = tempfile::tempdir().expect("socket dir");
    let fake = Fake {
        capabilities: vec![FLEET_CAPABILITY_USAGE_READ],
        ..Fake::joined(|_, _| summary())
    };
    let socket = listen(&dir.path().join("hangar.sock"), {
        let fake = fake.clone();
        move |_| fake.clone()
    });
    let framed: Rc<RefCell<Vec<SectionId>>> = Rc::default();
    let sink = Rc::clone(&framed);
    let mut host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Shell]),
        move |batch: FrameBatch| {
            sink.borrow_mut()
                .extend(batch.frames.iter().filter_map(|frame| frame.section_id()));
        },
    )
    .rescanning_every(Duration::from_secs(600))
    .without_attention_poll();
    host.enable_usage(dialer(socket));

    // Nothing subscribes to usage yet, so nothing asks the daemon.
    for _ in 0..10 {
        let _ = host.tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fake.reads_of(FLEET_USAGE_SUMMARY),
        0,
        "no read nothing draws"
    );

    host.subscribe(Subscription::only(&[SectionId::Shell, SectionId::Usage]));
    let deadline = Instant::now() + Duration::from_secs(5);
    while host.state().usage.summary.is_none() {
        assert!(Instant::now() < deadline, "the read never landed");
        let _ = host.tick();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(fake.reads_of(FLEET_USAGE_SUMMARY), 1);
    assert!(
        framed.borrow().iter().filter(|id| **id == SectionId::Usage).count() >= 2,
        "section 21 framed on subscribe and again once the read landed: {:?}",
        framed.borrow()
    );
}
