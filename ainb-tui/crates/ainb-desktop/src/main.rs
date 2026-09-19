//! The desktop window: the embedded host, its executor and the sidecar
//! supervisor wired to the webview.
//!
//! Frames cross one in-process Tauri channel; the webview sends intents back as
//! `invoke("dispatch")`. A terminal tab's bytes cross a channel of their own,
//! with input, resize and acknowledgements as commands. Nothing here listens on
//! a port.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod menu;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Intent, Keymap};
use ainb_desktop::executor::DesktopExecutor;
use ainb_desktop::host::{DesktopHost, FrameSink, agent_status_dialer};
use ainb_desktop::intent::{Refusal, RendererIntent};
use ainb_desktop::shell::Shell;
use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState, SidecarView};
use ainb_desktop::terminal::{TabEvents, TabsView, Terminals, Tmux};
use ainb_hangar_proto::agent_status::AgentState;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Emitter, Manager};

/// The webview's frame channel, once it has subscribed.
#[derive(Clone, Default)]
struct ChannelSink(Arc<Mutex<Option<Channel<FrameBatch>>>>);

impl FrameSink for ChannelSink {
    fn send(&mut self, batch: FrameBatch) {
        let channel = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(channel) = channel.as_ref() {
            if let Err(error) = channel.send(batch) {
                tracing::warn!(%error, "frame batch not delivered to the webview");
            }
        }
    }
}

/// Tab strip changes and tab notices, as webview events.
struct WebviewTabs(tauri::AppHandle);

impl TabEvents for WebviewTabs {
    fn tabs(&self, view: TabsView) {
        if let Err(error) = self.0.emit("terminal_tabs", view) {
            tracing::warn!(%error, "terminal tabs not delivered to the webview");
        }
    }

    fn toast(&self, message: String) {
        if let Err(error) = self.0.emit("toast", message) {
            tracing::warn!(%error, "toast not delivered to the webview");
        }
    }
}

struct Window {
    shell: Shell<ChannelSink>,
    frames: ChannelSink,
    /// `None` when no tmux was found: every attach then fails with a report
    /// naming why, and the tab commands have nothing to act on.
    terminals: Option<Terminals>,
    sidecar: Sidecar,
    sidecar_config: SidecarConfig,
}

/// What the renderer applied, for the proof harness to read from the log: the
/// sections of a batch, how many session rows the sidebar holds, and how many
/// cards each board column draws. Names and counts only, never a body.
///
/// The names arrive as a `Subscription`, which deserializes from the wire
/// names and drops anything else, so the line is bounded by the sections that
/// exist and a renderer cannot name one it never applied. The columns arrive
/// as `AgentState`s, so they are bounded the same way: one per state at most.
#[tauri::command]
fn renderer_applied(sections: Subscription, sessions: usize, board: Vec<(AgentState, usize)>) {
    let named: Vec<&str> = sections.sections().map(ainb_app::wire::section_name).collect();
    let (board, dropped) = ainb_desktop::shell::board_columns(&board);
    if dropped > 0 {
        // Said, not swallowed: a proof reading the line below would otherwise
        // take a renderer that drew no board for one that drew five columns.
        tracing::warn!(
            dropped,
            "renderer applied: the board list ran past the states"
        );
    }
    tracing::info!(sections = ?named, sessions, board = ?board, "renderer applied");
}

/// The terminal's copy: put the selection on the platform clipboard.
///
/// A webview cannot reach the clipboard under this CSP, and the pane's own
/// ctrl+shift+c never leaves the PTY, so the shell does it. Bounded by the
/// same limit as typed input.
#[tauri::command]
fn clipboard_write(text: String) {
    // Checked before the platform call: an oversized selection never reaches
    // the clipboard, and a box with no display refuses it the same way.
    let bytes = text.len();
    let Some(text) = ainb_desktop::clipboard::within_limit(text) else {
        tracing::warn!(bytes, "clipboard write over 1 MiB refused");
        return;
    };
    if let Err(error) = arboard::Clipboard::new().and_then(|mut board| board.set_text(text)) {
        tracing::warn!(%error, "the selection did not reach the clipboard");
    }
}

