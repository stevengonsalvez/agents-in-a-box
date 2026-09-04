//! Tripwire: the copilot's engine picker is the daemon's registry, and swapping
//! it keeps the channel.
//!
//! Two claims that only a real screen against a real daemon can settle:
//!
//! 1. **The engine list is CONFIG, not a compiled-in enum.** The picker cycles
//!    an adapter this test writes into `[acp.adapters]` and nothing else knows
//!    about. Before this phase `provider` was a two-variant enum, so an operator
//!    who installed a third adapter had a daemon that could spawn it and a
//!    picker that could not name it.
//! 2. **A swap keeps the CHANNEL.** The old refusal ("a provider change needs a
//!    new session on a new channel") made the picker's only working move
//!    abandoning the conversation: the channel scope is what every message,
//!    confirm card and activity row is keyed on. The swap now retires the old
//!    ACP session and mints a new one on the SAME scope.
//!
//! Plus the guardrail dial, which is a DIFFERENT setting from the adapter's
//! `permission_mode` and must never be mistaken for it: `g` moves the
//! daemon-side fleet-tool classifier, and `yolo` says so in red on the pane
//! because nothing else will stop and ask.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{EnvGuard, FleetHangar};

/// The adapter this test invents. Named so a failure cannot be confused with a
/// built-in: nothing but the config file below has ever heard of it.
const INVENTED_ADAPTER: &str = "tripwire-acp";

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A tmux session killed by EXACT name on drop. `=name` is the SESSION form;
/// capture and send below use the plain form, which is the only one tmux
/// accepts where a PANE is expected.
struct ExactTmuxSession {
    name: String,
}

impl ExactTmuxSession {
    fn create(name: String, command: &[&str]) -> Self {
        let mut args: Vec<String> =
            ["new-session", "-d", "-s"].iter().map(|a| (*a).to_string()).collect();
        args.push(name.clone());
        args.extend(["-x", "200", "-y", "50"].iter().map(|a| (*a).to_string()));
        args.extend(command.iter().map(|part| (*part).to_string()));
        let status = Command::new("tmux").args(&args).status().expect("tmux new-session");
        assert!(status.success(), "tmux new-session {name} failed");
        Self { name }
    }
}

impl Drop for ExactTmuxSession {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &format!("={}", self.name)])
            .status();
    }
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn send_key(session: &str, key: &str) {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
    assert!(status.success(), "tmux send-keys {key:?} failed");
}

fn poll<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(400));
    }
    None
}

/// Press `key` until `arrived`, then stop pressing and keep polling for `ok`.
fn poll_until<F, G>(
    session: &str,
    key: &str,
    deadline: Instant,
    mut arrived: G,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
    G: FnMut(&str) -> bool,
{
    let mut on_screen = false;
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        on_screen = on_screen || arrived(&cap);
        if !on_screen {
            send_key(session, key);
        }
        thread::sleep(Duration::from_millis(400));
    }
    None
}

/// Press `key` up to `attempts` times, stopping as soon as `ok` holds.
///
/// Re-checks between presses: the copilot pane dials the daemon when it opens,
/// so it takes more than one repaint to settle, and pressing again in that
/// window walks straight past it.
fn press_until<F>(
    session: &str,
    key: &str,
    attempts: usize,
    mut ok: F,
) -> Result<String, Vec<String>>
where
    F: FnMut(&str) -> bool,
{
    let mut seen = Vec::new();
    for _ in 0..attempts {
        for _ in 0..6 {
            let cap = capture_pane(session);
            if ok(&cap) {
                return Ok(cap);
            }
            if seen.last() != Some(&cap) {
                seen.push(cap);
            }
            thread::sleep(Duration::from_millis(250));
        }
        send_key(session, key);
    }
    Err(seen)
}

/// Press `key` ONCE, then wait for `ok`. For a dial that dials the daemon: a
/// second press would turn it past the value being asserted.
fn press_once_then_wait<F>(session: &str, key: &str, secs: u64, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    send_key(session, key);
    poll(session, Instant::now() + Duration::from_secs(secs), |c| {
        ok(c)
    })
}

