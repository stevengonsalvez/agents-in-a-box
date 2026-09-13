// ABOUTME: Renderer-agnostic half of the `daemons` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::daemons`, which re-exports this module.

use crate::cli::daemon::Action;
use crate::fleet::daemons::heartbeat::now_ms;
use crate::fleet::daemons::probe::{DaemonKind, DaemonStatus};
use crate::fleet::read::{CurrentStateIndex, EvidenceCensus, ProbeIndex};
use ainb_plugin_notifyd::install::BinaryIntent;
use ainb_plugin_notifyd::{HookHealth, Paths};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How often the BACKGROUND collector re-polls the aggregator. The collect only
/// reads a handful of small files, but it also performs a (bounded) socket
/// connect and `sysinfo` process lookups — work that must NEVER run on the UI
/// render thread (H-D2). A few seconds keeps the screen live while staying cheap.
const COLLECT_INTERVAL: Duration = Duration::from_secs(2);

/// How long the collector keeps polling after the screen last asked it for
/// anything, before it parks itself.
///
/// It used to be forever. Opening the Daemons screen ONCE armed a thread that
/// re-probed every daemon socket every two seconds for the rest of the
/// process's life — including the hours after the operator navigated away and
/// the whole time the TUI was suspended behind a tmux attach. On one real
/// session that was ~25 connects a minute to `hangar.sock` for three hours from
/// a single visit to the screen, none of which anything was going to read.
///
/// Comfortably longer than any render gap the screen can have while it is
/// actually on view (the TUI redraws far faster than this), so parking cannot
/// interrupt a screen someone is looking at; short enough that walking away
/// stops the polling within one screenful of time.
pub const COLLECT_IDLE_STOP_MS: i64 = 30_000;

/// Evidence needs fresh process checks, unlike daemon rows. Avoid one `ps` per
/// historical probe on every screen refresh.
const EVIDENCE_INTERVAL_MS: i64 = 30_000;

/// How long a lifecycle action may run before the row gives up on it. Generous:
/// a real restart genuinely takes seconds. The point is only that a wedged
/// action cannot hold its row's one-outstanding guard forever.
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a finished hook action's line stays on screen before the live issue
/// line comes back. Long enough to read a failure, short enough that it cannot
/// mask a fault that appears afterwards.
pub const STATUS_LINGER: Duration = Duration::from_secs(20);

/// The immutable snapshot the background collector publishes and `render` reads.
/// Cheap to clone the `Arc`; the `Mutex` is held only for the microseconds it
/// takes to swap or clone the row vector — never across I/O.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// Most-recently-collected daemon rows.
    pub rows: Vec<DaemonStatus>,
    /// The clock the cached rows' relative-time columns are measured against.
    pub collected_at_ms: i64,
    /// Most-recent hook wiring health. Collected beside daemon state, never in
    /// the render path.
    pub hook_health: Option<HookHealth>,
    /// Hook evidence freshness, collected beside the wiring health.
    pub evidence_census: Option<EvidenceCensus>,
    /// Clock of the last process-backed evidence census.
    pub evidence_collected_at_ms: i64,
    /// The ATC supervisor's mode + full-mode provider, when a single instance
    /// makes it unambiguous. Read off disk by the collector, never in render.
    ///
    /// `None` when there is no instance, several (nothing here could say WHICH
    /// one a switch would act on), or the meta will not parse. The mode toggle
    /// and the inline help are both hidden in that case rather than guessing.
    pub atc: Option<AtcModeView>,
    /// When the UI thread last reached for this snapshot. The collector reads
    /// it to decide whether anyone is still watching.
    pub last_touch_ms: i64,
    /// Set by the collector on its way out, cleared by whoever spawns the next
    /// one. Lives inside the snapshot's mutex rather than in a separate atomic
    /// so "is a collector running?" and "when was it last wanted?" are read and
    /// written under ONE lock — that is what makes at-most-one-collector an
    /// invariant instead of a race.
    ///
    /// Defaults to "not parked" so a test seam that installs a pre-seeded
    /// snapshot (see `seeded_state`) still never spawns the real collector.
    pub collector_parked: bool,
}

