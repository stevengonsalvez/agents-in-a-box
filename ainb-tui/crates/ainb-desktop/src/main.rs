//! The desktop window: the embedded host, its executor and the sidecar
//! supervisor wired to the webview.
//!
//! Frames cross one in-process Tauri channel; the webview sends intents back as
//! `invoke("dispatch")`. Nothing here listens on a port.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Intent, Keymap, SectionId};
use ainb_desktop::executor::DesktopExecutor;
use ainb_desktop::host::{DesktopHost, FrameSink};
use ainb_desktop::shell::Shell;
use ainb_desktop::sidecar::{Sidecar, SidecarConfig, SidecarState};
use tauri::ipc::Channel;
use tauri::{Emitter, Manager};

/// The sections the shell draws in this node: the sidebar, the header counts
/// and the terminal tabs.
const SECTIONS: &[SectionId] = &[
    SectionId::Sessions,
    SectionId::Shell,
    SectionId::Tmux,
    SectionId::Fleet,
    SectionId::Config,
    SectionId::AgentStatus,
];

/// How often the host frames work that happened outside a dispatch.
const TICK: Duration = Duration::from_millis(250);

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

struct Window {
    shell: Shell<ChannelSink>,
    frames: ChannelSink,
    sidecar: Sidecar,
}

/// Attach the webview's frame channel and send it every section it draws.
#[tauri::command]
fn subscribe(window: tauri::State<'_, Window>, frames: Channel<FrameBatch>) {
    *window.frames.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frames);
    window.shell.reframe();
}

/// Apply an intent from the webview: a key, a command, pasted text.
#[tauri::command]
fn dispatch(window: tauri::State<'_, Window>, intent: Intent) {
    window.shell.dispatch(intent);
}

/// Where the daemon connection stands, for the banner on first paint.
#[tauri::command]
fn sidecar_state(window: tauri::State<'_, Window>) -> SidecarState {
    window.sidecar.state().borrow().clone()
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

/// The `ainb` binary daemon lifecycle verbs run through, from `PATH`.
fn ainb_bin() -> Option<PathBuf> {
    let name = if cfg!(windows) { "ainb.exe" } else { "ainb" };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let hangar_home = ainb_hangar_core::hangar_home()
                .ok_or("the hangar home cannot be resolved: set AINB_HANGAR_HOME")?;
            // The desktop loads the user config itself and hands it to the
            // host, which reads nothing from disk for it.
            let config = AppConfig::load().unwrap_or_else(|error| {
                tracing::warn!(%error, "config did not load; using defaults");
                AppConfig::default()
            });
            let frames = ChannelSink::default();
            let host = DesktopHost::new(
                config,
                Keymap::defaults(),
                HostId::local(),
                Subscription::only(SECTIONS),
                frames.clone(),
            );
            let daemon_bin = daemon_bin()?;
            let sidecar = tauri::async_runtime::block_on(async {
                Sidecar::start(SidecarConfig::new(hangar_home, daemon_bin))
            });
            let mut states = sidecar.state();
            app.manage(Window {
                shell: Shell::new(host, DesktopExecutor::new(ainb_bin())),
                frames,
                sidecar,
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let state = states.borrow_and_update().clone();
                    if let Err(error) = handle.emit("sidecar", state) {
                        tracing::warn!(%error, "sidecar state not delivered to the webview");
                    }
                    if states.changed().await.is_err() {
                        return;
                    }
                }
            });

            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let mut interval = tokio::time::interval(TICK);
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
            retry_sidecar
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| {
            eprintln!("ainb desktop failed to start: {error}");
            std::process::exit(1);
        });
}