fn init_git_repo(dir: &Path) {
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "tripwire")
            .env("GIT_AUTHOR_EMAIL", "tripwire@example.invalid")
            .env("GIT_COMMITTER_NAME", "tripwire")
            .env("GIT_COMMITTER_EMAIL", "tripwire@example.invalid")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "--initial-branch=main"]);
    fs::write(dir.join("README.md"), "engine picker fixture\n").expect("seed a file");
    git(&["add", "README.md"]);
    git(&["-c", "commit.gpgsign=false", "commit", "-m", "seed"]);
}

/// The isolated `$HOME`, INCLUDING the `[acp.adapters]` table that invents a
/// third adapter.
///
/// The registry is read from `$HOME/.agents-in-a-box/config/config.toml` by the
/// daemon, which here is this test process, so `HOME` is set for the process
/// and not only for the TUI child.
fn seed_isolated_home(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let cfg = base.join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    fs::write(
        cfg.join("onboarding.toml"),
        format!(
            "completed = true\n\
             completed_at = \"2026-09-04T00:00:00+00:00\"\n\
             version = \"{ver}\"\n\
             skipped_dependencies = []\n\
             git_directories = []\n",
            ver = env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("seed onboarding.toml");
    fs::write(
        base.join("install.json"),
        r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#,
    )
    .expect("seed install.json");
    // The claim under test: an adapter that exists ONLY here is selectable in
    // the picker. `command` points at `true` so nothing tries to spawn a real
    // agent; the swap is a store write, and no adapter process starts until a
    // prompt reaches the pool.
    fs::write(
        cfg.join("config.toml"),
        format!(
            "[acp.adapters.{INVENTED_ADAPTER}]\n\
             command = \"/usr/bin/true\"\n\
             models = [\"tripwire-mini\", \"tripwire-max\"]\n"
        ),
    )
    .expect("seed the invented adapter");
}

fn seed_session_registry(home: &Path, tmux_name: &str, worktree: &Path) {
    let entry = serde_json::json!({
        "sessions": {
            tmux_name: {
                "session_id": "6f1f5f7e-0000-4000-8000-0000000000c1",
                "tmux_session_name": tmux_name,
                "worktree_path": worktree,
                "workspace_name": "enginepick",
                "created_at": "2026-09-04T00:00:00Z",
                "agent_type": "Claude",
                "skip_permissions": true,
            }
        }
    });
    fs::write(
        home.join(".agents-in-a-box").join("sessions.json"),
        serde_json::to_vec_pretty(&entry).expect("encode sessions.json"),
    )
    .expect("seed sessions.json");
}

#[test]
fn the_engine_picker_reads_the_registry_and_a_swap_keeps_the_channel() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-copilot-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let home = home_tmp.path();
    seed_isolated_home(home);
    // The DAEMON reads `[acp.adapters]` from `$HOME`, and the daemon here is
    // this process. Held for the whole test: a registry read after the guard
    // drops would answer with the developer's own adapters.
    let _home_guard = EnvGuard::set("HOME", home);

    let hangar_home = home.join("hangar-home");
    fs::create_dir_all(&hangar_home).expect("create isolated hangar home");
    fs::write(
        hangar_home.join("install.json"),
        r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#,
    )
    .expect("dismiss the notification prompt in the daemon home");
    let _hangar_home_guard = EnvGuard::set("AINB_HANGAR_HOME", &hangar_home);
    let hangar = FleetHangar::start(&hangar_home);

    let pid = std::process::id();
    let worktree = home.join("enginepick");
    fs::create_dir_all(&worktree).expect("create worktree dir");
    init_git_repo(&worktree);
    let agent_tmux = format!("tmux_enginepick_{pid}");
    seed_session_registry(home, &agent_tmux, &worktree);
    let _pane = ExactTmuxSession::create(agent_tmux, &["sh", "-c", "sleep 900"]);

    let tui_tmux = format!("tripwire-copilot-{pid}");
    let tui = ExactTmuxSession::create(tui_tmux.clone(), &[]);
    let launch = format!(
        "HOME={home} AINB_HANGAR_HOME={hangar} AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home.display(),
        hangar = hangar_home.display(),
        bin = ainb_bin().display()
    );
    assert!(
        Command::new("tmux")
            .args(["send-keys", "-t", &tui_tmux, &launch, "Enter"])
            .status()
            .expect("launch ainb tui")
            .success(),
        "tmux refused the launch command"
    );

    assert!(
        poll(&tui_tmux, Instant::now() + Duration::from_secs(90), |c| {
            c.contains("Sessions") && c.contains("[s]")
        })
        .is_some(),
        "HomeScreen never rendered:\n{}",
        capture_pane(&tui_tmux)
    );
    assert!(
        poll_until(
            &tui_tmux,
            "s",
            Instant::now() + Duration::from_secs(90),
            |c| c.contains("Workspaces ("),
            // The STRIP, not the bare word: the home screen's Recent line
            // carries the workspace name, so a loose match can fire before `s`
            // is ever pressed.
            |c| c.contains("preview") && c.contains("copilot"),
        )
        .is_some(),
        "the sessions screen never rendered:\n{}",
        capture_pane(&tui_tmux)
    );

    // Walk to the copilot tab. Its header is the thing under test, and it
    // renders whether or not the conversation below it has opened: the engine
    // picker is how an operator RECOVERS from an adapter that will not spawn,
    // so hiding it behind a working chat would put the fix behind the failure.
    // Gated on the dial MARKER, not the words: "engine" and "mode" both occur
    // on other panes, and a loose match stops the walk one tab early.
    let header = press_until(&tui_tmux, "Tab", 8, |c| {
        c.contains("\u{25c0} \u{2325}e") && c.contains("\u{25c0} \u{2325}g")
    })
    .unwrap_or_else(|seen| {
        panic!(
            "Tab never reached the copilot header. Panes visited:\n  {}\n---\n{}\n---",
            seen.iter()
                .filter_map(|cap| cap.lines().nth(4))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("\n  "),
            capture_pane(&tui_tmux)
        )
    });

    // The dials name their own keys, next to the values they turn. Matched by
    // the KEY marker, because the pane is inside a bordered panel and every
    // line starts with the border glyph, not the label.
    for (label, key) in [
        ("engine", "\u{2325}e"),
        ("model", "\u{2325}o"),
        ("mode", "\u{2325}g"),
    ] {
        let marker = format!("\u{25c0} {key}");
        let row = header
            .lines()
            .find(|line| line.contains(&marker))
            .unwrap_or_else(|| panic!("no row carrying `{marker}`:\n{header}"));
        assert!(
            row.contains(label),
            "the `{key}` key must sit on the {label} row, not somewhere else: {row}"
        );
    }
    assert!(
        header.contains("guarded"),
        "a fresh channel starts on the safe dial:\n{header}"
    );

    // The registry read is a daemon call, so the engine lands a frame or two
    // after the header does. It is the ADAPTER NAME, never a family token.
    let named = poll(&tui_tmux, Instant::now() + Duration::from_secs(60), |c| {
        c.contains("claude-agent-acp")
    })
    .unwrap_or_else(|| {
        panic!(
            "the registry never named the engine:\n{}",
            capture_pane(&tui_tmux)
        )
    });
    assert!(named.contains("claude-agent-acp"), "{named}");

    // `e` walks the registry in name order: claude-agent-acp -> codex-acp.
    // This is a real swap over the wire — the old ACP session is retired and a
    // new one minted on the same channel scope.
    let swapped = press_once_then_wait(&tui_tmux, "M-e", 60, |c| {
        c.lines()
            .any(|line| line.contains("\u{25c0} \u{2325}e") && line.contains("codex-acp"))
    })
    .unwrap_or_else(|| {
        panic!(
            "`\u{2325}e` never reached the second registry adapter:\n{}",
            capture_pane(&tui_tmux)
        )
    });
    assert!(
        swapped.contains("engine swapped"),
        "a swap replaces the session, and an operator mid-conversation has to be \
         told or the empty timeline reads as a bug:\n{swapped}"
    );

    // THE registry proof: `e` again reaches an adapter that exists only in the
    // config file this test wrote. A compiled-in enum could not name it.
    // Matched on the ENGINE ROW, not the whole pane: a failure line naming the
    // adapter it could not switch to would otherwise read as a success.
    let engine_row_has = |cap: &str, name: &str| {
        cap.lines()
            .any(|line| line.contains("\u{25c0} \u{2325}e") && line.contains(name))
    };
    let invented = press_once_then_wait(&tui_tmux, "M-e", 60, |c| {
        engine_row_has(c, INVENTED_ADAPTER)
    })
    .unwrap_or_else(|| {
        panic!(
            "`\u{2325}e` never reached `{INVENTED_ADAPTER}`, the adapter that exists \
                 only in [acp.adapters]:\n{}",
            capture_pane(&tui_tmux)
        )
    });
    assert!(engine_row_has(&invented, INVENTED_ADAPTER), "{invented}");

    // And the CHANNEL survived both swaps: one copilot channel throughout, with
    // its live ACP session now on the invented adapter. This is what the old
    // "a provider change needs a new session on a new channel" refusal cost.
    let (channels, live_provider) = hangar.block_on(async {
        let channels: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM fleet_channel WHERE kind = 'copilot'")
                .fetch_one(hangar.pool())
                .await
                .expect("count copilot channels");
        let provider: String = sqlx::query_scalar(
            "SELECT s.provider FROM fleet_acp_session s \
             JOIN fleet_channel c ON c.scope_key = s.scope_key \
             WHERE c.kind = 'copilot' AND s.state IN ('ACTIVE','IDLE')",
        )
        .fetch_one(hangar.pool())
        .await
        .expect("the copilot channel must have exactly one live session");
        (channels, provider)
    });
    assert_eq!(channels, 1, "a swap must not mint a second copilot channel");
    assert_eq!(
        live_provider, INVENTED_ADAPTER,
        "the channel's live session must be on the swapped-to adapter"
    );

    // `g` turns the guardrail dial. It is a DIFFERENT setting from the
    // adapter's permission mode, and `yolo` gets a banner because nothing else
    // will stop and ask.
    let armed =
        press_once_then_wait(&tui_tmux, "M-g", 60, |c| c.contains("yolo")).unwrap_or_else(|| {
            panic!(
                "`\u{2325}g` never turned the dial:\n{}",
                capture_pane(&tui_tmux)
            )
        });
    assert!(
        armed.contains("no confirm card"),
        "yolo must say what it costs:\n{armed}"
    );
    assert!(
        armed.contains("kill still asks"),
        "and that the never-overridable floor survives it:\n{armed}"
    );

    // The dial is the CHANNEL's and the adapter's pinned mode is untouched.
    // Two settings that must never be confused: an ambient bypassPermissions
    // disables the agent's whole permission surface.
    let (dial, pinned) = hangar.block_on(async {
        let dial: String =
            sqlx::query_scalar("SELECT copilot_mode FROM fleet_channel WHERE kind = 'copilot'")
                .fetch_one(hangar.pool())
                .await
                .expect("read the channel dial");
        let pinned: String = sqlx::query_scalar(
            "SELECT s.permission_mode FROM fleet_acp_session s \
             JOIN fleet_channel c ON c.scope_key = s.scope_key \
             WHERE c.kind = 'copilot' AND s.state IN ('ACTIVE','IDLE')",
        )
        .fetch_one(hangar.pool())
        .await
        .expect("read the pinned adapter mode");
        (dial, pinned)
    });
    assert_eq!(dial, "yolo", "the dial must be durable, not per-frame");
    assert_ne!(
        pinned, "yolo",
        "the guardrail dial must never reach the adapter's permission mode"
    );
    assert_ne!(pinned, "bypassPermissions", "and never loosen it");

    if let Some(ms) = std::env::var("AINB_CHIP_DEMO_HOLD_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    {
        eprintln!("holding {tui_tmux} for {ms}ms");
        thread::sleep(Duration::from_millis(ms));
    }

    drop(tui);
}

/// The daemon-down leg: the header NAMES the call that failed and offers the
/// way out.
///
/// Straight from the spec's failure table. A picker that renders an empty
/// engine and no explanation is the dead end this whole surface replaces: an
/// operator cannot tell "no adapters configured" from "the daemon is not
/// running", and the second one has an obvious fix.
///
/// Deliberately does NOT set `HOME` in this process — it needs no registry, and
/// the swap test above holds a `HOME` guard for its own run.
#[test]
fn with_no_daemon_the_copilot_header_names_the_failed_call() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-nodaemon-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let home = home_tmp.path();
    seed_isolated_home(home);

    // A hangar home with NO socket and no daemon behind it.
    let hangar_home = home.join("hangar-home");
    fs::create_dir_all(&hangar_home).expect("create empty hangar home");
    fs::write(
        hangar_home.join("install.json"),
        r#"{"agents":[],"hook_script":"","prompt_dismissed":true}"#,
    )
    .expect("dismiss the notification prompt");

    let pid = std::process::id();
    let worktree = home.join("nodaemon");
    fs::create_dir_all(&worktree).expect("create worktree dir");
    init_git_repo(&worktree);
    let agent_tmux = format!("tmux_nodaemon_{pid}");
    seed_session_registry(home, &agent_tmux, &worktree);
    let _pane = ExactTmuxSession::create(agent_tmux, &["sh", "-c", "sleep 900"]);

    let tui_tmux = format!("tripwire-nodaemon-{pid}");
    let tui = ExactTmuxSession::create(tui_tmux.clone(), &[]);
    let launch = format!(
        "HOME={home} AINB_HANGAR_HOME={hangar} AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home.display(),
        hangar = hangar_home.display(),
        bin = ainb_bin().display()
    );
    assert!(
        Command::new("tmux")
            .args(["send-keys", "-t", &tui_tmux, &launch, "Enter"])
            .status()
            .expect("launch ainb tui")
            .success(),
        "tmux refused the launch command"
    );
    assert!(
        poll(&tui_tmux, Instant::now() + Duration::from_secs(90), |c| {
            c.contains("Sessions") && c.contains("[s]")
        })
        .is_some(),
        "HomeScreen never rendered:\n{}",
        capture_pane(&tui_tmux)
    );
    assert!(
        poll_until(
            &tui_tmux,
            "s",
            Instant::now() + Duration::from_secs(90),
            |c| c.contains("Workspaces ("),
            |c| c.contains("preview") && c.contains("copilot"),
        )
        .is_some(),
        "the sessions screen never rendered:\n{}",
        capture_pane(&tui_tmux)
    );

    let failed = press_until(&tui_tmux, "Tab", 8, |c| {
        c.contains("fleet/adapter_list failed")
    })
    .unwrap_or_else(|seen| {
        panic!(
            "the copilot header never named the failed call. Panes visited:\n  {}\n---\n{}\n---",
            seen.iter()
                .filter_map(|cap| cap.lines().nth(4))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("\n  "),
            capture_pane(&tui_tmux)
        )
    });
    assert!(
        failed.contains("\u{2325}r retry"),
        "a named failure with no way forward is still a dead end:\n{failed}"
    );
    // The dials are still THERE, dimmed rather than hidden, so the pane does not
    // reflow into something unrecognisable the moment the daemon goes away.
    for key in ["\u{2325}e", "\u{2325}o", "\u{2325}g"] {
        assert!(
            failed.contains(key),
            "the header must keep its dials when the daemon is down: `{key}` gone:\n{failed}"
        );
    }

    if let Some(ms) = std::env::var("AINB_CHIP_DEMO_HOLD_MS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
    {
        eprintln!("holding {tui_tmux} for {ms}ms");
        thread::sleep(Duration::from_millis(ms));
    }

    drop(tui);
}
