//! [`AbtopPlugin`] — the SDK `Plugin` trait impl that the binary serves
//! over stdio.
//!
//! Detect-on-init populates lifecycle; render dispatches to the install
//! hint (Missing) or the ready hint (Ready); `handle_key` handles `r`
//! (re-detect when not Ready) via the pure-dispatch + async-side-effect
//! pattern witr uses; `cli_dispatch` execs `abtop --once` and prints
//! the text.

use async_trait::async_trait;

use ainb_plugin_sdk::{
    CliOutput, HandleEventParams, HandleKeyParams, HostClient, InitContext, KeyCode, Plugin,
    RenderParams, Result, WireBuffer,
};

use crate::cli::{self};
use crate::detect::detect_abtop;
use crate::exec::{ExecResult, exec_abtop_once};
use crate::render::empty;
use crate::state::{Lifecycle, UiMode};

/// The canonical manifest bytes. Compiled into the binary so the SDK's
/// `plugin/init` handler can echo `name`/`version` back to the host
/// without a runtime file read.
const MANIFEST_TOML: &str = include_str!("../manifest.toml");

/// Plugin state.
pub struct AbtopPlugin {
    /// Resolved abtop binary path (set by `on_init` after detect).
    /// `None` while lifecycle is Missing/Unknown.
    abtop_path: Option<std::path::PathBuf>,
    /// Current lifecycle gate — drives the render dispatcher.
    lifecycle: Lifecycle,
    /// Current input mode. Only `Browsing` today.
    ui_mode: UiMode,
}

impl Default for AbtopPlugin {
    fn default() -> Self {
        Self {
            abtop_path: None,
            lifecycle: Lifecycle::Unknown,
            ui_mode: UiMode::default(),
        }
    }
}

#[async_trait]
impl Plugin for AbtopPlugin {
    fn manifest(&self) -> &'static str {
        MANIFEST_TOML
    }

    async fn on_init(&mut self, _host: &HostClient, _ctx: InitContext<'_>) -> Result<()> {
        // Idempotent. The binary already detected in `AbtopPlugin::detected`
        // before the stdio server started, so this normally re-affirms the
        // same answer; it matters only for a plugin built via `Default`.
        self.refresh_detect().await;
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
                    "checking abtop…",
                );
            }
            Lifecycle::Missing(reason) => {
                empty::render_missing(&mut buf, params.viewport, reason);
            }
            Lifecycle::Ready => {
                empty::render_ready(&mut buf, params.viewport);
            }
        }
        Ok(buf)
    }

    async fn handle_key(&mut self, _host: &HostClient, params: HandleKeyParams) -> Result<()> {
        // Capture pre-state so we can spot a transition that needs an
        // async follow-up (re-detect on empty-state `r`). The pure
        // dispatcher mutates state synchronously; we run the side
        // effect afterwards.
        let was_ready = matches!(self.lifecycle, Lifecycle::Ready);

        self.dispatch_key_pure(&params.key.code);

        // `r` on a non-Ready screen — the dispatcher just bounced
        // lifecycle to Unknown. Run a real re-detect so the user
        // actually transitions to Ready when abtop appears on PATH.
        let r_pressed = matches!(params.key.code, KeyCode::Char { ch: 'r' });
        if r_pressed && !was_ready && matches!(self.lifecycle, Lifecycle::Unknown) {
            self.refresh_detect().await;
        }

        Ok(())
    }

    async fn cli_dispatch(
        &mut self,
        _host: &HostClient,
        namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        // The host channel is unused — abtop's CLI execs the binary
        // directly. Delegate to the host-free core so tests can drive
        // it without a HostClient (the SDK exposes no test constructor).
        Ok(self.cli_dispatch_core(namespace, argv).await)
    }

    async fn handle_event(&mut self, _host: &HostClient, _params: HandleEventParams) -> Result<()> {
        Ok(())
    }
}

impl AbtopPlugin {
    /// Build a plugin that has ALREADY resolved whether abtop is on
    /// PATH, so [`Lifecycle::Unknown`] never reaches the wire.
    ///
    /// The host does not wait for `plugin/init` to be answered before it
    /// sends `plugin/render` (`ainb-plugin-runtime`'s `send_init` writes
    /// the request and returns; `spawn_and_init` marks the plugin Running
    /// immediately), and the SDK serves `plugin/init` and `plugin/render`
    /// on independent tasks. A render can therefore be answered before
    /// `on_init` has run. Every other plugin needs that window because its
    /// startup work is genuinely slow (a subprocess exec, a socket dial)
    /// and the transient IS the wanted UI. abtop's is a single `which`
    /// lookup with no blocking call in it, so paying for a "checking
    /// abtop…" flash buys nothing: resolve it here, before the stdio
    /// server reads its first frame, and the screen is correct from frame
    /// zero.
    pub async fn detected() -> Self {
        let mut plugin = Self::default();
        plugin.refresh_detect().await;
        plugin
    }