/// The terminal's paste: the clipboard's text for the pane `key` is showing.
///
/// Answered only for the tab the window has in front of the operator, which is
/// the only caller: paste is a pane's own accelerator, so no other renderer
/// path, and no driver on a `wdio` build, reads what was last copied. Empty
/// when the clipboard holds no text, cannot be read, or holds more than a
/// pane's input limit, and an oversized clipboard says so on a toast rather
/// than pasting nothing in silence.
#[tauri::command]
fn clipboard_read(app: tauri::AppHandle, window: tauri::State<'_, Window>, key: String) -> String {
    let showing = window.terminals.as_ref().is_some_and(|terminals| terminals.showing(&key));
    if !showing {
        tracing::warn!(
            tab = key,
            "clipboard read for a tab that is not in view; refused"
        );
        return String::new();
    }
    match arboard::Clipboard::new().and_then(|mut board| board.get_text()) {
        Ok(text) => {
            let bytes = text.len();
            ainb_desktop::clipboard::within_limit(text).unwrap_or_else(|| {
                tracing::warn!(bytes, "clipboard read over 1 MiB refused");
                WebviewTabs(app)
                    .toast("what was copied is over 1 MiB; it was not pasted".to_string());
                String::new()
            })
        }
        Err(error) => {
            tracing::warn!(%error, "the clipboard was not read");
            String::new()
        }
    }
}

/// Every command the palette may offer, with whether each is active now.
#[tauri::command]
fn palette(window: tauri::State<'_, Window>) -> Vec<ainb_desktop::host::PaletteEntry> {
    window.shell.palette()
}

/// The tab strip, for the webview's first paint.
#[tauri::command]
fn terminal_tabs(window: tauri::State<'_, Window>) -> TabsView {
    window.terminals.as_ref().map_or(
        TabsView {
            tabs: Vec::new(),
            focus: None,
        },
        Terminals::view,
    )
}

/// Send the tab's output to `bytes` as raw buffers. `false` for an unknown tab.
#[tauri::command]
fn terminal_output(
    window: tauri::State<'_, Window>,
    key: String,
    bytes: Channel<InvokeResponseBody>,
) -> bool {
    window.terminals.as_ref().is_some_and(|terminals| {
        terminals.attach_output(
            &key,
            Box::new(move |chunk| bytes.send(InvokeResponseBody::Raw(chunk)).is_ok()),
        )
    })
}

/// The webview painted `bytes` of the tab's output.
#[tauri::command]
fn terminal_ack(window: tauri::State<'_, Window>, key: String, bytes: usize) {
    if let Some(terminals) = &window.terminals {
        terminals.ack(&key, bytes);
    }
}

/// The most text one input call may carry. The input queue bounds how many
/// calls wait, so this bounds the bytes they hold; a larger paste is refused.
const MAX_INPUT_BYTES: usize = ainb_desktop::clipboard::MAX_CLIPBOARD_BYTES;

/// Typed or pasted text for the tab's pane.
#[tauri::command]
fn terminal_input(window: tauri::State<'_, Window>, key: String, data: String) {
    if data.len() > MAX_INPUT_BYTES {
        tracing::warn!(
            tab = key,
            bytes = data.len(),
            "terminal input over 1 MiB refused"
        );
        return;
    }
    if let Some(terminals) = &window.terminals {
        terminals.input(&key, data.into_bytes());
    }
}

#[tauri::command]
fn terminal_resize(window: tauri::State<'_, Window>, key: String, cols: u16, rows: u16) {
    if let Some(terminals) = &window.terminals {
        terminals.resize(&key, cols, rows);
    }
}

#[tauri::command]
fn terminal_close(window: tauri::State<'_, Window>, key: String) {
    if let Some(terminals) = &window.terminals {
        terminals.close(&key);
    }
}

/// The most of the sidecar log "show log" returns.
const LOG_TAIL_BYTES: u64 = 64 * 1024;

