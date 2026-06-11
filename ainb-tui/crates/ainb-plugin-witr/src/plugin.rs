//! [`WitrPlugin`] — the SDK `Plugin` trait impl that the binary serves
//! over stdio.
//!
//! cfx.5 wires the full UI loop: detect-on-init populates lifecycle,
//! render dispatches to empty-state (cfx.4) or per-tab painters, and
//! `handle_key` routes 1/2/3/4 (tab switch), `r` (refresh — coalesced
//! via [`ScanGate`]), `t` (target entry), `/` (open detail overlay),
//! `q` (close detail). cfx.6 paints the detail overlay modal on top
//! of the tab body when `ui_mode` is `DetailOpen`.

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;

use ainb_plugin_sdk::{
    CliOutput, HandleEventParams, HandleKeyParams, HostClient, InitContext, KeyCode, Plugin,
    RenderParams, Result, Viewport, WireBuffer,
};

use crate::cli::{self, OutputFormat};
use crate::detect::detect_witr;
use crate::exec::{
    ExecResult, PassthroughResult, WitrTarget, exec_witr_json, exec_witr_passthrough,
};
use crate::model::WitrSnapshot;
use crate::render::{containers, detail, empty, locks, ports, processes, tabs};
use crate::slash::{SlashError, parse_slash};
use crate::state::{Lifecycle, ScanGate, SnapshotCache, Tab, UiMode};

/// The canonical manifest bytes. Compiled into the binary so the SDK's
/// `plugin/init` handler can echo `name`/`version` back to the host
/// without a runtime file read.
const MANIFEST_TOML: &str = include_str!("../manifest.toml");

/// Plugin state.
pub struct WitrPlugin {
    /// Resolved witr binary path (set by `on_init` after detect).
    /// `None` while lifecycle is Missing/Outdated/Unknown.
    witr_path: Option<std::path::PathBuf>,
    /// Current lifecycle gate — drives the render dispatcher.
    lifecycle: Lifecycle,
    /// Active tab inside the screen body.
    current_tab: Tab,
    /// Current target the user is investigating. Empty until they
    /// press `t` and commit one.
    current_target: String,
    /// Snapshot cache — coalesces repeated `r` presses + bounds memory.
    cache: SnapshotCache,
    /// In-flight scan gate — second `r` while a scan runs is a no-op.
    /// Exercised by `handle_key`'s `r`-while-Ready path (cfx.5) and
    /// by cfx.7's CLI dispatch.
    scan_gate: ScanGate,
    /// Current input mode (browsing / typing-target / detail-open).
    ui_mode: UiMode,
}

impl Default for WitrPlugin {
    fn default() -> Self {
        Self {
            witr_path: None,
            lifecycle: Lifecycle::Unknown,
            current_tab: Tab::Processes,
            current_target: String::new(),
            cache: SnapshotCache::with_default_capacity(),
            scan_gate: ScanGate::default(),
            ui_mode: UiMode::default(),
        }
    }
}

