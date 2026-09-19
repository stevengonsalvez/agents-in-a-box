//! The desktop's agent status reads are recorded as the desktop's (#1188): the
//! dialer the window starts the shared reader with announces `desktop` in its
//! hello. Its own test binary: it writes a daemon socket and token under a
//! scratch hangar home, which the host contract asserts nothing touches.

use std::time::{Duration, Instant};

use ainb_app::config::AppConfig;
use ainb_app::fleet::agent_status_reader::fake_daemon::{Fake, listen};
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Keymap, SectionId};
use ainb_desktop::host::{DesktopHost, agent_status_dialer};

mod support;

#[test]
fn the_reader_dials_as_the_desktop() {
    support::isolated_home();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let _runtime = runtime.enter();

    // A daemon where the environment says one is: its socket and token.
    let socket = ainb_hangar_client::socket_path().expect("a hangar home");
    std::fs::create_dir_all(socket.parent().expect("socket dir")).expect("socket dir");
    let token = ainb_hangar_proto::auth::default_token_file().expect("a token path");
    std::fs::create_dir_all(token.parent().expect("token dir")).expect("token dir");
    std::fs::write(&token, "t").expect("token");
    let fake = Fake::joined(|_, _| serde_json::json!({"result": {"rows": [], "read_revision": 1}}));
    let observed = fake.clone();
    listen(&socket, move |_| fake.clone());

    let mut host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::AgentStatus]),
        |_batch: FrameBatch| {},
    )
    .rescanning_every(Duration::from_secs(600))
    .without_attention_poll();
    host.start_agent_status(agent_status_dialer(), false);

    let deadline = Instant::now() + Duration::from_secs(5);
    while observed.hellos.lock().expect("hellos").is_empty() {
        assert!(Instant::now() < deadline, "no hello reached the daemon");
        std::thread::sleep(Duration::from_millis(20));
        let _ = host.tick();
    }
    let hellos = observed.hellos.lock().expect("hellos").clone();
    assert!(
        hellos.iter().all(|hello| hello["surface"]["kind"] == "desktop"),
        "{hellos:?}"
    );
}
