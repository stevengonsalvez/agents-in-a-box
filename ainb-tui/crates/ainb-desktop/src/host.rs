//! The desktop's embedded host: one `AppState`, driven through `dispatch`, with
//! every change framed for the webview.

use std::time::Duration;

use ainb_app::app::RendererHost;
use ainb_app::app::intent::{Btn, Pos};
use ainb_app::app::keymap::{HostAction, active_contexts};
use ainb_app::app::state::WorkspaceRescan;
use ainb_app::config::AppConfig;
use ainb_app::fleet::agent_status_reader::{AgentStatusReader, Dialer};
use ainb_app::fleet::inbox_reader::{Dialer as InboxDialer, InboxReader};
use ainb_app::fleet::usage_reader::UsageReader;
use ainb_app::wire::frame::{FrameBatch, HostId, Mirror, Subscription};
use ainb_app::{AppState, CommandId, Effect, Intent, Keymap, SectionId};
use ainb_hangar_proto::connections::SurfaceKind;
use serde::Serialize;

use crate::intent::Refusal;

/// Where framed state goes: the Tauri channel in the app, a recorder in tests.
pub trait FrameSink {
    fn send(&mut self, batch: FrameBatch);
}

impl<F: FnMut(FrameBatch)> FrameSink for F {
    fn send(&mut self, batch: FrameBatch) {
        self(batch);
    }
}

/// Carries out one effect and returns the reports it produced, in order.
pub trait Executor {
    fn execute(&mut self, effect: Effect) -> Vec<Intent>;
}

/// The renderer-local half of [`RendererHost`] for a DOM renderer.
///
/// Layout work the keymap resolves (a pane scroll, the sidebar toggle) is
/// queued for the webview, which owns that layout. Pointer presses never reach
/// it: the webview hit-tests its own DOM and sends the command the press means
/// as an `Intent::Command`, so there is nothing under a `Pos` to find here.
#[derive(Debug, Default)]
pub struct DesktopLayout {
    queued: Vec<HostAction>,
}

impl DesktopLayout {
    /// The layout work queued since the last call, oldest first.
    pub fn take(&mut self) -> Vec<HostAction> {
        std::mem::take(&mut self.queued)
    }
}

impl RendererHost for DesktopLayout {
    fn queue(&mut self, action: HostAction) {
        self.queued.push(action);
    }

    fn pointer(&mut self, _state: &AppState, _pos: Pos, _btn: Btn) -> Option<Intent> {
        None
    }
}

/// Reports an effect may lead to before the chain is cut. A report that queued
/// another effect that reported again is legitimate; one that loops is a bug,
/// and a bounded chain keeps it from wedging the shell.
const MAX_REPORT_ROUNDS: usize = 32;

/// One row the palette offers: a command the webview may send by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct PaletteEntry {
    #[cfg_attr(feature = "typescript-bindings", specta(type = String))]
    pub id: CommandId,
    /// What the row does, as the keymap documents it.
    pub doc: &'static str,
    /// The context the row belongs to, for the palette to group by.
    pub context: String,
    /// The key that runs it, when it has one.
    pub chord: Option<String>,
    /// Whether the reducer would run it in the state as it stands. A row that
    /// is not active is still offered, greyed, rather than vanishing as the
    /// user moves around.
    pub active: bool,
}

/// The dialer the desktop's agent status reader uses: the daemon client from
/// the environment, announced as the desktop, so its reads are recorded as this
/// surface's and never as a terminal this process never ran.
#[must_use]
pub fn agent_status_dialer() -> Dialer {
    Box::new(|| ainb_app::fleet::bridge::daemon::surface_client(SurfaceKind::Desktop))
}

/// `[fleet.status] legacy_panel` as the desktop's own config sets it, with the
/// same `AINB_FLEET_LEGACY_PANEL` override the terminal honours: both surfaces
/// take the same read path (#1188).
#[must_use]
pub fn legacy_panel(config: &AppConfig) -> bool {
    use ainb_app::config::tunables::{LEGACY_PANEL_ENV, resolved_bool};
    resolved_bool(LEGACY_PANEL_ENV, config.fleet.status.legacy_panel)
}

