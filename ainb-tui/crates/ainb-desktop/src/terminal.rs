//! Terminal tabs: `tmux attach-session` clients the desktop owns in
//! pseudo-terminals, their output streamed to the webview.
//!
//! ```text
//! tmux pane ──PTY──▶ reader thread ──bounded queue──▶ pump ──credit window──▶ sink (webview)
//! webview ──input / resize / ack / close──▶ Terminals
//! ```
//!
//! The webview never holds a PTY and nothing here listens on a port: a tab's
//! output leaves through the sink the webview hands it, and input comes back
//! as calls. The pump sends only while the webview has acknowledged all but
//! [`WINDOW_BYTES`] of what it was sent, so a slow webview stalls the reader
//! and tmux holds the output, instead of a queue growing without bound. A
//! stalled tab holds at most `WINDOW_BYTES + READ_QUEUE * CHUNK_BYTES`, about
//! 8 MiB, so 64 MiB with [`MAX_ATTACHED_TABS`] attached.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

use ainb_app::Intent;
use ainb_app::app::reports::{self, AttachOutcome, AttachedTo};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::Serialize;
use uuid::Uuid;

/// The most tabs one host keeps attached. Opening one more detaches the tab
/// idle longest; it stays listed and re-attaches on click. Each attached tab
/// is a tmux client and four threads, so the cap bounds both.
pub const MAX_ATTACHED_TABS: usize = 8;

/// Waits before each automatic re-attach of a tab whose client dropped while
/// its tmux session lives. After the last, the tab is detached and offers
/// "reattach".
pub const REDIAL_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];

/// Unacknowledged output bytes a tab may have in flight to the webview before
/// the pump waits. Large enough that a burst paints without per-chunk
/// round trips, small enough that a stalled webview holds at most this much.
pub const WINDOW_BYTES: usize = 4 * 1024 * 1024;

/// Output read in one go is coalesced up to this size before it is sent, so a
/// fast pane crosses the IPC boundary in few, large messages.
const CHUNK_BYTES: usize = 64 * 1024;

/// How long a full window waits for an acknowledgement before it is assumed
/// lost and the window reopens.
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// How long typed input waits for a full pane queue before it is dropped.
const INPUT_WAIT: Duration = Duration::from_millis(500);

/// The largest grid a tab may ask its PTY for.
const MAX_ROWS: u16 = 500;
const MAX_COLS: u16 = 1000;

/// Read chunks queued between the PTY reader and the pump.
const READ_QUEUE: usize = 64;

/// A client that exits sooner than this after attaching counts as a failed
/// re-attach, so a session that refuses clients cannot redial forever.
const STABLE_AFTER: Duration = Duration::from_secs(2);

/// Where a tab's output goes. Returns `false` once the receiver is gone.
pub type Sink = Box<dyn FnMut(Vec<u8>) -> bool + Send>;

/// What a tab is attached to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TabTarget {
    /// An ainb session, through its tmux session.
    Session { id: Uuid, tmux: String },
    /// A tmux session ainb did not create.
    Tmux { tmux: String },
}

impl TabTarget {
    /// The tmux session, which is also the tab's key: one tab per session.
    #[must_use]
    pub fn tmux(&self) -> &str {
        match self {
            Self::Session { tmux, .. } | Self::Tmux { tmux } => tmux,
        }
    }

    fn attached_to(&self) -> AttachedTo {
        match self {
            Self::Session { id, .. } => AttachedTo::Session(*id),
            Self::Tmux { tmux } => AttachedTo::Tmux(tmux.clone()),
        }
    }
}

/// Where a tab's client stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TabState {
    Attached,
    /// Re-attaching on its own; `attempt` counts from 1.
    Reconnecting {
        attempt: usize,
    },
    /// No client: evicted by the cap or out of redials. A click re-attaches.
    Detached,
}

/// One tab as the webview draws it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TabView {
    pub key: String,
    pub target: TabTarget,
    #[serde(flatten)]
    pub state: TabState,
}

/// The tab strip: every listed tab, and the one to focus when an open asked
/// for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TabsView {
    pub tabs: Vec<TabView>,
    pub focus: Option<String>,
}