/// What the Daemons screen needs to know about the ATC supervisor beyond its
/// runtime row: which brain its heartbeat would use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtcModeView {
    pub name: String,
    pub provider: String,
    /// The help, rendered once by the collector.
    ///
    /// `mode_help` rebuilds the whole provider registry (five `Arc`s, a HashMap,
    /// a Vec) and allocates several `String`s. Calling it from `render` ran that
    /// every frame while the ATC row was selected, in the file whose entire
    /// design is about keeping work off the UI thread. It is a pure function of
    /// the provider, so it belongs on the snapshot with everything else.
    pub help: Vec<String>,
}

/// All state owned by the Daemons screen. Stored at app-level so the cached
/// snapshot survives cross-screen navigation. Cheap to default.
///
/// H-D2: `render` performs ZERO disk I/O and ZERO socket connects. A dedicated
/// background thread runs [`crate::fleet::daemons::collect`] every
/// [`COLLECT_INTERVAL`] and publishes the result into the shared [`Snapshot`];
/// `render` only ever clones the latest published snapshot under a microsecond
/// lock. A mid-crash daemon, a stale socket on a slow FS, or a saturated accept
/// backlog can stall the background thread but can NEVER freeze the UI.
#[derive(Debug, Default)]
pub struct DaemonsState {
    /// The snapshot the background collector publishes into. `None` until the
    /// first render lazily spawns the collector.
    pub shared: Option<Arc<Mutex<Snapshot>>>,
    /// Wakes the collector for an immediate re-collect. `None` until the
    /// collector is armed.
    pub wake: Option<std::sync::mpsc::Sender<()>>,
    /// Index of the highlighted row, clamped to the snapshot on every render.
    pub selected: usize,
    /// The open per-row action menu, if any.
    pub menu: Option<ActionMenu>,
    /// The daemon whose full error is showing, if the error view is open. Held
    /// separately from `menu` so the view never depends on the menu still being
    /// open to know what it is displaying.
    pub error_open: Option<DaemonKind>,
    /// Last action outcome per daemon, keyed by [`DaemonKind::id`]. A failure
    /// stays on its own row rather than becoming a toast that scrolls away from
    /// the thing it is about.
    pub outcomes: std::collections::HashMap<&'static str, ActionOutcome>,
    /// In-flight actions per daemon. Present = an action is running, which also
    /// serves as the one-outstanding guard for that row.
    pub inflight: std::collections::HashMap<
        &'static str,
        (
            tokio::sync::mpsc::UnboundedReceiver<ActionOutcome>,
            std::time::Instant,
        ),
    >,
    /// The in-flight hook install/repair, if one is running. Same shape and
    /// same one-outstanding guarantee as [`DaemonsState::inflight`]; the Hooks
    /// box is a panel rather than a row, so it needs its own slot.
    pub hooks_inflight: Option<(
        tokio::sync::mpsc::UnboundedReceiver<String>,
        std::time::Instant,
    )>,
    /// What the last hook action reported, and when it may stop being shown.
    ///
    /// It has to expire: the panel replaces the live issue line while a status
    /// is set, and this state is app-level, so a status that never expired
    /// would hide every fault found after it for the rest of the process. It
    /// also has to be READABLE, and the collector republishes every two
    /// seconds, so expiry is a wall clock the reader can keep up with rather
    /// than the next collect.
    pub hooks_status: Option<(String, std::time::Instant)>,
    /// A tmux session the screen wants attached. Drained by the key handler,
    /// which owns the app-level pending-action slot; the component itself must
    /// not reach into `AppState`.
    pub attach_request: Option<String>,
}

/// The open action menu: which daemon it belongs to and where the cursor is.
#[derive(Debug)]
pub struct ActionMenu {
    pub kind: DaemonKind,
    /// Index into [`ActionMenu::entries`].
    pub cursor: usize,
    /// The row's state when the menu opened, NOT re-read while it is open: the
    /// background collector republishes every two seconds, and a menu whose
    /// entries move under the cursor mid-keystroke acts on the wrong one.
    row: RowFacts,
}