/// Attach the webview's frame channel, send it every section it names, and
/// answer with the host id those frames name: the id the mirror is pinned to,
/// `local` until the daemon names one (#1066). A later re-pin reaches the
/// webview as a `host` event before its frames. The webview owns the one
/// subscription list; unknown section names are dropped.
#[tauri::command]
fn subscribe(
    window: tauri::State<'_, Window>,
    frames: Channel<FrameBatch>,
    sections: Subscription,
) -> HostId {
    *window.frames.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frames);
    window.shell.subscribe(sections)
}

/// Apply an intent from the webview: a key, a command, pasted text. A
/// host-authored command, or a key or name the host refuses from the window,
/// is not applied, and the answer says which row and why, for a toast.
#[tauri::command]
fn dispatch(window: tauri::State<'_, Window>, intent: RendererIntent) -> Option<Refusal> {
    // What the webview asked for and what became of it, so a reader of the log
    // can tell what the window authored and what the host actually applied: a
    // command's id, never its arguments, and never a key's chord or a text's
    // characters, which are what a person typed.
    let asked = match &intent {
        RendererIntent::Command(id, _) => id.as_str().to_string(),
        RendererIntent::Key(_) => "key".to_string(),
        RendererIntent::Text(text) => format!("text({} chars)", text.chars().count()),
    };
    let refusal = match Intent::try_from(intent) {
        Ok(intent) => window.shell.dispatch_renderer(intent),
        Err(refusal) => Some(refusal),
    };
    let outcome = if refusal.is_some() {
        "refused"
    } else {
        "dispatched"
    };
    tracing::info!(command = %asked, outcome, "renderer intent");
    refusal
}

/// What the settings page's Setup panel shows: the catalog's dependencies as
/// detected now, and the two files (#1175). A host read, never a frame, off
/// the main thread because detection probes binaries.
#[tauri::command]
async fn setup_status() -> ainb_desktop::setup::SetupView {
    tauri::async_runtime::spawn_blocking(ainb_desktop::setup::status)
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "setup status did not run");
            ainb_desktop::setup::SetupView {
                dependencies: Vec::new(),
                tmux_conf_present: false,
                otel: ainb_desktop::setup::OtelView {
                    env_file_present: false,
                    settings_env_present: false,
                    alloy_installed: false,
                    alloy_running: false,
                },
            }
        })
}

/// Whether a setup confirmation dialog is on screen.
static SETUP_DIALOG_OPEN: AtomicBool = AtomicBool::new(false);

/// Ask the shell to run one of the onboarding writes that the window may
/// not run itself (#1175). The write runs only after the person answers a
/// native dialog the shell owns; a script in the page can call this command
/// and can do nothing more, because the dialog is the OS's. The outcome comes
/// back as a toast, and `true` says the write ran. Async, so the blocking
/// dialog and the install never sit on the main thread.
#[tauri::command]
async fn setup_write(app: tauri::AppHandle, write: ainb_desktop::setup::SetupWrite) -> bool {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    // The variant only: the telemetry write carries a token, and this log is
    // what `show_log` reads back into the window.
    let kind = write.kind();
    if let Err(message) = write.validate() {
        tracing::info!(write = kind, "setup write refused before the dialog");
        if let Err(error) = app.emit("toast", &message) {
            tracing::warn!(%error, "setup refusal not delivered to the webview");
        }
        return false;
    }
    // One dialog at a time: a script that calls this in a loop must not
    // stack native dialogs on the person.
    if SETUP_DIALOG_OPEN
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        tracing::info!(write = kind, "setup write refused: a dialog is open");
        if let Err(error) = app.emit("toast", "a setup dialog is already open") {
            tracing::warn!(%error, "setup refusal not delivered to the webview");
        }
        return false;
    }
    let confirmation = write.confirmation();
    let confirmed = app
        .dialog()
        .message(confirmation.body)
        .title(confirmation.title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "Continue".to_string(),
            "Cancel".to_string(),
        ))
        .blocking_show();
    SETUP_DIALOG_OPEN.store(false, Ordering::Release);
    tracing::info!(write = kind, confirmed, "setup write");
    if !confirmed {
        return false;
    }
    let outcome = tauri::async_runtime::spawn_blocking(move || write.run())
        .await
        .unwrap_or_else(|error| Err(format!("the write did not run: {error}")));
    let (ran, message) = match outcome {
        Ok(message) => (true, message),
        Err(message) => (false, message),
    };
    if let Err(error) = app.emit("toast", &message) {
        tracing::warn!(%error, "setup outcome not delivered to the webview");
    }
    ran
}