/// What the webview hears about tabs.
pub trait TabEvents: Send + Sync {
    fn tabs(&self, view: TabsView);
    /// A notice naming a tab: an eviction, a session that ended, redials spent.
    fn toast(&self, message: String);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The tmux a tab attaches through: the program, and the server socket when
/// it is not the user's default one.
#[derive(Debug, Clone)]
pub struct Tmux {
    program: PathBuf,
    socket: Option<PathBuf>,
}

impl Tmux {
    /// `program` against the default server.
    #[must_use]
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            socket: None,
        }
    }

    /// Against the server at `socket` (`tmux -S`) instead.
    #[must_use]
    pub fn on_socket(mut self, socket: PathBuf) -> Self {
        self.socket = Some(socket);
        self
    }

    /// `-S <socket>` when a socket was named, before any tmux command.
    fn server_args(&self) -> Vec<&std::ffi::OsStr> {
        self.socket
            .as_deref()
            .map(|socket| vec!["-S".as_ref(), socket.as_os_str()])
            .unwrap_or_default()
    }
}

/// `tmux` on this machine: the absolute `PATH` entries first, then where a
/// package manager puts it, since an app started from a desktop launcher often
/// has a `PATH` without either.
#[must_use]
pub fn find_tmux() -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from))
        .map(|dir| dir.join("tmux"))
        .find(|candidate| candidate.is_file())
}

/// The output flow of one tab, shared by every client it has had: the sink the
/// webview gave it, the bytes not yet acknowledged, and which client may send.
struct Flow {
    state: Mutex<FlowState>,
    wake: Condvar,
}

struct FlowState {
    sink: Option<Sink>,
    /// Bumped by every new sink, so a send that finishes after a reload does
    /// not put back the sink the reload replaced.
    sink_epoch: u64,
    unacked: usize,
    /// The client generation allowed to deliver; an older pump stops.
    generation: u64,
    closed: bool,
    last_active: Instant,
}

impl Flow {
    fn new(generation: u64) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FlowState {
                sink: None,
                sink_epoch: 0,
                unacked: 0,
                generation,
                closed: false,
                last_active: Instant::now(),
            }),
            wake: Condvar::new(),
        })
    }

    /// Send `bytes` once there is a sink and room in the window. `false` when
    /// the client that read them is no longer the tab's.
    fn deliver(&self, generation: u64, bytes: Vec<u8>) -> bool {
        let mut state = lock(&self.state);
        loop {
            if state.closed || state.generation != generation {
                return false;
            }
            if state.sink.is_some() && state.unacked < WINDOW_BYTES {
                break;
            }
            let (next, waited) = self
                .wake
                .wait_timeout(state, ACK_TIMEOUT)
                .unwrap_or_else(PoisonError::into_inner);
            state = next;
            if waited.timed_out() && state.sink.is_some() && state.unacked >= WINDOW_BYTES {
                // An acknowledgement was lost (a webview that dropped one):
                // resend rather than hold the tab forever.
                tracing::warn!(
                    unacked = state.unacked,
                    "no terminal acknowledgement for 10 s; reopening the window"
                );
                state.unacked = 0;
            }
        }
        // Sent with the lock released: an IPC send can be slow, and an ack or
        // a new sink must not wait behind it.
        // The chunk is counted before the send, so an acknowledgement landing
        // mid-send subtracts from a total that already includes it.
        let len = bytes.len();
        let (Some(mut sink), epoch) = (state.sink.take(), state.sink_epoch) else {
            return true;
        };
        state.unacked += len;
        drop(state);
        let sent = sink(bytes);
        let mut state = lock(&self.state);
        if state.sink_epoch != epoch {
            // A new sink arrived mid-send and started its own count at zero.
            return true;
        }
        if sent && !state.closed {
            state.sink = Some(sink);
            state.last_active = Instant::now();
        } else {
            // Unsent: the webview went away (a reload), so the chunk was never
            // in flight; the next sink starts clean.
            state.unacked = state.unacked.saturating_sub(len);
        }
        true
    }

    /// Install the webview's sink. `true` when it replaces an earlier one: the
    /// webview reloaded and holds none of the output sent so far.
    fn set_sink(&self, sink: Sink) -> bool {
        let mut state = lock(&self.state);
        let replaced = state.sink_epoch > 0;
        state.sink = Some(sink);
        state.sink_epoch += 1;
        state.unacked = 0;
        self.wake.notify_all();
        replaced
    }

    fn ack(&self, bytes: usize) {
        let mut state = lock(&self.state);
        state.unacked = state.unacked.saturating_sub(bytes);
        self.wake.notify_all();
    }

    fn retarget(&self, generation: u64) {
        lock(&self.state).generation = generation;
        self.wake.notify_all();
    }

    fn touch(&self) {
        lock(&self.state).last_active = Instant::now();
    }

    fn last_active(&self) -> Instant {
        lock(&self.state).last_active
    }

    fn close(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.sink = None;
        self.wake.notify_all();
    }
}

