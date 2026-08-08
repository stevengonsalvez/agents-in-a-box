//! ABOUTME: End-to-end send-path tests against a REAL tmux pane. Opt-in via
//! `--features tmux-tests` (mirrors ainb-core's flag of the same name) because
//! they need a tmux server on the host.
//!
//! SCOPE, stated honestly. Two kinds of pane are driven here.
//!
//! A bare `cat` pane proves what tmux itself does: the payload reaches the
//! receiving PTY byte for byte with no duplication, concurrent senders are
//! serialised rather than interleaved, and a capture failure is an error rather
//! than a silently-empty pane.
//!
//! A `composer_pane/emulator.py` pane proves what the VERIFY LOOP does. It is
//! not a claude session and does not pretend to be: it models only the three
//! measured properties the send path depends on (a rule-delimited composer, a
//! tail-anchored viewport that scrolls the payload's head out of view, and a
//! target that can swallow the CR). That is enough to make the loop
//! load-bearing here: a target that never submits must be reported as a
//! failure, and a target that does submit must be reported as delivered.
//!
//! What still cannot be covered without a live claude: the paste classifier
//! itself, and the DIM ghost-suggestion/queued-message rendering. Those are
//! pinned by the unit tests in `src/fleet/send/tmux_tests.rs`, which run
//! against verbatim `capture-pane` output from real claude 2.1.224 sessions at
//! both 200x50 and the 80x24 that `ainb run` actually creates.
//!
//! CI SAFETY. `--features tmux-tests` is not the gate CI actually applies: the
//! `Test` job runs `cargo nextest run --workspace --lib --tests --all-features
//!  --no-fail-fast -E 'not binary(/^tripwire_/)'` on ubuntu-latest AND
//! macos-latest, so `--all-features` turns this feature ON and the binary name
//! is not excluded by that filter. Neither runner image ships tmux (the
//! `hangar-e2e` job installs it explicitly, and its own comment records that
//! macOS runner images do not ship it), so every test below MUST skip cleanly
//! rather than panic on a missing binary. Hence [`tmux_available`].
//!
//! ISOLATION, why this file redirects the tmux server, following
//! `ainb-core/tests/resume_command_tmux.rs`. A tmux server exits when its LAST
//! session dies, so a test whose teardown kills its own session can tear the
//! server down under a sibling that is mid-`new-session` (a real CI failure,
//! run 30209167091). Worse on a developer box: without this, the panes below
//! are created on the engineer's live tmux server alongside their real work.
//! Env vars (not `tmux -L`) are the right lever because the library under test
//! shells out to `tmux` itself and targets the DEFAULT socket: env is inherited
//! by those children, a `-L` flag on our own calls would not be. See
//! [`SERVER_SELECTORS`] for why `TMUX_TMPDIR` alone is not enough, and
//! [`panes_live_on_a_private_tmux_server_not_the_ambient_one`] for the assertion
//! that keeps it honest.

#![cfg(feature = "tmux-tests")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use ainb_fleet_core::read::capture_pane;
use ainb_fleet_core::send::{SendError, SendFailure, tmux_send};

/// Env vars that, between them, decide WHICH tmux server a `tmux` child talks
/// to. `TMUX` is the load-bearing one and the easy one to miss: when it is set
/// (i.e. the test was launched from inside a tmux session, which is how every
/// agent-driven run on a developer box happens) tmux takes the socket path from
/// it and IGNORES `TMUX_TMPDIR` entirely. Pinning `TMUX_TMPDIR` alone therefore
/// silently keeps every pane on the ambient server. Proven, not assumed:
///
/// ```text
/// $ TMUX_TMPDIR=$D tmux new-session -d -s probe 'sleep 60'   # TMUX set
/// $ env -u TMUX_TMPDIR tmux has-session -t probe; echo $?
/// 0                       <- on the AMBIENT server; $D is empty
/// $ env -u TMUX -u TMUX_PANE TMUX_TMPDIR=$D tmux new-session -d -s probe2 …
/// $ env -u TMUX_TMPDIR tmux has-session -t probe2; echo $?
/// 1                       <- isolated; $D/tmux-504/ now holds the socket
/// ```
const SERVER_SELECTORS: [&str; 3] = ["TMUX", "TMUX_PANE", "TMUX_TMPDIR"];

