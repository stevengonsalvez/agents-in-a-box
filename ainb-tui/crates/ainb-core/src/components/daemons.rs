// ABOUTME: Daemons screen component — live runtime health of the four ainb daemons.
//
// Renders fleet daemons, system services, and hook health in one table-driven
// screen. Runtime actions run asynchronously; the screen never performs I/O in
// render. Follows the ainb-tui style guide: rounded borders, gold title,
// cornflower-blue panel, green for healthy.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table},
};

use crate::cli::daemon::Action;
use crate::cli::fleet::daemons::{fmt_ago, fmt_duration_ms};
use crate::fleet::daemons::heartbeat::now_ms;
use crate::fleet::daemons::probe::{DaemonKind, DaemonState, DaemonStatus};
use ainb_plugin_notifyd::install::BinaryIntent;
use ainb_plugin_notifyd::{HookHealth, Paths};

// Palette shared with the rest of ainb-tui (see components/layout.rs).
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const HEALTHY_GREEN: Color = Color::Rgb(100, 200, 100);
/// Cursor colour, per the TUI style guide. Same RGB as HEALTHY_GREEN but kept
/// separate: one means "this daemon is up", the other means "Enter acts on this
/// row", and they must be free to diverge.
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const STOPPED_RED: Color = Color::Rgb(220, 100, 100);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const PANEL_BG: Color = Color::Rgb(30, 30, 40);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);

/// How often the BACKGROUND collector re-polls the aggregator. The collect only
/// reads a handful of small files, but it also performs a (bounded) socket
/// connect and `sysinfo` process lookups — work that must NEVER run on the UI
/// render thread (H-D2). A few seconds keeps the screen live while staying cheap.
const COLLECT_INTERVAL: Duration = Duration::from_secs(2);

/// How long a lifecycle action may run before the row gives up on it. Generous:
/// a real restart genuinely takes seconds. The point is only that a wedged
/// action cannot hold its row's one-outstanding guard forever.
const ACTION_TIMEOUT: Duration = Duration::from_secs(60);

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
    shared: Option<Arc<Mutex<Snapshot>>>,
    /// Index of the highlighted row, clamped to the snapshot on every render.
    selected: usize,
    /// The open per-row action menu, if any.
    menu: Option<ActionMenu>,
    /// The daemon whose full error is showing, if the error view is open. Held
    /// separately from `menu` so the view never depends on the menu still being
    /// open to know what it is displaying.
    error_open: Option<DaemonKind>,
    /// Last action outcome per daemon, keyed by [`DaemonKind::id`]. A failure
    /// stays on its own row rather than becoming a toast that scrolls away from
    /// the thing it is about.
    outcomes: std::collections::HashMap<&'static str, ActionOutcome>,
    /// In-flight actions per daemon. Present = an action is running, which also
    /// serves as the one-outstanding guard for that row.
    inflight: std::collections::HashMap<
        &'static str,
        (
            tokio::sync::mpsc::UnboundedReceiver<ActionOutcome>,
            std::time::Instant,
        ),
    >,
    /// The in-flight hook install/repair, if one is running. Same shape and
    /// same one-outstanding guarantee as [`DaemonsState::inflight`]; the Hooks
    /// box is a panel rather than a row, so it needs its own slot.
    hooks_inflight: Option<(
        tokio::sync::mpsc::UnboundedReceiver<String>,
        std::time::Instant,
    )>,
    /// What the last hook action reported, shown in the Hooks box.
    hooks_status: Option<String>,
    /// A tmux session the screen wants attached. Drained by the key handler,
    /// which owns the app-level pending-action slot; the component itself must
    /// not reach into `AppState`.
    attach_request: Option<String>,
}

/// The open action menu: which daemon it belongs to and where the cursor is.
#[derive(Debug)]
struct ActionMenu {
    kind: DaemonKind,
    /// Index into [`ActionMenu::entries`].
    cursor: usize,
    /// The row's state when the menu opened, NOT re-read while it is open: the
    /// background collector republishes every two seconds, and a menu whose
    /// entries move under the cursor mid-keystroke acts on the wrong one.
    row: RowFacts,
}

/// The parts of a row's status that decide which entries its menu offers.
#[derive(Debug, Clone, Default)]
struct RowFacts {
    /// ATC has no provisioned instance, so every lifecycle verb would bail.
    unprovisioned: bool,
    /// ATC has an OS timer installed for an instance that does not exist.
    orphan: bool,
}

impl RowFacts {
    fn of(status: &DaemonStatus) -> Self {
        Self {
            unprovisioned: status.reason.contains(crate::fleet::daemons::probe::ATC_UNPROVISIONED),
            orphan: status.scheduler_orphan.is_some(),
        }
    }
}

/// One entry in the action menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuEntry {
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
    fn entries(&self, has_error: bool) -> Vec<MenuEntry> {
        // Per-kind, not `Action::ALL`: only the daemon that owns the Codex
        // transport offers `pair`.
        let mut entries: Vec<MenuEntry> = Action::for_kind(self.kind)
            .into_iter()
            .filter(|action| self.offers(*action))
            .map(MenuEntry::Act)
            .collect();
        if self.kind == DaemonKind::Atc {
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
            Action::Provision => self.row.unprovisioned,
            Action::RemoveOrphan => self.row.orphan,
            _ => !self.row.unprovisioned,
        }
    }
}

/// What a finished lifecycle action reported.
#[derive(Debug, Clone)]
pub struct ActionOutcome {
    action: Action,
    ok: bool,
    /// One line for the row itself.
    summary: String,
    /// Everything the command said: the argv, its exit status, and its output.
    /// This is what the error view shows, verbatim.
    detail: String,
}