/// The parts of a row's status that decide which entries its menu offers.
#[derive(Debug, Clone, Default)]
struct RowFacts {
    /// The provisioned ATC instance, when there is one. `None` means every
    /// lifecycle verb would bail. Read from a typed field, never inferred from
    /// the reason sentence: which actions the row offers, and which tmux
    /// session it attaches, must not change when someone rewords a message.
    instance: Option<String>,
    /// ATC has an OS timer installed for an instance that does not exist.
    orphan: bool,
}

impl RowFacts {
    fn of(status: &DaemonStatus) -> Self {
        Self {
            instance: status.atc_instance.clone(),
            orphan: status.scheduler_orphan.is_some(),
        }
    }

    fn unprovisioned(&self) -> bool {
        self.instance.is_none()
    }
}

/// One entry in the action menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEntry {
    /// A lifecycle verb.
    Act(Action),
    /// Attach to the ATC mission-control tmux session.
    OpenMissionControl,
    /// Show the full error of this row's last failed action.
    ViewError,
}

impl ActionMenu {
    /// The entries for this menu. `view last error` only appears when there IS
    /// one — an always-present entry that usually does nothing is noise.
    pub fn entries(&self, has_error: bool) -> Vec<MenuEntry> {
        // Per-kind AND per-mode, not `Action::ALL`: only the daemon that owns
        // the Codex transport offers `pair`, and ATC offers exactly the
        // supervisor mode it is NOT in.
        //
        // `for_kind_in_mode` is asked for that rather than the menu re-deriving
        // it: the same rule already decides what `ainb daemon atc --help`
        // documents, and a second copy here is a rule that can drift on one
        // surface while the other keeps the old answer. `offers` still gates
        // every verb it returns, so an unprovisioned row is filtered as before.
        let mut entries: Vec<MenuEntry> = Action::for_kind(self.kind)
            .into_iter()
            .filter(|action| self.offers(*action))
            .map(MenuEntry::Act)
            .collect();
        // Same rule as `offers`: with nothing provisioned there is no tmux
        // session, so attaching could only fail. `provision` is the entry that
        // gets you one.
        if self.kind == DaemonKind::Atc && !self.row.unprovisioned() {
            entries.push(MenuEntry::OpenMissionControl);
        }
        if has_error {
            entries.push(MenuEntry::ViewError);
        }
        entries
    }

    /// Whether this row can act on `action` right now.
    ///
    /// An entry that is guaranteed to fail is worse than no entry: it reads as
    /// the fix and is not one. With no instance provisioned every lifecycle
    /// verb bails before it looks at the verb, and removing a timer that is
    /// not orphaned would tear down a live instance's heartbeat.
    fn offers(&self, action: Action) -> bool {
        if self.kind != DaemonKind::Atc {
            return true;
        }
        match action {
            Action::Provision => self.row.unprovisioned(),
            Action::RemoveOrphan => self.row.orphan,
            _ => !self.row.unprovisioned(),
        }
    }
}

/// What a finished lifecycle action reported.
#[derive(Debug, Clone)]
pub struct ActionOutcome {
    pub action: Action,
    pub ok: bool,
    /// One line for the row itself.
    pub summary: String,
    /// Everything the command said: the argv, its exit status, and its output.
    /// This is what the error view shows, verbatim.
    pub detail: String,
}

impl DaemonsState {
    /// Lazily spawn the background collector on first use and return the shared
    /// snapshot handle. Idempotent: subsequent calls reuse the running thread.
    ///
    /// The collector seeds an immediate first collect (so the screen is populated
    /// within one interval of opening), then re-collects every [`COLLECT_INTERVAL`]
    /// for as long as the screen keeps asking. It is intentionally a detached
    /// daemon thread — the snapshot is the only shared state and it is
    /// best-effort, so there is nothing to join on teardown.
    ///
    /// Every call through here is the UI thread saying "I still want this",
    /// which is what keeps the collector alive and what revives it after a park.
    pub fn shared(&mut self) -> Arc<Mutex<Snapshot>> {
        if let Some(shared) = &self.shared {
            let shared = Arc::clone(shared);
            if Self::touch(&shared, now_ms()) {
                self.spawn_collector_for(&shared);
            }
            return shared;
        }
        let shared = Arc::new(Mutex::new(Snapshot {
            last_touch_ms: now_ms(),
            ..Snapshot::default()
        }));
        self.spawn_collector_for(&shared);
        self.shared = Some(Arc::clone(&shared));
        shared
    }

