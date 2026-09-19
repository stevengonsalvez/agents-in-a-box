//! The desktop-native path for the onboarding writes (#1175): the webview
//! cannot reach a write without the shell's own confirmation, the reducer's
//! rows for the same writes stay refused and point at the path, and the
//! writes the tests can run under a scratch home do what they say.

use std::cell::RefCell;
use std::rc::Rc;

use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Chord, CommandId, Intent, Keymap, SectionId};
use ainb_desktop::host::DesktopHost;
use ainb_desktop::intent::{RendererIntent, refused_from_webview};
use ainb_desktop::setup::{DESKTOP_PATH, SetupWrite, desktop_path, status};

mod support;

type Log = Rc<RefCell<Vec<String>>>;

fn host(log: &Log) -> DesktopHost<impl FnMut(FrameBatch)> {
    support::isolated_home();
    let log = Rc::clone(log);
    DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Shell]),
        move |batch: FrameBatch| {
            for frame in batch.frames {
                log.borrow_mut().push(format!("frame {}", frame.section));
            }
        },
    )
}

/// The reducer's rows for the three writes: the dependency install, the
/// tmux config, and the wizard's Next, which finishes telemetry when set up.
fn onboarding_write_rows(keymap: &Keymap) -> Vec<CommandId> {
    let rows: Vec<CommandId> = keymap
        .commands()
        .filter(|(id, _)| {
            let id = id.as_str();
            id.starts_with("onboarding.")
                && (id.ends_with(".install")
                    || id.ends_with(".install_upper")
                    || id.ends_with(".install_config")
                    || id.ends_with(".config_upper"))
        })
        .map(|(id, _)| id)
        .collect();
    assert!(
        rows.len() >= 4,
        "the onboarding install rows exist: {rows:?}"
    );
    rows
}

/// No command the webview can send names a setup write: the keymap has no
/// row for it, so `dispatch` cannot reach `SetupWrite::run`, and the rows
/// that would run the same writes through the reducer are refused.
#[test]
fn the_webview_cannot_name_a_setup_write_and_the_reducer_rows_stay_refused() {
    let keymap = Keymap::defaults();
    let named: Vec<String> = keymap
        .commands()
        .map(|(id, _)| id.as_str().to_string())
        .filter(|id| id.contains("setup_write") || id.contains("setup.write"))
        .collect();
    assert!(
        named.is_empty(),
        "a keymap row names the setup write: {named:?}"
    );

    for id in onboarding_write_rows(&keymap) {
        assert!(
            refused_from_webview(&keymap, &id),
            "`{id}` is refused from the webview"
        );
        let intent = RendererIntent::Command(id.clone(), serde_json::Value::Null);
        // The seam's own check is only the host-authored one; the key-only
        // refusal is the host's, exercised below.
        assert!(
            Intent::try_from(intent).is_ok(),
            "`{id}` reaches the host to be refused there"
        );
    }
}

/// The refusal of an onboarding write, by key or by name, points at the
/// window's own path now that it exists; every other key-only row's reason
/// stands as the reducer gives it.
#[test]
fn a_refused_onboarding_write_points_at_settings_setup() {
    let log = Log::default();
    let mut host = host(&log);
    let keymap = Keymap::defaults();

    // A row by name, off its screen: judged by its action, not its context.
    for id in onboarding_write_rows(&keymap) {
        let refusal = host
            .refused_from_renderer(&Intent::Command(id.clone(), serde_json::Value::Null))
            .unwrap_or_else(|| panic!("`{id}` is refused"));
        assert_eq!(refusal.reason, DESKTOP_PATH, "{id}");
        assert!(
            refusal.reason.contains("Settings, Setup"),
            "{id}: {}",
            refusal.reason
        );
    }

    // The same rows by key, on the dependency step of the wizard, reached
    // through the setup menu's own rows: Setup, then its second item.
    for step in [
        "home.setup",
        "setup_menu.menu.next",
        "setup_menu.menu.select",
    ] {
        let _ = host.dispatch(Intent::Command(
            CommandId::new(step),
            serde_json::Value::Null,
        ));
    }
    assert!(
        host.state().onboarding.onboarding_state.is_some(),
        "the setup menu's Check Dependencies starts the wizard on that step"
    );
    let refusal = host
        .refused_from_renderer(&Intent::Key(Chord::parse("t").expect("chord")))
        .expect("t writes ~/.tmux.conf");
    assert!(
        refusal.command.as_str().starts_with("onboarding."),
        "{refusal:?}"
    );
    assert_eq!(refusal.reason, DESKTOP_PATH);

    // A key-only row the panel does not cover keeps the reducer's reason.
    let statusline = host
        .refused_from_renderer(&Intent::Key(Chord::parse("W").expect("chord")))
        .expect("W is key-only");
    assert_eq!(statusline.command.as_str(), "global.wire_statusline");
    assert!(
        statusline.reason.contains("only from its key"),
        "{statusline:?}"
    );
    assert_eq!(desktop_path(&statusline.command), None);
}

