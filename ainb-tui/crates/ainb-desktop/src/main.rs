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
use ainb_desktop::host::{DesktopHost, Executor, FrameSink};
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

struct Shell {
    host: Mutex<DesktopHost<ChannelSink>>,
    executor: Mutex<DesktopExecutor>,
    frames: ChannelSink,
    sidecar: Sidecar,
}

impl Shell {
    fn host(&self) -> std::sync::MutexGuard<'_, DesktopHost<ChannelSink>> {
        self.host.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn executor(&self) -> std::sync::MutexGuard<'_, DesktopExecutor> {
        self.executor.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Frame what moved since the last tick, and run the effects and deferred
    /// reports that work produced.
    fn tick(&self) {
        let mut host = self.host();
        let mut executor = self.executor();
        let mut reports: Vec<Intent> = Vec::new();
        for effect in host.tick() {
            reports.extend(executor.execute(effect));
        }
        reports.extend(executor.take_deferred());
        for report in reports {
            host.run(report, &mut *executor);
        }
    }
}

/// Attach the webview's frame channel and send it every section it draws.
#[tauri::command]
fn subscribe(shell: tauri::State<'_, Shell>, frames: Channel<FrameBatch>) {
    *shell.frames.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frames);
    shell.host().reframe();
}

/// Apply an intent from the webview: a key, a command, pasted text.
#[tauri::command]
fn dispatch(shell: tauri::State<'_, Shell>, intent: Intent) {
    let mut executor = shell.executor();
    shell.host().run(intent, &mut *executor);
}

/// Where the daemon connection stands, for the banner on first paint.
#[tauri::command]
fn sidecar_state(shell: tauri::State<'_, Shell>) -> SidecarState {
    shell.sidecar.state().borrow().clone()
}

/// Leave the degraded state and look for a daemon again.
#[tauri::command]
fn retry_sidecar(shell: tauri::State<'_, Shell>) {
    shell.sidecar.retry();
}

/// The bundled daemon: `AINB_DESKTOP_DAEMON_BIN`, else beside this executable,
/// where the bundle installs `bundle.externalBin`.
fn daemon_bin() -> PathBuf {
    std::env::var_os("AINB_DESKTOP_DAEMON_BIN").map_or_else(
        || {
            let exe = if cfg!(windows) {
                "ainb-hangar-daemon.exe"
            } else {
                "ainb-hangar-daemon"
            };
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|dir| dir.join(exe)))
                .unwrap_or_else(|| PathBuf::from(exe))
        },
        PathBuf::from,
    )
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
            let sidecar = tauri::async_runtime::block_on(async {
                Sidecar::start(SidecarConfig::new(hangar_home, daemon_bin()))
            });
            let mut states = sidecar.state();
            app.manage(Shell {
                host: Mutex::new(host),
                executor: Mutex::new(DesktopExecutor::new(ainb_bin())),
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
                    handle.state::<Shell>().tick();
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