#[async_trait]
impl Plugin for WitrPlugin {
    fn manifest(&self) -> &'static str {
        MANIFEST_TOML
    }

    async fn on_init(&mut self, _host: &HostClient, _ctx: InitContext<'_>) -> Result<()> {
        let result = detect_witr().await;
        match &result {
            crate::detect::DetectResult::Ready { path, .. } => {
                self.witr_path = Some(path.clone());
            }
            _ => {
                self.witr_path = None;
            }
        }
        self.lifecycle = Lifecycle::from_detect(result);
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, params: RenderParams) -> Result<WireBuffer> {
        let mut buf = WireBuffer::new(params.viewport.width, params.viewport.height);
        match &self.lifecycle {
            Lifecycle::Unknown => {
                // Transient — detect not yet done. Render a single hint.
                paint_centered_hint(
                    &mut buf,
                    params.viewport.width,
                    params.viewport.height,
                    "checking witr…",
                );
            }
            Lifecycle::Missing(reason) => {
                empty::render_missing(&mut buf, params.viewport, reason);
            }
            Lifecycle::Outdated {
                found_version,
                minimum,
            } => {
                empty::render_outdated(&mut buf, params.viewport, found_version, minimum);
            }
            Lifecycle::Ready => {
                render_screen(
                    &mut buf,
                    params.viewport.width,
                    params.viewport.height,
                    self.current_tab,
                    self.current_target.as_str(),
                    cached_snapshot(&mut self.cache, &self.current_target),
                    &self.ui_mode,
                );
            }
        }
        Ok(buf)
    }

    async fn handle_key(&mut self, host: &HostClient, params: HandleKeyParams) -> Result<()> {
        // Capture pre-state so we can spot a transition that needs
        // async follow-up (re-detect on empty-state `r`, refresh on
        // Ready `r`). The pure dispatcher mutates state synchronously;
        // we run the side effect afterwards.
        let pre_lifecycle = self.lifecycle.clone();
        let was_ready = matches!(pre_lifecycle, Lifecycle::Ready);
        let pre_target = self.current_target.clone();

        self.dispatch_key_pure(&params.key.code);

        // `r` on a non-Ready screen — dispatcher just bounced lifecycle
        // to Unknown. Run a real re-detect so the user actually
        // transitions back to Ready when witr appears on PATH.
        let r_pressed = matches!(params.key.code, KeyCode::Char { ch: 'r' });
        if r_pressed && !was_ready && matches!(self.lifecycle, Lifecycle::Unknown) {
            let result = detect_witr().await;
            if let crate::detect::DetectResult::Ready { path, .. } = &result {
                self.witr_path = Some(path.clone());
            } else {
                self.witr_path = None;
            }
            self.lifecycle = Lifecycle::from_detect(result);
        }

        // `r` on Ready with a target set — refresh the cached snapshot
        // by re-exec'ing witr. `current_target` holds the cache-key
        // form; decode it back to a typed target for exec. Coalesced
        // via `ScanGate`; concurrent `r` presses are no-ops.
        if r_pressed && was_ready && !pre_target.is_empty() {
            let _ = self.scan(&WitrTarget::parse(&pre_target), Some(host)).await;
        }

        Ok(())
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        // The host channel is unused — witr's CLI execs the binary
        // directly. Delegate to the host-free core so tests can drive
        // it without a HostClient (the SDK exposes no test constructor).
        Ok(self.cli_dispatch_core(namespace, argv).await)
    }

    async fn handle_event(&mut self, _host: &HostClient, _params: HandleEventParams) -> Result<()> {
        Ok(())
    }
}