/// Prefix of every socket dir this file creates, and the only thing
/// [`sweep_dead_socket_dirs`] will ever delete.
const SOCKET_DIR_PREFIX: &str = "ainb-send-integrity-tmux-";

struct TmuxIsolation {
    /// Value this process forces into `TMUX_TMPDIR`.
    dir: PathBuf,
    /// Whatever [`SERVER_SELECTORS`] held before we overrode them, so a probe
    /// can still reach the AMBIENT server on purpose.
    ambient: Vec<(&'static str, Option<String>)>,
}

impl Drop for TmuxIsolation {
    /// Remove ONLY this process's own socket dir. Nothing here touches a tmux
    /// server: each pane above already kills its own session by exact name, and
    /// a tmux server exits by itself once its last session is gone.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Remove the socket dirs left behind by EARLIER runs of this test binary.
///
/// Honest about why a sweep exists at all: [`isolation`] parks its value in a
/// `static OnceLock`, and a `static` is never dropped, so the `Drop` impl above
/// is not what bounds the leak in practice. `unsafe_code = "forbid"` rules out
/// an `atexit` hook and libtest has no end-of-run hook, so the cleanup that
/// actually runs is this one, at the start of the NEXT run. Accumulation is
/// therefore bounded at one run's worth of dirs instead of growing forever
/// (20 had piled up under `/tmp` before this).
///
/// Three guards keep it from touching anything it does not own: the name must
/// carry this file's own prefix, the pid baked into that name must be dead, and
/// the dir must hold no tmux server that still ANSWERS. `kill(pid, None)` is
/// signal 0, a liveness probe that delivers NOTHING; it is the same check
/// `fleet/discover/peers.rs` uses. Without the pid guard, a CONCURRENT sibling
/// (nextest runs one process per test) could have its socket dir pulled out
/// from under it; without the socket guard, a run that was hard-killed before
/// its panes' `Drop` ran would leave a live tmux server whose socket dir then
/// vanished, i.e. a leaked PROCESS in place of a leaked directory.
fn sweep_dead_socket_dirs() {
    let Ok(entries) = std::fs::read_dir("/tmp") else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name.to_string_lossy().strip_prefix(SOCKET_DIR_PREFIX).map(str::to_owned)
        else {
            continue;
        };
        let Some(Ok(pid)) = rest.split('-').next().map(str::parse::<u32>) else {
            continue;
        };
        if pid == me {
            continue;
        }
        let alive = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap_or(i32::MAX)),
            None,
        )
        .is_ok();
        if !alive && !holds_a_live_tmux_server(&entry.path()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Does any socket under this dir still have a tmux server listening?
///
/// tmux leaves its socket FILE behind when the server exits, so existence
/// proves nothing; only a successful connect does. A refused connect is a stale
/// socket, which is the normal shape of a finished run.
fn holds_a_live_tmux_server(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    // tmux nests its sockets one level down, in `<TMUX_TMPDIR>/tmux-<uid>/`.
    entries.flatten().any(|entry| {
        std::fs::read_dir(entry.path()).is_ok_and(|sockets| {
            sockets
                .flatten()
                .any(|socket| std::os::unix::net::UnixStream::connect(socket.path()).is_ok())
        })
    })
}

/// Redirect every tmux call this process makes onto a private socket dir.
///
/// Runs exactly once and BEFORE the first tmux invocation (every test enters
/// through [`tmux_available`], which calls this first), so the tmux children
/// spawned inside `ainb_fleet_core` inherit it too. The dir lives under `/tmp`
/// rather than `TMPDIR`: a unix socket path is capped at 104 bytes on macOS and
/// `$TMPDIR` there is already ~50 of them.
fn isolation() -> &'static TmuxIsolation {
    static ISO: OnceLock<TmuxIsolation> = OnceLock::new();
    ISO.get_or_init(|| {
        sweep_dead_socket_dirs();
        let ambient: Vec<(&'static str, Option<String>)> =
            SERVER_SELECTORS.iter().map(|k| (*k, std::env::var(k).ok())).collect();
        // pid + nanos: containers recycle low pids, and a leftover dir owned by
        // another uid would make tmux refuse the socket.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or_default();
        let dir = PathBuf::from("/tmp")
            .join(format!("{SOCKET_DIR_PREFIX}{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create private tmux socket dir");
        // Order matters only in that both happen before the first tmux call.
        std::env::remove_var("TMUX");
        std::env::remove_var("TMUX_PANE");
        std::env::set_var("TMUX_TMPDIR", &dir);
        TmuxIsolation { dir, ambient }
    })
}

/// A `tmux` command pinned to this file's private server.
fn tmux() -> Command {
    isolation();
    Command::new("tmux")
}

/// A `tmux` command pointed back at the AMBIENT (default) server, used only to
/// PROVE our sessions are not on it.
fn ambient_tmux() -> Command {
    let iso = isolation();
    let mut cmd = Command::new("tmux");
    for (key, value) in &iso.ambient {
        match value {
            Some(v) => cmd.env(key, v),
            None => cmd.env_remove(key),
        };
    }
    cmd
}

/// Is there a tmux on this host at all?
///
/// Establishes the private socket dir as a side effect, so it is safe to call
/// this as the first line of every test and nothing ever touches the ambient
/// server. Returning `false` must lead to a SKIP, never a failure: without the
/// skip these tests panic with `spawning tmux: Os { code: 2, kind: NotFound }`
/// on both CI runners.
fn tmux_available() -> bool {
    isolation();
    Command::new("tmux").arg("-V").output().is_ok_and(|o| o.status.success())
}

/// Print the skip in the shape the repo's suite runners grep for (`^SKIP:`), so
/// a host with no tmux reads as skipped rather than as vacuously green.
fn skip(what: &str) {
    eprintln!("SKIP: {what}, tmux is not installed on this host");
}

/// A pane running `cat` with the line discipline in raw mode: canonical mode
/// caps a line at `MAX_CANON` (1024 bytes on macOS) and would itself truncate the
/// payload, which would test the terminal rather than the send path.
struct CatPane {
    name: String,
    out: PathBuf,
    _dir: tempfile::TempDir,
}

impl CatPane {
    fn start(tag: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = dir.path().join("received.bin");
        let name = format!("ainb-send-integrity-{tag}-{}", std::process::id());
        let status = tmux()
            .args([
                "new-session",
                "-d",
                "-s",
                &name,
                &format!("sh -c 'stty raw -echo; cat > {}'", out.display()),
            ])
            .status()
            .expect("spawning tmux");
        assert!(status.success(), "tmux new-session failed");
        wait_until(Duration::from_secs(5), || out.exists());
        Self {
            name,
            out,
            _dir: dir,
        }
    }

    fn received(&self) -> Vec<u8> {
        std::fs::read(&self.out).unwrap_or_default()
    }
}

impl Drop for CatPane {
    fn drop(&mut self) {
        // Kill ONLY this test's session, by exact name, on the private server.
        let _ = tmux().args(["kill-session", "-t", &self.name]).status();
    }
}

fn wait_until(limit: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
}

fn file_len(path: &Path) -> usize {
    std::fs::metadata(path).map_or(0, |m| usize::try_from(m.len()).unwrap_or(usize::MAX))
}

/// A name guaranteed absent from a LIVE private server, with both halves of
/// that guarantee proven rather than assumed.
///
/// Both are load-bearing, and both were missing. The "missing pane" tests used
/// to pass on a host with no tmux at all, where the error came from the absent
/// BINARY and the pane was never consulted; under `cargo nextest` (one process
/// per test) they would equally pass against an absent SERVER. The returned
/// `keepalive` pane holds a real server up, so the only thing missing is the
/// pane, which is the production shape the tests claim to cover.
fn absent_pane(tag: &str) -> (String, CatPane) {
    let keepalive = CatPane::start(&format!("live-{tag}"));
    let name = format!("ainb-send-integrity-absent-{tag}-{}", std::process::id());
    let probe = tmux()
        .args(["has-session", "-t", &name])
        .status()
        .expect("tmux has-session runs, so tmux itself is present");
    assert!(
        !probe.success(),
        "precondition: {name} must not exist on the private server"
    );
    assert!(
        tmux()
            .args(["has-session", "-t", &keepalive.name])
            .status()
            .expect("tmux has-session runs")
            .success(),
        "precondition: the private server must be up, holding {}",
        keepalive.name
    );
    (name, keepalive)
}

/// An error raised because tmux could not be SPAWNED proves nothing about pane
/// handling, so the missing-pane tests reject it explicitly.
fn assert_not_a_missing_binary(text: &str) {
    let lowered = text.to_lowercase();
    assert!(
        !lowered.contains("no such file or directory") && !lowered.contains("os error 2"),
        "this failed because tmux is missing, not because the PANE is missing: {text}"
    );
}

/// A pane running the composer emulator. `mode` is `submit` (a healthy target)
/// or `deaf` (one that swallows the CR, the measured busy-target failure).
struct ComposerPane {
    name: String,
    log: PathBuf,
    _dir: tempfile::TempDir,
}

impl ComposerPane {
    fn start(tag: &str, mode: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("accepted.bin");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/composer_pane/emulator.py");
        let name = format!("ainb-send-composer-{tag}-{}", std::process::id());
        let status = tmux()
            .args([
                "new-session",
                "-d",
                "-s",
                &name,
                "-x",
                "80",
                "-y",
                "24",
                &format!(
                    "sh -c 'stty raw -echo; exec python3 {} {mode} {}'",
                    script.display(),
                    log.display()
                ),
            ])
            .status()
            .expect("spawning tmux");
        assert!(status.success(), "tmux new-session failed");
        let pane = Self {
            name,
            log,
            _dir: dir,
        };
        assert!(
            wait_until(Duration::from_secs(10), || pane
                .capture()
                .contains("status line")),
            "the emulator never drew its first frame: {}",
            pane.capture()
        );
        pane
    }

    fn capture(&self) -> String {
        let out = tmux()
            .args(["capture-pane", "-t", &self.name, "-p"])
            .output()
            .expect("capture-pane");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Every message the target ACCEPTED, in order.
    fn accepted(&self) -> Vec<String> {
        let raw = std::fs::read(&self.log).unwrap_or_default();
        raw.split(|b| *b == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect()
    }
}

impl Drop for ComposerPane {
    fn drop(&mut self) {
        // Kill ONLY this test's session, by exact name, on the private server.
        let _ = tmux().args(["kill-session", "-t", &self.name]).status();
    }
}

fn python3_available() -> bool {
    Command::new("python3").arg("-V").output().is_ok_and(|o| o.status.success())
}

/// A heartbeat-shaped payload: one long line, distinctive head, distinctive
/// tail, long enough that its head cannot fit the emulator's viewport.
fn head_and_tail_payload() -> String {
    let mut body =
        "HEADMARKER7788 [HEARTBEAT 1782949818090] 16 session(s) need attention. ".to_string();
    while body.len() < 2360 {
        body.push_str("ignore this payload, it is a test. ");
    }
    body.truncate(2360);
    body.push_str("TAILMARKER9911");
    body
}

#[tokio::test]
async fn a_target_that_swallows_the_enter_is_never_reported_as_delivered() {
    if !tmux_available() || !python3_available() {
        skip("deaf-target send verification");
        return;
    }
    // THE P0 IN ONE TEST. The target ingests the payload and then swallows
    // every CR, exactly as a busy claude does when the Enter arrives fused onto
    // the tail of a payload read. The payload is parked in the composer; the
    // send path must say so.
    //
    // This is the test that dies if `tmux_send`'s attempt loop is replaced by
    // one blind `tmux_press_enter` and `Ok(())`.
    let pane = ComposerPane::start("deaf", "deaf");
    let payload = head_and_tail_payload();

    let error = tmux_send(&pane.name, &payload)
        .await
        .expect_err("a payload left in the composer is NOT delivered");
    let text = format!("{error:#}");
    assert_not_a_missing_binary(&text);
    assert!(
        text.contains("NOT submitted") && text.contains("parked"),
        "the error must name the real failure, got: {text}"
    );
    assert_eq!(
        error
            .downcast_ref::<SendError>()
            .map(SendError::failure)
            .expect("a typed SendError"),
        SendFailure::Written,
        "the bytes ARE in the pane, so no other transport may resend them"
    );
    assert!(
        pane.accepted().is_empty(),
        "the target never accepted anything, by construction"
    );
    assert!(
        pane.capture().contains("TAILMARKER9911"),
        "the payload is still parked in the composer: {}",
        pane.capture()
    );
}

#[tokio::test]
async fn a_composer_that_never_shows_the_payload_is_never_enter_ed_into() {
    if !tmux_available() || !python3_available() {
        skip("unobserved-composer refusal");
        return;
    }
    // THE OTHER HALF OF THE GATE CONTRACT. A composer EXISTS here but never
    // renders the payload, which is the measured shape of a pane that has not
    // started draining. Pressing Enter into it is exactly the CR-fusion
    // condition, so the send path must refuse, and it must NOT report delivery.
    //
    // The sibling half, a pane with NO composer at all, is accepted instead:
    // see `a_long_single_line_payload_arrives_byte_for_byte_exactly_once`. The
    // pair is what keeps the composer-less exemption narrow.
    let pane = ComposerPane::start("mute", "mute");
    let payload = head_and_tail_payload();

    let error = tmux_send(&pane.name, &payload)
        .await
        .expect_err("a composer that never showed the payload is not delivery");
    let text = format!("{error:#}");
    assert_not_a_missing_binary(&text);
    assert!(
        text.contains("never appeared in the composer"),
        "the error must name the real failure, got: {text}"
    );
    assert_eq!(
        error
            .downcast_ref::<SendError>()
            .map(SendError::failure)
            .expect("a typed SendError"),
        SendFailure::Written,
        "the bytes ARE in the pane, so no other transport may resend them"
    );
    assert!(
        pane.accepted().is_empty(),
        "Enter must NOT be pressed into a composer that has not shown the payload, got {:?}",
        pane.accepted()
    );
}

#[tokio::test]
async fn a_target_that_submits_is_reported_as_delivered_exactly_once() {
    if !tmux_available() || !python3_available() {
        skip("healthy-target send verification");
        return;
    }
    let pane = ComposerPane::start("healthy", "submit");
    let payload = head_and_tail_payload();

    tmux_send(&pane.name, &payload)
        .await
        .expect("a target that submits must be reported as delivered");

    let accepted = pane.accepted();
    assert_eq!(accepted.len(), 1, "exactly one turn, got {accepted:?}");
    assert_eq!(accepted[0], payload, "the payload was corrupted in flight");
}

#[tokio::test]
async fn a_payload_whose_head_scrolled_off_screen_is_still_verified() {
    if !tmux_available() || !python3_available() {
        skip("tail-anchored viewport verification");
        return;
    }
    // CRITICAL-1, end to end. The emulator's viewport is tail-anchored and
    // short, so this payload's head is NOT on the screen the verifier reads,
    // which is the measured 80x24 behaviour. A head-derived needle sees nothing
    // and reports success; the tail needle sees the parked payload.
    let deaf = ComposerPane::start("headoff", "deaf");
    let payload = head_and_tail_payload();
    let error = tmux_send(&deaf.name, &payload).await.expect_err("parked");
    assert_not_a_missing_binary(&format!("{error:#}"));

    let screen = deaf.capture();
    assert!(
        !screen.contains("HEADMARKER7788") && !screen.contains("[HEARTBEAT"),
        "precondition: the payload HEAD must be off-screen, got: {screen}"
    );
    assert!(
        screen.contains("TAILMARKER9911"),
        "precondition: the payload TAIL must be on-screen, got: {screen}"
    );
}

#[tokio::test]
async fn concurrent_sends_to_one_pane_are_serialised_not_interleaved() {
    if !tmux_available() {
        skip("per-pane send lock");
        return;
    }
    // Symptom B at the byte level: two overlapping senders produce one read
    // shaped `[tail of A][CR][head of B]`, so the target submits at the
    // embedded CR and the head of B is stranded in the now-empty composer.
    // Only mutual exclusion over the whole write + Enter sequence prevents it.
    let pane = CatPane::start("lock");
    let alpha: String = "AAAA".repeat(600);
    let bravo: String = "BBBB".repeat(600);

    let (first, second) =
        tokio::join!(tmux_send(&pane.name, &alpha), tmux_send(&pane.name, &bravo));
    // A bare `cat` has no composer, so both sends are accepted unverified, as
    // `main` accepted them. What is under test here is the BYTES.
    for outcome in [&first, &second] {
        assert!(
            outcome.is_ok(),
            "a pane with no composer must not be reported as a failure: {outcome:?}"
        );
    }

    let expected = alpha.len() + bravo.len() + 2;
    assert!(
        wait_until(Duration::from_secs(10), || file_len(&pane.out) >= expected),
        "only {} of {expected} bytes arrived",
        file_len(&pane.out)
    );
    let got = String::from_utf8(pane.received()).expect("ascii payloads");
    let a_then_b = format!("{alpha}\r{bravo}\r");
    let b_then_a = format!("{bravo}\r{alpha}\r");
    assert!(
        got == a_then_b || got == b_then_a,
        "the two sends interleaved. first 80 bytes: {:?}",
        &got[..80.min(got.len())]
    );
}

#[tokio::test]
async fn a_long_single_line_payload_arrives_byte_for_byte_exactly_once() {
    if !tmux_available() {
        skip("long-payload send integrity");
        return;
    }
    // 2400 chars, no newline: the exact shape of an ATC heartbeat body. It goes
    // out as ONE literal write (round 1 split it into sub-801-byte chunks,
    // which suppressed the paste placeholder the ATC coalesce guard reads), so
    // this pins losslessness, ordering, and exactly-once delivery.
    let pane = CatPane::start("single-write");
    let payload: String = "MARKERSTART7788 ignore this payload. "
        .chars()
        .cycle()
        .take(2360)
        .collect::<String>()
        + "MARKEREND9911";
    assert!(!payload.contains('\n'));
    assert!(payload.len() > 2000);

    // A bare `cat` pane has no composer to verify against, so the send is
    // reported as delivered on the strength of its WIRE behaviour alone: one
    // literal write and one Enter, byte for byte what `main` sent. Reporting a
    // failure here instead flipped every hangar send to an unparseable pane to
    // FAILED and left the ATC inbox undrained; see `gate_action` in
    // `src/fleet/send/tmux.rs` for the rationale and the residual hole.
    tmux_send(&pane.name, &payload)
        .await
        .expect("a pane with no composer is delivered, exactly as on main");

    // Enter is delivered as CR, so the pane receives payload + exactly one \r:
    // an unverifiable pane must not be hammered with the whole retry budget.
    let expected = payload.len() + 1;
    assert!(
        wait_until(Duration::from_secs(10), || file_len(&pane.out) >= expected),
        "only {} of {expected} bytes arrived",
        file_len(&pane.out)
    );
    let got = pane.received();
    assert_eq!(got.len(), expected, "extra bytes: the payload was resent");
    assert_eq!(
        String::from_utf8_lossy(&got[..payload.len()]),
        payload,
        "payload was reordered or corrupted"
    );
    assert_eq!(got[payload.len()], b'\r', "submit key must follow the text");
}

#[tokio::test]
async fn a_multibyte_payload_survives_the_wire() {
    if !tmux_available() || !python3_available() {
        skip("multi-byte payload integrity");
        return;
    }
    // PTY reads split at arbitrary byte offsets, so a payload that is cut mid
    // character anywhere on the path arrives as replacement characters.
    let pane = ComposerPane::start("utf8", "submit");
    let payload: String = "日本語テキスト ".chars().cycle().take(1200).collect();
    assert!(payload.len() > 2400, "multi-byte: bytes exceed chars");

    tmux_send(&pane.name, &payload).await.expect("send");

    let accepted = pane.accepted();
    assert_eq!(
        accepted.len(),
        1,
        "exactly one turn, got {}",
        accepted.len()
    );
    assert_eq!(accepted[0], payload, "multi-byte payload was corrupted");
}

#[tokio::test]
async fn capturing_a_missing_pane_is_an_error_not_an_empty_pane() {
    if !tmux_available() {
        skip("missing-pane capture");
        return;
    }
    // Regression: `tmux capture-pane -t <missing>` exits 1 with EMPTY stdout
    // (the diagnostic goes to stderr). Returning that as `Ok("")` made every
    // capture failure look like a clean, empty composer, which is how the send
    // path concluded "submitted" for a session that had died mid-send.
    let (missing, _live) = absent_pane("capture");
    let error = capture_pane(&missing, 0)
        .await
        .expect_err("a missing pane must not read as an empty pane");
    let text = format!("{error:#}");
    assert_not_a_missing_binary(&text);
    assert!(
        text.contains(&missing),
        "the error must name the pane, got: {text}"
    );
}

#[tokio::test]
async fn sending_to_a_missing_pane_fails() {
    if !tmux_available() {
        skip("missing-pane send");
        return;
    }
    let (missing, _live) = absent_pane("send");
    let error = tmux_send(&missing, "continue")
        .await
        .expect_err("a send to a pane that does not exist must not report success");
    // A live server refusing an absent target is the only acceptable reason to
    // be here; `absent_pane` has already proven tmux runs and the server is up.
    assert_not_a_missing_binary(&format!("{error:#}"));
    // HIGH-5: a pane that died before the first write wrote NOTHING, so the
    // router must still be allowed to reach that session over the broker.
    // Round 1 collapsed this into "typed but not submitted" and suppressed the
    // fallback, leaving the message delivered by no transport at all.
    assert_eq!(
        error
            .downcast_ref::<SendError>()
            .map(SendError::failure)
            .expect("a typed SendError"),
        SendFailure::NothingWritten,
        "got: {error:#}"
    );
}

/// The isolation contract: a pane this file creates must NOT exist on the
/// ambient/default tmux server, i.e. not among the developer's real sessions
/// and not where a sibling test binary's teardown can reach it.
#[test]
fn panes_live_on_a_private_tmux_server_not_the_ambient_one() {
    if !tmux_available() {
        skip("tmux server isolation");
        return;
    }
    let pane = CatPane::start("isolation");
    let private = tmux()
        .args(["has-session", "-t", &pane.name])
        .output()
        .expect("tmux has-session (private)");
    let ambient = ambient_tmux()
        .args(["has-session", "-t", &pane.name])
        .output()
        .expect("tmux has-session (ambient)");

    assert!(
        private.status.success(),
        "the pane must exist on this file's private server"
    );
    assert!(
        !ambient.status.success(),
        "the pane leaked onto the AMBIENT tmux server, alongside the developer's \
         own sessions, where a sibling's teardown can kill the server under us"
    );
    assert!(isolation().dir.is_dir(), "private socket dir must exist");
}

/// The socket dir must not outlive what needs it. Before this, every run left
/// one `/tmp/ainb-send-integrity-tmux-*` behind for good.
#[test]
fn a_private_socket_dir_is_removed_and_a_live_one_is_left_alone() {
    let victim = PathBuf::from("/tmp").join(format!(
        "{SOCKET_DIR_PREFIX}{}-droptest",
        std::process::id()
    ));
    std::fs::create_dir_all(&victim).expect("create probe dir");
    drop(TmuxIsolation {
        dir: victim.clone(),
        ambient: Vec::new(),
    });
    assert!(!victim.exists(), "Drop must remove its own socket dir");

    // The sweep's live-pid guard: this process is alive, so its dir survives.
    if tmux_available() {
        sweep_dead_socket_dirs();
        assert!(
            isolation().dir.is_dir(),
            "the sweep must never take a LIVE process's socket dir"
        );
    }
}
