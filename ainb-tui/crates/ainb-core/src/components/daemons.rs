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

use crate::app::state::DaemonsOverlayState;
use crate::cli::daemon::Action;
use crate::cli::fleet::daemons::{fmt_ago, fmt_duration_ms};
use crate::fleet::atc::SupervisorMode;
use crate::fleet::daemons::heartbeat::now_ms;
use crate::fleet::daemons::probe::{DaemonKind, DaemonState, DaemonStatus};
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
    /// The ATC supervisor's mode + full-mode provider, when a single instance
    /// makes it unambiguous. Read off disk by the collector, never in render.
    ///
    /// `None` when there is no instance, several (nothing here could say WHICH
    /// one a switch would act on), or the meta will not parse. The mode toggle
    /// and the inline help are both hidden in that case rather than guessing.
    pub atc: Option<AtcModeView>,
}

/// What the Daemons screen needs to know about the ATC supervisor beyond its
/// runtime row: which mode owns the fleet, and which brain full mode would use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtcModeView {
    pub name: String,
    pub mode: SupervisorMode,
    pub provider: String,
    /// The mode help, rendered once by the collector.
    ///
    /// `mode_help` rebuilds the whole provider registry (five `Arc`s, a HashMap,
    /// a Vec) and allocates seven `String`s. Calling it from `render` ran that
    /// every frame while the ATC row was selected, in the file whose entire
    /// design is about keeping work off the UI thread. It is a pure function of
    /// (mode, provider), so it belongs on the snapshot with everything else.
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
}

/// The open action menu: which daemon it belongs to and where the cursor is.
#[derive(Debug)]
struct ActionMenu {
    kind: DaemonKind,
    /// Index into [`ActionMenu::entries`].
    cursor: usize,
    /// The ATC supervisor mode as of the moment the menu opened. Captured, not
    /// re-read per frame: the entry list must not reshuffle under the cursor
    /// while someone is on it, which is how you press "switch to lite" and get
    /// "stop".
    atc_mode: Option<SupervisorMode>,
}

/// One entry in the action menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuEntry {
    /// A lifecycle verb.
    Act(Action),
    /// Show the full error of this row's last failed action.
    ViewError,
}

impl ActionMenu {
    /// The entries for this menu. `view last error` only appears when there IS
    /// one — an always-present entry that usually does nothing is noise.
    fn entries(&self, has_error: bool) -> Vec<MenuEntry> {
        // Per-kind, not `Action::ALL`: only the daemon that owns the Codex
        // transport offers `pair`.
        let mut entries: Vec<MenuEntry> = Action::for_kind_in_mode(self.kind, self.atc_mode)
            .into_iter()
            .map(MenuEntry::Act)
            .collect();
        if has_error {
            entries.push(MenuEntry::ViewError);
        }
        entries
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
        let shared = self.shared();
        let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.rows.get(self.selected).map(|r| r.kind)
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
        let atc_mode = self.atc_mode();
        if let Some(kind) = self.selected_kind() {
            self.menu = Some(ActionMenu {
                kind,
                cursor: 0,
                atc_mode,
            });
        }
    }

