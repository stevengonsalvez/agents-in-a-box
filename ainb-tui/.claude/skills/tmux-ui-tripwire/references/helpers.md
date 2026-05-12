# helpers.md — copy-paste Rust scaffolding for tripwires

Drop these into the top of any `crates/ainb-core/tests/tripwire_*.rs`. All
proven in `tripwire_real_data_in_tui.rs` and `tripwire_sessions_screen.rs`.

## Imports

```rust
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
```

## Binary + tmux probes

```rust
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
```

## Staged-plugin discovery (only if test exercises plugin path)

```rust
/// Walk up from the ainb binary looking for `dist/plugins/`. Returns
/// `None` if absent so the test can skip rather than fail in fresh
/// checkouts where `just stage-plugins` hasn't been run.
fn plugins_staged() -> Option<PathBuf> {
    let bin = ainb_bin();
    let mut dir = bin.parent()?;
    for _ in 0..6 {
        let candidate = dir.join("dist").join("plugins");
        if candidate.join("burndown").join("burndown").exists()
            && candidate.join("session-reader").join("session-reader").exists()
        {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}
```

## Isolated HOME + wizard skip

```rust
fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
}
```

Critical: `env!("CARGO_PKG_VERSION")` not a shell grep — workspace crates
literally have `version.workspace = true` in their Cargo.toml.

## Capture + poll

```rust
fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Poll `capture_pane` every 500ms until `predicate(captured)` is true
/// or `deadline` elapses. Returns the matching capture, or `None`.
fn poll_capture<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(500));
    }
    None
}
```

## Session lifecycle

```rust
fn kill_session(session: &str) {
    // Specific session by EXACT name. Never kill-server, never wildcard.
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", session])
        .status();
}
```

## Launch pattern

```rust
let session = format!("tripwire-<feature>-{}", std::process::id());

// Geometry must be explicit and consistent — render output depends on it.
let status = Command::new("tmux")
    .args([
        "new-session", "-d", "-s", &session,
        "-x", "180", "-y", "50",
    ])
    .status()
    .expect("tmux new-session");
assert!(status.success(), "tmux new-session failed");

// `exec` replaces the shell — keystrokes hit ainb directly, no shell
// prefix to escape. Env vars go on the command line, not via tmux setenv
// (tmux setenv only affects future panes, not the one we just created).
let cmd = format!(
    "HOME={} AINB_PLUGIN_ROOT={} exec {} tui",
    home_tmp.path().display(),
    plugin_root.display(),
    ainb_bin().display(),
);
Command::new("tmux")
    .args(["send-keys", "-t", &session, &cmd, "Enter"])
    .status()
    .expect("send launch cmd");
```

For non-plugin path tests, drop `AINB_PLUGIN_ROOT` and add `AINB_DISABLE_PLUGINS=1`:

```rust
let cmd = format!(
    "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
    home_tmp.path().display(),
    ainb_bin().display(),
);
```

## Keystroke pattern

```rust
// Single-character nav — NO trailing Enter.
Command::new("tmux")
    .args(["send-keys", "-t", &session, "i"])
    .status()
    .expect("send i");

// Multi-character line — Enter is a SEPARATE argument, not appended.
Command::new("tmux")
    .args(["send-keys", "-t", &session, ":some-command", "Enter"])
    .status()
    .expect("send command");
```

## End-of-test teardown (with capture on failure)

```rust
let final_cap = post_cap.unwrap_or_else(|| capture_pane(&session));
kill_session(&session);

assert!(
    final_cap.contains("Usage Analytics"),
    "analytics screen never rendered after 'i'.\n{final_cap}"
);
```

Always include the captured pane in assertion messages — debugging is 10×
faster when you can see what the test actually saw.