/// Where the daemon connection stands, for the banner on first paint.
#[tauri::command]
fn sidecar_state(window: tauri::State<'_, Window>) -> SidecarView {
    window.sidecar.state().borrow().view()
}

/// The end of the sidecar log, for the degraded banner's "show log".
#[tauri::command]
fn show_log(window: tauri::State<'_, Window>) -> Option<String> {
    ainb_desktop::sidecar::log_tail(&window.sidecar_config, LOG_TAIL_BYTES)
}

/// Leave the degraded state and look for a daemon again.
#[tauri::command]
fn retry_sidecar(window: tauri::State<'_, Window>) {
    window.sidecar.retry();
}

/// The bundled daemon, beside this executable where the bundle installs
/// `bundle.externalBin`. A debug build also honours `AINB_DESKTOP_DAEMON_BIN`;
/// a release build never takes the binary it runs from the environment or from
/// `PATH`, and fails closed when it cannot place itself.
fn daemon_bin() -> Result<PathBuf, String> {
    #[cfg(debug_assertions)]
    if let Some(bin) = std::env::var_os("AINB_DESKTOP_DAEMON_BIN") {
        return Ok(PathBuf::from(bin));
    }
    let name = if cfg!(windows) {
        "ainb-hangar-daemon.exe"
    } else {
        "ainb-hangar-daemon"
    };
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot locate the desktop executable: {error}"))?;
    let dir = exe
        .parent()
        .ok_or("the desktop executable has no directory to find its daemon in")?;
    Ok(dir.join(name))
}

/// The `ainb` binary daemon lifecycle verbs run through, resolved once at
/// startup from the absolute `PATH` entries only (a relative or empty entry
/// would resolve against whatever directory the app was started in), and only
/// a file this user may execute.
fn ainb_bin() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ainb.exe" } else { "ainb" };
    let found = std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|path| is_executable(path));
    match &found {
        Some(path) => tracing::info!(path = %path.display(), "daemon verbs run through ainb"),
        None => tracing::warn!("no ainb on PATH; daemon verbs will report failed"),
    }
    found
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}

/// Send the shell's tracing to `<hangar home>/desktop.log`, so its warnings
/// exist somewhere once the window hides the terminal it was started from.
fn init_logging(hangar_home: &std::path::Path) {
    let path = hangar_home.join("desktop.log");
    let file = std::fs::create_dir_all(hangar_home)
        .and_then(|()| std::fs::OpenOptions::new().create(true).append(true).open(&path));
    match file {
        Ok(file) => {
            let installed = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(Mutex::new(file))
                .try_init();
            if let Err(error) = installed {
                eprintln!("desktop logging not installed: {error}");
            }
        }
        Err(error) => eprintln!("desktop log {} not opened: {error}", path.display()),
    }
}

// The WebDriver the journey drives serves unauthenticated commands on
// 127.0.0.1, so it exists in debug builds only. `--all-features` (the clippy
// step uses it) would otherwise reach a release binary.
#[cfg(all(feature = "wdio", not(debug_assertions)))]
compile_error!("the wdio WebDriver must never be built into a release binary");

// A release window has to serve its own frontend: without `bundled` the
// context embeds no assets and the window loads `build.devUrl`, which is a
// dev server nobody is running.
#[cfg(all(feature = "app", not(feature = "bundled"), not(debug_assertions)))]
compile_error!("a release build must carry `bundled`, or the window loads build.devUrl");