    /// Record that the UI thread wants the snapshot, and claim the right to
    /// spawn a collector if the last one parked. Returns `true` when the caller
    /// must spawn.
    ///
    /// The claim and the timestamp are one critical section on purpose. Whoever
    /// takes the lock first wins cleanly: if the UI wins, the collector reads a
    /// fresh `last_touch_ms` and does not park; if the collector wins, it parks
    /// and the UI sees `collector_parked` and revives it. `collector_parked` is
    /// only ever set by a collector about to return and only ever cleared here,
    /// so at most one collector can be live at a time.
    pub fn touch(shared: &Mutex<Snapshot>, now: i64) -> bool {
        let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.last_touch_ms = now;
        std::mem::take(&mut guard.collector_parked)
    }

    fn spawn_collector_for(&mut self, shared: &Arc<Mutex<Snapshot>>) {
        let (wake, wake_rx) = std::sync::mpsc::channel();
        spawn_collector(Arc::clone(shared), wake_rx);
        self.wake = Some(wake);
    }

    /// Ask the collector to re-collect now. The table free-runs anyway, so this
    /// only shortens the wait; it never collects on the UI thread.
    pub fn force_collect(&mut self) {
        self.arm();
        if let Some(wake) = &self.wake {
            let _ = wake.send(());
        }
    }

    /// Arm the background collector without rendering — called on navigation INTO
    /// the Daemons screen so collection starts (and the first snapshot lands)
    /// before the first frame, keeping the screen feeling live on entry.
    /// Idempotent: re-entering the screen reuses the already-running collector.
    pub fn arm(&mut self) {
        let _ = self.shared();
    }

    // ── Selection ───────────────────────────────────────────────────────────

    /// Move the row selection. `delta` is rows down (negative = up); the
    /// selection saturates at both ends rather than wrapping, so holding a key
    /// parks at the edge instead of cycling past what you were aiming at.
    pub fn move_selection(&mut self, delta: isize) {
        let len = self.row_count();
        if len == 0 {
            return;
        }
        let next = self.selected.saturating_add_signed(delta).min(len - 1);
        self.selected = next;
    }

    fn row_count(&mut self) -> usize {
        let shared = self.shared();
        let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.rows.len()
    }

    /// The whole selected row, so a caller can read more than its kind.
    pub fn selected_status(&mut self) -> Option<DaemonStatus> {
        let shared = self.shared();
        let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.rows.get(self.selected).cloned()
    }

    // ── Action menu ─────────────────────────────────────────────────────────

    /// True while the action menu or the error view is open, so the key handler
    /// knows Esc should close that rather than leave the screen.
    #[must_use]
    pub fn has_overlay(&self) -> bool {
        self.menu.is_some() || self.error_open.is_some()
    }

    /// Open the action menu on the selected row. No-op before the first
    /// snapshot lands — a menu over an empty table has nothing to act on.
    pub fn open_menu(&mut self) {
        if let Some(status) = self.selected_status() {
            self.menu = Some(ActionMenu {
                kind: status.kind,
                cursor: 0,
                row: RowFacts::of(&status),
            });
        }
    }

    /// Close every overlay at once — for a key that leaves the screen outright.
    ///
    /// The state is app-level, so an overlay left armed would still be there on
    /// re-entry, bound to a row the selection no longer sits on.
    pub fn close_all_overlays(&mut self) {
        self.error_open = None;
        self.menu = None;
    }

    /// Close whichever overlay is open, innermost first.
    pub fn close_overlay(&mut self) {
        if self.error_open.is_some() {
            self.error_open = None;
            return;
        }
        self.menu = None;
    }

