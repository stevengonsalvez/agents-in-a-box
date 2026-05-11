# gotchas.md — silent traps that cost hours

Each item burned at least one debugging session. Reading this file before
writing a new tripwire prevents re-learning these the hard way.

## 1. macOS AMFI silent SIGKILL

See `amfi.md`. Exit 137 in <1ms, no stderr, host reports `Broken pipe`.
Fix: `just stage-plugins` re-signs after `cp`.

## 2. `version.workspace = true` literal

Workspace member `Cargo.toml` files have `version.workspace = true` —
that string is what `grep '^version' Cargo.toml` returns. Don't use shell
greps for the version in test seeds. Use `env!("CARGO_PKG_VERSION")` in
Rust or read `[workspace.package].version` from the root `Cargo.toml`.

## 3. EnvFilter default crate-name drift

`crates/ainb-core/src/main.rs::setup_logging()` used to default to
`agents_box=info` — a crate name that no longer exists after the Phase 7
rename. Anything the plugin runtime emitted was silently dropped. Now
defaults to `info,ainb_plugin_runtime=debug,...`. If logs are missing
when debugging, check the EnvFilter default matches the current crate
names. Override via `RUST_LOG=...`.

## 4. `drain_stderr` was debug-level

The host's stderr drain logged plugin output at `debug!`, which the
default filter excluded. Bumped to `info!` — plugin `eprintln!` now
appears by default. If you add a new diagnostic `eprintln!` and don't
see it in the JSONL, check this hasn't regressed.

## 5. Substring-OR assertion theater

The original 7f.3 asserted on `c.contains("ainb") || c.contains("session") || c.contains("Container")`.
All three appear in the sidebar regardless of feature state. Test passed
for 4+ commits while burndown was broken. **Always pair a POSITIVE marker
with a NEGATIVE placeholder check** AND **assert pre-press state is NOT
already the post-press state** (catches stale-state leaks).

## 6. First-run wizard intercepts keystrokes

The setup wizard eats every keystroke until completed. If your test
sends `i` and the capture still shows "Welcome to AINB", the wizard is
in the way. Fix: pre-seed `$HOME/.agents-in-a-box/config/onboarding.toml`
BEFORE launch. See `helpers.md::seed_isolated_home`.

## 7. `send-keys` Enter pitfall

```
tmux send-keys -t S "iEnter"      # wrong — sends literal "iEnter"
tmux send-keys -t S "i" Enter     # wrong for nav — sends i then Enter
tmux send-keys -t S "i"           # right — single-char nav
tmux send-keys -t S "cmd" Enter   # right — shell line + commit
```

Enter is a SEPARATE argument, only for committing shell command lines.
Single-character TUI keybindings should NEVER have it appended.

## 8. Bare `sleep` before capture

TUI render rate is 4–30 Hz. A bare `sleep 2` after `send-keys` may catch
a transient state. Always poll:

```rust
let post = poll_capture(&session, Instant::now() + Duration::from_secs(30),
    |c| /* predicate */).unwrap_or_else(|| capture_pane(&session));
```

Predicate runs against each capture every 500 ms until deadline. Faster
when the state appears, deterministic when it doesn't.

## 9. Multi-plugin registration ordering

Plugins are registered in discovery order. If your test only watches one
plugin's lifecycle, you may miss that ANOTHER plugin failed (e.g.
session-reader is eager so it spawns immediately; burndown is lazy so
its failure surfaces later only when the user presses `i`). Check JSONL
for BOTH `registered plugin` lines AND each plugin's subsequent
lifecycle events.

## 10. Hardlink vs re-sign

A hardlink (`ln`) shares the inode and therefore the original signature
path-binding, but the *path* used to launch the binary is what AMFI
checks at `exec()` time. Hardlinking from `dist/plugins/<id>/<id>` to
`target/debug/<crate>` does NOT bypass the kill — the dist path is still
the one the host execs. Re-sign is the only reliable fix on macOS.

## 11. tmux geometry default varies

Without explicit `-x 180 -y 50`, tmux uses the calling terminal's
geometry (or 80×24 in `-d` mode). Width-sensitive renders (burndown
bars, multi-column layouts) shift between hosts. ALWAYS set explicit
geometry. 180×50 is the agreed default for ainb tripwires.

## 12. `exec` vs no-`exec` in launch command

```rust
let cmd = format!("HOME={} exec {} tui", ...);
```

`exec` replaces the shell with the ainb binary. Without `exec`, the
shell stays as PID 1 in the tmux pane and any future `send-keys`
goes to the shell, not ainb. Always include `exec`.

## 13. `kill_on_drop` on host's Child

The host runtime's `Command::new(...).kill_on_drop(true)` means if the
host's plugin task panics or returns Err early, the child process gets
SIGKILLed. That's correct behaviour, but it means an unrelated panic in
the host can mask the real symptom (plugin appears killed, but the
killer was the host's own teardown). Read the JSONL chronologically.

## 14. `tempfile::tempdir()` cleanup races

`tempdir()` is cleaned up when its handle drops. If a test panics, the
dir may stay around (Rust's panic unwinding may or may not run the
drop). If tripwires leave `/var/folders/.../tmp.*` orphans, that's why.
Periodic `rm -rf /var/folders/*/T/tmp.*` is fine (or `mktemp -d` from
shell wrappers — manual cleanup).
