//! The desktop's embedded host: one `AppState`, driven through `dispatch`, with
//! every change framed for the webview.

use std::time::{Duration, Instant};

use ainb_app::app::intent::{Btn, Pos};
use ainb_app::app::keymap::{HostAction, active_contexts};
use ainb_app::app::{KEY_ONLY_COMMANDS, RendererHost};
use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Mirror, Subscription};
use ainb_app::{AppState, Chord, CommandId, Effect, Intent, Keymap};
use serde::Serialize;

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

/// How long after a scan finishes the window asks for the next one.
///
/// A session another process creates reaches the sidebar only because this
/// runs: the scan is what finds it, and nothing else tells this window it
/// exists. A scan that finds the same list writes no Sessions frame, so the
/// cadence costs a scan rather than a reframe; the WorkspaceLoad flag it does
/// move is the "write only what changed" audit's, #1139.
///
/// Strictly longer than the floor the state publishes
/// (`AppState::workspace_rescan_floor`, its own scan budget), and measured
/// from the end of a scan, so a scan that times out is followed by a gap
/// instead of the next one starting as it gives up.
pub const WORKSPACE_RESCAN: Duration =
    Duration::from_secs(ainb_app::AppState::workspace_rescan_floor().as_secs() + 5);

/// One `AppState` hosted for the desktop renderer.
pub struct DesktopHost<S: FrameSink> {
    state: AppState,
    keymap: Keymap,
    layout: DesktopLayout,
    mirror: Mirror,
    sink: S,
    /// When the last scan was asked for, so the tick can pace the next.
    scanned_at: Instant,
    rescan_every: Duration,
    /// The daemon's publish counter as it stood when the last scan started, so
    /// news the poller brings can start one before the cadence would (#1156).
    ///
    /// `None` until this window has scanned at all: news is a reason to look
    /// AGAIN, and a host that has never asked for a list has nothing to
    /// refresh.
    scanned_generation: Option<u64>,
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
        Self {
            state,
            keymap,
            layout: DesktopLayout::default(),
            mirror: Mirror::new(host_id, subscription),
            sink,
            scanned_at: Instant::now(),
            rescan_every: WORKSPACE_RESCAN,
            scanned_generation: None,
        }
    }

    /// Rescan on `every` instead of [`WORKSPACE_RESCAN`]. For tests, which
    /// cannot wait ten seconds to see the second scan.
    #[must_use]
    pub const fn rescanning_every(mut self, every: Duration) -> Self {
        self.rescan_every = every;
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
        self.scanned_generation = Some(self.daemon_generation());
        self.state.start_workspace_load();
    }

    /// The attention poller's publish counter as it stands.
    fn daemon_generation(&self) -> u64 {
        self.state
            .host
            .daemon_attention_generation
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Apply background work that finished (a workspace load, a daemon
    /// attention poll), frame whatever moved outside a dispatch, and hand back
    /// the effects that work queued.
    #[must_use = "the effects are host work the reducer did not perform; run them or they are lost"]
    pub fn tick(&mut self) -> Vec<Effect> {
        let was_scanning = self.state.workspace_scan_running();
        self.state.check_workspace_loading_complete();
        // The cadence runs from the end of a scan, not its start: a scan that
        // took the whole Docker budget would otherwise be followed by the next
        // one immediately.
        if was_scanning && !self.state.workspace_scan_running() {
            self.scanned_at = Instant::now();
        }
        // The poller is idempotent by an atomic, so starting it every tick is
        // its documented use. Every read here is by shared reference: a `&mut`
        // path through the `Versioned` Fleet section would bump it each tick.
        ainb_app::fleet::attention_poll::spawn(
            &self.state.fleet.daemon_attention,
            &self.state.fleet.fleet_snapshot,
            &self.state.host.attention_poll_running,
            &self.state.host.daemon_attention_generation,
        );
        // The merged attention each session row carries on its frame. The
        // reducer paces it: at once on daemon news, otherwise on its own
        // cadence, and a merge that finds nothing new bumps nothing.
        self.state.refresh_attention(ainb_app::fleet::daemons::heartbeat::now_ms());
        // What the answer worker reported, the tab reconciled, and the composer
        // pointed at the request it is showing. Without it an answer sent from
        // this window would leave the row reading SENT for as long as the shell
        // is open: the worker reports into the state, and this is the only
        // thing in this process that folds it.
        self.state.tick_surfaces();
        // A session another process created is found by a scan and by nothing
        // else, so the window keeps asking for one. Never two at once: the
        // reducer owns the load and reports it running.
        //
        // The daemon already knows when something happened, so its publish
        // counter starts a scan at once and the cadence is the floor under it
        // (#1156): a box whose sessions never touch the daemon still gets one
        // on the timer. The counter is recorded at the START of the scan, so
        // news that arrives while it runs is still news when it finishes.
        let generation = self.daemon_generation();
        let news = self.scanned_generation.is_some_and(|seen| seen != generation);
        if !self.state.workspace_scan_running()
            && (news || self.scanned_at.elapsed() >= self.rescan_every)
        {
            self.scanned_generation = Some(generation);
            self.state.start_workspace_load();
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
    /// through the home sidebar's own rows, two clicks on its Sessions item as
    /// a double click opens it, so the host writes no state of its own.
    pub fn open_sessions(&mut self, executor: &mut impl Executor) {
        use ainb_app::app::pointer::click_home_sidebar_item;
        use ainb_app::components::sidebar::SidebarItem;
        for _ in 0..2 {
            self.run(click_home_sidebar_item(SidebarItem::Sessions), executor);
        }
    }

    /// Every command the palette may offer, in the keymap's own order.
    ///
    /// Built from [`crate::intent::refused_from_webview`], the list the
    /// dispatch seam refuses by, plus the pointer rows: those carry a payload
    /// only a hit test can supply, so a palette that named them would offer a
    /// row that cannot run. Each entry says whether it is active now.
    #[must_use]
    pub fn palette(&self) -> Vec<PaletteEntry> {
        let contexts = ainb_app::app::keymap::command_contexts(&self.state);
        self.keymap
            .commands()
            .filter(|(id, row)| crate::intent::palette_offers(id, row))
            .map(|(id, row)| PaletteEntry {
                id,
                doc: row.doc,
                context: row.ctx.name(),
                chord: row.chord.as_ref().map(|chord| chord.as_str().to_string()),
                active: contexts.contains(&row.ctx),
            })
            .collect()
    }

    /// The key-only row `chord` runs in the current state, if it runs one.
    ///
    /// Those rows write outside ainb (`global.wire_statusline` edits Claude
    /// Code's settings), so the reducer runs them only from a key. A chord the
    /// webview sends is script-reachable, so the shell refuses it there.
    #[must_use]
    pub fn key_only_command(&self, chord: &Chord) -> Option<CommandId> {
        let (ctx, _) = self.keymap.resolve_with_context(&active_contexts(&self.state), chord)?;
        self.keymap
            .commands()
            .find(|(_, row)| row.ctx == ctx && row.chord.as_ref() == Some(chord))
            .map(|(id, _)| id)
            .filter(|id| KEY_ONLY_COMMANDS.contains(&id.as_str()))
    }

    /// Layout work for the webview queued since the last call.
    pub fn take_layout(&mut self) -> Vec<HostAction> {
        self.layout.take()
    }

    /// Change the sections the renderer wants. A newly added one is framed in
    /// full now.
    pub fn resubscribe(&mut self, subscription: Subscription) {
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
