//! Tripwire: an error notice is READABLE, DISMISSABLE, and never the only copy.
//!
//! The complaint was one sentence with three things in it — "make it disappear
//! after like a minute or dismissable sort of (so can add a bit more detail as
//! well)". A five-second box clipped to one 48-column line was setting the
//! wording of every message that went through it.
//!
//! What has to be true on a real screen, and none of which a unit test can
//! reach (the wrap maths, the key routing and the log write are each unit
//! tested; that all of them meet on one pane is not):
//!
//! - a failure paints its DETAIL, wrapped over several rows, not clipped;
//! - the box advertises the key that retires it, on the box;
//! - that key works, and works long before the notice would have expired;
//! - a notice left alone retires itself on the configured clock;
//! - and afterwards — dismissed and expired — both messages are still in the
//!   app log, so nothing the operator did not read has been thrown away.
//!
//! Both halves run off ONE launch with two lifetimes seeded into `[ui]`:
//! errors at [`ERROR_SECS`], so a notice that vanishes seconds after the chord
//! cannot be a notice that timed out, and warnings at [`WARNING_SECS`], so
//! expiry is observable without the suite sitting out the shipped minute. The
//! shipped minute is pinned by the unit test
//! `an_error_notice_lives_for_one_minute_out_of_the_box` — the only honest way
//! to assert a duration nobody can afford to wait for.
//!
//! Runs on its OWN tmux server (`-L`), for two reasons. The TUI lists every
//! session on whatever server it can see, so on a shared one this test's
//! screen is whatever the machine happens to be running; and an attach driven
//! by a test has no business reaching a real session. On a private server the
//! only session is this test's own, which makes the failure it drives exact:
//! tmux refuses to nest a session inside itself, every time.
//!
//! Skips gracefully if `tmux` isn't on `$PATH`. Plugins are disabled: the
//! notice stack is host chrome.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// Long enough that a dismissed notice cannot be mistaken for an expired one.
const ERROR_SECS: u64 = 30;
/// Short enough to watch a notice retire itself inside a test.
const WARNING_SECS: u64 = 4;

/// From the attach failure's REMEDY, not its headline. The headline is what
/// fit in the old box; this is what the extra rows bought.
const ERROR_REMEDY: &str = "press f to refresh";
/// The hint painted on the box, per keybinding-hints-near-the-control.
const DISMISS_HINT: &str = "Ctrl+X dismiss";
/// The session list's tab strip, flattened. Unique to that screen.
const SESSION_LIST_MARKER: &str = "preview ask err";

/// The tail of the session name the attach failure quotes.
///
/// The name is deliberately longer than a notice row, so the box has to BREAK
/// it rather than break before it. Doubled because a break lands every ~60
/// cells and cannot fall inside both copies, so one always survives whole —
/// and if the row were clipped at the border instead of broken, neither would
/// reach the screen at all. That is the difference this asserts.
const NAME_TAIL: &str = "tailmark";

/// The warning `$` raises with no workspace selected. Deliberately unrelated
/// wording, so the expiry half cannot pass on the dismiss half's leftovers.
const WARNING_TEXT: &str = "No workspace selected";
/// The left half of the preview placeholder's headline for the TUI's own session.
const OWN_SESSION_PLACEHOLDER: &str = "This is the tmux session ainb";

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

/// A tmux server of this test's own, addressed by socket name.
struct Tmux {
    socket: String,
    session: String,
}