    /// Run detection and apply it to lifecycle + the cached binary path.
    /// The single place that maps a [`crate::detect::DetectResult`] onto
    /// state, shared by construction, `on_init`, and the `r` re-detect.
    async fn refresh_detect(&mut self) {
        let result = detect_abtop().await;
        match &result {
            crate::detect::DetectResult::Ready { path } => {
                self.abtop_path = Some(path.clone());
            }
            crate::detect::DetectResult::Missing(_) => {
                self.abtop_path = None;
            }
        }
        self.lifecycle = Lifecycle::from_detect(result);
    }

    /// Host-free body of [`Plugin::cli_dispatch`]. Public (doc-hidden)
    /// so integration tests can exercise the full `ainb abtop` surface
    /// against a stub binary without constructing a [`HostClient`].
    #[doc(hidden)]
    pub async fn cli_dispatch_core(&mut self, namespace: &str, argv: &[String]) -> CliOutput {
        if namespace != "abtop" {
            return CliOutput {
                stdout: Vec::new(),
                stderr: format!("abtop: unknown namespace `{namespace}`\n").into_bytes(),
                exit_code: 2,
            };
        }

        // `--format` is a host-global flag — pull it out before clap
        // (which doesn't declare it) parses the abtop surface. abtop
        // has no JSON mode, so the value is informational only.
        let _format = cli::extract_format(argv);
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

        // No usable abtop binary — print the install hint to stdout and
        // exit 1 (acceptance: "Missing-abtop CLI exit 1").
        let Some(path) = self.abtop_path.clone() else {
            // Full docsite URL on its own line: terminals auto-linkify it, it
            // stays copyable, and it never truncates.
            let hint = format!(
                "abtop not found on PATH. Install: {}  (or `cargo install abtop`)\n\
                 What it does + screenshots: https://stevengonsalvez.github.io/agents-in-a-box/plugins/abtop/\n",
                platform_install_command(),
            );
            return CliOutput {
                stdout: hint.into_bytes(),
                stderr: Vec::new(),
                exit_code: 1,
            };
        };

        match exec_abtop_once(&path, args.forward_args()).await {
            ExecResult::Ok(text) => CliOutput::ok(text.into_bytes()),
            ExecResult::Timeout => CliOutput::err(b"abtop: --once timed out\n".to_vec()),
            ExecResult::NonZero { code, stderr } => CliOutput {
                stdout: Vec::new(),
                stderr: format!("abtop exited {}: {stderr}\n", code.unwrap_or(-1)).into_bytes(),
                exit_code: code.unwrap_or(1),
            },
            ExecResult::SpawnFailed(e) => {
                CliOutput::err(format!("abtop: spawn failed: {e}\n").into_bytes())
            }
        }
    }

    /// Pure key-dispatch — applies every non-host-touching binding to
    /// `self` and returns nothing. Lets unit tests exercise the state
    /// machine without spinning up a `HostClient`.
    ///
    /// The `r` key needs host I/O to re-detect — its async path lives
    /// in `handle_key`. The pure dispatcher only bounces lifecycle to
    /// `Unknown` for empty-state re-detect.
    pub fn dispatch_key_pure(&mut self, code: &KeyCode) {
        if let KeyCode::Char { ch: 'r' } = *code {
            // `r` on an empty-state screen triggers a re-detect — bounce
            // lifecycle to Unknown so the next render shows
            // "checking abtop…" and `handle_key` runs a real re-detect.
            // On Ready, `r` is a no-op here (nothing to refresh — the
            // live view is host-side).
            if !matches!(self.lifecycle, Lifecycle::Ready) {
                self.lifecycle = Lifecycle::Unknown;
            }
        }
    }

    /// Test seam: mark the plugin Ready with a known abtop binary path
    /// (bypasses the async detect handshake). Integration tests stage a
    /// stub binary and inject it here.
    #[doc(hidden)]
    pub fn set_ready_for_test(&mut self, abtop_path: std::path::PathBuf) {
        self.abtop_path = Some(abtop_path);
        self.lifecycle = Lifecycle::Ready;
    }

    /// Test seam: inspect the lifecycle gate.
    #[doc(hidden)]
    #[must_use]
    pub fn lifecycle_for_test(&self) -> &Lifecycle {
        &self.lifecycle
    }