impl WitrPlugin {
    /// Host-free body of [`Plugin::cli_dispatch`]. Public (doc-hidden)
    /// so integration tests can exercise the full `ainb witr` surface
    /// against a stub binary without constructing a [`HostClient`].
    #[doc(hidden)]
    pub async fn cli_dispatch_core(&mut self, namespace: &str, argv: &[String]) -> CliOutput {
        if namespace != "witr" {
            return CliOutput {
                stdout: Vec::new(),
                stderr: format!("witr: unknown namespace `{namespace}`\n").into_bytes(),
                exit_code: 2,
            };
        }

        // `--format` is a host-global flag — pull it out before clap
        // (which doesn't declare it) parses the witr surface.
        let format = cli::extract_format(argv);
        let stripped = cli::strip_format_flag(argv);

        let args = match cli::parse_args(&stripped) {
            cli::ParseOutcome::Parsed(a) => *a,
            // `--help` / `--version` → stdout, exit 0 (conventional).
            cli::ParseOutcome::HelpOrVersion(text) => {
                return CliOutput::ok(text.into_bytes());
            }
            cli::ParseOutcome::UsageError(usage) => {
                return CliOutput {
                    stdout: Vec::new(),
                    stderr: usage.into_bytes(),
                    exit_code: 2,
                };
            }
        };

        // `--short` (raw passthrough) + `--format json` is contradictory
        // and can't be a clap conflict (`--format` is stripped before
        // clap sees the args). Reject explicitly rather than silently
        // dropping the requested format.
        if args.short && format == OutputFormat::Json {
            return CliOutput {
                stdout: Vec::new(),
                stderr: b"witr: --short cannot be combined with --format json\n".to_vec(),
                exit_code: 2,
            };
        }

        // Typed target — selects the right witr addressing flag.
        let target = args.resolve_target();

        // No usable witr binary — print the install hint to stdout and
        // exit 1 (acceptance: "Missing-witr CLI exit 1").
        let Some(path) = self.witr_path.clone() else {
            let hint = match &self.lifecycle {
                Lifecycle::Outdated {
                    found_version,
                    minimum,
                } => format!(
                    "witr {found_version} is too old (need >= {minimum}). Upgrade: brew upgrade witr\n"
                ),
                _ => "witr not found on PATH. Install: brew install witr\n".to_string(),
            };
            return CliOutput {
                stdout: hint.into_bytes(),
                stderr: Vec::new(),
                exit_code: 1,
            };
        };

        // `--short` forwards raw `witr <target>` text (no JSON parse).
        if args.short {
            return match exec_witr_passthrough(&path, &target).await {
                PassthroughResult::Ok(out) => CliOutput::ok(out.into_bytes()),
                PassthroughResult::Timeout => CliOutput::err(b"witr: scan timed out\n".to_vec()),
                PassthroughResult::NonZero { code, stderr } => CliOutput {
                    stdout: Vec::new(),
                    stderr: format!("witr exited {}: {stderr}\n", code.unwrap_or(-1)).into_bytes(),
                    exit_code: code.unwrap_or(1),
                },
                PassthroughResult::SpawnFailed(e) => {
                    CliOutput::err(format!("witr: {e}\n").into_bytes())
                }
            };
        }

        // JSON-mode exec, routed through the single `scan()` authority
        // so the cache insert goes through `ScanGate` — one coalescing
        // owner shared by the TUI `r`-refresh and this CLI path. `None`
        // host: a one-shot `ainb witr` run has no live bus subscribers,
        // so the event-bus publish is skipped here.
        match self.scan(&target, None).await {
            Some(ExecResult::Ok(snap)) => {
                let body = if format == OutputFormat::Json {
                    match cli::format_json(&snap) {
                        Ok(s) => s,
                        Err(e) => {
                            return CliOutput::err(
                                format!("witr: failed to serialise snapshot: {e}\n").into_bytes(),
                            );
                        }
                    }
                } else if args.tree {
                    cli::format_tree(&snap)
                } else if args.warnings {
                    cli::format_warnings(&snap)
                } else {
                    cli::format_text(&snap)
                };
                CliOutput::ok(body.into_bytes())
            }
            Some(ExecResult::Timeout) => CliOutput::err(b"witr: scan timed out\n".to_vec()),
            Some(ExecResult::NonZero { code, stderr }) => CliOutput {
                stdout: Vec::new(),
                stderr: format!("witr exited {}: {stderr}\n", code.unwrap_or(-1)).into_bytes(),
                exit_code: code.unwrap_or(1),
            },
            Some(ExecResult::SpawnFailed(e)) => {
                CliOutput::err(format!("witr: spawn failed: {e}\n").into_bytes())
            }
            Some(ExecResult::ParseError { error, .. }) => CliOutput::err(
                format!("witr: could not parse --json output: {error}\n").into_bytes(),
            ),
            // scan() returns None only if a scan for this target is
            // already in flight (ScanGate). The SDK serialises handlers
            // so this is essentially unreachable from the CLI, but
            // handle it rather than panic.
            None => {
                CliOutput::err(b"witr: a scan for this target is already in progress\n".to_vec())
            }
        }
    }

    /// Pure key-dispatch — applies every non-host-touching binding
    /// to `self` and returns nothing. Lets unit tests exercise the
    /// state machine without spinning up a `HostClient`.
    ///
    /// The `r` key needs host I/O to refresh — its async path lives
    /// in `handle_key`. The pure dispatcher only bounces lifecycle
    /// to `Unknown` for empty-state re-detect.
    pub fn dispatch_key_pure(&mut self, code: &KeyCode) {
        if matches!(self.ui_mode, UiMode::EnteringTarget { .. }) {
            apply_target_input_key(code, &mut self.ui_mode, &mut self.current_target);
        } else {
            apply_browsing_key(
                code,
                &mut self.current_tab,
                &mut self.ui_mode,
                &mut self.lifecycle,
            );
        }
    }

    /// Test seam: mark the plugin Ready with a known witr binary path
    /// (bypasses the async detect handshake). Integration tests stage
    /// a stub binary and inject it here.
    #[doc(hidden)]
    pub fn set_ready_for_test(&mut self, witr_path: std::path::PathBuf) {
        self.witr_path = Some(witr_path);
        self.lifecycle = Lifecycle::Ready;
    }