fn main() {
    // The native confirmation in front of the onboarding writes (#1175).
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    // Only a `wdio` build carries the embedded WebDriver the journey drives.
    #[cfg(feature = "wdio")]
    let builder = builder.plugin(tauri_plugin_wdio_webdriver::init());
    builder
        .setup(|app| {
            let hangar_home = ainb_hangar_core::hangar_home()
                .ok_or("the hangar home cannot be resolved: set AINB_HANGAR_HOME")?;
            init_logging(&hangar_home);
            // The desktop loads the user config itself and hands it to the
            // host, which reads nothing from disk for it.
            let config = AppConfig::load().unwrap_or_else(|error| {
                tracing::warn!(%error, "config did not load; using defaults");
                AppConfig::default()
            });
            // How often the host frames work that happened outside a
            // dispatch: the same `ui.app_tick_ms` the terminal host paces by.
            let tick = Duration::from_millis(config.ui.app_tick_ms.max(1));
            let legacy_panel = ainb_desktop::host::legacy_panel(&config);
            let frames = ChannelSink::default();
            let socket = ainb_hangar_client::socket_path_in(&hangar_home);
            let mut host = DesktopHost::new(
                config,
                Keymap::defaults(),
                // `local` until this home's daemon names its host; the sidecar
                // task below re-pins once it has.
                HostId::of_daemon(&socket),
                // Nothing is framed until the webview subscribes.
                Subscription::none(),
                frames.clone(),
            );
            let daemon_bin = daemon_bin()?;
            let sidecar_config = SidecarConfig::new(hangar_home, daemon_bin);
            // Both spawn onto the app's tokio runtime, so they start inside it.
            let sidecar = tauri::async_runtime::block_on(async {
                host.start_workspace_load();
                // Section 20, the board: the reader both hosts share (#1188).
                host.start_agent_status(agent_status_dialer(), legacy_panel);
                Sidecar::start(sidecar_config.clone())
            });
            let mut states = sidecar.state();
            let executor = DesktopExecutor::new(ainb_bin());
            // Only an absolute tmux: a bare "tmux" would re-admit the relative
            // PATH entries `find_tmux` leaves out.
            let terminals = ainb_desktop::terminal::find_tmux().map(|tmux| {
                Terminals::new(
                    Tmux::new(tmux),
                    WebviewTabs(app.handle().clone()),
                    executor.report_sender(),
                )
            });
            if terminals.is_none() {
                tracing::warn!("no tmux found; terminal tabs will report why they cannot open");
            }
            let executor = match &terminals {
                Some(terminals) => executor.with_terminals(terminals.clone()),
                None => executor,
            };
            let shell = Shell::new(host, executor);
            // The sidebar is the session list: a row click must be in context.
            shell.open_sessions();
            app.manage(Window {
                shell,
                frames,
                terminals,
                sidecar,
                sidecar_config,
            });

            menu::install(app.handle());

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let (view, connected) = {
                        let state = states.borrow_and_update();
                        (
                            state.view(),
                            matches!(*state, SidecarState::Connected { .. }),
                        )
                    };
                    // A connected sidecar has completed a hello, so the daemon
                    // may now have named its host (#1066). The webview hears the
                    // new id first, then the mirror re-pins and reframes under
                    // it; the other order would have the webview drop the
                    // reframe as another host's. Ordering dependency: the id is
                    // there to read because the sidecar publishes Connected only
                    // after its presence lease's `dial_presence`
                    // (ainb-hangar-client) completed the hello that recorded it.
                    if connected {
                        let host_id = HostId::of_daemon(&socket);
                        let window = handle.state::<Window>();
                        if host_id != window.shell.host_id() {
                            if let Err(error) = handle.emit("host", &host_id) {
                                tracing::warn!(%error, "host id not delivered to the webview");
                            }
                            window.shell.set_host(host_id);
                        }
                    }
                    if let Err(error) = handle.emit("sidecar", view) {
                        tracing::warn!(%error, "sidecar state not delivered to the webview");
                    }
                    if states.changed().await.is_err() {
                        return;
                    }
                }
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(tick);
                loop {
                    interval.tick().await;
                    handle.state::<Window>().shell.tick();
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            subscribe,
            dispatch,
            sidecar_state,
            setup_status,
            setup_write,
            show_log,
            retry_sidecar,
            palette,
            renderer_applied,
            clipboard_read,
            clipboard_write,
            terminal_tabs,
            terminal_output,
            terminal_ack,
            terminal_input,
            terminal_resize,
            terminal_close
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| {
            eprintln!("ainb desktop failed to start: {error}");
            std::process::exit(1);
        });
}