impl DaemonsState {
    /// Lazily spawn the background collector on first use and return the shared
    /// snapshot handle. Idempotent: subsequent calls reuse the running thread.
    ///
    /// The collector seeds an immediate first collect (so the screen is populated
    /// within one interval of opening), then re-collects every [`COLLECT_INTERVAL`]
    /// for the lifetime of the process. It is intentionally a detached daemon
    /// thread — the snapshot is the only shared state and it is best-effort, so
    /// there is nothing to join on teardown.
    fn shared(&mut self) -> Arc<Mutex<Snapshot>> {
        if let Some(shared) = &self.shared {
            return Arc::clone(shared);
        }
        let shared = Arc::new(Mutex::new(Snapshot::default()));
        spawn_collector(Arc::clone(&shared));
        self.shared = Some(Arc::clone(&shared));
        shared
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

    /// The daemon the selection is on, if the snapshot has landed.
    fn selected_kind(&mut self) -> Option<DaemonKind> {
        self.selected_status().map(|status| status.kind)
    }

    /// The whole selected row, so a caller can read more than its kind.
    fn selected_status(&mut self) -> Option<DaemonStatus> {
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

    fn has_error_for(&self, kind: DaemonKind) -> bool {
        self.outcomes.get(kind.id()).is_some_and(|o| !o.ok)
    }

    /// Run the highlighted menu entry.
    pub fn confirm_menu(&mut self) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let kind = menu.kind;
        let entries = menu.entries(self.has_error_for(kind));
        let Some(entry) = entries.get(menu.cursor).copied() else {
            return;
        };
        match entry {
            MenuEntry::ViewError => self.error_open = Some(kind),
            MenuEntry::OpenMissionControl => {
                self.error_open = None;
                self.menu = None;
                // Resolving the instance name reads one directory. That is a
                // keystroke path, not render, so H-D2 does not apply.
                if let Ok(name) = crate::cli::daemon::atc_target_name() {
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
    fn poll_actions(&mut self) {
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
        self.hooks_status = Some(
            match intent {
                BinaryIntent::Install => "installing hooks…",
                BinaryIntent::PinRunning => "pinning the running binary…",
            }
            .to_string(),
        );
        std::thread::spawn(move || {
            let _ = tx.send(run_hook_action(intent));
        });
    }

    /// Drain a finished hook action. A channel poll, not I/O — same reasoning
    /// as [`DaemonsState::poll_actions`].
    fn poll_hooks(&mut self) {
        let Some((rx, started)) = self.hooks_inflight.as_mut() else {
            return;
        };
        if let Ok(line) = rx.try_recv() {
            self.hooks_status = Some(line);
            self.hooks_inflight = None;
        } else if started.elapsed() > ACTION_TIMEOUT {
            self.hooks_status = Some(format!(
                "hook repair did not finish within {}s",
                ACTION_TIMEOUT.as_secs()
            ));
            self.hooks_inflight = None;
        }
    }

    /// Read the latest published snapshot. Off the render path this is a pure
    /// memory read under a microsecond lock.
    pub fn snapshot(&mut self) -> Snapshot {
        let shared = self.shared();
        let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        Snapshot {
            rows: guard.rows.clone(),
            collected_at_ms: guard.collected_at_ms,
            hook_health: guard.hook_health.clone(),
        }
    }
}

/// Shell one `ainb daemon <kind> <action>` and capture everything it said.
///
/// Runs on a throwaway thread. The captured argv, exit status, and output are
/// what the row's error view shows verbatim — the operator sees the actual
/// failure, not our summary of it.
/// Run one hook install/repair in-process and report it in one line.
///
/// In-process, not shelled: unlike a daemon lifecycle verb there is no separate
/// process to talk to, and the repair is exactly the library call `ainb doctor
/// --fix-hooks` would make.
fn run_hook_action(intent: BinaryIntent) -> String {
    let paths = match ainb_plugin_notifyd::Paths::from_home() {
        Ok(paths) => paths,
        Err(error) => return format!("hook repair failed: {error:#}"),
    };
    match intent {
        BinaryIntent::Install => match ainb_plugin_notifyd::repair_or_install_hooks(&paths) {
            Ok(report) => format!(
                "hooks installed for {}",
                report
                    .record
                    .agents
                    .iter()
                    .map(|agent| agent.name())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
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

fn run_daemon_action(kind_id: &str, verb: &str, action: Action) -> ActionOutcome {
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
            let first_line = |s: &str| {
                s.lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            };
            let summary = if ok {
                let line = first_line(&stdout);
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
fn collect_into(shared: &Mutex<Snapshot>) {
    // Hook health only opens local files and attempts local Unix sockets. It
    // still belongs here, not in render: hooks may live on a slow volume and a
    // stale socket can block briefly while connecting.
    let hook_health = Paths::from_home().ok().map(|paths| ainb_plugin_notifyd::hook_health(&paths));
    match crate::fleet::daemons::collect() {
        Ok(rows) => {
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.rows = rows;
            guard.collected_at_ms = now_ms();
            guard.hook_health = hook_health;
        }
        // Best-effort: an error leaves the prior snapshot in place (and logs)
        // rather than blanking the view.
        Err(e) => tracing::warn!(error = %e, "daemons screen: collect failed"),
    }
}

/// Spawn the detached background collector: one immediate collect, then a collect
/// every [`COLLECT_INTERVAL`] forever. Keeps ALL disk I/O / socket connects off
/// the UI render thread (H-D2).
fn spawn_collector(shared: Arc<Mutex<Snapshot>>) {
    std::thread::Builder::new()
        .name("ainb-daemons-collect".into())
        .spawn(move || {
            loop {
                collect_into(&shared);
                std::thread::sleep(COLLECT_INTERVAL);
            }
        })
        // A failure to spawn the collector must not crash the app: the screen
        // then simply shows an empty (never stale-wrong) table.
        .map_err(|e| tracing::warn!(error = %e, "daemons screen: collector thread spawn failed"))
        .ok();
}

/// Render the Daemons screen into `area`. Reads ONLY the cached background
/// snapshot — no disk I/O, no socket connects on the UI thread (H-D2).
pub fn render(frame: &mut Frame, area: Rect, state: &mut DaemonsState) {
    state.poll_actions();
    state.poll_hooks();
    let snapshot = state.snapshot();
    // Clamp before painting: the collector can shrink the table under us.
    state.selected = state.selected.min(snapshot.rows.len().saturating_sub(1));

    let outer = Block::default()
        .title(Line::from(vec![
            Span::styled(" ⚙ ", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(
                "Daemons",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  runtime health",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            ),
            // The footer carries the live key hints, which change with whatever
            // overlay is open. Repeating a fixed set here just gives the title
            // a second, staler copy — `R restart selected` outlived the key.
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(PANEL_BG));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    // Carve a one-line help footer off the bottom.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // One table, plus the Hooks box. The old System services panel is gone: it
    // listed the MCP pool, the Hangar daemon and the Headroom proxy as ad-hoc
    // lines because they had no DaemonKind, and it sat on "collecting…" when
    // its separate async fetch wedged. Those three are real rows now, so the
    // panel was a second place to look that showed strictly less.
    if chunks[0].height >= 15 {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(7), Constraint::Length(8)])
            .split(chunks[0]);
        render_table(frame, sections[0], &snapshot, state);
        render_hook_section(
            frame,
            sections[1],
            snapshot.hook_health.as_ref(),
            state.hooks_status.as_deref(),
            snapshot.collected_at_ms > 0,
        );
    } else {
        render_table(frame, chunks[0], &snapshot, state);
    }
    render_footer(frame, chunks[1], state);
    // Overlays paint last so they float above the table.
    if state.error_open.is_some() {
        render_error_view(frame, inner, state);
    } else if state.menu.is_some() {
        render_action_menu(frame, inner, state);
    }
}

/// The per-row action menu — the one place every daemon offers the same verbs.
fn render_action_menu(frame: &mut Frame, area: Rect, state: &DaemonsState) {
    let Some(menu) = state.menu.as_ref() else {
        return;
    };
    let entries = menu.entries(state.has_error_for(menu.kind));
    let width = 30_u16.min(area.width.saturating_sub(2));
    let height = u16::try_from(entries.len()).unwrap_or(3) + 3;
    let popup = centered(area, width, height.min(area.height));
    frame.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                menu.kind.display_name(),
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" ", Style::default()),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .style(Style::default().bg(PANEL_BG));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::with_capacity(entries.len() + 1);
    for (i, entry) in entries.iter().enumerate() {
        let label = match entry {
            MenuEntry::Act(a) => a.id(),
            MenuEntry::OpenMissionControl => "open mission control",
            MenuEntry::ViewError => "view last error",
        };
        let selected = i == menu.cursor;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "▶ " } else { "  " },
                Style::default().fg(HEALTHY_GREEN),
            ),
            Span::styled(
                label,
                if selected {
                    Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED_GRAY)
                },
            ),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("Enter", Style::default().fg(CORNFLOWER_BLUE)),
        Span::styled(" run · ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Esc", Style::default().fg(CORNFLOWER_BLUE)),
        Span::styled(" close", Style::default().fg(MUTED_GRAY)),
    ]));
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(PANEL_BG)),
        inner,
    );
}

/// The full text of the selected row's last failed action.
fn render_error_view(frame: &mut Frame, area: Rect, state: &DaemonsState) {
    let Some(kind) = state.error_open else {
        return;
    };
    let Some(outcome) = state.outcomes.get(kind.id()) else {
        return;
    };
    let width = area.width.saturating_sub(6).min(76);
    let height = area.height.saturating_sub(4).min(18);
    let popup = centered(area, width, height);
    frame.render_widget(ratatui::widgets::Clear, popup);

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                kind.display_name(),
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {} failed ", outcome.action.id()),
                Style::default().fg(STOPPED_RED),
            ),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(STOPPED_RED))
        .style(Style::default().bg(PANEL_BG));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = outcome
        .detail
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(SOFT_WHITE))))
        .collect();
    lines.push(Line::from(Span::styled(String::new(), Style::default())));
    lines.push(Line::from(vec![
        Span::styled("Esc", Style::default().fg(CORNFLOWER_BLUE)),
        Span::styled(" close", Style::default().fg(MUTED_GRAY)),
    ]));
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(ratatui::widgets::Wrap { trim: false })
            .style(Style::default().bg(PANEL_BG)),
        inner,
    );
}