/// The dialer the desktop's inbox reader uses: the daemon client from the
/// environment, announced as the desktop, so its reads are recorded as this
/// surface's.
#[must_use]
pub fn inbox_dialer() -> InboxDialer {
    Box::new(|| ainb_app::fleet::bridge::daemon::surface_client(SurfaceKind::Desktop))
}

/// The home sidebar's `select` row, the one Enter runs on a focused item.
const HOME_SIDEBAR_SELECT: &str = "home.sidebar.select";

/// One `AppState` hosted for the desktop renderer.
pub struct DesktopHost<S: FrameSink> {
    state: AppState,
    keymap: Keymap,
    layout: DesktopLayout,
    mirror: Mirror,
    sink: S,
    /// How the tick asks the state to keep the session list fresh: the
    /// cadence, and the floor under daemon news (#1156).
    rescan: WorkspaceRescan,
    /// The agent status reader both hosts share (#1188), once
    /// [`Self::start_agent_status`] has started it.
    agent_status: Option<AgentStatusReader>,
    /// How to dial the daemon for section 21, once [`Self::enable_usage`]
    /// named one; taken when the reader starts.
    usage_dialer: Option<Dialer>,
    /// The usage reader behind section 21, the stats tab. Started by the tick
    /// on the first subscription that names `usage`, so a renderer that does
    /// not subscribe to it costs the daemon no read (D3p-e).
    usage: Option<UsageReader>,
    /// How the inbox reader dials the daemon. The reader itself runs only
    /// while a renderer subscribes to `inbox`, so no read is issued for a
    /// screen nobody has open.
    inbox_dialer: std::sync::Arc<InboxDialer>,
    /// The inbox reader, while `inbox` is subscribed.
    inbox: Option<InboxReader>,
    /// The runtime the inbox reader runs on. The window's `subscribe` is a
    /// synchronous command on the main thread, outside any runtime, so the
    /// host is handed one at construction rather than asking the thread.
    runtime: Option<tokio::runtime::Handle>,
    /// Whether the tick starts the daemon attention poller. A test that is
    /// about the reducer turns it off: the poller is a thread on a real
    /// socket, and its first publish is news whenever it lands.
    poll_attention: bool,
}

impl<S: FrameSink> DesktopHost<S> {
    /// Host a state built on `config`, as given: nothing is read from disk for
    /// it. Frames for the sections in `subscription` go to `sink`, stamped with
    /// `host_id` until [`Self::set_host`] re-pins it.
    pub fn new(
        config: AppConfig,
        keymap: Keymap,
        host_id: HostId,
        subscription: Subscription,
        sink: S,
    ) -> Self {
        Self::hosting(
            AppState::with_config(config),
            keymap,
            host_id,
            subscription,
            sink,
        )
    }

    /// Host `state` as it was built, rather than one built on a config. For a
    /// caller that has already assembled the state it wants hosted: a test
    /// seeding a session that is waiting on a question, for one.
    pub fn hosting(
        mut state: AppState,
        keymap: Keymap,
        host_id: HostId,
        subscription: Subscription,
        sink: S,
    ) -> Self {
        // This shell is the surface a person sits at, so an answer sent from
        // this window is recorded as the desktop's. The sidecar already tells
        // the daemon the same thing about this process (`sidecar::surface`).
        state.host.surface = ainb_hangar_proto::connections::SurfaceKind::Desktop;
        // The window shows every row. The persisted filter is the terminal's
        // (Shift+F), and this shell draws no filter indicator and offers no
        // control, so a filter seeded here would hide rows with nothing on
        // screen to say why (#1208). Until the desktop has its own filter chip
        // it starts on All, and the reducer refuses to cycle it from here.
        state.sessions.session_filter = ainb_app::app::state::SessionFilter::All;
        Self {
            state,
            keymap,
            layout: DesktopLayout::default(),
            mirror: Mirror::new(host_id, subscription),
            sink,
            rescan: WorkspaceRescan::default(),
            agent_status: None,
            usage_dialer: None,
            usage: None,
            inbox_dialer: std::sync::Arc::new(inbox_dialer()),
            inbox: None,
            runtime: tokio::runtime::Handle::try_current().ok(),
            poll_attention: true,
        }
    }