    /// Move the menu cursor, saturating at both ends.
    pub fn move_menu(&mut self, delta: isize) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let len = menu.entries(self.has_error_for(menu.kind)).len();
        let Some(menu) = self.menu.as_mut() else {
            return;
        };
        menu.cursor = menu.cursor.saturating_add_signed(delta).min(len.saturating_sub(1));
    }

    pub fn has_error_for(&self, kind: DaemonKind) -> bool {
        self.outcomes.get(kind.id()).is_some_and(|o| !o.ok)
    }

    /// Run the highlighted menu entry.
    pub fn confirm_menu(&mut self) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let kind = menu.kind;
        let menu_instance = menu.row.instance.clone();
        let entries = menu.entries(self.has_error_for(kind));
        let Some(entry) = entries.get(menu.cursor).copied() else {
            return;
        };
        match entry {
            MenuEntry::ViewError => self.error_open = Some(kind),
            MenuEntry::OpenMissionControl => {
                self.error_open = None;
                self.menu = None;
                // The name comes off THIS row. Re-deriving it would resolve a
                // leftover or a default rather than the instance the row is
                // reporting, and attach a session that does not exist.
                if let Some(name) = menu_instance {
                    self.attach_request =
                        Some(crate::fleet::atc::meta::AtcMeta::new(&name).tmux_session());
                }
            }
            MenuEntry::Act(action) => {
                // BOTH overlays close. Clearing only `menu` left `error_open`
                // set with nothing to render it: the screen painted normally but
                // `has_overlay` stayed true, so every key but Esc was swallowed.
                self.error_open = None;
                self.menu = None;
                self.dispatch(kind, action);
            }
        }
    }

    /// Start one lifecycle action off the UI thread.
    ///
    /// The whole point of the Daemons screen's rewrite: an action must never be
    /// run inline. Shelling `ainb daemon …` on a throwaway thread keeps the UI
    /// responsive AND gives the error view the real argv, exit status, and
    /// stderr to show instead of a paraphrase.
    pub fn dispatch(&mut self, kind: DaemonKind, action: Action) {
        if self.inflight.contains_key(kind.id()) {
            return;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.inflight.insert(kind.id(), (rx, std::time::Instant::now()));
        self.outcomes.remove(kind.id());
        let (kind_id, verb) = (kind.id(), action.id());
        std::thread::spawn(move || {
            let _ = tx.send(run_daemon_action(kind_id, verb, action));
        });
    }

    /// Drain finished actions. Cheap enough for the render path — it is a
    /// channel poll, not I/O, and the H-D2 rule is about blocking syscalls.
    pub fn poll_actions(&mut self) {
        let mut done = Vec::new();
        for (id, (rx, started)) in &mut self.inflight {
            if let Ok(outcome) = rx.try_recv() {
                done.push((*id, outcome));
            } else if started.elapsed() > ACTION_TIMEOUT {
                // `inflight` doubles as the one-outstanding guard, so an action
                // that never returns would pin its row on `⟳ working` and
                // silently swallow every later action on that daemon for the
                // rest of the process. Give up and say so.
                done.push((
                    *id,
                    ActionOutcome {
                        action: Action::Restart,
                        ok: false,
                        summary: "timed out".to_string(),
                        detail: format!(
                            "`ainb daemon {id} …` did not finish within {}s.\n\n\
                             It may still be running. Check with `ainb daemon {id} \
                             start` from a terminal, where you can watch it.",
                            ACTION_TIMEOUT.as_secs()
                        ),
                    },
                ));
            }
        }
        for (id, outcome) in done {
            self.inflight.remove(id);
            self.outcomes.insert(id, outcome);
        }
    }

    /// Take the pending tmux attach, if the menu asked for one.
    pub fn take_attach_request(&mut self) -> Option<String> {
        self.attach_request.take()
    }

    /// Install or repair the hooks off the UI thread.
    ///
    /// [`BinaryIntent::Install`] repoints at the INSTALLED ainb, which is what
    /// rescues a pointer left aimed at a deleted worktree build.
    /// [`BinaryIntent::PinRunning`] deliberately aims at the binary running
    /// now, for testing a local build's hooks.
    pub fn dispatch_hooks(&mut self, intent: BinaryIntent) {
        if self.hooks_inflight.is_some() {
            return;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.hooks_inflight = Some((rx, std::time::Instant::now()));
        self.hooks_status = Some((
            match intent {
                BinaryIntent::Install => "installing hooks…",
                BinaryIntent::PinRunning => "pinning the running binary…",
            }
            .to_string(),
            // A running action outlives every clock until it finishes.
            std::time::Instant::now() + ACTION_TIMEOUT + STATUS_LINGER,
        ));
        std::thread::spawn(move || {
            let _ = tx.send(run_hook_action(intent));
        });
    }

    /// Drain a finished hook action. A channel poll, not I/O, same reasoning
    /// as [`DaemonsState::poll_actions`].
    fn poll_hooks(&mut self) {
        let Some((rx, started)) = self.hooks_inflight.as_mut() else {
            return;
        };
        if let Ok(line) = rx.try_recv() {
            self.hooks_status = Some((line, std::time::Instant::now() + STATUS_LINGER));
            self.hooks_inflight = None;
        } else if started.elapsed() > ACTION_TIMEOUT {
            self.hooks_status = Some((
                format!(
                    "hook repair did not finish within {}s",
                    ACTION_TIMEOUT.as_secs()
                ),
                std::time::Instant::now() + STATUS_LINGER,
            ));
            self.hooks_inflight = None;
        }
    }

    /// The hook status to paint, until it expires and the issue line returns.
    pub fn live_hooks_status(&self) -> Option<&str> {
        self.hooks_status
            .as_ref()
            .filter(|(_, until)| std::time::Instant::now() < *until)
            .map(|(line, _)| line.as_str())
    }

    /// Fold in whatever the workers reported and keep the collector alive.
    ///
    /// Everything here is a side effect — joining finished actions, reviving a
    /// parked collector thread, clamping the cursor against a table that shrank
    /// — so it runs once per frame BEFORE the draw rather than inside it.
    pub fn tick(&mut self) {
        self.poll_actions();
        self.poll_hooks();
        let rows = {
            let shared = self.shared();
            let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.rows.len()
        };
        self.selected = self.selected.min(rows.saturating_sub(1));
    }

    /// Read the latest published snapshot. A pure memory read under a
    /// microsecond lock; the collector that fills it is started by [`Self::tick`],
    /// so an unticked state reads empty rather than spawning a thread mid-paint.
    pub fn snapshot(&self) -> Snapshot {
        let Some(shared) = self.shared.as_ref() else {
            return Snapshot::default();
        };
        let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        Snapshot {
            rows: guard.rows.clone(),
            collected_at_ms: guard.collected_at_ms,
            hook_health: guard.hook_health.clone(),
            evidence_census: guard.evidence_census,
            evidence_collected_at_ms: guard.evidence_collected_at_ms,
            atc: guard.atc.clone(),
            last_touch_ms: guard.last_touch_ms,
            collector_parked: guard.collector_parked,
        }
    }
}

/// Run one hook install/repair in-process and report it in one line.
///
/// In-process, not shelled: unlike a daemon lifecycle verb there is no separate
/// process to talk to, and the repair is exactly the library call
/// `repair_or_install_hooks` performs.
fn run_hook_action(intent: BinaryIntent) -> String {
    // The row's one-outstanding guard is released on ACTION_TIMEOUT while the
    // thread is still alive, so a wedged install can be joined by a second one.
    // Unlike a row action these run IN-PROCESS and both write install.json and
    // hooks/ainb-bin, so they are serialised here rather than left to race.
    static HOOK_WRITES: Mutex<()> = Mutex::new(());
    let _serialised = HOOK_WRITES.lock().unwrap_or_else(|p| p.into_inner());

    let paths = match ainb_plugin_notifyd::Paths::from_home() {
        Ok(paths) => paths,
        Err(error) => return format!("hook repair failed: {error:#}"),
    };
    match intent {
        BinaryIntent::Install => match ainb_plugin_notifyd::repair_or_install_hooks(&paths) {
            Ok(report) => {
                let agents = report
                    .record
                    .agents
                    .iter()
                    .map(|agent| agent.name())
                    .collect::<Vec<_>>()
                    .join(", ");
                // Claude is wired through its marketplace, which can fail on
                // its own while the record still lists Claude. Reporting a
                // clean install then is how hooks that never fire look fine.
                let mut line = match &report.claude {
                    Some(ainb_plugin_notifyd::ClaudeRegister::Failed(error)) => {
                        format!("hooks installed for {agents}, but Claude plugin FAILED: {error}")
                    }
                    Some(ainb_plugin_notifyd::ClaudeRegister::ClaudeCliMissing) => {
                        format!(
                            "hooks installed for {agents}; Claude plugin skipped (no claude on \
                             PATH)"
                        )
                    }
                    _ => format!("hooks installed for {agents}"),
                };
                // `agents` is the cumulative record, so an agent that failed
                // THIS run is still in it. Name the failures explicitly.
                for (agent, error) in &report.failures {
                    line.push_str(&format!("; {} FAILED: {error}", agent.name()));
                }
                line
            }
            Err(error) => format!("hook repair failed: {error:#}"),
        },
        BinaryIntent::PinRunning => {
            match ainb_plugin_notifyd::install::pin_running_hook_binary(&paths) {
                Ok(target) => format!("hooks pinned to {}", target.path.display()),
                Err(error) => format!("pinning the running binary failed: {error:#}"),
            }
        }
    }
}

/// Shell one `ainb daemon <kind> <action>` and capture everything it said.
///
/// Runs on a throwaway thread. The captured argv, exit status, and output are
/// what the row's error view shows verbatim — the operator sees the actual
/// failure, not our summary of it.
pub fn run_daemon_action(kind_id: &str, verb: &str, action: Action) -> ActionOutcome {
    let argv = format!("ainb daemon {kind_id} {verb}");
    // Never self-exec a test harness: under `cargo test` current_exe() is the
    // test binary, and libtest treats the trailing argv as name filters, so
    // this would re-run the suite instead of running a subcommand. See
    // `crate::self_exec_guard` and issue #715.
    if crate::self_exec_guard::running_under_cargo_test() {
        return ActionOutcome {
            action,
            ok: false,
            summary: format!("{verb} unavailable"),
            detail: format!(
                "cmd: {argv}\nrefusing to self-exec a cargo test binary \
                 (current_exe is a test harness, not `ainb`)"
            ),
        };
    }
    let bin = match std::env::current_exe() {
        Ok(bin) => bin,
        Err(e) => {
            return ActionOutcome {
                action,
                ok: false,
                summary: format!("{verb} failed"),
                detail: format!("cmd: {argv}\ncould not resolve the running ainb binary: {e}"),
            };
        }
    };
    match std::process::Command::new(bin).args(["daemon", kind_id, verb]).output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let ok = out.status.success();
            // The LAST non-empty line, not the first — it was named `first_line`
            // for long enough that a CLI ending its output with a help block
            // badged the row with the help's closing line. Every verb reachable
            // from this menu therefore has to end its stdout with the sentence
            // worth badging; `fleet atc mode --set` prints its notes first for
            // exactly this reason.
            let badge_line = |s: &str| {
                s.lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            };
            let summary = if ok {
                let line = badge_line(&stdout);
                if line.is_empty() {
                    format!("{verb} ok")
                } else {
                    line
                }
            } else {
                format!("{verb} failed")
            };
            ActionOutcome {
                action,
                ok,
                summary,
                detail: format!(
                    "cmd: {argv}\nexit: {}\n\nstdout:\n{}\n\nstderr:\n{}",
                    out.status,
                    if stdout.is_empty() { "(none)" } else { &stdout },
                    if stderr.is_empty() { "(none)" } else { &stderr },
                ),
            }
        }
        Err(e) => ActionOutcome {
            action,
            ok: false,
            summary: format!("{verb} failed"),
            detail: format!("cmd: {argv}\ncould not run it: {e}"),
        },
    }
}