    /// Test seam: inspect the lifecycle gate.
    #[doc(hidden)]
    #[must_use]
    pub fn lifecycle_for_test(&self) -> &Lifecycle {
        &self.lifecycle
    }

    /// Test seam: inspect the committed target.
    #[doc(hidden)]
    #[must_use]
    pub fn current_target_for_test(&self) -> &str {
        &self.current_target
    }

    /// Test seam: is the detail overlay open?
    #[doc(hidden)]
    #[must_use]
    pub fn is_detail_open_for_test(&self) -> bool {
        matches!(self.ui_mode, UiMode::DetailOpen)
    }

    /// Handle a `/witr <target>` slash command: parse the line, set
    /// the current target, scan it, and open the detail overlay (the
    /// "focused overlay panel" the spec calls for). Returns the parse
    /// result so the host can surface a usage error on a malformed line.
    ///
    /// Leaves `current_tab` untouched so the overlay floats over
    /// whatever tab the user was on — honours the cfx.6 tab-preservation
    /// invariant.
    ///
    /// TODO(agents-in-a-box-6qc): no host transport reaches this yet —
    /// the SDK `Plugin` trait has no slash method and the host slash
    /// dispatch is stubbed (host Phase 4). This is the plugin-side
    /// foundation + parser; wire `/witr` → `handle_slash` once the host
    /// lands slash dispatch. Reachable today only from tests.
    pub async fn handle_slash(&mut self, input: &str) -> std::result::Result<(), SlashError> {
        let target = parse_slash(input)?;
        // Store the cache-key form so render's `cached_snapshot` lookup
        // (keyed on `current_target`) matches what `scan` cached.
        self.current_target = target.cache_key();
        // Scan now so the overlay has data; ignore the result — render
        // reads from the cache, and a failed scan falls back to the
        // empty-target hint. `None` host: the slash transport (host
        // Phase 4, bead 6qc) will pass a real host so the snapshot
        // publishes — until then there's no live channel anyway.
        let _ = self.scan(&target, None).await;
        self.ui_mode = UiMode::DetailOpen;
        Ok(())
    }

    /// Run a scan against `target`: exec `witr --json` with the right
    /// addressing flag for the target kind, decode, and insert the
    /// snapshot into the cache (keyed by [`WitrTarget::cache_key`]).
    /// Coalesced via [`ScanGate`] — a second call while a scan is in
    /// flight against the same target returns `None` immediately,
    /// letting the first scan win.
    ///
    /// Called from `handle_key` on `r`-while-Ready, from `handle_slash`,
    /// and from `cli_dispatch`. Returns `None` when there's no witr
    /// binary cached (defensive — we shouldn't be `Ready` without one)
    /// or when a scan is already in flight.
    pub async fn scan(
        &mut self,
        target: &WitrTarget,
        host: Option<&HostClient>,
    ) -> Option<ExecResult> {
        let path = self.witr_path.clone()?;
        let key = target.cache_key();
        if !self.scan_gate.try_acquire(&key) {
            return None;
        }
        let result = exec_witr_json(&path, target).await;
        if let ExecResult::Ok(snap) = &result {
            self.cache.insert(key.clone(), (**snap).clone(), Instant::now());
            // Publish the fresh snapshot on the event bus (cfx.8) when
            // a live host channel is available. The CLI one-shot path
            // passes `None` — no bus subscribers during a `ainb witr`
            // invocation, so publishing there is moot.
            if let Some(host) = host {
                self.publish_snapshot(host, snap).await;
            }
        }
        self.scan_gate.release(&key);
        Some(result)
    }