    /// Test seam: inspect the current UI mode.
    #[doc(hidden)]
    #[must_use]
    pub fn ui_mode_for_test(&self) -> UiMode {
        self.ui_mode
    }
}

/// The platform's primary install command, for the CLI missing-binary
/// hint. Mirrors `render::empty`'s platform note without dragging the
/// full `InstallHint` struct into the CLI path.
fn platform_install_command() -> &'static str {
    match std::env::consts::OS {
        "macos" => "brew install graykode/tap/abtop",
        "linux" => {
            "curl -sSL https://github.com/graykode/abtop/releases/latest/download/abtop-installer.sh | sh"
        }
        _ => "see https://github.com/graykode/abtop",
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
    /// through the async trait method (which needs a `HostClient` only
    /// the SDK can construct). Mirrors the render dispatcher.
    fn render_test(p: &mut AbtopPlugin, viewport: Viewport) -> WireBuffer {
        let mut buf = WireBuffer::new(viewport.width, viewport.height);
        match &p.lifecycle {
            Lifecycle::Unknown => {
                paint_centered_hint(&mut buf, viewport.width, viewport.height, "checking abtop…");
            }
            Lifecycle::Missing(reason) => empty::render_missing(&mut buf, viewport, reason),
            Lifecycle::Ready => empty::render_ready(&mut buf, viewport),
        }
        buf
    }

    #[test]
    fn r_in_empty_state_bounces_lifecycle_to_unknown() {
        let mut p = AbtopPlugin::default();
        p.lifecycle = Lifecycle::Missing(crate::detect::MissingReason::NotOnPath);

        p.dispatch_key_pure(&KeyCode::Char { ch: 'r' });
        assert_eq!(p.lifecycle, Lifecycle::Unknown);
    }

    #[test]
    fn r_on_ready_is_a_no_op() {
        let mut p = AbtopPlugin::default();
        p.lifecycle = Lifecycle::Ready;
        p.dispatch_key_pure(&KeyCode::Char { ch: 'r' });
        assert_eq!(p.lifecycle, Lifecycle::Ready, "r on Ready must not bounce");
    }

    #[test]
    fn non_r_keys_are_ignored_by_pure_dispatch() {
        let mut p = AbtopPlugin::default();
        p.lifecycle = Lifecycle::Missing(crate::detect::MissingReason::NotOnPath);
        p.dispatch_key_pure(&KeyCode::Char { ch: 'x' });
        assert!(matches!(p.lifecycle, Lifecycle::Missing(_)));
    }

    #[test]
    fn render_dispatches_to_empty_state_when_missing() {
        let mut p = AbtopPlugin::default();
        p.lifecycle = Lifecycle::Missing(crate::detect::MissingReason::NotOnPath);
        let buf = render_test(&mut p, vp(100, 20));
        assert!(buffer_contains(&buf, "abtop not found"));
        assert!(buffer_contains(&buf, "cargo install abtop"));
    }

    #[test]
    fn render_shows_ready_hint_when_ready() {
        let mut p = AbtopPlugin::default();
        p.lifecycle = Lifecycle::Ready;
        let buf = render_test(&mut p, vp(80, 20));
        assert!(buffer_contains(&buf, "abtop is installed"));
        assert!(buffer_contains(&buf, "press t"));
    }

    #[test]
    fn lifecycle_unknown_renders_centered_hint() {
        let mut p = AbtopPlugin::default();
        assert_eq!(p.lifecycle, Lifecycle::Unknown);
        let buf = render_test(&mut p, vp(40, 6));
        assert!(buffer_contains(&buf, "checking abtop"));
    }

    /// The invariant that keeps abtop's frames deterministic: the binary
    /// hands `Server` an already-detected plugin, so the host can never be
    /// answered with the `Unknown` placeholder no matter how the init and
    /// render tasks interleave. Holds whether or not abtop is installed on
    /// the machine running the test.
    #[tokio::test]
    async fn detected_plugin_is_never_unknown() {
        let p = AbtopPlugin::detected().await;
        assert_ne!(
            p.lifecycle_for_test(),
            &Lifecycle::Unknown,
            "detected() must resolve lifecycle before the server loop starts"
        );
    }

    #[test]
    fn default_plugin_is_unknown_and_browsing() {
        let p = AbtopPlugin::default();
        assert_eq!(p.lifecycle_for_test(), &Lifecycle::Unknown);
        assert_eq!(p.ui_mode_for_test(), UiMode::Browsing);
    }
}