    /// Dial the inbox reads through `dialer` instead of the daemon client from
    /// the environment. For tests, which count the dials.
    #[must_use]
    pub fn reading_inbox_with(mut self, dialer: InboxDialer) -> Self {
        self.inbox_dialer = std::sync::Arc::new(dialer);
        self
    }

    /// Run the inbox reader on `runtime`. The app's main thread has none of
    /// its own, so the shell hands the host the app runtime's handle.
    #[must_use]
    pub fn on_runtime(mut self, runtime: tokio::runtime::Handle) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Whether the inbox reader is running, which it is exactly while a
    /// renderer subscribes to `inbox`.
    #[must_use]
    pub const fn inbox_reader_running(&self) -> bool {
        self.inbox.is_some()
    }

    /// Start or stop the inbox reader to match `subscription`: a renderer
    /// that reads `inbox` gets a reader, one that stops reading it stops the
    /// reads. Must be called inside a tokio runtime to start one.
    fn sync_inbox_reader(&mut self, subscription: &Subscription) {
        let wanted = subscription.contains(SectionId::Inbox);
        match (wanted, self.inbox.is_some()) {
            (true, false) => {
                let Some(runtime) = &self.runtime else {
                    tracing::warn!("inbox: no runtime to start the reader on");
                    self.state.inbox_absent("this window has no runtime to read the inbox on");
                    return;
                };
                let dialer = std::sync::Arc::clone(&self.inbox_dialer);
                let _entered = runtime.enter();
                self.inbox = Some(InboxReader::spawn(Box::new(move || dialer())));
            }
            (false, true) => {
                self.inbox = None;
                self.state.inbox_reset();
            }
            _ => {}
        }
    }

    /// Rescan on `every` instead of [`AppState::WORKSPACE_RESCAN`]. For tests, which
    /// cannot wait ten seconds to see the second scan.
    #[must_use]
    pub const fn rescanning_every(mut self, every: Duration) -> Self {
        self.rescan.every = every;
        self
    }

    /// Let news start a scan `floor` after the last one instead of after the
    /// state's own scan budget. For tests, as [`Self::rescanning_every`] is.
    #[must_use]
    pub const fn flooring_news_at(mut self, floor: Duration) -> Self {
        self.rescan.news_floor = floor;
        self
    }

    /// Start the agent status reader that keeps section 20, the board, current:
    /// the one reader both hosts run, on the daemon's Fleet subscription
    /// (#1188). A dropped subscription reads unreachable and a reconnect resets
    /// the section, so the host tracks no daemon liveness of its own. Must be
    /// called inside a tokio runtime; the tick folds what it reports.
    pub fn start_agent_status(&mut self, dialer: Dialer, legacy_panel: bool) {
        self.agent_status = Some(AgentStatusReader::spawn(dialer, legacy_panel));
    }

    /// Let the tick start the usage reader behind section 21, the stats tab,
    /// dialing with `dialer`, once a subscription names `usage` (D3p-e). Until
    /// then nothing reads: a renderer that does not subscribe to `usage` costs
    /// the daemon nothing. The webview subscribes to it at launch
    /// (`ui/src/subscription.ts`), so today the read starts with the window.
    pub fn enable_usage(&mut self, dialer: Dialer) {
        self.usage_dialer = Some(dialer);
    }

    /// Never start the daemon attention poller on a tick. For tests: the
    /// poller's first publish counts as news, which runs the attention merge
    /// whenever the thread happens to get there, whatever the cadence says.
    #[must_use]
    pub const fn without_attention_poll(mut self) -> Self {
        self.poll_attention = false;
        self
    }

    /// The hosted state, read-only: the host never writes it outside dispatch.
    pub const fn state(&self) -> &AppState {
        &self.state
    }

    /// Apply `intent`, frame what it moved, and return the effects it queued.
    /// The state write has finished before any effect is handed back.
    #[must_use = "the effects are host work the reducer did not perform; run them or they are lost"]
    pub fn dispatch(&mut self, intent: Intent) -> Vec<Effect> {
        let effects = ainb_app::dispatch(&mut self.state, &self.keymap, &mut self.layout, intent);
        self.pump();
        effects
    }