    /// Publish a snapshot to the `witr.snapshot` topic. Encoding
    /// failures and host-channel errors are logged, not propagated —
    /// a publish failure must never break the scan/render the user
    /// asked for.
    async fn publish_snapshot(&self, host: &HostClient, snap: &WitrSnapshot) {
        match crate::publish::encode_snapshot_payload(snap) {
            Ok(bytes) => {
                if let Err(e) =
                    host.snapshot_publish(crate::publish::WITR_SNAPSHOT_TOPIC, bytes).await
                {
                    tracing::warn!(error = %e, "failed to publish witr.snapshot");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to encode witr.snapshot payload");
            }
        }
    }
}

fn cached_snapshot(cache: &mut SnapshotCache, target: &str) -> Option<Arc<WitrSnapshot>> {
    if target.is_empty() {
        return None;
    }
    cache.get(target, Instant::now())
}

fn apply_browsing_key(
    code: &KeyCode,
    current_tab: &mut Tab,
    ui_mode: &mut UiMode,
    lifecycle: &mut Lifecycle,
) {
    if let KeyCode::Char { ch } = *code {
        match ch {
            '1' => *current_tab = Tab::Processes,
            '2' => *current_tab = Tab::Ports,
            '3' => *current_tab = Tab::Containers,
            '4' => *current_tab = Tab::Locks,
            't' => {
                *ui_mode = UiMode::EnteringTarget {
                    buffer: String::new(),
                };
            }
            '/' => {
                *ui_mode = UiMode::DetailOpen;
            }
            'q' => {
                if matches!(ui_mode, UiMode::DetailOpen) {
                    *ui_mode = UiMode::Browsing;
                }
            }
            // `r` on empty-state screens triggers a re-detect — bounce
            // lifecycle to Unknown so the next render shows "checking
            // witr…" and rely on host re-init or cfx.7 wiring an
            // in-place re-detect. In Ready, `r` is "refresh snapshot",
            // handled in `Plugin::handle_key`'s async path (deferred
            // until cfx.7 wires the CLI dispatch surface).
            'r' if !matches!(lifecycle, Lifecycle::Ready) => {
                *lifecycle = Lifecycle::Unknown;
            }
            _ => {}
        }
    }
}

/// Apply a key while the UI is in `EnteringTarget` mode. Caller
/// guarantees `ui_mode` matches that variant — we re-borrow the
/// inner buffer in-place to satisfy the borrow checker against the
/// outer `&mut ui_mode`.
///
/// Cancellation is `Backspace` on an empty buffer (not `Esc`, which
/// the host reserves to pop the plugin screen back to the home
/// surface — the plugin never sees Esc presses).
fn apply_target_input_key(code: &KeyCode, ui_mode: &mut UiMode, current_target: &mut String) {
    match *code {
        KeyCode::Char { ch } => {
            if let UiMode::EnteringTarget { buffer } = ui_mode {
                // Sane upper bound — matches `validate_target`'s 256-char cap.
                if buffer.chars().count() < 256 {
                    buffer.push(ch);
                }
            }
        }
        KeyCode::Backspace => {
            // Buffer empty → treat as cancel (rebound from Esc).
            // Non-empty → delete the last char.
            if let UiMode::EnteringTarget { buffer } = ui_mode {
                if buffer.is_empty() {
                    *ui_mode = UiMode::Browsing;
                } else {
                    buffer.pop();
                }
            }
        }
        KeyCode::Enter => {
            // Drain the buffer, then transition mode.
            if let UiMode::EnteringTarget { buffer } = ui_mode {
                *current_target = std::mem::take(buffer);
            }
            *ui_mode = UiMode::Browsing;
        }
        _ => {}
    }
}

/// Paint the assembled screen — tab strip at row 0, body below.
fn render_screen(
    buf: &mut WireBuffer,
    width: u16,
    height: u16,
    current_tab: Tab,
    target: &str,
    snapshot: Option<Arc<WitrSnapshot>>,
    ui_mode: &UiMode,
) {
    if width == 0 || height == 0 {
        return;
    }

    tabs::render(buf, width, current_tab);

    // Body area starts at row 1.
    let body_origin = 1u16;
    if body_origin >= height {
        return;
    }
    let body_height = height - body_origin;

    if let UiMode::EnteringTarget { buffer } = ui_mode {
        paint_target_prompt(buf, body_origin, body_height, width, buffer);
        return;
    }

    match snapshot {
        Some(snap) => {
            match current_tab {
                Tab::Processes => processes::render(buf, body_origin, body_height, width, &snap),
                Tab::Ports => ports::render(buf, body_origin, body_height, width, &snap),
                Tab::Containers => containers::render(buf, body_origin, body_height, width, &snap),
                Tab::Locks => locks::render(buf, body_origin, body_height, width, &snap),
            }
            // Detail overlay paints on top of the current tab body so
            // `q` drops straight back to the same tab + state. Works
            // identically on all four tabs because it reads the
            // snapshot's primary process, not tab-specific data.
            if matches!(ui_mode, UiMode::DetailOpen) {
                detail::render_overlay(buf, Viewport { width, height }, &snap);
            }
        }
        None => paint_empty_target_hint(buf, body_origin, body_height, width, target),
    }
}

fn paint_target_prompt(buf: &mut WireBuffer, origin_y: u16, height: u16, width: u16, buffer: &str) {
    if height == 0 {
        return;
    }
    let prompt = format!("target> {buffer}_");
    paint_line(buf, origin_y, width, &prompt);
    if height >= 2 {
        paint_line(
            buf,
            origin_y + 1,
            width,
            "  (Enter to commit · Backspace to cancel)",
        );
    }
}

fn paint_empty_target_hint(
    buf: &mut WireBuffer,
    origin_y: u16,
    height: u16,
    width: u16,
    target: &str,
) {
    if height == 0 {
        return;
    }
    let mut composed: Vec<String> = if target.is_empty() {
        vec![
            "no target selected".to_string(),
            String::new(),
            "press `t` to enter a target".to_string(),
            "(PID · port · container · file · process name)".to_string(),
        ]
    } else {
        // Target set but no cached snapshot — prompt a scan.
        let mut lines = vec![format!("target: {target}")];
        lines.push(String::new());
        lines.push("snapshot missing — press `r` to (re-)scan".to_string());
        lines
    };
    // Paint into [origin_y, origin_y+height).
    let mut y = origin_y;
    let last = origin_y.saturating_add(height);
    for line in composed.drain(..) {
        if y >= last {
            break;
        }
        paint_line(buf, y, width, &line);
        y = y.saturating_add(1);
    }
}

fn paint_centered_hint(buf: &mut WireBuffer, width: u16, height: u16, hint: &str) {
    if width == 0 || height == 0 {
        return;
    }
    let y = height / 2;
    paint_line(buf, y, width, hint);
}

fn paint_line(buf: &mut WireBuffer, y: u16, width: u16, text: &str) {
    use ainb_plugin_sdk::{Cell, Coord};
    let mut x: u16 = 0;
    for ch in text.chars() {
        if x >= width {
            break;
        }
        buf.push(Coord::new(x, y), Cell::new(ch.to_string()));
        x = x.saturating_add(1);
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // Tests build state step-by-step for clarity.
mod tests {
    use super::*;
    use crate::render::test_support::buffer_contains;
    use ainb_plugin_sdk::Viewport;

    fn vp(w: u16, h: u16) -> Viewport {
        Viewport {
            width: w,
            height: h,
        }
    }

    /// Build the same buffer `render()` would produce, without going
    /// through the async trait method (which needs a `HostClient`
    /// only the SDK can construct). Mirrors the render dispatcher
    /// in `Plugin::render`.
    fn render_test(p: &mut WitrPlugin, viewport: Viewport) -> WireBuffer {
        let mut buf = WireBuffer::new(viewport.width, viewport.height);
        match &p.lifecycle {
            Lifecycle::Unknown => {
                paint_centered_hint(&mut buf, viewport.width, viewport.height, "checking witr…");
            }
            Lifecycle::Missing(reason) => empty::render_missing(&mut buf, viewport, reason),
            Lifecycle::Outdated {
                found_version,
                minimum,
            } => empty::render_outdated(&mut buf, viewport, found_version, minimum),
            Lifecycle::Ready => render_screen(
                &mut buf,
                viewport.width,
                viewport.height,
                p.current_tab,
                p.current_target.as_str(),
                cached_snapshot(&mut p.cache, &p.current_target),
                &p.ui_mode,
            ),
        }
        buf
    }

    #[test]
    fn digit_keys_switch_tabs() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;

        p.dispatch_key_pure(&KeyCode::Char { ch: '2' });
        assert_eq!(p.current_tab, Tab::Ports);

        p.dispatch_key_pure(&KeyCode::Char { ch: '4' });
        assert_eq!(p.current_tab, Tab::Locks);

        p.dispatch_key_pure(&KeyCode::Char { ch: '1' });
        assert_eq!(p.current_tab, Tab::Processes);
    }

    #[test]
    fn t_enters_target_input_mode_and_enter_commits() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;

        p.dispatch_key_pure(&KeyCode::Char { ch: 't' });
        assert!(matches!(p.ui_mode, UiMode::EnteringTarget { .. }));

        p.dispatch_key_pure(&KeyCode::Char { ch: 'n' });
        p.dispatch_key_pure(&KeyCode::Char { ch: 'g' });
        p.dispatch_key_pure(&KeyCode::Char { ch: 'i' });

        p.dispatch_key_pure(&KeyCode::Enter);
        assert_eq!(p.current_target, "ngi");
        assert_eq!(p.ui_mode, UiMode::Browsing);
    }

    #[test]
    fn backspace_on_empty_buffer_cancels_target_input() {
        // Esc is host-reserved (pops the plugin screen back to home),
        // so the plugin can't bind it. Cancel is rebound to
        // `Backspace`-on-empty-buffer per the cfx.5 review.
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;

        p.dispatch_key_pure(&KeyCode::Char { ch: 't' });
        // First backspace: buffer is empty -> cancel.
        p.dispatch_key_pure(&KeyCode::Backspace);
        assert_eq!(p.ui_mode, UiMode::Browsing);
        assert_eq!(p.current_target, "");
    }

    #[test]
    fn backspace_removes_last_target_char() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;

        p.dispatch_key_pure(&KeyCode::Char { ch: 't' });
        p.dispatch_key_pure(&KeyCode::Char { ch: 'a' });
        p.dispatch_key_pure(&KeyCode::Char { ch: 'b' });
        p.dispatch_key_pure(&KeyCode::Backspace);
        p.dispatch_key_pure(&KeyCode::Enter);
        assert_eq!(p.current_target, "a");
    }