    /// The ATC supervisor mode from the latest snapshot, when unambiguous.
    fn atc_mode(&mut self) -> Option<SupervisorMode> {
        let shared = self.shared();
        let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        guard.atc.as_ref().map(|a| a.mode)
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

    /// Read the latest published snapshot. Off the render path this is a pure
    /// memory read under a microsecond lock.
    pub fn snapshot(&mut self) -> Snapshot {
        let shared = self.shared();
        let guard = shared.lock().unwrap_or_else(|p| p.into_inner());
        Snapshot {
            rows: guard.rows.clone(),
            collected_at_ms: guard.collected_at_ms,
            hook_health: guard.hook_health.clone(),
            atc: guard.atc.clone(),
        }
    }
}

/// Shell one `ainb daemon <kind> <action>` and capture everything it said.
///
/// Runs on a throwaway thread. The captured argv, exit status, and output are
/// what the row's error view shows verbatim — the operator sees the actual
/// failure, not our summary of it.
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
    let atc = collect_atc_mode();
    match crate::fleet::daemons::collect() {
        Ok(rows) => {
            let mut guard = shared.lock().unwrap_or_else(|p| p.into_inner());
            guard.rows = rows;
            guard.collected_at_ms = now_ms();
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
    let help = crate::fleet::atc::mode_help(meta.mode, &meta.provider);
    Some(AtcModeView {
        name: meta.name,
        mode: meta.mode,
        provider: meta.provider,
        help,
    })
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
pub fn render(
    frame: &mut Frame,
    area: Rect,
    state: &mut DaemonsState,
    runtime: Option<&DaemonsOverlayState>,
) {
    state.poll_actions();
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

    // Carve a one-line help footer off the bottom, plus — when the cursor is on
    // the ATC row — the supervisor-mode help above it. The help is inline rather
    // than an overlay on purpose: an operator about to switch the thing that
    // drives their whole fleet should be reading what each mode does WHILE the
    // row's real state is still on screen, not instead of it.
    // WRAP FIRST, then budget. `mode_help`'s longest line is ~95 chars, so on an
    // 80-column terminal an unwrapped Paragraph clipped it to
    // "never resolves an ambiguo" — the stated limit of the mode cut off exactly
    // where it matters, on the screen whose whole purpose is to inform a switch.
    // The height must come from the WRAPPED count or the extra lines overflow
    // the chunk they were budgeted into.
    let atc_help = wrap_help(&atc_help_lines(&snapshot, state.selected), inner.width);
    let wanted = u16::try_from(atc_help.len()).unwrap_or(0);
    // The eviction order is: help first, then the Hooks box, then never the
    // table. Budgeting only against the table let the help silently delete the
    // hook-health panel on a 19-23 row terminal — it vanished when the cursor
    // landed on the ATC row and came back when it left, with nothing to say so.
    // HOOKS_SECTION_ROWS + HOOKS_MIN_TABLE is what `render` below requires to
    // draw both.
    // The eviction order is: help first, then never the Hooks box, then never
    // the table. Budgeting only against the table let the help silently delete
    // the hook-health panel on a mid-size terminal — it vanished when the cursor
    // landed on the ATC row and came back when it left, with nothing to say so.
    //
    // `render` below draws the Hooks section only when the table chunk still has
    // 14 rows, so that is the number the help has to respect.
    const HOOKS_NEEDS: u16 = 14;
    const TABLE_MIN: u16 = 7;
    const FOOTER: u16 = 1;
    // The first attempt at this budget fixed the silent eviction by making the
    // help almost never appear: it required `inner.height >= wanted + 15`, so on
    // a standard 80x24 terminal (inner 78x22, wanted 12 after wrapping) the
    // screen whose stated purpose is to inform a mode switch showed nothing at
    // all. Hiding the thing is not a fix for hiding the wrong thing.
    //
    // What was actually wrong in round one was that the hooks panel vanished
    // SILENTLY. So the help renders whenever the table stays usable, and when it
    // costs the operator the hooks panel it SAYS so, on a line it pays for out
    // of its own budget.
    let without_help = inner.height.saturating_sub(FOOTER);
    let hooks_fit_without_help = without_help >= HOOKS_NEEDS;
    let displaces_hooks = wanted > 0
        && hooks_fit_without_help
        && inner.height.saturating_sub(wanted + FOOTER) < HOOKS_NEEDS;
    let atc_help = if displaces_hooks {
        let mut lines = atc_help;
        lines.push("(hook health hidden at this height — move off this row to see it)".to_string());
        lines
    } else {
        atc_help
    };
    let wanted = u16::try_from(atc_help.len()).unwrap_or(0);
    let with_help = inner.height.saturating_sub(wanted + FOOTER);
    let help_height = if wanted > 0 && with_help >= TABLE_MIN {
        wanted
    } else {
        // No help, or showing it would squeeze the table itself — and the table
        // is the screen. The footer still names the CLI verb that prints the
        // same text.
        0
    };
    debug_assert!(
        help_height == 0 || inner.height.saturating_sub(help_height + FOOTER) >= TABLE_MIN,
        "the help must never squeeze the table below a usable size"
    );
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(help_height),
            Constraint::Length(1),
        ])
        .split(inner);

    // One table, plus the Hooks box. The old System services panel is gone: it
    // listed the MCP pool, the Hangar daemon and the Headroom proxy as ad-hoc
    // lines because they had no DaemonKind, and it sat on "collecting…" when
    // its separate async fetch wedged. Those three are real rows now, so the
    // panel was a second place to look that showed strictly less.
    if chunks[0].height >= 14 {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(7), Constraint::Length(7)])
            .split(chunks[0]);
        render_table(frame, sections[0], &snapshot, state);
        render_hook_section(
            frame,
            sections[1],
            snapshot.hook_health.as_ref(),
            runtime,
            snapshot.collected_at_ms > 0,
        );
    } else {
        render_table(frame, chunks[0], &snapshot, state);
    }
    if help_height > 0 {
        render_atc_help(frame, chunks[1], &atc_help);
    }
    // The hint follows the SELECTION, not merely "an ATC exists somewhere":
    // advertising a mode switch while the cursor sits on the bridge promises a
    // menu entry that `Action::for_kind_in_mode` will not offer.
    let atc_selected = snapshot.rows.get(state.selected).map(|r| r.kind) == Some(DaemonKind::Atc);
    render_footer(
        frame,
        chunks[2],
        state,
        snapshot.atc.as_ref().filter(|_| atc_selected),
    );
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
    // Wide enough for the longest label ("switch to full mode"), which the old
    // 30-column popup clipped.
    let width = 34_u16.min(area.width.saturating_sub(2));
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
            MenuEntry::Act(a) => a.label(),
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
    runtime: Option<&DaemonsOverlayState>,
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
            let issue =
                runtime.and_then(|runtime| runtime.hooks_repair_status.as_deref()).map_or_else(
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
                        "script {}  ·  ainb binary {} ({}){}",
                        if health.script_ready { "✓" } else { "✗" },
                        if health.hook_binary_ready {
                            "✓"
                        } else {
                            "✗"
                        },
                        health.hook_binary_mode.map(|mode| mode.label()).unwrap_or("unknown"),
                        health
                            .hook_binary
                            .as_ref()
                            .map_or_else(String::new, |target| format!(" · {}", target.display()),),
                    ),
                    Style::default().fg(if health.script_ready && health.hook_binary_ready {
                        HEALTHY_GREEN
                    } else {
                        STOPPED_RED
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

/// The supervisor-mode help for the ATC row, or empty when the cursor is
/// elsewhere / the mode is unknown.
///
/// The lines come from [`crate::fleet::atc::mode_help`] — the same text
/// `ainb fleet atc mode` prints — so the screen and the CLI can never describe
/// the modes differently.
fn atc_help_lines(snapshot: &Snapshot, selected: usize) -> Vec<String> {
    if snapshot.rows.get(selected).map(|r| r.kind) != Some(DaemonKind::Atc) {
        return Vec::new();
    }
    let Some(atc) = snapshot.atc.as_ref() else {
        return Vec::new();
    };
    atc.help.clone()
}

/// Wrap help lines to `width`, preserving each line's leading indent.
///
/// Done here rather than with `Paragraph::wrap` because the caller has to budget
/// the block's height, and only the WRAPPED count is the real height.
///
/// The indent is load-bearing: `mode_help` indents its "limits:" lines so they
/// read as belonging to the mode above them. An earlier version seeded the first
/// output line from the first WORD, which silently dropped that indent and
/// detached every limits line from its mode.
///
/// ponytail: a single word longer than `width` is emitted over-long rather than
/// hard-split. Nothing in `mode_help` comes close, and an over-long line is
/// clipped horizontally without changing the line COUNT, so the height budget
/// stays correct. Hard-split if that ever stops being true.
fn wrap_help(lines: &[String], width: u16) -> Vec<String> {
    let width = usize::from(width).max(20);
    let mut out = Vec::new();
    for line in lines {
        if line.chars().count() <= width {
            out.push(line.clone());
            continue;
        }
        let lead: String = line.chars().take_while(|c| c.is_whitespace()).collect();
        // Continuations sit two columns inside the original indent, so a wrapped
        // line is visibly a continuation and not a new bullet.
        let hang = format!("{lead}  ");
        let mut current = lead.clone();
        let mut has_word = false;
        for word in line.split_whitespace() {
            let prospective = if has_word {
                current.chars().count() + 1 + word.chars().count()
            } else {
                current.chars().count() + word.chars().count()
            };
            if prospective > width && has_word {
                out.push(std::mem::take(&mut current));
                current.push_str(&hang);
                has_word = false;
            }
            if has_word {
                current.push(' ');
            }
            current.push_str(word);
            has_word = true;
        }
        if has_word {
            out.push(current);
        }
    }
    out
}

/// Paint the mode help. The first line (the current owner) is emphasised: it is
/// the fact the rest of the block is context for.
fn render_atc_help(frame: &mut Frame, area: Rect, lines: &[String]) {
    let painted: Vec<Line> = lines
        .iter()
        .enumerate()
        .map(|(i, text)| {
            let style = if i == 0 {
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(MUTED_GRAY)
            };
            Line::from(Span::styled(text.clone(), style))
        })
        .collect();
    frame.render_widget(
        Paragraph::new(painted).style(Style::default().bg(PANEL_BG)),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, state: &DaemonsState, atc: Option<&AtcModeView>) {
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
        let enter_hint = match atc {
            Some(a) => format!(
                " start / restart / stop / switch to {} mode  ",
                a.mode.other().id()
            ),
            None => " start / restart / stop  ".to_string(),
        };
        vec![
            Span::styled("↑/↓", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(" select  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("Enter", Style::default().fg(CORNFLOWER_BLUE)),
            Span::styled(enter_hint, Style::default().fg(MUTED_GRAY)),
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
            atc: None,
            rows,
            collected_at_ms: now_ms(),
            hook_health: None,
        }));
        DaemonsState {
            shared: Some(shared),
            ..DaemonsState::default()
        }
    }

    fn atc_view(mode: SupervisorMode, provider: &str) -> AtcModeView {
        AtcModeView {
            name: "tower".to_string(),
            mode,
            provider: provider.to_string(),
            help: crate::fleet::atc::mode_help(mode, provider),
        }
    }

    /// A seeded state whose ATC supervisor mode is known — the shape the mode
    /// toggle and the inline help both read.
    fn seeded_state_with_atc(rows: Vec<DaemonStatus>, mode: SupervisorMode) -> DaemonsState {
        let shared = Arc::new(Mutex::new(Snapshot {
            atc: Some(atc_view(mode, "claude")),
            rows,
            collected_at_ms: now_ms(),
            hook_health: None,
        }));
        DaemonsState {
            shared: Some(shared),
            ..DaemonsState::default()
        }
    }

    /// A seeded state carrying BOTH the ATC mode and hook health, for the
    /// layout-budget tests.
    fn seeded_state_with_atc_and_hooks(
        rows: Vec<DaemonStatus>,
        mode: SupervisorMode,
    ) -> DaemonsState {
        let shared = Arc::new(Mutex::new(Snapshot {
            atc: Some(atc_view(mode, "claude")),
            rows,
            collected_at_ms: now_ms(),
            hook_health: Some(hook_health()),
        }));
        DaemonsState {
            shared: Some(shared),
            ..DaemonsState::default()
        }
    }

    fn atc_row() -> DaemonStatus {
        status(DaemonKind::Atc, DaemonState::Running, true, Some("tower"))
    }

    // ── Supervisor mode toggle ──────────────────────────────────────────────

    #[test]
    fn the_atc_menu_offers_only_the_mode_the_fleet_is_not_in() {
        // Offering both would put "switch to the mode you are already in" on
        // screen, which reads as a state the fleet might not be in.
        for (mode, expected, forbidden) in [
            (SupervisorMode::Full, Action::ModeLite, Action::ModeFull),
            (SupervisorMode::Lite, Action::ModeFull, Action::ModeLite),
        ] {
            let mut state = seeded_state_with_atc(vec![atc_row()], mode);
            state.open_menu();
            let menu = state.menu.as_ref().expect("menu opens on the ATC row");
            let entries = menu.entries(false);
            assert!(
                entries.contains(&MenuEntry::Act(expected)),
                "{} mode must offer {expected:?}: {entries:?}",
                mode.id()
            );
            assert!(
                !entries.contains(&MenuEntry::Act(forbidden)),
                "{} mode must not offer {forbidden:?}",
                mode.id()
            );
        }
    }

    #[test]
    fn a_non_atc_row_never_offers_a_mode_switch() {
        // Mode is an ATC supervisor concept; a bridge has no modes to switch.
        let mut state = seeded_state_with_atc(
            vec![status(DaemonKind::Bridge, DaemonState::Running, true, None)],
            SupervisorMode::Full,
        );
        state.open_menu();
        let menu = state.menu.as_ref().unwrap();
        for entry in menu.entries(false) {
            assert!(
                !matches!(entry, MenuEntry::Act(Action::ModeLite | Action::ModeFull)),
                "bridge offered a mode switch: {entry:?}"
            );
        }
    }

    #[test]
    fn no_mode_switch_is_offered_when_the_mode_is_unknown() {
        // Several instances, or an unreadable meta: nothing here could say WHICH
        // fleet a switch would act on, and a guessed one is worse than none.
        let mut state = seeded_state(vec![atc_row()]);
        state.open_menu();
        let menu = state.menu.as_ref().unwrap();
        for entry in menu.entries(false) {
            assert!(
                !matches!(entry, MenuEntry::Act(Action::ModeLite | Action::ModeFull)),
                "offered a switch with no known mode: {entry:?}"
            );
        }
    }

    #[test]
    fn the_open_menu_does_not_reshuffle_when_the_collector_republishes() {
        // The entry list is captured at open. If it re-read the mode per frame,
        // a collect landing mid-keystroke would move the cursor's meaning — you
        // press "switch to lite" and get "stop".
        let mut state = seeded_state_with_atc(vec![atc_row()], SupervisorMode::Full);
        state.open_menu();
        let before = state.menu.as_ref().unwrap().entries(false);

        // The collector publishes a switched mode underneath the open menu.
        {
            let shared = state.shared();
            let mut guard = shared.lock().unwrap();
            guard.atc = Some(atc_view(SupervisorMode::Lite, "claude"));
        }
        let after = state.menu.as_ref().unwrap().entries(false);
        assert_eq!(before, after, "the open menu must not reshuffle");
    }

    #[test]
    fn the_menu_labels_say_which_mode_they_switch_to() {
        let mut state = seeded_state_with_atc(vec![atc_row()], SupervisorMode::Full);
        state.open_menu();
        let text = render_to_string(&mut state, None, 100, 24);
        assert!(
            text.contains("switch to lite mode"),
            "a bare verb id would not say what it does: {text}"
        );
    }

    // ── Inline mode help ────────────────────────────────────────────────────

    #[test]
    fn selecting_the_atc_row_explains_both_modes_and_names_the_owner() {
        let mut state = seeded_state_with_atc(vec![atc_row()], SupervisorMode::Full);
        let text = render_to_string(&mut state, None, 120, 30);
        assert!(text.contains("full heartbeat"), "current owner: {text}");
        assert!(text.contains("no LLM"), "lite behaviour: {text}");
        assert!(text.contains("never answers an ASK"), "lite limits: {text}");
        assert!(text.contains("spends tokens"), "full limits: {text}");
    }

    #[test]
    fn the_help_comes_from_the_same_text_the_cli_prints() {
        // One source for both surfaces: a screen and a CLI that describe the
        // modes differently is how an operator switches the wrong way.
        let snapshot = Snapshot {
            atc: Some(atc_view(SupervisorMode::Lite, "codex")),
            rows: vec![atc_row()],
            collected_at_ms: now_ms(),
            hook_health: None,
        };
        assert_eq!(
            atc_help_lines(&snapshot, 0),
            crate::fleet::atc::mode_help(SupervisorMode::Lite, "codex")
        );
    }

    #[test]
    fn the_help_is_absent_on_every_other_row() {
        let snapshot = Snapshot {
            atc: Some(atc_view(SupervisorMode::Full, "claude")),
            rows: vec![
                status(DaemonKind::Bridge, DaemonState::Running, true, None),
                atc_row(),
            ],
            collected_at_ms: now_ms(),
            hook_health: None,
        };
        assert!(atc_help_lines(&snapshot, 0).is_empty(), "bridge row");
        assert!(!atc_help_lines(&snapshot, 1).is_empty(), "atc row");
    }

    #[test]
    fn the_footer_offers_the_mode_switch_only_on_the_atc_row() {
        // The footer promised "switch to lite mode" on every row, while the menu
        // only ever offers it on ATC — a hint for a key that does nothing.
        let rows = vec![
            status(DaemonKind::Bridge, DaemonState::Running, true, None),
            atc_row(),
        ];
        let mut state = seeded_state_with_atc(rows, SupervisorMode::Full);

        let on_bridge = render_to_string(&mut state, None, 120, 30);
        assert!(
            !on_bridge.contains("switch to lite mode"),
            "the bridge row must not advertise an ATC-only action: {on_bridge}"
        );

        state.move_selection(1);
        let on_atc = render_to_string(&mut state, None, 120, 30);
        assert!(on_atc.contains("switch to lite mode"), "{on_atc}");
    }

    #[test]
    fn the_help_wraps_instead_of_clipping_the_limits_it_exists_to_state() {
        // On 80 columns the longest help line was cut at "…ambiguo", losing the
        // limit on the screen whose purpose is to inform a switch.
        let lines = crate::fleet::atc::mode_help(SupervisorMode::Full, "claude");
        let wrapped = wrap_help(&lines, 78);
        assert!(
            wrapped.iter().all(|l| l.chars().count() <= 78),
            "a wrapped line still overflows: {wrapped:?}"
        );
        // Wrapping inserts a continuation indent, so compare on collapsed
        // whitespace — the phrase surviving across a line break is the point.
        let joined = wrapped.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        for phrase in [
            "never answers an ASK",
            "no fleet coordination",
            "spends tokens",
        ] {
            assert!(
                joined.contains(phrase),
                "wrapping lost {phrase:?}: {joined}"
            );
        }
        assert!(
            wrapped.len() >= lines.len(),
            "wrapping cannot shrink the block"
        );
    }

    #[test]
    fn the_help_renders_on_a_standard_eighty_by_twentyfour_terminal() {
        // The regression an over-cautious budget introduced: suppressing the
        // help whenever it would cost the hooks panel meant it needed a 29-row
        // terminal, so the screen whose purpose is to inform a mode switch
        // showed nothing at the commonest size. Hiding the thing is not a fix
        // for hiding the wrong thing.
        let mut state = seeded_state_with_atc_and_hooks(vec![atc_row()], SupervisorMode::Full);
        let text = render_to_string(&mut state, None, 80, 24);
        assert!(
            text.contains("never answers an ASK"),
            "the mode help must render at 80x24: {text}"
        );
    }

    #[test]
    fn the_help_never_squeezes_the_table_itself() {
        // The one thing that outranks both help and hooks: the rows ARE the
        // screen.
        for height in 8..40_u16 {
            let mut state = seeded_state_with_atc_and_hooks(vec![atc_row()], SupervisorMode::Full);
            let text = render_to_string(&mut state, None, 100, height);
            assert!(
                text.contains("ATC"),
                "height {height}: the daemon row was squeezed out"
            );
        }
    }

    #[test]
    fn the_hooks_panel_never_disappears_without_saying_so() {
        // A sweep rather than one band: the budget is a size calculation, so the
        // property is the invariant, not any single terminal size.
        for height in 8..40_u16 {
            let mut bare = seeded_state_with_atc_and_hooks(vec![atc_row()], SupervisorMode::Full);
            // Selection defaults to the ATC row (index 0) in both, so the only
            // difference is whether the help is eligible at this height.
            let with_atc_selected = render_to_string(&mut bare, None, 100, height);

            let mut other = seeded_state_with_atc_and_hooks(
                vec![
                    atc_row(),
                    status(DaemonKind::Bridge, DaemonState::Running, true, None),
                ],
                SupervisorMode::Full,
            );
            other.move_selection(1); // off the ATC row: no help
            let without_help = render_to_string(&mut other, None, 100, height);

            // The panel may yield to the help. What it may never do is vanish
            // in silence, which was the actual complaint.
            if without_help.contains("Hooks") && !with_atc_selected.contains("Hooks") {
                assert!(
                    with_atc_selected.contains("hook health hidden"),
                    "height {height}: the hooks panel vanished with nothing to say so"
                );
            }
        }
    }

    #[test]
    fn a_short_terminal_drops_the_help_rather_than_the_table() {
        // The rows are the point of the screen; the help is context. On a
        // terminal too short for both, the help goes.
        let mut state = seeded_state_with_atc(vec![atc_row()], SupervisorMode::Full);
        let text = render_to_string(&mut state, None, 100, 10);
        assert!(text.contains("ATC"), "the row must survive: {text}");
        assert!(
            !text.contains("never answers an ASK"),
            "the help must not squeeze the table: {text}"
        );
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
            atc: None,
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
    fn render_to_string(
        state: &mut DaemonsState,
        runtime: Option<&DaemonsOverlayState>,
        w: u16,
        h: u16,
    ) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.area(), state, runtime)).unwrap();
        let buf = terminal.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    /// Render and return the buffer as LINES, so a test can ask which row the
    /// cursor is drawn on rather than only whether a glyph exists somewhere.
    fn render_to_lines(
        state: &mut DaemonsState,
        runtime: Option<&DaemonsOverlayState>,
        w: u16,
        h: u16,
    ) -> Vec<String> {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, f.area(), state, runtime)).unwrap();
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

            let lines = render_to_lines(&mut state, None, 160, 30);
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

    fn system_runtime() -> DaemonsOverlayState {
        DaemonsOverlayState {
            selected: crate::app::state::DaemonRow::ORDER[0],
            mcp_alive: true,
            mcp_runtime: crate::mcp_pool::client::DaemonRuntimeStatus::default(),
            headroom: ProxyStatus {
                running: true,
                port: 8787,
                pid: Some(42),
                tokens_saved: Some(9),
            },
            headroom_consumers: Vec::new(),
            notifyd: Vec::new(),
            approve_running: true,
            approve_reason: "serving".to_string(),
            hangar_running: true,
            hangar_reason: "running".to_string(),
            hangar_runtime: crate::cli::hangar::DaemonRuntimeStatus::default(),
            loading: false,
            last_refreshed: None,
            fetch_rx: None,
            restart_rx: None,
            restart_status: None,
            hooks_repair_rx: None,
            hooks_repair_status: None,
            hangar_start_rx: None,
            hangar_start_status: None,
            mcp_start_rx: None,
            mcp_start_status: None,
            headroom_start_rx: None,
            headroom_start_status: None,
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
        let out = render_to_string(&mut state, None, 120, 12);
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
        let out = render_to_string(&mut state, None, 120, 8);
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
        let runtime = system_runtime();
        let out = render_to_string(&mut state, Some(&runtime), 120, 24);
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
        let out = render_to_string(&mut state, None, 120, 24);
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
        let out = render_to_string(&mut state, None, 120, 24);
        assert!(
            out.contains("✗ failed"),
            "row must badge the failure: {out}"
        );
        assert!(
            out.contains("Enter → error"),
            "the row must say how to read the error: {out}"
        );

        state.open_menu();
        // start, restart, stop, then `view last error` — the entry only exists
        // because this row HAS an error.
        state.move_menu(3);
        state.confirm_menu();
        let out = render_to_string(&mut state, None, 120, 24);
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
        let out = render_to_string(&mut state, None, 120, 24);
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
    fn renders_hook_repair_progress_from_runtime_state() {
        let mut state = seeded_state_with_hook(Vec::new(), hook_health());
        let mut runtime = system_runtime();
        runtime.hooks_repair_status = Some("hooks repaired for claude, codex".to_string());
        let out = render_to_string(&mut state, Some(&runtime), 120, 24);
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
        let _ = render_to_string(&mut state, None, 100, 10);
        // The collector handle is now installed (spawned lazily on first render).
        assert!(
            state.shared.is_some(),
            "first render must arm the collector"
        );
    }
}