    /// Load the workspaces in the background, under the state's own load
    /// policy; a later [`Self::tick`] applies the result. Must be called inside
    /// a tokio runtime.
    pub fn start_workspace_load(&mut self) {
        self.state.start_workspace_load();
    }

    /// Apply background work that finished (a workspace load, a daemon
    /// attention poll), frame whatever moved outside a dispatch, and hand back
    /// the effects that work queued.
    #[must_use = "the effects are host work the reducer did not perform; run them or they are lost"]
    pub fn tick(&mut self) -> Vec<Effect> {
        // A session another process created is found by a scan and by nothing
        // else, so the window keeps asking for one: on daemon news once the
        // news floor has passed, else on the cadence. The pacing is the
        // state's (#1107, #1156).
        self.state.pace_workspace_load(Some(self.rescan));
        // The poller is idempotent by an atomic, so starting it every tick is
        // its documented use. Every read here is by shared reference: a `&mut`
        // path through the `Versioned` Fleet section would bump it each tick.
        if self.poll_attention {
            ainb_app::fleet::attention_poll::spawn(
                &self.state.fleet.daemon_attention,
                &self.state.fleet.fleet_snapshot,
                &self.state.host.attention_poll_running,
                &self.state.host.daemon_attention_generation,
            );
        }
        // The merged attention each session row carries on its frame. The
        // reducer paces it: at once on daemon news, otherwise on its own
        // cadence, and a merge that finds nothing new bumps nothing.
        self.state.refresh_attention(ainb_app::fleet::daemons::heartbeat::now_ms());
        // What the answer worker reported, the tab reconciled, and the composer
        // pointed at the request it is showing. Without it an answer sent from
        // this window would leave the row reading SENT for as long as the shell
        // is open: the worker reports into the state, and this is the only
        // thing in this process that folds it.
        self.state.tick_surfaces(ainb_app::fleet::daemons::heartbeat::now_ms());
        // Section 20, which the board draws, from the shared reader.
        if let Some(reader) = &mut self.agent_status {
            reader.drain_into(&mut self.state);
        }
        // Section 21, which the stats tab draws: started by the first
        // subscription that names it, then drained like section 20.
        if self.usage.is_none() && self.mirror.subscription().contains(SectionId::Usage) {
            if let Some(dialer) = self.usage_dialer.take() {
                self.usage = Some(UsageReader::spawn(dialer));
            }
        }
        if let Some(reader) = &mut self.usage {
            reader.drain_into(&mut self.state);
        }
        // Section 16, while a renderer reads it.
        if let Some(reader) = &mut self.inbox {
            reader.drain_into(&mut self.state);
        }
        let effects = self.state.take_effects();
        self.pump();
        effects
    }

    /// Apply `intent`, run each effect it queued with `executor` once the
    /// write is done, and apply the reports those effects produced the same
    /// way, until nothing more is queued.
    pub fn run(&mut self, intent: Intent, executor: &mut impl Executor) {
        let mut pending = vec![intent];
        for _ in 0..MAX_REPORT_ROUNDS {
            if pending.is_empty() {
                return;
            }
            let mut reports = Vec::new();
            for intent in pending {
                for effect in self.dispatch(intent) {
                    reports.extend(executor.execute(effect));
                }
            }
            pending = reports;
        }
        tracing::error!(
            dropped = pending.len(),
            "effect reports kept queueing effects; the chain was cut"
        );
    }

    /// Put the reducer on the session list, which the desktop's sidebar is.
    ///
    /// The state starts on the home screen, where the session list's rows (a
    /// row click among them) are refused by the context gate. The move goes
    /// through the home sidebar's own rows, so the host writes no state of its
    /// own: a click on its Sessions item selects and focuses it, and the
    /// sidebar's `select` row (Enter) opens it.
    ///
    /// Not two clicks: the reducer opens on a double click only inside the
    /// double-click window, timed with the wall clock, so a host slow enough to
    /// spend the window on the first click stayed on the home screen.
    pub fn open_sessions(&mut self, executor: &mut impl Executor) {
        use ainb_app::app::pointer::click_home_sidebar_item;
        use ainb_app::components::sidebar::SidebarItem;
        self.run(click_home_sidebar_item(SidebarItem::Sessions), executor);
        self.run(
            Intent::Command(CommandId::new(HOME_SIDEBAR_SELECT), serde_json::Value::Null),
            executor,
        );
    }