/// One live `tmux attach-session` in a PTY. Dropping it kills the client
/// (tmux detaches it; the session lives on) and its threads wind down.
struct Client {
    master: Box<dyn MasterPty + Send>,
    input: mpsc::SyncSender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    attached_at: Instant,
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.killer.kill();
    }
}

/// Run `command` in a PTY of `size`, pumping its output into `flow` as
/// `generation`, and call `exited` when the child exits.
///
/// Exit is watched on the child, not the output: a tab nobody has painted yet
/// has a pump waiting for a sink, and its reader never reaches end of file.
fn spawn_client(
    command: CommandBuilder,
    size: PtySize,
    flow: &Arc<Flow>,
    generation: u64,
    exited: impl FnOnce() + Send + 'static,
) -> Result<Client, String> {
    let pair = native_pty_system()
        .openpty(size)
        .map_err(|error| format!("no pseudo-terminal: {error}"))?;
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("tmux did not start: {error}"))?;
    // The child owns the slave now; holding it here would hide its exit.
    drop(pair.slave);
    let killer = child.clone_killer();
    let mut reader = pair.master.try_clone_reader().map_err(|error| error.to_string())?;
    let mut writer = pair.master.take_writer().map_err(|error| error.to_string())?;

    let (chunks_tx, chunks_rx) = mpsc::sync_channel::<Vec<u8>>(READ_QUEUE);
    thread("ainb-desktop-pty-reader", move || {
        use std::io::Read;
        let mut buf = vec![0u8; CHUNK_BYTES];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if chunks_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    })?;

    let pump_flow = Arc::clone(flow);
    thread("ainb-desktop-pty-pump", move || {
        while let Ok(mut bytes) = chunks_rx.recv() {
            while bytes.len() < CHUNK_BYTES {
                match chunks_rx.try_recv() {
                    Ok(more) => bytes.extend_from_slice(&more),
                    Err(_) => break,
                }
            }
            if !pump_flow.deliver(generation, bytes) {
                return;
            }
        }
    })?;

    let (input, input_rx) = mpsc::sync_channel::<Vec<u8>>(1024);
    thread("ainb-desktop-pty-writer", move || {
        use std::io::Write;
        while let Ok(bytes) = input_rx.recv() {
            if writer.write_all(&bytes).and_then(|()| writer.flush()).is_err() {
                break;
            }
        }
    })?;

    thread("ainb-desktop-pty-reaper", move || {
        let _ = child.wait();
        exited();
    })?;

    Ok(Client {
        master: pair.master,
        input,
        killer,
        attached_at: Instant::now(),
    })
}

fn thread(name: &str, run: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(run)
        .map(|_| ())
        .map_err(|error| format!("{name} did not start: {error}"))
}

struct Tab {
    target: TabTarget,
    state: TabState,
    flow: Arc<Flow>,
    client: Option<Client>,
    size: PtySize,
    /// Bumped whenever the tab's client changes, so a stale exit or pump is
    /// told apart from the current one.
    generation: u64,
    /// Automatic re-attaches spent since the tab last held a stable client.
    redials: usize,
}

struct Inner {
    tmux: Tmux,
    tabs: Mutex<Vec<Tab>>,
    events: Box<dyn TabEvents>,
    reports: Mutex<mpsc::Sender<Intent>>,
    /// The tab the webview last showed, which the cap never evicts. Locked
    /// only while the tabs lock is held, or on its own.
    in_view: Mutex<Option<String>>,
}

/// Every terminal tab of one host. Cheap to clone: clones share the tabs.
#[derive(Clone)]
pub struct Terminals {
    inner: Arc<Inner>,
}