    #[test]
    fn slash_opens_and_q_closes_detail_overlay() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;

        p.dispatch_key_pure(&KeyCode::Char { ch: '/' });
        assert_eq!(p.ui_mode, UiMode::DetailOpen);

        p.dispatch_key_pure(&KeyCode::Char { ch: 'q' });
        assert_eq!(p.ui_mode, UiMode::Browsing);
    }

    #[test]
    fn detail_overlay_open_close_preserves_current_tab() {
        // Opening + closing the detail overlay on a non-default tab
        // must not lose the tab selection (cfx.6 acceptance: "doesn't
        // lose the underlying tab selection state" + "works on all 4
        // tabs"). current_tab is structurally independent of ui_mode;
        // this pins that invariant.
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;
        p.current_tab = Tab::Containers;

        p.dispatch_key_pure(&KeyCode::Char { ch: '/' });
        assert_eq!(p.ui_mode, UiMode::DetailOpen);
        assert_eq!(
            p.current_tab,
            Tab::Containers,
            "tab preserved while overlay open"
        );

        p.dispatch_key_pure(&KeyCode::Char { ch: 'q' });
        assert_eq!(p.ui_mode, UiMode::Browsing);
        assert_eq!(
            p.current_tab,
            Tab::Containers,
            "tab preserved after overlay close"
        );
    }

    #[test]
    fn r_in_empty_state_bounces_lifecycle_to_unknown() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Missing(crate::detect::MissingReason::NotOnPath);

        p.dispatch_key_pure(&KeyCode::Char { ch: 'r' });
        assert_eq!(p.lifecycle, Lifecycle::Unknown);
    }

    #[test]
    fn render_dispatches_to_empty_state_when_missing() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Missing(crate::detect::MissingReason::NotOnPath);
        let buf = render_test(&mut p, vp(80, 20));
        assert!(buffer_contains(&buf, "witr not found"));
    }

    #[test]
    fn render_shows_no_target_hint_when_ready_without_target() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;
        let buf = render_test(&mut p, vp(80, 20));
        assert!(buffer_contains(&buf, "[Processes]"));
        assert!(buffer_contains(&buf, "no target selected"));
        assert!(buffer_contains(&buf, "press `t`"));
    }

    #[test]
    fn render_shows_target_prompt_in_entering_mode() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;
        p.ui_mode = UiMode::EnteringTarget {
            buffer: "ng".to_string(),
        };
        let buf = render_test(&mut p, vp(80, 20));
        assert!(buffer_contains(&buf, "target> ng"));
        assert!(buffer_contains(&buf, "Enter to commit"));
    }

    #[test]
    fn render_routes_to_per_tab_painter_when_snapshot_cached() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;
        p.current_target = "1234".to_string();
        p.cache.insert(
            "1234".to_string(),
            serde_json::from_value(serde_json::json!({
                "Target": {"Type": "pid", "Value": "1234"},
                "ResolvedTarget": "1234",
                "Process": {"PID": 1234, "PPID": 1, "Command": "nginx"},
                "Ancestry": [],
                "Source": {"Type": "systemd", "Name": "nginx"},
                "Warnings": []
            }))
            .unwrap(),
            Instant::now(),
        );
        let buf = render_test(&mut p, vp(80, 20));
        assert!(buffer_contains(&buf, "PID"));
        assert!(buffer_contains(&buf, "1234"));
        assert!(buffer_contains(&buf, "nginx"));
    }

    #[test]
    fn render_routes_to_ports_painter_after_tab_switch() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;
        p.current_target = "5432".to_string();
        p.current_tab = Tab::Ports;
        p.cache.insert(
            "5432".to_string(),
            serde_json::from_value(serde_json::json!({
                "Target": {"Type": "port", "Value": "5432"},
                "ResolvedTarget": "5432",
                "Process": {"PID": 1, "PPID": 0, "Command": "postgres"},
                "Ancestry": [],
                "Source": {"Type": "systemd", "Name": "pg"},
                "Warnings": [],
                "SocketInfo": {"Port": 5432, "State": "LISTEN", "LocalAddr": "0.0.0.0"}
            }))
            .unwrap(),
            Instant::now(),
        );
        let buf = render_test(&mut p, vp(80, 20));
        assert!(buffer_contains(&buf, "[Ports]"));
        assert!(buffer_contains(&buf, "5432"));
        assert!(buffer_contains(&buf, "LISTEN"));
    }

    #[test]
    fn target_input_caps_at_256_chars() {
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;
        p.dispatch_key_pure(&KeyCode::Char { ch: 't' });
        for _ in 0..300 {
            p.dispatch_key_pure(&KeyCode::Char { ch: 'x' });
        }
        p.dispatch_key_pure(&KeyCode::Enter);
        // Cap is on `chars().count()`, not `len()` (bytes) — pin
        // that explicitly so a regression that mistakes the unit
        // shows up here.
        assert_eq!(p.current_target.chars().count(), 256);
    }

    #[test]
    fn target_input_cap_is_chars_not_bytes() {
        // Multi-byte glyph `ñ` is 2 bytes / 1 char. After 256 of them
        // the buffer is 256 chars (the cap) but 512 bytes.
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;
        p.dispatch_key_pure(&KeyCode::Char { ch: 't' });
        for _ in 0..300 {
            p.dispatch_key_pure(&KeyCode::Char { ch: 'ñ' });
        }
        p.dispatch_key_pure(&KeyCode::Enter);
        assert_eq!(p.current_target.chars().count(), 256);
        assert_eq!(p.current_target.len(), 512, "bytes ≠ chars for multi-byte");
    }

    #[test]
    fn lifecycle_unknown_renders_centered_hint() {
        let mut p = WitrPlugin::default();
        // Default lifecycle is `Unknown`. No state setup needed.
        assert_eq!(p.lifecycle, Lifecycle::Unknown);
        let buf = render_test(&mut p, vp(40, 6));
        assert!(buffer_contains(&buf, "checking witr"));
    }

    #[test]
    fn empty_target_hint_shows_two_lines_when_target_set() {
        // After Major #1 fix: composed lines paint in order without
        // collisions, so both the target row and the "snapshot
        // missing" prompt are visible.
        let mut p = WitrPlugin::default();
        p.lifecycle = Lifecycle::Ready;
        p.current_target = "nginx".into();
        let buf = render_test(&mut p, vp(80, 20));
        assert!(buffer_contains(&buf, "target: nginx"));
        assert!(buffer_contains(&buf, "snapshot missing"));
    }
}