/// Run one collect and publish it into `shared`. Shared by the background thread
/// and the test seam so the publish/merge logic is exercised without a thread.
pub fn collect_into(shared: &Mutex<Snapshot>) {
    // Hook health only opens local files and attempts local Unix sockets. It
    // still belongs here, not in render: hooks may live on a slow volume and a
    // stale socket can block briefly while connecting.
    let hook_health = Paths::from_home().ok().map(|paths| ainb_plugin_notifyd::hook_health(&paths));
    let hooks_ready = hook_health.as_ref().is_some_and(|health| {
        health.script_ready
            && health.hook_binary_ready
            && health.agents.iter().any(|agent| agent.agent == "claude" && agent.wiring_ready)
    });
    let now = now_ms();
    let collect_evidence = shared
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .evidence_collected_at_ms
        .saturating_add(EVIDENCE_INTERVAL_MS)
        <= now;
    let evidence_census = collect_evidence
        .then(|| CurrentStateIndex::load().evidence_census(&ProbeIndex::load(), hooks_ready, now));
    if let Some(evidence_census) = evidence_census {
        let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.evidence_census = Some(evidence_census);
        guard.evidence_collected_at_ms = now;
    }
    let atc = collect_atc_mode();
    match crate::fleet::daemons::collect() {
        Ok(rows) => {
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.rows = rows;
            guard.collected_at_ms = now;
            guard.hook_health = hook_health;
            guard.atc = atc;
        }
        // Best-effort: an error leaves the prior snapshot in place (and logs)
        // rather than blanking the view.
        Err(e) => tracing::warn!(error = %e, "daemons screen: collect failed"),
    }
}