impl Terminals {
    /// Tabs attached through `tmux`, announced to `events`, with reports for
    /// the reducer (an ended session, a closed tab) sent on `reports`.
    pub fn new(
        tmux: Tmux,
        events: impl TabEvents + 'static,
        reports: mpsc::Sender<Intent>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                tmux,
                tabs: Mutex::new(Vec::new()),
                events: Box::new(events),
                reports: Mutex::new(reports),
                in_view: Mutex::new(None),
            }),
        }
    }

    /// Open a tab on `target`, or focus the one already open. `Some` is the
    /// failure report when the session is gone or tmux would not start.
    pub fn open(&self, target: TabTarget) -> Option<Intent> {
        let key = target.tmux().to_string();
        // Probed before the tabs lock: it forks tmux, and a wedged tmux must
        // not freeze every tab (or the shell tick waiting on this call).
        let alive = has_session(&self.inner.tmux, &key);
        let mut tabs = lock(&self.inner.tabs);
        if let Some(index) = position(&tabs, &key) {
            if tabs[index].state == TabState::Detached {
                if let Err(report) = self.reattach_at(&mut tabs, index, alive) {
                    // The tab may be gone with its session: the strip hears it
                    // before the reducer does.
                    self.emit(&tabs, None);
                    return Some(report);
                }
            }
            self.emit(&tabs, Some(key));
            return None;
        }
        if !alive {
            return Some(reports::attach_finished(
                &target.attached_to(),
                &AttachOutcome::TargetMissing(format!("tmux session `{key}` is gone")),
            ));
        }
        self.make_room(&mut tabs);
        let generation = 1;
        let flow = Flow::new(generation);
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };
        match self.spawn(&key, size, &flow, generation) {
            Ok(client) => {
                tabs.push(Tab {
                    target,
                    state: TabState::Attached,
                    flow,
                    client: Some(client),
                    size,
                    generation,
                    redials: 0,
                });
                self.emit(&tabs, Some(key));
                None
            }
            Err(error) => Some(reports::attach_finished(
                &target.attached_to(),
                &AttachOutcome::Failed(error),
            )),
        }
    }

    /// Route the tab's output to `sink`, replacing any earlier one. `false`
    /// for a tab that is not listed.
    ///
    /// A replaced sink is a webview that reloaded: its fresh terminal holds
    /// nothing, so the client is replaced too and tmux redraws the whole
    /// screen, as a redial does.
    pub fn attach_output(&self, key: &str, sink: Sink) -> bool {
        let mut tabs = lock(&self.inner.tabs);
        let Some(index) = position(&tabs, key) else {
            return false;
        };
        let reloaded = tabs[index].flow.set_sink(sink);
        if reloaded && tabs[index].client.is_some() {
            let tab = &mut tabs[index];
            // Bumped first, so the old client's exit is stale when it lands.
            tab.generation += 1;
            tab.flow.retarget(tab.generation);
            tab.client = None;
            let (size, flow, generation) = (tab.size, Arc::clone(&tab.flow), tab.generation);
            match self.spawn(key, size, &flow, generation) {
                Ok(client) => tabs[index].client = Some(client),
                Err(error) => {
                    tracing::warn!(tab = key, %error, "terminal reattach after reload failed");
                    self.schedule_redial(&mut tabs, index);
                }
            }
        }
        true
    }

    /// The webview painted `bytes` of the tab's output.
    pub fn ack(&self, key: &str, bytes: usize) {
        // The tabs lock is held only to find the flow.
        if let Some(flow) = self.flow(key) {
            flow.ack(bytes);
        }
    }

    fn flow(&self, key: &str) -> Option<Arc<Flow>> {
        let tabs = lock(&self.inner.tabs);
        position(&tabs, key).map(|index| Arc::clone(&tabs[index].flow))
    }

    /// Type `bytes` into the tab's pane.
    pub fn input(&self, key: &str, bytes: Vec<u8>) {
        // A paste xterm.js wrapped in bracketed-paste markers is rebuilt from
        // its escape-free payload, so a clipboard carrying its own terminator
        // cannot end the paste and type the rest as keys (#1003). Every input
        // to a pane passes here, whichever webview path sent it.
        let bytes = match ainb_app::tmux::paste::rebracket(&bytes) {
            std::borrow::Cow::Borrowed(_) => bytes,
            std::borrow::Cow::Owned(rebuilt) => rebuilt,
        };
        let (flow, input) = {
            let tabs = lock(&self.inner.tabs);
            let Some(tab) = position(&tabs, key).map(|index| &tabs[index]) else {
                return;
            };
            (
                Arc::clone(&tab.flow),
                tab.client.as_ref().map(|client| client.input.clone()),
            )
        };
        flow.touch();
        let Some(input) = input else {
            tracing::warn!(tab = key, "input for a tab with no client; not sent");
            self.inner.events.toast(format!(
                "{key} is not attached; what you typed was not sent"
            ));
            return;
        };
        // A pane that stopped reading fills the queue: wait a little, then
        // say the input was lost rather than drop it silently.
        let deadline = Instant::now() + INPUT_WAIT;
        let mut pending = bytes;
        loop {
            match input.try_send(pending) {
                Ok(()) => return,
                Err(mpsc::TrySendError::Full(back)) if Instant::now() < deadline => {
                    pending = back;
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
        tracing::warn!(tab = key, "terminal input not taken within 500 ms; dropped");
        self.inner.events.toast(format!(
            "{key} is not reading input; what you typed was dropped"
        ));
    }

    /// Whether `key` names the tab the window is showing.
    ///
    /// The webview sizes a tab when it shows it, which is what marks it, so
    /// this is the window's own answer to "is this pane in front of the
    /// operator" without the renderer asserting it.
    #[must_use]
    pub fn showing(&self, key: &str) -> bool {
        lock(&self.inner.in_view).as_deref() == Some(key)
    }

    /// Size the tab's client to the webview's grid.
    ///
    /// The webview sizes a tab whenever it shows it, so this also marks the
    /// tab in view, which the cap skips: a quiet tab on screen is not evicted
    /// while others stream.
    pub fn resize(&self, key: &str, cols: u16, rows: u16) {
        let mut tabs = lock(&self.inner.tabs);
        let Some(index) = position(&tabs, key) else {
            return;
        };
        *lock(&self.inner.in_view) = Some(key.to_string());
        let tab = &mut tabs[index];
        tab.flow.touch();
        // The size reaches the shared tmux window of every client on the
        // session, so a webview cannot ask for more than a screen holds.
        tab.size = PtySize {
            rows: rows.clamp(1, MAX_ROWS),
            cols: cols.clamp(1, MAX_COLS),
            pixel_width: 0,
            pixel_height: 0,
        };
        if let Some(client) = &tab.client {
            if let Err(error) = client.master.resize(tab.size) {
                tracing::warn!(tab = key, %error, "terminal resize failed");
            }
        }
    }

    /// Close the tab: its client goes, and the reducer hears the user left it.
    pub fn close(&self, key: &str) {
        let mut tabs = lock(&self.inner.tabs);
        let Some(index) = position(&tabs, key) else {
            return;
        };
        let tab = tabs.remove(index);
        tab.flow.close();
        self.report(reports::attach_finished(
            &tab.target.attached_to(),
            &AttachOutcome::Detached,
        ));
        drop(tab);
        self.emit(&tabs, None);
    }

    /// The tab strip as it stands.
    #[must_use]
    pub fn view(&self) -> TabsView {
        view(&lock(&self.inner.tabs), None)
    }

    fn spawn(
        &self,
        key: &str,
        size: PtySize,
        flow: &Arc<Flow>,
        generation: u64,
    ) -> Result<Client, String> {
        let this = self.clone();
        let exited_key = key.to_string();
        spawn_client(
            attach_command(&self.inner.tmux, key),
            size,
            flow,
            generation,
            move || this.client_exited(&exited_key, generation),
        )
    }

    /// Detach the tab idle longest when the cap is reached.
    fn make_room(&self, tabs: &mut [Tab]) {
        let attached = tabs.iter().filter(|tab| tab.client.is_some()).count();
        if attached < MAX_ATTACHED_TABS {
            return;
        }
        let in_view = lock(&self.inner.in_view).clone();
        let Some(tab) = tabs
            .iter_mut()
            .filter(|tab| tab.client.is_some() && Some(tab.target.tmux()) != in_view.as_deref())
            .min_by_key(|tab| tab.flow.last_active())
        else {
            return;
        };
        tab.client = None;
        tab.generation += 1;
        tab.flow.retarget(tab.generation);
        tab.state = TabState::Detached;
        self.inner.events.toast(format!(
            "Detached {}: at most {MAX_ATTACHED_TABS} terminals stay attached",
            tab.target.tmux()
        ));
        // The reducer marked the session attached when it opened; only this
        // report marks it detached again, and an attached row never rings.
        self.report(reports::attach_finished(
            &tab.target.attached_to(),
            &AttachOutcome::Detached,
        ));
    }

    fn reattach_at(&self, tabs: &mut Vec<Tab>, index: usize, alive: bool) -> Result<(), Intent> {
        let key = tabs[index].target.tmux().to_string();
        if !alive {
            return Err(self.remove_ended(tabs, index));
        }
        self.make_room(tabs);
        let tab = &mut tabs[index];
        tab.generation += 1;
        tab.flow.retarget(tab.generation);
        tab.redials = 0;
        let (size, flow, generation) = (tab.size, Arc::clone(&tab.flow), tab.generation);
        match self.spawn(&key, size, &flow, generation) {
            Ok(client) => {
                let tab = &mut tabs[index];
                tab.client = Some(client);
                tab.state = TabState::Attached;
                Ok(())
            }
            Err(error) => Err(reports::attach_finished(
                &tabs[index].target.attached_to(),
                &AttachOutcome::Failed(error),
            )),
        }
    }

    /// The client of generation `generation` ended its output.
    fn client_exited(&self, key: &str, generation: u64) {
        let alive = has_session(&self.inner.tmux, key);
        let mut tabs = lock(&self.inner.tabs);
        let Some(index) = position(&tabs, key) else {
            return;
        };
        let tab = &mut tabs[index];
        if tab.generation != generation {
            return;
        }
        let Some(client) = tab.client.take() else {
            return;
        };
        if client.attached_at.elapsed() >= STABLE_AFTER {
            tab.redials = 0;
        }
        drop(client);
        if !alive {
            let report = self.remove_ended(&mut tabs, index);
            self.report(report);
            self.emit(&tabs, None);
            return;
        }
        self.schedule_redial(&mut tabs, index);
    }

    fn schedule_redial(&self, tabs: &mut [Tab], index: usize) {
        let tab = &mut tabs[index];
        tab.generation += 1;
        tab.flow.retarget(tab.generation);
        if tab.redials >= REDIAL_DELAYS.len() {
            tab.state = TabState::Detached;
            let key = tab.target.tmux().to_string();
            self.inner.events.toast(format!("Lost {key}: reattach when it is back"));
            self.report(reports::attach_finished(
                &tab.target.attached_to(),
                &AttachOutcome::Detached,
            ));
            self.emit(tabs, None);
            return;
        }
        let delay = REDIAL_DELAYS[tab.redials];
        tab.redials += 1;
        tab.state = TabState::Reconnecting {
            attempt: tab.redials,
        };
        let (key, generation) = (tab.target.tmux().to_string(), tab.generation);
        self.emit(tabs, None);
        let this = self.clone();
        let started = thread("ainb-desktop-pty-redial", move || {
            std::thread::sleep(delay);
            this.redial(&key, generation);
        });
        if let Err(error) = started {
            tracing::warn!(%error, "terminal redial did not start");
        }
    }

    fn redial(&self, key: &str, generation: u64) {
        let alive = has_session(&self.inner.tmux, key);
        let mut tabs = lock(&self.inner.tabs);
        let Some(index) = position(&tabs, key) else {
            return;
        };
        let tab = &tabs[index];
        if tab.generation != generation || !matches!(tab.state, TabState::Reconnecting { .. }) {
            return;
        }
        if !alive {
            let report = self.remove_ended(&mut tabs, index);
            self.report(report);
            self.emit(&tabs, None);
            return;
        }
        let (size, flow) = (tab.size, Arc::clone(&tab.flow));
        // A redialing tab holds no client, so it is not counted: without this
        // eight attached tabs plus a redial would exceed the cap.
        self.make_room(&mut tabs);
        match self.spawn(key, size, &flow, generation) {
            Ok(client) => {
                let tab = &mut tabs[index];
                tab.client = Some(client);
                tab.state = TabState::Attached;
                self.emit(&tabs, None);
            }
            Err(error) => {
                tracing::warn!(tab = key, %error, "terminal redial failed");
                self.schedule_redial(&mut tabs, index);
            }
        }
    }

    /// Drop a tab whose tmux session ended, with the toast and the report.
    fn remove_ended(&self, tabs: &mut Vec<Tab>, index: usize) -> Intent {
        let tab = tabs.remove(index);
        tab.flow.close();
        let key = tab.target.tmux();
        self.inner.events.toast(format!("{key} ended; its tab closed"));
        reports::attach_finished(
            &tab.target.attached_to(),
            &AttachOutcome::TargetMissing(format!("tmux session `{key}` ended")),
        )
    }

    fn report(&self, report: Intent) {
        let _ = lock(&self.inner.reports).send(report);
    }

    fn emit(&self, tabs: &[Tab], focus: Option<String>) {
        self.inner.events.tabs(view(tabs, focus));
    }
}

fn position(tabs: &[Tab], key: &str) -> Option<usize> {
    tabs.iter().position(|tab| tab.target.tmux() == key)
}

fn view(tabs: &[Tab], focus: Option<String>) -> TabsView {
    TabsView {
        tabs: tabs
            .iter()
            .map(|tab| TabView {
                key: tab.target.tmux().to_string(),
                target: tab.target.clone(),
                state: tab.state,
            })
            .collect(),
        focus,
    }
}

/// `tmux has-session` for exactly `name` (`=` stops a prefix match).
fn has_session(tmux: &Tmux, name: &str) -> bool {
    Command::new(&tmux.program)
        .args(tmux.server_args())
        .args(["has-session", "-t", &format!("={name}")])
        .env_remove("TMUX")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn attach_command(tmux: &Tmux, name: &str) -> CommandBuilder {
    let mut command = CommandBuilder::new(&tmux.program);
    command.args(tmux.server_args());
    command.args(["attach-session", "-t", &format!("={name}")]);
    apply_client_env(&mut command, std::env::vars());
    command
}

/// The environment a tab's tmux client needs, as the terminal host's embed
/// client sets it (`ainb-core/src/tmux/embed_client.rs`): `PATH`, the locale
/// and `TMUX_TMPDIR` from the parent, `TERM` for the xterm.js it draws into,
/// and no `TMUX`, or a desktop started inside tmux would refuse to nest.
///
/// An app started from a desktop launcher often has no locale, or a C one,
/// and under it tmux draws UTF-8 as underscores. The character type in effect
/// is the first set of `LC_ALL`, `LC_CTYPE` and `LANG`; when it is not UTF-8 a
/// default goes in `LC_ALL`, which outranks a copied-through `LC_ALL=C`.
fn apply_client_env(
    command: &mut CommandBuilder,
    vars: impl IntoIterator<Item = (String, String)>,
) {
    let (mut all, mut ctype, mut lang) = (None, None, None);
    for (key, value) in vars {
        if key == "PATH" || key == "LANG" || key == "TMUX_TMPDIR" || key.starts_with("LC_") {
            match key.as_str() {
                "LC_ALL" => all = Some(value.clone()),
                "LC_CTYPE" => ctype = Some(value.clone()),
                "LANG" => lang = Some(value.clone()),
                _ => {}
            }
            command.env(key, value);
        }
    }
    let in_effect = [all, ctype, lang].into_iter().flatten().find(|value| !value.is_empty());
    if !in_effect.is_some_and(|value| value.to_uppercase().replace('-', "").contains("UTF8")) {
        command.env(
            "LC_ALL",
            if cfg!(target_os = "macos") {
                "en_US.UTF-8"
            } else {
                "C.UTF-8"
            },
        );
    }
    command.env("TERM", "xterm-256color");
    command.env_remove("TMUX");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pump's throughput with the PTY and no tmux: `head -c` writes bytes
    /// with no newline, so the line discipline passes them through unchanged
    /// and every byte can be counted. Recorded, not gated (spec `:350`).
    #[test]
    fn fifty_megabytes_cross_the_pty_pump_with_none_dropped() {
        const TOTAL: usize = 50 * 1024 * 1024;
        let generation = 1;
        let flow = Flow::new(generation);
        let (bytes_tx, bytes_rx) = mpsc::channel::<usize>();
        flow.set_sink(Box::new(move |bytes: Vec<u8>| {
            bytes_tx.send(bytes.len()).is_ok()
        }));
        // The webview acknowledges after painting; here, as soon as it hears.
        let acker = Arc::clone(&flow);
        let (all_tx, all_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut received = 0;
            for len in bytes_rx {
                received += len;
                acker.ack(len);
                if received >= TOTAL {
                    let _ = all_tx.send(received);
                }
            }
        });
        let mut command = CommandBuilder::new("sh");
        command.args(["-c", &format!("head -c {TOTAL} /dev/zero | tr '\\0' a")]);
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };

        let started = Instant::now();
        let client =
            spawn_client(command, size, &flow, generation, || {}).expect("the PTY client starts");
        let received = all_rx
            .recv_timeout(Duration::from_secs(60))
            .expect("every byte arrived within a minute");
        let elapsed = started.elapsed();
        drop(client);
        flow.close();
        eprintln!(
            "pty pump: {received} bytes in {:.2?} ({:.1} MiB/s)",
            elapsed,
            received as f64 / 1_048_576.0 / elapsed.as_secs_f64()
        );
        assert_eq!(received, TOTAL, "every byte the child wrote was delivered");
    }

    fn env_of(command: &CommandBuilder, key: &str) -> Option<String> {
        command.get_env(key).map(|value| value.to_string_lossy().into_owned())
    }

    #[test]
    fn a_client_without_a_locale_gets_a_utf8_one_and_keeps_the_rest() {
        let vars = [
            ("PATH", "/usr/bin"),
            ("TMUX_TMPDIR", "/tmp/t"),
            ("TMUX", "/tmp/s,1,0"),
            ("TERM", "dumb"),
        ]
        .map(|(key, value)| (key.to_string(), value.to_string()));
        let mut command = CommandBuilder::new("tmux");
        command.env_clear();
        apply_client_env(&mut command, vars);

        assert_eq!(env_of(&command, "PATH").as_deref(), Some("/usr/bin"));
        assert_eq!(env_of(&command, "TMUX_TMPDIR").as_deref(), Some("/tmp/t"));
        assert!(env_of(&command, "LC_ALL").is_some_and(|all| all.ends_with("UTF-8")));
        assert_eq!(env_of(&command, "TERM").as_deref(), Some("xterm-256color"));
        assert_eq!(env_of(&command, "TMUX"), None);

        let mut command = CommandBuilder::new("tmux");
        command.env_clear();
        apply_client_env(
            &mut command,
            [("LC_ALL".to_string(), "de_DE.UTF-8".to_string())],
        );
        assert_eq!(env_of(&command, "LC_ALL").as_deref(), Some("de_DE.UTF-8"));
        assert_eq!(
            env_of(&command, "LANG"),
            None,
            "a UTF-8 locale from the parent is kept as is"
        );

        // LC_ALL=C outranks a UTF-8 LANG, so the default replaces it.
        let mut command = CommandBuilder::new("tmux");
        command.env_clear();
        apply_client_env(
            &mut command,
            [("LC_ALL", "C"), ("LANG", "en_GB.UTF-8")]
                .map(|(key, value)| (key.to_string(), value.to_string())),
        );
        assert!(env_of(&command, "LC_ALL").is_some_and(|all| all.ends_with("UTF-8")));
        assert_eq!(env_of(&command, "LANG").as_deref(), Some("en_GB.UTF-8"));
    }

    /// A pump whose webview acknowledges nothing stops at the window.
    #[test]
    fn an_unacknowledged_window_stalls_the_pump() {
        let flow = Flow::new(1);
        let sent = Arc::new(Mutex::new(0usize));
        let counted = Arc::clone(&sent);
        flow.set_sink(Box::new(move |bytes: Vec<u8>| {
            *lock(&counted) += bytes.len();
            true
        }));
        let pump = Arc::clone(&flow);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..(WINDOW_BYTES / CHUNK_BYTES + 8) {
                if !pump.deliver(1, vec![0; CHUNK_BYTES]) {
                    break;
                }
            }
            let _ = done_tx.send(());
        });

        assert!(
            done_rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "the pump waits for acknowledgements"
        );
        assert!(*lock(&sent) <= WINDOW_BYTES + CHUNK_BYTES);

        flow.ack(usize::MAX);
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("acknowledging the window lets the pump finish");
    }
}