    /// Every command the palette may offer, in the keymap's own order.
    ///
    /// Built from [`crate::intent::refused_from_webview`], the list the
    /// dispatch seam refuses by, plus the pointer rows: those carry a payload
    /// only a hit test can supply, so a palette that named them would offer a
    /// row that cannot run. Each entry says whether it is active now.
    #[must_use]
    pub fn palette(&self) -> Vec<PaletteEntry> {
        // The reducer owns the row set (#1161); this surface narrows it by what
        // a webview may send and by nothing else.
        ainb_app::app::palette::rows(&self.state, &self.keymap)
            .into_iter()
            .filter(|row| !crate::intent::is_host_authored(&row.id))
            .map(|row| PaletteEntry {
                id: row.id,
                doc: row.doc,
                context: row.context.name(),
                chord: row.chord,
                active: row.active,
            })
            .collect()
    }

    /// The row `intent` would run now and why the webview may not run it, for
    /// the webview to show, or `None` when it may (or when the intent names no
    /// row).
    ///
    /// A key or a name the webview sends is script-reachable, so one judgement
    /// covers both: a row that writes outside ainb runs only from a key the
    /// host reads ([`Keymap::is_key_only`]), and a row whose effect depends on
    /// state is refused while that state makes it write outside ainb
    /// ([`AppState::remote_command_refusal`]): Enter on a dialog holding the
    /// hook install or the abtop setup, Next on onboarding with telemetry set
    /// up.
    #[must_use]
    pub fn refused_from_renderer(&self, intent: &Intent) -> Option<Refusal> {
        let (id, row) = match intent {
            Intent::Key(chord) => {
                let (ctx, _) =
                    self.keymap.resolve_with_context(&active_contexts(&self.state), chord)?;
                self.keymap
                    .commands()
                    .find(|(_, row)| row.ctx == ctx && row.chord.as_ref() == Some(chord))?
            }
            Intent::Command(id, _) => (id.clone(), self.keymap.command(id)?),
            _ => return None,
        };
        let why = if self.keymap.is_key_only(&id) {
            Some("it writes outside ainb, so it runs only from its key")
        } else {
            self.state.remote_command_refusal(&row.action)
        };
        why.map(|reason| Refusal {
            command: id,
            reason,
        })
    }

    /// Layout work for the webview queued since the last call.
    pub fn take_layout(&mut self) -> Vec<HostAction> {
        self.layout.take()
    }

    /// Change the sections the renderer wants. A newly added one is framed in
    /// full now.
    pub fn resubscribe(&mut self, subscription: Subscription) {
        self.sync_inbox_reader(&subscription);
        self.mirror.resubscribe(subscription);
        self.pump();
    }

    /// Frame every subscribed section again, for a renderer that attached (or
    /// reloaded) after the last batch and so holds none of them.
    pub fn reframe(&mut self) {
        self.mirror.reframe();
        self.pump();
    }

    /// The host every frame names.
    pub const fn host_id(&self) -> &HostId {
        self.mirror.host_id()
    }

    /// Re-pin the host every frame names (#1066), framing every subscribed
    /// section again under it; see [`Mirror::set_host`]. The renderer must
    /// already know `host_id`, or it drops the batch this sends. Returns
    /// whether the host changed.
    pub fn set_host(&mut self, host_id: HostId) -> bool {
        let changed = self.mirror.set_host(host_id);
        if changed {
            self.pump();
        }
        changed
    }

    /// Take a renderer that just attached (or reloaded) wanting
    /// `subscription`: every section in it is framed in full, in one batch.
    pub fn subscribe(&mut self, subscription: Subscription) {
        self.sync_inbox_reader(&subscription);
        self.mirror.resubscribe(subscription);
        self.mirror.reframe();
        self.pump();
    }

    fn pump(&mut self) {
        let batch = self.mirror.batch(&self.state);
        if !batch.is_empty() {
            self.sink.send(batch);
        }
    }
}