/// Read the ATC supervisor mode, but only when ONE instance makes it
/// unambiguous — the same rule `ainb daemon atc` uses to refuse acting on a
/// guessed instance. Disk I/O, so it runs on the collector thread (H-D2).
fn collect_atc_mode() -> Option<AtcModeView> {
    use crate::fleet::atc::meta::AtcMeta;
    use crate::fleet::atc::paths::{AtcPaths, list_instance_names_in};
    let root = crate::fleet::plumbing::paths::ainb_home().ok()?.join("atc");
    let names = list_instance_names_in(&root);
    let [name] = names.as_slice() else {
        return None;
    };
    let paths = AtcPaths::under_root(&root, name);
    let meta = AtcMeta::from_json(&std::fs::read_to_string(&paths.meta).ok()?).ok()?;
    let help = crate::fleet::atc::mode_help(&meta.provider);
    Some(AtcModeView {
        name: meta.name,
        provider: meta.provider,
        help,
    })
}

/// Has the screen stopped asking for snapshots long enough that the collector
/// should stop producing them? Pure so the window is testable without threads.
pub fn collector_should_park(last_touch_ms: i64, now_ms: i64) -> bool {
    now_ms.saturating_sub(last_touch_ms) > COLLECT_IDLE_STOP_MS
}