/// A `width` × `height` rect centred in `area`, clamped to fit.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn render_hook_section(
    frame: &mut Frame,
    area: Rect,
    health: Option<&HookHealth>,
    status: Option<&str>,
    collected: bool,
) {
    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" ◇ ", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(
                "Hooks",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ainb-hooks", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "  I",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" install / repair", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "  B",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" pin running", Style::default().fg(MUTED_GRAY)),
        ]))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(SUBDUED_BORDER))
        .style(Style::default().bg(PANEL_BG));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = match health {
        // Once a collect HAS run, "collecting…" is a lie — the collector looked
        // and found nothing readable. Saying so is what lets the operator act
        // on it instead of waiting for a placeholder that never resolves.
        None if collected => vec![Line::from(Span::styled(
            "hook health unavailable — run `ainb doctor --fix-hooks`",
            Style::default().fg(STOPPED_RED),
        ))],
        None => vec![Line::from(Span::styled(
            "reading hook health…",
            Style::default().fg(MUTED_GRAY),
        ))],
        Some(health) => {
            let installed = health.installed_version.as_deref().unwrap_or("not installed");
            let version_style = if health.version_current {
                Style::default().fg(HEALTHY_GREEN)
            } else {
                Style::default().fg(GOLD)
            };
            let agent_line = health
                .agents
                .iter()
                .map(|agent| {
                    format!(
                        "{} {}",
                        agent.agent,
                        if agent.wiring_ready { "✓" } else { "✗" }
                    )
                })
                .collect::<Vec<_>>()
                .join("   ");
            let issue = status.map_or_else(
                || {
                    health.issues.first().map_or_else(
                        || "✓ wiring healthy".to_string(),
                        |issue| {
                            format!(
                                "! {}: {} — {}",
                                issue.component, issue.message, issue.repair
                            )
                        },
                    )
                },
                |status| format!("I {status}"),
            );
            // The pointer and the binary you are looking at are separate facts,
            // and the failure being diagnosed here is precisely them being
            // different. One combined line hid that; two lines cannot.
            let pointer = health
                .hook_binary
                .as_ref()
                .map_or_else(|| "(no pointer)".to_string(), |p| p.display().to_string());
            let running = health
                .running_binary
                .as_ref()
                .map_or_else(|| "(unknown)".to_string(), |p| p.display().to_string());
            let pointer_matches_running =
                health.hook_binary.is_some() && health.hook_binary == health.running_binary;
            vec![
                Line::from(vec![
                    Span::styled("version ", Style::default().fg(MUTED_GRAY)),
                    Span::styled(
                        format!("{installed} → {}", health.bundled_version),
                        version_style,
                    ),
                ]),
                Line::from(Span::styled(
                    format!(
                        "script {}  ·  hooks {} ({}) → {pointer}",
                        if health.script_ready { "✓" } else { "✗" },
                        if health.hook_binary_ready {
                            "✓"
                        } else {
                            "✗"
                        },
                        health.hook_binary_mode.map(|mode| mode.label()).unwrap_or("unknown"),
                    ),
                    Style::default().fg(if health.script_ready && health.hook_binary_ready {
                        HEALTHY_GREEN
                    } else {
                        STOPPED_RED
                    }),
                )),
                Line::from(Span::styled(
                    format!("running → {running}"),
                    Style::default().fg(if pointer_matches_running {
                        MUTED_GRAY
                    } else {
                        GOLD
                    }),
                )),
                Line::from(Span::styled(agent_line, Style::default().fg(SOFT_WHITE))),
                Line::from(Span::styled(
                    format!(
                        "notifyd {}  ·  approval broker {}",
                        if health.notify_socket_live {
                            "running"
                        } else {
                            "idle"
                        },
                        if health.approve_socket_live {
                            "running"
                        } else {
                            "idle"
                        },
                    ),
                    Style::default().fg(MUTED_GRAY),
                )),
                Line::from(Span::styled(
                    issue,
                    Style::default().fg(if health.issues.is_empty() {
                        HEALTHY_GREEN
                    } else {
                        GOLD
                    }),
                )),
            ]
        }
    };
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(PANEL_BG)),
        inner,
    );
}