impl Tmux {
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new("tmux")
            .args(["-L", &self.socket])
            .args(args)
            .output()
            .expect("tmux")
    }

    fn start(&self) {
        let out = self.run(&[
            "new-session",
            "-d",
            "-s",
            &self.session,
            "-x",
            "200",
            "-y",
            "50",
        ]);
        assert!(
            out.status.success(),
            "tmux new-session failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn send_key(&self, key: &str) {
        self.run(&["send-keys", "-t", &self.session, key]);
    }

    /// Send `text` as literal characters. `$` is not a tmux key name, and
    /// putting it through the key parser is a coin toss not worth throwing.
    fn send_literal(&self, text: &str) {
        self.run(&["send-keys", "-t", &self.session, "-l", text]);
    }

    fn capture(&self) -> String {
        String::from_utf8_lossy(&self.run(&["capture-pane", "-t", &self.session, "-p"]).stdout)
            .to_string()
    }

    /// Capture with the box drawing removed and whitespace runs collapsed.
    ///
    /// A notice WRAPS now, so any phrase longer than a word is split across
    /// rows with a border on each side. Searching the raw capture would fail
    /// for exactly the reason this feature exists.
    fn flat(&self) -> String {
        let stripped: String = self
            .capture()
            .chars()
            .map(|c| {
                if ('\u{2500}'..='\u{257F}').contains(&c) {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        stripped.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn poll<F>(&self, deadline: Instant, mut ok: F) -> Option<String>
    where
        F: FnMut(&str) -> bool,
    {
        loop {
            let flat = self.flat();
            if ok(&flat) {
                return Some(flat);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(300));
        }
    }

    /// Kill this test's session, by EXACT name (`=`). Never a bulk kill and
    /// never the server: `-L` already keeps this off everyone else's sessions,
    /// and an exact target keeps it off theirs even if it did not.
    fn kill(&self) {
        self.run(&["kill-session", "-t", &format!("={}", self.session)]);
    }

    /// Abandon the run with the screen attached to the message.
    fn bail(&self, what: &str) -> ! {
        let last = self.capture();
        self.kill();
        panic!("{what}\n---\n{last}\n---");
    }
}

fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
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
        cfg.join("config.toml"),
        format!("[ui]\nnotice_error_secs = {ERROR_SECS}\nnotice_warning_secs = {WARNING_SECS}\n"),
    )
    .expect("seed config.toml");
    let install_record = r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#;
    fs::write(
        home.join(".agents-in-a-box").join("install.json"),
        install_record,
    )
    .expect("seed install.json");
}

/// Everything the TUI logged this run. The Log History screen (`l` from home)
/// reads this same directory.
fn app_log(home: &Path) -> String {
    let dir = home.join(".agents-in-a-box").join("logs");
    let Ok(entries) = fs::read_dir(&dir) else {
        return String::new();
    };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

fn launch_to_session_list(tmux: &Tmux, home: &Path) {
    tmux.start();
    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home.display(),
        ainb_bin().display()
    );
    tmux.run(&["send-keys", "-t", &tmux.session, &cmd, "Enter"]);

    if tmux
        .poll(Instant::now() + Duration::from_secs(120), |c| {
            c.contains("Stats") && c.contains("[i]")
        })
        .is_none()
    {
        tmux.bail("the home screen never rendered");
    }

    // `s` opens the session list, re-sent while the terminal settles because
    // the pane paints before it accepts its first key.
    //
    // The marker is the session list's OWN tab strip. "Workspaces" is not: the
    // home screen carries that word too, so keying off it returned a launcher
    // still sitting on home and left the next step pressing `a` at a screen
    // with no attach on it for the whole of its deadline.
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        tmux.send_key("s");
        thread::sleep(Duration::from_millis(600));
        if tmux.flat().contains(SESSION_LIST_MARKER) {
            return;
        }
        if Instant::now() >= deadline {
            tmux.bail("the session list never opened");
        }
    }
}

#[test]
fn an_error_notice_carries_its_remedy_is_dismissable_and_outlives_itself_in_the_log() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    let tmux = Tmux {
        socket: format!("ainb-notice-{}", std::process::id()),
        // Longer than a notice row on purpose: see NAME_TAIL.
        session: format!(
            "tripwire-notice-{}-a-name-long-enough-to-need-breaking-{NAME_TAIL}-{NAME_TAIL}",
            std::process::id()
        ),
    };
    launch_to_session_list(&tmux, home_tmp.path());

    // ── 1. A failure says what to do about it ───────────────────────────────
    // On a private server the only session listed is this test's own, and `a`
    // asks tmux to attach it to itself. tmux refuses to nest, so this is a
    // real error notice off the real key handler, raised the same way it would
    // be if the session had simply died.
    let deadline = Instant::now() + Duration::from_secs(45);
    let shown = loop {
        tmux.send_key("a");
        thread::sleep(Duration::from_millis(700));
        let flat = tmux.flat();
        if flat.contains(ERROR_REMEDY) {
            break flat;
        }
        if Instant::now() >= deadline {
            tmux.bail("the attach failure never painted its remedy");
        }
    };

    // The headline is what the old box could hold. These are the rows it could
    // not, and they are the whole point of the longer lifetime.
    //
    // "refuses" and "to nest" are checked apart: the selected row is the TUI's
    // own session, so the preview pane beside the notice shows the own-session
    // placeholder, and `flat()` joins each screen row left to right. Where the
    // notice wraps between the two words, placeholder text lands between them.
    for token in ["Failed to attach to", ERROR_REMEDY, "refuses", "to nest"] {
        if !shown.contains(token) {
            tmux.bail(&format!("the notice is missing {token:?}"));
        }
    }

    // The selected row is this TUI's own session, so the preview beside the
    // notice must be the own-session placeholder (#990), not a mirror of the
    // TUI inside itself. The notice covers the right half of the headline, so
    // assert on its left half.
    if !shown.contains(OWN_SESSION_PLACEHOLDER) {
        tmux.bail(&format!(
            "the own-session preview placeholder is missing {OWN_SESSION_PLACEHOLDER:?}"
        ));
    }

    // And the session name — one token wider than the box — was BROKEN across
    // rows, not run past the border and clipped. Clipping would take the tail
    // and everything after it, which is most of the message.
    if !shown.contains(NAME_TAIL) {
        tmux.bail(&format!(
            "the quoted session name was clipped at the border: {NAME_TAIL:?} never \
             reached the screen"
        ));
    }

    // ── 2. The box advertises the key that retires it ───────────────────────
    if !shown.contains(DISMISS_HINT) {
        tmux.bail(&format!(
            "the notice does not say how to dismiss it (expected {DISMISS_HINT:?})"
        ));
    }

    // ── 3. And that key works, decisively before the notice would expire ────
    let dismissed_at = Instant::now();
    tmux.send_key("C-x");
    if tmux
        .poll(Instant::now() + Duration::from_secs(10), |c| {
            !c.contains(ERROR_REMEDY)
        })
        .is_none()
    {
        tmux.bail("Ctrl+X did not dismiss the notice");
    }
    let elapsed = dismissed_at.elapsed();
    assert!(
        elapsed < Duration::from_secs(ERROR_SECS),
        "the notice went away after {elapsed:?}, which is its own {ERROR_SECS}s lifetime — \
         that proves expiry, not dismissal"
    );

    // ── 4. A notice left alone retires itself ───────────────────────────────
    // `$` with no workspace selected raises a warning, seeded at a lifetime
    // short enough to sit through.
    tmux.send_literal("$");
    if tmux
        .poll(Instant::now() + Duration::from_secs(20), |c| {
            c.contains(WARNING_TEXT)
        })
        .is_none()
    {
        tmux.bail("the workspace warning never painted");
    }
    if tmux
        .poll(
            Instant::now() + Duration::from_secs(WARNING_SECS * 4 + 10),
            |c| !c.contains(WARNING_TEXT),
        )
        .is_none()
    {
        tmux.bail(&format!(
            "the warning never expired on its {WARNING_SECS}s clock"
        ));
    }

    // ── 5. Neither message was thrown away ──────────────────────────────────
    // The whole feature is only safe because the log has a copy. If this
    // assertion goes, an expiring notice starts losing failures nobody read.
    let log = app_log(home_tmp.path());
    tmux.kill();

    for token in ["Failed to attach to", WARNING_TEXT] {
        assert!(
            log.contains(token),
            "a notice left the screen without leaving a copy in the app log: {token:?} \
             missing from {} bytes of JSONL",
            log.len()
        );
    }
    assert!(
        log.contains("\"notice\""),
        "notices must be tagged in the log so they can be told from ordinary tracing"
    );
}