/// Each write's confirmation names what it writes outside ainb, so the
/// dialog never reads as a generic yes.
#[test]
fn every_confirmation_names_what_it_writes() {
    let cases = [
        (
            SetupWrite::InstallDependency {
                id: "tmux".to_string(),
            },
            "package manager",
        ),
        (SetupWrite::InstallAllDependencies, "package manager"),
        (SetupWrite::WriteTmuxConfig, "~/.tmux.conf"),
        (
            SetupWrite::FinishOpenTelemetry {
                otlp_endpoint: "https://otlp.example/otlp".to_string(),
                instance_id: "1".to_string(),
                api_token: "t".to_string(),
            },
            "settings.json",
        ),
    ];
    for (write, names) in cases {
        let confirmation = write.confirmation();
        assert!(confirmation.title.ends_with('?'), "{confirmation:?}");
        assert!(confirmation.body.contains(names), "{confirmation:?}");
    }
}

/// Under a scratch home, the tmux write lands where the wizard's `t` puts it
/// and the status reads it back; a telemetry finish with a field missing
/// writes nothing and says which.
#[test]
fn the_tmux_write_lands_under_home_and_the_status_reads_it() {
    let home = support::isolated_home();
    let conf = home.join(".tmux.conf");
    let _ = std::fs::remove_file(&conf);
    assert!(!status().tmux_conf_present);

    let outcome = SetupWrite::WriteTmuxConfig.run().expect("the write runs");
    assert!(outcome.contains("~/.tmux.conf"), "{outcome}");
    assert!(
        conf.exists(),
        "the file is written under the process's home"
    );
    assert!(status().tmux_conf_present);

    let refused = SetupWrite::FinishOpenTelemetry {
        otlp_endpoint: "https://otlp.example/otlp".to_string(),
        instance_id: String::new(),
        api_token: "t".to_string(),
    }
    .run()
    .expect_err("a field is missing");
    assert!(refused.contains("all three fields"), "{refused}");
    assert!(!status().otel.env_file_present, "nothing was written");
    assert!(
        !home.join(".claude/settings.json").exists(),
        "nothing outside ainb was written"
    );

    let unknown = SetupWrite::InstallDependency {
        id: "no-such-dep".to_string(),
    }
    .run()
    .expect_err("an unknown dependency installs nothing");
    assert!(unknown.contains("unknown dependency"), "{unknown}");
}

/// The status lists the catalog's dependencies as the wizard detects them.
#[test]
fn the_status_lists_the_catalog() {
    support::isolated_home();
    let view = status();
    assert!(!view.dependencies.is_empty());
    assert!(
        view.dependencies.iter().any(|dep| dep.id == "tmux"),
        "{:?}",
        view.dependencies
    );
    for dep in &view.dependencies {
        assert!(!dep.name.is_empty());
        assert!(
            !dep.hint.is_empty(),
            "{}: a hint says how to install it by hand",
            dep.id
        );
    }
}