fn render_table(frame: &mut Frame, area: Rect, snapshot: &Snapshot, state: &DaemonsState) {
    let now = if snapshot.collected_at_ms > 0 {
        snapshot.collected_at_ms
    } else {
        now_ms()
    };

    let header = Row::new(vec![
        // The cursor gets its own column. Prefixing it into the DAEMON cell ate
        // two characters of every name, so long ones truncated.
        Cell::from(""),
        Cell::from("DAEMON"),
        Cell::from("STATE"),
        Cell::from("PID"),
        Cell::from("UPTIME"),
        Cell::from("VERSION"),
        Cell::from("LAST ACTIVITY"),
        Cell::from("ERR"),
        Cell::from("HEALTH"),
    ])
    .style(Style::default().fg(MUTED_GRAY).add_modifier(Modifier::BOLD));

    let rows: Vec<Row> = snapshot
        .rows
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let selected = i == state.selected;
            let (glyph, glyph_style) = match d.state {
                DaemonState::Running => ("● running", Style::default().fg(HEALTHY_GREEN)),
                // Amber, not green: the process is up but one half of its job is
                // provably not happening (bridge outbound push).
                DaemonState::Degraded => ("◐ degraded", Style::default().fg(GOLD)),
                DaemonState::Stopped => ("○ stopped", Style::default().fg(STOPPED_RED)),
                DaemonState::Unknown => ("? unknown", Style::default().fg(MUTED_GRAY)),
            };
            let pid = d.pid.map_or_else(|| "-".to_string(), |p| p.to_string());
            let uptime = d.uptime_ms.map_or_else(|| "-".to_string(), fmt_duration_ms);
            let version = daemon_version_label(d);
            let last_activity =
                d.last_activity_at.map_or_else(|| "-".to_string(), |ts| fmt_ago(now, ts));
            let health = match (&d.channel, d.connected, d.state) {
                (Some(ch), true, DaemonState::Running | DaemonState::Degraded) => {
                    format!("{ch} - {}", d.reason)
                }
                _ => d.reason.clone(),
            };
            // A running or finished action owns the STATE cell until the next
            // collect: it is the most recent truth about this row, and a
            // failure has to be attached to the daemon it happened to.
            let (glyph, glyph_style) = match (
                state.inflight.contains_key(d.kind.id()),
                state.outcomes.get(d.kind.id()),
            ) {
                (true, _) => ("⟳ working", Style::default().fg(GOLD)),
                (false, Some(o)) if !o.ok => (
                    "✗ failed",
                    Style::default().fg(STOPPED_RED).add_modifier(Modifier::BOLD),
                ),
                _ => (glyph, glyph_style),
            };
            // A failed action replaces HEALTH with its own summary plus the way
            // to read the rest. A stale probe reason under a red badge reads as
            // if nothing happened.
            let health = match state.outcomes.get(d.kind.id()) {
                Some(o) if !o.ok => format!("{}  ·  Enter → error", o.summary),
                Some(o) => o.summary.clone(),
                None => health,
            };
            Row::new(vec![
                Cell::from(if selected { "▶" } else { "" })
                    .style(Style::default().fg(SELECTION_GREEN)),
                Cell::from(d.kind.display_name()).style(if selected {
                    Style::default().fg(HEALTHY_GREEN).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
                }),
                Cell::from(glyph).style(glyph_style),
                Cell::from(pid),
                Cell::from(uptime),
                Cell::from(version.0).style(version.1),
                Cell::from(last_activity),
                Cell::from(d.error_count.to_string()).style(if d.error_count > 0 {
                    Style::default().fg(STOPPED_RED)
                } else {
                    Style::default().fg(MUTED_GRAY)
                }),
                Cell::from(health).style(Style::default().fg(SOFT_WHITE)),
            ])
        })
        .collect();

    // The cursor lives in its OWN gutter column rather than being prefixed onto
    // the name: "approve broker" is exactly the 14 columns DAEMON allows, so a
    // 2-char marker inside that cell truncated the daemon's name.
    let widths = [
        Constraint::Length(1),
        Constraint::Length(15),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(17),
        Constraint::Length(14),
        Constraint::Length(4),
        Constraint::Min(20),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(1)
        .style(Style::default().bg(PANEL_BG));
    frame.render_widget(table, area);
}