/// Spawn the detached background collector: one immediate collect, then a collect
/// every [`COLLECT_INTERVAL`] for as long as the screen keeps reaching for the
/// snapshot. Keeps ALL disk I/O / socket connects off the UI render thread
/// (H-D2).
fn spawn_collector(shared: Arc<Mutex<Snapshot>>, wake: std::sync::mpsc::Receiver<()>) {
    std::thread::Builder::new()
        .name("ainb-daemons-collect".into())
        .spawn(move || {
            loop {
                collect_into(&shared);
                // Blocks for the whole interval and returns the INSTANT `r` is
                // pressed. Polling a flag on a short timer would wake this
                // detached thread several times a second for the life of the
                // process to answer a question that is almost always "no".
                match wake.recv_timeout(COLLECT_INTERVAL) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    // The screen's state was dropped; nothing will read another
                    // snapshot.
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
                // Park when nobody has looked at the snapshot for a while. The
                // flag is set under the same lock that publishes it, so the
                // next `shared()` sees a parked collector and revives it.
                let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
                if collector_should_park(guard.last_touch_ms, now_ms()) {
                    guard.collector_parked = true;
                    return;
                }
            }
        })
        // A failure to spawn the collector must not crash the app: the screen
        // then simply shows an empty (never stale-wrong) table.
        .map_err(|e| tracing::warn!(error = %e, "daemons screen: collector thread spawn failed"))
        .ok();
}