/// Short, operator-facing release verdict. Keep expected version in the cell:
/// a bare red old version forces humans to remember what Ainb they launched.
fn daemon_version_label(daemon: &DaemonStatus) -> (String, Style) {
    match (&daemon.version, daemon.version_current) {
        (Some(version), Some(true)) => (format!("{version} ✓"), Style::default().fg(HEALTHY_GREEN)),
        (Some(version), Some(false))
            if crate::fleet::daemons::probe::release_version_is_older(
                version,
                env!("CARGO_PKG_VERSION"),
            ) =>
        {
            (
                format!("{version} → {}", env!("CARGO_PKG_VERSION")),
                Style::default().fg(GOLD),
            )
        }
        (Some(version), Some(false)) => (
            format!("{version} newer"),
            Style::default().fg(HEALTHY_GREEN),
        ),
        _ => ("unknown".to_string(), Style::default().fg(MUTED_GRAY)),
    }
}

fn render_footer(frame: &mut Frame, area: Rect, state: &DaemonsState) {
    // Hints name the keys that work RIGHT NOW: an overlay owns Enter and Esc,
    // so advertising the table's keys underneath it would be a lie.
    let spans = if state.error_open.is_some() {
        vec![
            Span::styled("Esc", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" close error", Style::default().fg(MUTED_GRAY)),
        ]
    } else if state.menu.is_some() {
        vec![
            Span::styled("↑/↓", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" choose  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" run  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Esc", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" close", Style::default().fg(MUTED_GRAY)),
        ]
    } else {
        vec![
            Span::styled("↑/↓", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" select  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" start / restart / stop  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("r", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" refresh", Style::default().fg(MUTED_GRAY)),
            Span::styled("  │  ", Style::default().fg(SUBDUED_BORDER)),
            Span::styled("q/Esc", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" back", Style::default().fg(MUTED_GRAY)),
        ]
    };
    let footer = Paragraph::new(Line::from(spans)).style(Style::default().bg(PANEL_BG));
    frame.render_widget(footer, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::daemons::probe::DaemonKind;
    use crate::headroom::ProxyStatus;
    use ainb_plugin_notifyd::{HookAgentHealth, HookBinaryMode, HookHealth, HookHealthIssue};
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    fn status(
        kind: DaemonKind,
        state: DaemonState,
        connected: bool,
        channel: Option<&str>,
    ) -> DaemonStatus {
        DaemonStatus {
            kind,
            state,
            pid: Some(1234),
            uptime_ms: Some(3_600_000),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            version_current: Some(true),
            connected,
            channel: channel.map(str::to_string),
            last_activity_at: Some(now_ms() - 5_000),
            error_count: 0,
            last_error: None,
            last_attention_poll_at: None,
            last_attention_error: None,
            inbound_expected: 0,
            inbound_live: 0,
            last_inbound_error: None,
            scheduler_orphan: None,
            reason: if connected {
                "running + connected".to_string()
            } else {
                "no heartbeat — not running this session".to_string()
            },
        }
    }

    #[test]
    fn daemon_version_label_names_current_target_for_stale_daemon() {
        let mut daemon = status(DaemonKind::Bridge, DaemonState::Running, true, None);
        daemon.version = Some("0.0.0".to_string());
        daemon.version_current = Some(false);
        assert_eq!(
            daemon_version_label(&daemon).0,
            format!("0.0.0 → {}", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn daemon_version_label_does_not_suggest_downgrading_newer_daemon() {
        let mut daemon = status(DaemonKind::Bridge, DaemonState::Running, true, None);
        daemon.version = Some("999.0.0".to_string());
        daemon.version_current = Some(false);
        assert_eq!(daemon_version_label(&daemon).0, "999.0.0 newer");
    }

    /// A `DaemonsState` whose background collector is pre-empted: the shared
    /// snapshot is seeded with `rows` and the `shared` handle is installed, so
    /// `render`/`snapshot` read the seed and never spawn the real collector.
    /// This is the H-D2 test seam — render is decoupled from any live collect.
    fn seeded_state(rows: Vec<DaemonStatus>) -> DaemonsState {
        let shared = Arc::new(Mutex::new(Snapshot {
            rows,
            collected_at_ms: now_ms(),
            hook_health: None,
        }));
        DaemonsState {
            shared: Some(shared),
            ..DaemonsState::default()
        }
    }

    fn hook_health() -> HookHealth {
        HookHealth {
            bundled_version: "0.4.5".to_string(),
            installed_version: Some("0.4.4".to_string()),
            version_current: false,
            script_path: PathBuf::from("/tmp/notify.sh"),
            script_ready: true,
            hook_binary: Some(PathBuf::from("/usr/local/bin/ainb")),
            hook_binary_mode: Some(HookBinaryMode::Release),
            hook_binary_ready: true,
            running_binary: Some(PathBuf::from("/usr/local/bin/ainb")),
            agents: vec![
                HookAgentHealth {
                    agent: "claude".to_string(),
                    installed: true,
                    wiring_ready: true,
                    detail: "marketplace install recorded".to_string(),
                },
                HookAgentHealth {
                    agent: "codex".to_string(),
                    installed: true,
                    wiring_ready: true,
                    detail: "hooks.json points at shared hook".to_string(),
                },
                HookAgentHealth {
                    agent: "copilot".to_string(),
                    installed: false,
                    wiring_ready: false,
                    detail: "not installed".to_string(),
                },
            ],
            notify_socket_live: true,
            approve_socket_live: false,
            last_event: None,
            issues: vec![HookHealthIssue {
                component: "version".to_string(),
                message: "installed 0.4.4; ainb bundles 0.4.5".to_string(),
                repair: "ainb doctor --fix-hooks".to_string(),
            }],
        }
    }

    fn seeded_state_with_hook(rows: Vec<DaemonStatus>, hook_health: HookHealth) -> DaemonsState {
        let shared = Arc::new(Mutex::new(Snapshot {
            rows,
            collected_at_ms: now_ms(),
            hook_health: Some(hook_health),
        }));
        DaemonsState {
            shared: Some(shared),
            ..DaemonsState::default()
        }
    }

    /// Render the screen against an in-memory TestBackend and return the buffer
    /// as a single string for substring assertions.
    fn render_to_string(state: &mut DaemonsState, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.area(), state)).unwrap();
        let buf = terminal.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    /// Render and return the buffer as LINES, so a test can ask which row the
    /// cursor is drawn on rather than only whether a glyph exists somewhere.
    fn render_to_lines(state: &mut DaemonsState, w: u16, h: u16) -> Vec<String> {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.area(), state)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf.cell((x, y)).map_or(" ", |c| c.symbol()).to_string())
                    .collect::<String>()
            })
            .collect()
    }

    /// The row the cursor is DRAWN on is the row `R` restarts — always.
    ///
    /// This is the safety property, so it is asserted against the rendered
    /// buffer rather than against state: it reads back which line carries the
    /// cursor glyph and requires that line to name the same daemon that
    /// `selected_kind` hands the action menu. If render and dispatch ever
    /// compute the row separately and disagree — the highlighted daemon
    /// differing from the one acted on — this fails.
    #[test]
    fn the_highlighted_row_is_the_row_the_menu_acts_on() {
        let rows = vec![
            status(DaemonKind::Bridge, DaemonState::Stopped, false, None),
            status(
                DaemonKind::Notifyd,
                DaemonState::Running,
                true,
                Some("unix socket"),
            ),
            status(DaemonKind::ApproveBroker, DaemonState::Running, true, None),
            status(DaemonKind::Atc, DaemonState::Running, true, None),
        ];
        let mut state = seeded_state(rows.clone());

        for want in 0..rows.len() {
            // Drive the cursor the way the key handler does, from wherever it is.
            state.selected = 0;
            state.move_selection(want as isize);

            let target =
                state.selected_kind().expect("a populated table always has a selected row");

            let lines = render_to_lines(&mut state, 160, 30);
            let marked: Vec<&String> = lines.iter().filter(|l| l.contains('\u{25b6}')).collect();
            assert_eq!(
                marked.len(),
                1,
                "exactly one row may carry the cursor, got {marked:?}"
            );
            assert!(
                marked[0].contains(target.display_name()),
                "cursor is drawn on {:?} but restart would target {:?}",
                marked[0].trim(),
                target.display_name()
            );
        }
    }

    #[test]
    fn renders_title_header_and_all_daemon_rows() {
        // Seed the shared snapshot so render reads a deterministic cache and never
        // touches the host's real ~/.agents-in-a-box state (H-D2: render does no
        // collect of its own).
        let mut state = seeded_state(vec![
            status(
                DaemonKind::Bridge,
                DaemonState::Running,
                true,
                Some("Telegram (@bot)"),
            ),
            status(DaemonKind::Notifyd, DaemonState::Stopped, false, None),
            status(
                DaemonKind::ApproveBroker,
                DaemonState::Running,
                true,
                Some("approve socket"),
            ),
            status(
                DaemonKind::Atc,
                DaemonState::Running,
                true,
                Some("primary (every 15m)"),
            ),
            status(DaemonKind::FleetDaemon, DaemonState::Stopped, false, None),
        ]);
        let out = render_to_string(&mut state, 120, 12);
        assert!(out.contains("Daemons"), "title missing: {out}");
        assert!(out.contains("DAEMON"), "header missing");
        assert!(out.contains("HEALTH"), "header missing");
        // Every daemon's display name renders as a row.
        assert!(out.contains("phone bridge"), "bridge row missing");
        assert!(out.contains("notifyd"), "notifyd row missing");
        assert!(out.contains("approve broker"), "approve broker row missing");
        assert!(out.contains("ATC"), "ATC row missing");
        assert!(out.contains("fleet daemon"), "fleet daemon row missing");
        // State glyphs + a connected channel render.
        assert!(out.contains("running"), "running state missing");
        assert!(out.contains("stopped"), "stopped state missing");
        assert!(out.contains("Telegram (@bot)"), "channel missing");
    }

    #[test]
    fn render_reads_only_the_cached_snapshot_no_io() {
        // H-D2: with a pre-seeded snapshot, render must reflect exactly the seed —
        // proving it reads the cache and performs no collect of its own.
        let mut state = seeded_state(vec![status(
            DaemonKind::Bridge,
            DaemonState::Running,
            true,
            Some("Telegram (@seam)"),
        )]);
        let out = render_to_string(&mut state, 120, 8);
        assert!(
            out.contains("Telegram (@seam)"),
            "seeded row missing: {out}"
        );
        // The host's real daemons (notifyd/ATC/fleet) are NOT in the seed, so
        // their display names must be absent — render didn't collect them.
        assert!(
            !out.contains("ATC"),
            "render must not collect beyond the seed"
        );
    }

    #[test]
    fn renders_hook_version_state_and_repair_command_on_tall_screen() {
        let mut state = seeded_state_with_hook(
            vec![status(
                DaemonKind::Notifyd,
                DaemonState::Running,
                true,
                Some("socket+db"),
            )],
            hook_health(),
        );
        let out = render_to_string(&mut state, 120, 24);
        // The System services panel is gone on purpose: everything it listed is
        // a real table row now, so a second panel would show strictly less.
        assert!(
            !out.contains("System services"),
            "the System services panel must not come back: {out}"
        );
        assert!(
            !out.contains("collecting…"),
            "nothing on this screen may sit on a collecting placeholder: {out}"
        );
        assert!(out.contains("Hooks"), "hook section missing: {out}");
        assert!(
            out.contains("I install / repair"),
            "hook install/repair action missing: {out}"
        );
        assert!(out.contains("release"), "hook mode missing: {out}");
        assert!(
            out.contains("/usr/local/bin/ainb"),
            "hook target missing: {out}"
        );
        assert!(
            out.contains("0.4.4 → 0.4.5"),
            "version state missing: {out}"
        );
        assert!(
            out.contains("ainb doctor --fix-hooks"),
            "repair missing: {out}"
        );
        assert!(out.contains("claude ✓"), "agent wiring missing: {out}");
    }

    /// Bug 3: a stopped daemon had no way back up. Every row is now selectable
    /// and Enter offers the same three verbs — the point of the whole screen.
    #[test]
    fn enter_opens_an_action_menu_offering_start_restart_and_stop() {
        let mut state = seeded_state(vec![
            status(DaemonKind::Atc, DaemonState::Stopped, false, None),
            status(
                DaemonKind::McpPool,
                DaemonState::Running,
                true,
                Some("sock"),
            ),
        ]);
        state.open_menu();
        let out = render_to_string(&mut state, 120, 24);
        assert!(out.contains("start"), "menu must offer start: {out}");
        assert!(out.contains("restart"), "menu must offer restart: {out}");
        assert!(out.contains("stop"), "menu must offer stop: {out}");
        assert!(
            out.contains("ATC"),
            "the menu must name the row it acts on: {out}"
        );
    }

    /// The menu acts on the SELECTED row, not always the first one.
    #[test]
    fn the_menu_follows_the_selection() {
        let mut state = seeded_state(vec![
            status(DaemonKind::Atc, DaemonState::Stopped, false, None),
            status(
                DaemonKind::McpPool,
                DaemonState::Running,
                true,
                Some("sock"),
            ),
        ]);
        state.move_selection(1);
        state.open_menu();
        assert_eq!(
            state.menu.as_ref().map(|m| m.kind),
            Some(DaemonKind::McpPool)
        );
    }

    /// Selection saturates rather than wrapping, so holding a key parks at the
    /// edge instead of cycling past the row you were aiming for.
    #[test]
    fn selection_saturates_at_both_ends() {
        let mut state = seeded_state(vec![
            status(DaemonKind::Atc, DaemonState::Stopped, false, None),
            status(
                DaemonKind::McpPool,
                DaemonState::Running,
                true,
                Some("sock"),
            ),
        ]);
        state.move_selection(-5);
        assert_eq!(state.selected, 0);
        state.move_selection(50);
        assert_eq!(state.selected, 1);
    }

    /// A failure belongs to the daemon it happened to. The row shows a badge
    /// plus the way to read the rest, and the full text is one Enter away.
    #[test]
    /// The reported complaint: an ATC with no instance offered start, restart
    /// and stop, all three of which bail before they read the verb, and the
    /// only real fix was a CLI command quoted in the error.
    #[test]
    fn an_unprovisioned_atc_offers_provisioning_instead_of_dead_verbs() {
        let mut row = status(DaemonKind::Atc, DaemonState::Stopped, false, None);
        row.reason = format!(
            "{}, but a heartbeat timer for 'main' is installed and failing every interval",
            crate::fleet::daemons::probe::ATC_UNPROVISIONED
        );
        row.scheduler_orphan = Some("main".to_string());
        let mut state = seeded_state(vec![row]);

        state.open_menu();
        let menu = state.menu.as_ref().expect("menu opened");
        let entries = menu.entries(false);

        assert!(
            entries.contains(&MenuEntry::Act(Action::Provision)),
            "provisioning must be offered: {entries:?}"
        );
        assert!(
            entries.contains(&MenuEntry::Act(Action::RemoveOrphan)),
            "the orphan timer must be removable: {entries:?}"
        );
        assert!(
            entries.contains(&MenuEntry::OpenMissionControl),
            "mission control must be reachable: {entries:?}"
        );
        for dead in [Action::Start, Action::Restart, Action::Stop] {
            assert!(
                !entries.contains(&MenuEntry::Act(dead)),
                "{} would bail on an unprovisioned ATC: {entries:?}",
                dead.id()
            );
        }
    }

    /// The mirror image: a provisioned ATC must not be offered a `provision`
    /// that would reset its meta, nor a teardown of a timer that is doing its
    /// job.
    #[test]
    fn a_provisioned_atc_keeps_the_lifecycle_verbs_and_hides_provisioning() {
        let mut state = seeded_state(vec![status(
            DaemonKind::Atc,
            DaemonState::Running,
            true,
            Some("main"),
        )]);

        state.open_menu();
        let entries = state.menu.as_ref().expect("menu opened").entries(false);

        assert!(entries.contains(&MenuEntry::Act(Action::Restart)));
        assert!(!entries.contains(&MenuEntry::Act(Action::Provision)));
        assert!(!entries.contains(&MenuEntry::Act(Action::RemoveOrphan)));
    }

    #[test]
    fn a_failed_action_badges_its_own_row_and_its_detail_is_readable() {
        let mut state = seeded_state(vec![status(
            DaemonKind::Atc,
            DaemonState::Stopped,
            false,
            None,
        )]);
        state.outcomes.insert(
            DaemonKind::Atc.id(),
            ActionOutcome {
                action: Action::Start,
                ok: false,
                summary: "start failed".to_string(),
                detail: "cmd: ainb daemon atc start\nexit: exit status: 1\n\nstderr:\nsocket already bound by pid 4412".to_string(),
            },
        );
        let out = render_to_string(&mut state, 120, 24);
        assert!(
            out.contains("✗ failed"),
            "row must badge the failure: {out}"
        );
        assert!(
            out.contains("Enter → error"),
            "the row must say how to read the error: {out}"
        );

        state.open_menu();
        // `view last error` is always last, and it exists only because this row
        // HAS an error. Saturating to the end rather than counting entries: the
        // count moves whenever a row gains an action, and this test is about
        // the error view, not the menu's length.
        state.move_menu(isize::MAX);
        state.confirm_menu();
        let out = render_to_string(&mut state, 120, 24);
        assert!(
            out.contains("socket already bound by pid 4412"),
            "the error view must show the real stderr: {out}"
        );
        assert!(
            out.contains("ainb daemon atc start"),
            "the error view must show the command that failed: {out}"
        );
    }

    /// A row with no failure does not offer to show one.
    #[test]
    fn a_clean_row_has_no_view_error_entry() {
        let mut state = seeded_state(vec![status(
            DaemonKind::McpPool,
            DaemonState::Running,
            true,
            Some("sock"),
        )]);
        state.open_menu();
        let out = render_to_string(&mut state, 120, 24);
        assert!(
            !out.contains("view last error"),
            "nothing failed here, so there is nothing to view: {out}"
        );
    }

    /// Running an action from under the open error view must close BOTH
    /// overlays. Clearing only the menu left `error_open` set with nothing able
    /// to render it: the screen painted normally, but `has_overlay` stayed true
    /// so every key except Esc was swallowed and the table was unusable.
    #[test]
    fn acting_from_under_the_error_view_closes_both_overlays() {
        let mut state = seeded_state(vec![status(
            DaemonKind::Atc,
            DaemonState::Stopped,
            false,
            None,
        )]);
        state.outcomes.insert(
            DaemonKind::Atc.id(),
            ActionOutcome {
                action: Action::Start,
                ok: false,
                summary: "start failed".to_string(),
                detail: "boom".to_string(),
            },
        );
        state.open_menu();
        state.error_open = Some(DaemonKind::Atc);
        // Move the cursor back onto a verb and run it.
        state.move_menu(1);
        state.confirm_menu();
        assert!(state.error_open.is_none(), "the error view must close");
        assert!(state.menu.is_none(), "the menu must close");
        assert!(
            !state.has_overlay(),
            "no overlay may remain armed, or the screen swallows every key"
        );
    }

    /// `q` leaves the screen outright, so it must not leave an overlay armed:
    /// the state is app-level and would still be there on re-entry, bound to a
    /// row the selection no longer sits on.
    #[test]
    fn close_all_overlays_clears_both_layers() {
        let mut state = seeded_state(vec![status(
            DaemonKind::Atc,
            DaemonState::Stopped,
            false,
            None,
        )]);
        state.open_menu();
        state.error_open = Some(DaemonKind::Atc);
        state.close_all_overlays();
        assert!(!state.has_overlay());
    }

    /// An action that never returns must not pin its row: `inflight` doubles as
    /// the one-outstanding guard, so a stuck entry would silently swallow every
    /// later action on that daemon for the rest of the process.
    #[test]
    fn an_action_that_never_returns_gives_up_instead_of_pinning_its_row() {
        let mut state = seeded_state(vec![status(
            DaemonKind::McpPool,
            DaemonState::Running,
            true,
            Some("sock"),
        )]);
        // A sender that never sends, started long enough ago to be past the
        // give-up point.
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ActionOutcome>();
        let started = std::time::Instant::now() - (ACTION_TIMEOUT + Duration::from_secs(1));
        state.inflight.insert(DaemonKind::McpPool.id(), (rx, started));

        state.poll_actions();

        assert!(
            !state.inflight.contains_key(DaemonKind::McpPool.id()),
            "the guard must release so the row is actionable again"
        );
        let outcome = state
            .outcomes
            .get(DaemonKind::McpPool.id())
            .expect("giving up must leave a visible outcome");
        assert!(!outcome.ok);
        assert!(
            outcome.detail.contains("did not finish"),
            "the row must say what happened, got {:?}",
            outcome.detail
        );
        drop(tx);
    }

    /// Esc unwinds the innermost overlay first. Popping straight out from under
    /// an open error view would throw away what the user just opened.
    #[test]
    fn esc_closes_the_error_view_before_the_menu() {
        let mut state = seeded_state(vec![status(
            DaemonKind::Atc,
            DaemonState::Stopped,
            false,
            None,
        )]);
        state.open_menu();
        state.error_open = Some(DaemonKind::Atc);
        state.close_overlay();
        assert!(state.error_open.is_none(), "the error view closes first");
        assert!(state.menu.is_some(), "the menu is still open underneath");
        state.close_overlay();
        assert!(state.menu.is_none(), "then the menu closes");
        assert!(!state.has_overlay(), "and the screen is free to pop");
    }

    #[test]
    /// The reported failure was a pointer aimed at a deleted worktree while a
    /// perfectly good ainb was running. The panel has to show BOTH paths, or
    /// the only way to see the mismatch is to go and read the pointer file.
    #[test]
    fn hook_panel_shows_the_pointer_and_the_running_binary() {
        let mut health = hook_health();
        health.hook_binary = Some(PathBuf::from("/gone/worktree/target/debug/ainb"));
        health.hook_binary_ready = false;
        health.hook_binary_mode = Some(HookBinaryMode::Dev);
        health.running_binary = Some(PathBuf::from("/home/u/.local/bin/ainb"));
        let mut state = seeded_state_with_hook(Vec::new(), health);

        let out = render_to_string(&mut state, 160, 30);

        assert!(
            out.contains("/gone/worktree/target/debug/ainb"),
            "the hook pointer must be visible: {out}"
        );
        assert!(
            out.contains("/home/u/.local/bin/ainb"),
            "the running binary must be visible beside it: {out}"
        );
        assert!(
            out.contains("B pin running"),
            "the pin-running action must be advertised: {out}"
        );
    }

    #[test]
    fn renders_hook_repair_progress_from_its_own_state() {
        let mut state = seeded_state_with_hook(Vec::new(), hook_health());
        state.hooks_status = Some("hooks repaired for claude, codex".to_string());
        let out = render_to_string(&mut state, 120, 24);
        assert!(
            out.contains("I hooks repaired for claude, codex"),
            "hook repair status missing: {out}"
        );
    }

    #[test]
    fn collect_into_publishes_into_the_shared_snapshot() {
        // The background collector's publish step populates the snapshot from a
        // real collect() (every daemon) without any render involved.
        let shared = Mutex::new(Snapshot::default());
        collect_into(&shared);
        let guard = shared.lock().unwrap();
        assert_eq!(
            guard.rows.len(),
            crate::cli::daemon::CONTROLLABLE.len(),
            "collect publishes every daemon"
        );
        assert!(
            guard.collected_at_ms > 0,
            "publish stamps the collect clock"
        );
    }

    #[test]
    fn render_does_not_panic_on_default_state() {
        // A fresh state lazily spawns the background collector; the first render
        // sees an empty snapshot (the collector hasn't published yet) and must
        // render an empty table without panicking.
        let mut state = DaemonsState::default();
        let _ = render_to_string(&mut state, 100, 10);
        // The collector handle is now installed (spawned lazily on first render).
        assert!(
            state.shared.is_some(),
            "first render must arm the collector"
        );
    }
}
