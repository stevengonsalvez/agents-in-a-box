// ABOUTME: Screen 1 of the new-session redesign — the unified repo picker.
// Renders favorites + recents + local scans in a single fuzzy-filtered list,
// smart-parses raw input on Enter, and persists screen-1 last-selection via
// `SessionDefaults`. Phase 4 of `plans/new-session-redesign-spec.md`.
//
// The screen is host-owned (no plugin involvement). Its key handler returns a
// `PickRepoOutcome` that the central event dispatcher translates into the
// appropriate next action (advance to `Configure`, kick off a clone, return
// to home, etc.). See `app/events.rs::handle_new_session_keys` for the wire.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph},
};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::config::favorites_store::{Favorite, FavoritesStore, SourceType};
use crate::config::session_defaults::SessionDefaults;
use crate::git::repo_source::{RealFs, RepoSource, parse_with};

// Palette — matches `components/layout.rs` style guide. Kept module-local so
// the picker doesn't reach into layout internals.
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const LIST_HIGHLIGHT_BG: Color = Color::Rgb(40, 40, 60);

/// What kind of row this is in the unified picker. Drives the leading marker
/// (`★` favorite, `⌚` recent, `📁` local) and the sort precedence
/// (favorites → recents → locals).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// User-pinned favorite, sourced from `favorites.yaml`.
    Favorite,
    /// Recently launched repo, sourced from `session-defaults.yaml.per_repo`.
    Recent,
    /// Local-disk scan or favorite-with-local-path.
    Local,
}

impl RowKind {
    const fn marker(self) -> &'static str {
        match self {
            Self::Favorite => "\u{2605}", // ★
            Self::Recent => "\u{231a}",   // ⌚
            Self::Local => "\u{1f4c1}",   // 📁
        }
    }
}

/// A single row in the picker list. `id` is the stable identity used by
/// persistence (`SessionDefaults.last_repo`) — for favorites it's the alias,
/// for locals it's the filesystem path stringified.
#[derive(Debug, Clone)]
pub struct PickRepoRow {
    pub id: String,
    pub label: String,
    pub source: RepoSource,
    pub kind: RowKind,
}

/// Inline clone progress shown on the highlighted row when a remote clone is
/// in flight. Phase 4 wires the spinner; the bytes/total fields are populated
/// by the async clone driver in Phase 5+.
#[derive(Debug, Clone)]
pub struct CloneProgress {
    pub url: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub error: Option<String>,
}

/// GitHub auth pre-check status shown inline on the picker when a remote
/// URL requires authentication. The dispatcher runs `gh auth status` before
/// advancing to Configure for HTTPS/GitHub sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitAuthStatus {
    /// Async check in flight.
    Checking,
    /// `gh auth status` succeeded — the dispatcher auto-advances.
    Authenticated,
    /// Not authenticated — show inline instructions.
    NotAuthenticated,
}

/// Outcome of a single key press on the picker. The caller (events.rs)
/// translates this into the appropriate `AppEvent` / async action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickRepoOutcome {
    /// Re-render the same state — filter typed, selection moved, etc.
    Stay,
    /// Advance to Configure screen with the resolved source.
    AdvanceTo(RepoSource),
    /// Esc pressed with no filter and no in-flight clone — return to home.
    BackToHome,
    /// Source needs an async clone before advancing. Phase 5 wires the
    /// spinner display; Phase 4 stops here.
    StartClone(RepoSource),
    /// Ctrl+V pressed — the caller (events.rs) reads the OS clipboard and
    /// appends it to the filter via `append_filter`. Clipboard access lives
    /// in the app layer (`EventHandler::get_clipboard_text`), keeping this
    /// component pure and testable.
    PasteFromClipboard,
    /// Surface a transient message to the user (favorite added/removed, or a
    /// refusal) and stay on the picker. The dispatcher maps `is_error` to an
    /// error vs. info notification.
    Notice { message: String, is_error: bool },
}

/// Result of toggling a favorite — drives the `^F` notification.
enum FavoriteToggle {
    /// Newly favorited; carries the stored remote source for display.
    Added(String),
    /// Un-favorited; carries the row label for display.
    Removed(String),
    /// Refused because the row has no remote repository indicator.
    Refused(String),
}

/// Persistent state for the picker. Constructed once per new-session
/// invocation. Owned by `NewSessionState.pick_repo_state`.
#[derive(Debug)]
pub struct PickRepoState {
    /// Current filter text (also doubles as smart-parse input on Enter when
    /// no row matches).
    pub filter: String,
    /// All rows in display order (favorites → recents → locals).
    pub rows: Vec<PickRepoRow>,
    /// Indices into `rows` that match the current filter, preserving order.
    pub filtered_indices: Vec<usize>,
    /// Cursor position in `filtered_indices`.
    pub selected: usize,
    /// Inline clone progress for the highlighted row (None when idle).
    pub clone_progress: Option<CloneProgress>,
    /// GitHub auth pre-check status. Set by the dispatcher before allowing
    /// HTTPS/GitHub clones. `None` = no check in progress or needed.
    pub git_auth_status: Option<GitAuthStatus>,
    /// Source that triggered the auth check, held until auth passes or user skips.
    pub pending_clone_source: Option<RepoSource>,
    /// Snapshot of session-defaults — read on open, updated on `^R`.
    pub defaults: SessionDefaults,
    /// Snapshot of favorites — read on open, updated on `^F`.
    pub favorites: FavoritesStore,
}

impl PickRepoState {
    /// Build initial state from on-disk persistence + a list of locally
    /// available repo paths. The caller (events.rs / `state.rs::AppState`)
    /// passes the local scan result so this module stays pure.
    pub fn from_disk(local_repos: &[PathBuf]) -> Self {
        let defaults = SessionDefaults::load_from(&SessionDefaults::default_path());
        let favorites = FavoritesStore::load();
        let rows = build_rows(&favorites, &defaults, local_repos);
        let filtered_indices: Vec<usize> = (0..rows.len()).collect();
        let selected = pick_default_selection(&rows, &filtered_indices, &defaults);
        Self {
            filter: String::new(),
            rows,
            filtered_indices,
            selected,
            clone_progress: None,
            git_auth_status: None,
            pending_clone_source: None,
            defaults,
            favorites,
        }
    }

    /// Convenience for the legacy code path / tests that don't have local
    /// scan data yet. Yields favorites + recents only.
    pub fn from_disk_no_locals() -> Self {
        Self::from_disk(&[])
    }

    /// The highlighted row, if any rows are visible.
    pub fn highlighted(&self) -> Option<&PickRepoRow> {
        let idx = *self.filtered_indices.get(self.selected)?;
        self.rows.get(idx)
    }

    /// Append pasted text to the filter (clipboard paste — Ctrl+V or
    /// bracketed `Event::Paste`). Control characters are stripped because the
    /// filter is a single-line field; a pasted `owner/repo\n` should filter,
    /// not submit. Refilters so the list (and Enter's smart-parse) reflect it.
    pub fn append_filter(&mut self, text: &str) {
        // Cap total filter length so a pathological clipboard payload can't
        // bloat the field / stall refilter. A repo URL or path is well under
        // this; anything larger is not a sensible picker query.
        const MAX_FILTER_LEN: usize = 4096;
        let mut cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
        if cleaned.is_empty() {
            return;
        }
        let room = MAX_FILTER_LEN.saturating_sub(self.filter.chars().count());
        if room == 0 {
            return;
        }
        if cleaned.chars().count() > room {
            cleaned = cleaned.chars().take(room).collect();
        }
        self.filter.push_str(&cleaned);
        self.refilter();
    }

    /// Recompute `filtered_indices` when filter or rows change. Preserves
    /// highlight on the previously selected row when possible.
    fn refilter(&mut self) {
        let prev_id = self.highlighted().map(|r| r.id.clone());
        let q = self.filter.to_lowercase();
        self.filtered_indices = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                q.is_empty()
                    || r.label.to_lowercase().contains(&q)
                    || r.id.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        // Restore the highlight if the previously selected row still matches.
        self.selected = match prev_id {
            Some(id) => {
                self.filtered_indices.iter().position(|&i| self.rows[i].id == id).unwrap_or(0)
            }
            None => 0,
        };
    }

    /// Rebuild rows from the latest favorites/defaults snapshots. Called
    /// after `^F` toggles a favorite to refresh the marker.
    fn rebuild_rows(&mut self, local_repos: &[PathBuf]) {
        self.rows = build_rows(&self.favorites, &self.defaults, local_repos);
        self.refilter();
    }
}

/// Pure helper: build the ordered row list from on-disk sources. Favorites
/// pinned first (in their stored order), then recents (most-recent first),
/// then any local-only repos not already represented above.
fn build_rows(
    favorites: &FavoritesStore,
    defaults: &SessionDefaults,
    local_repos: &[PathBuf],
) -> Vec<PickRepoRow> {
    let mut rows: Vec<PickRepoRow> = Vec::new();
    let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Favorites
    for fav in &favorites.favorites {
        let source = favorite_to_source(fav);
        let row = PickRepoRow {
            id: fav.alias.clone(),
            label: fav.display().to_string(),
            source,
            kind: RowKind::Favorite,
        };
        if seen_ids.insert(row.id.clone()) {
            rows.push(row);
        }
    }

    // 2. Recents (from per_repo). Sort by last_used_at descending — most
    // recent first.
    //
    // Finding #1: previously every recent row was stamped with
    // `RepoSource::Filter(alias)` which dispatched to `Stay` — Enter on a
    // recent was a silent no-op. Now we reconstruct the original
    // `RepoSource` from the persisted `source_type` + `source` (added in
    // finding #1), falling back to the favorite-by-alias lookup, and finally
    // to `parse_with(alias, RealFs)` for legacy entries with no provenance.
    let mut recents: Vec<(&String, &crate::config::session_defaults::PerRepoDefaults)> =
        defaults.per_repo.iter().collect();
    recents.sort_by_key(|(_, p)| std::cmp::Reverse(p.last_used_at));
    for (alias, per) in recents {
        if seen_ids.contains(alias) {
            continue;
        }
        let source = recent_source(alias, per, favorites);
        let row = PickRepoRow {
            id: alias.clone(),
            label: alias.clone(),
            source,
            kind: RowKind::Recent,
        };
        if seen_ids.insert(row.id.clone()) {
            rows.push(row);
        }
    }

    // 3. Local-scan repos
    for path in local_repos {
        let id = path.display().to_string();
        if seen_ids.contains(&id) {
            continue;
        }
        let label = path
            .file_name()
            .and_then(|n| n.to_str())
            .map_or_else(|| id.clone(), str::to_string);
        let row = PickRepoRow {
            id: id.clone(),
            label,
            source: RepoSource::LocalPath(path.clone()),
            kind: RowKind::Local,
        };
        if seen_ids.insert(row.id.clone()) {
            rows.push(row);
        }
    }

    rows
}

/// Reconstruct a recent row's `RepoSource` from its persisted provenance
/// (finding #1). Order matches the spec's precedence:
///   1. `per_repo[alias].source_type + source` if set — the explicit
///      provenance written by `record_launch` since the fix.
///   2. Lookup the favorite by alias and clone its `source`/`source_type`.
///   3. Fallback: re-parse the alias via `parse_with` (`RealFs`) — handles
///      legacy pre-fix entries that have no provenance fields.
fn recent_source(
    alias: &str,
    per: &crate::config::session_defaults::PerRepoDefaults,
    favorites: &FavoritesStore,
) -> RepoSource {
    if let (Some(st), Some(src)) = (per.source_type, per.source.as_deref()) {
        return match st {
            SourceType::HttpsUrl => RepoSource::HttpsUrl(src.to_string()),
            SourceType::SshUrl => RepoSource::SshUrl(src.to_string()),
            SourceType::GithubShorthand => parse_with(src, &RealFs),
            SourceType::LocalPath => RepoSource::LocalPath(PathBuf::from(src)),
        };
    }
    if let Some(fav) = favorites.favorites.iter().find(|f| f.alias == alias) {
        return favorite_to_source(fav);
    }
    parse_with(alias, &RealFs)
}

/// Translate a stored `Favorite` into the in-memory `RepoSource` enum so the
/// picker can dispatch identically regardless of provenance.
fn favorite_to_source(fav: &Favorite) -> RepoSource {
    match fav.source_type {
        SourceType::HttpsUrl => RepoSource::HttpsUrl(fav.source.clone()),
        SourceType::SshUrl => RepoSource::SshUrl(fav.source.clone()),
        SourceType::GithubShorthand => {
            // Stored as "owner/repo" — parse_with handles that shape.
            parse_with(&fav.source, &RealFs)
        }
        SourceType::LocalPath => RepoSource::LocalPath(PathBuf::from(&fav.source)),
    }
}

/// Pick the initial cursor position. If `defaults.last_repo` is present and
/// still in the row list, highlight it; otherwise highlight the first row.
fn pick_default_selection(
    rows: &[PickRepoRow],
    filtered: &[usize],
    defaults: &SessionDefaults,
) -> usize {
    if let Some(last) = defaults.last_repo.as_ref() {
        for (i, &row_idx) in filtered.iter().enumerate() {
            if rows.get(row_idx).is_some_and(|r| &r.id == last) {
                return i;
            }
        }
    }
    0
}

/// A dimmed locator shown after the row name so identically-named repos are
/// distinguishable. Derived from the row's `RepoSource`: a `~`-abbreviated path
/// for local repos, the URL for remotes, `owner/repo` for shorthand. `Filter`
/// rows carry no real source, so they get nothing.
fn row_detail(source: &RepoSource) -> Option<Cow<'_, str>> {
    match source {
        RepoSource::LocalPath(p) => Some(Cow::Owned(abbreviate_home(p))),
        RepoSource::HttpsUrl(u) | RepoSource::SshUrl(u) | RepoSource::SshSession(u) => {
            Some(Cow::Borrowed(u))
        }
        RepoSource::GithubShorthand { owner, repo } => Some(Cow::Owned(format!("{owner}/{repo}"))),
        RepoSource::Filter(_) => None,
    }
}

/// Collapse the home-directory prefix to `~` for display. Returns `~` for the
/// home dir itself and the unchanged full path when it lies outside home or
/// when `dirs::home_dir()` is unavailable. The home lookup is cached for the
/// process so it stays off the per-frame render path.
fn abbreviate_home(path: &Path) -> String {
    static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();
    let home = HOME.get_or_init(dirs::home_dir);
    if let Some(home) = home {
        if let Ok(rest) = path.strip_prefix(home) {
            if rest.as_os_str().is_empty() {
                return "~".to_string();
            }
            // `Path::join` so the platform's native separator is used.
            return Path::new("~").join(rest).display().to_string();
        }
    }
    path.display().to_string()
}

/// Render the picker into `area`. Layout: title bar → filter prompt → list →
/// help bar at the bottom. `BorderType::Rounded` everywhere.
#[allow(clippy::too_many_lines)]
pub fn render(f: &mut Frame, state: &PickRepoState, area: Rect) {
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .title(Span::styled(
            " New Session ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(DARK_BG));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // filter prompt
            Constraint::Min(3),    // list
            Constraint::Length(2), // help bar
        ])
        .split(inner);

    // Filter prompt — smart-parse accepts: free text (filter), owner/repo
    // (GitHub clone), https://… or git@host:… (clone), ssh://user@host
    // (SSH session), or a local path. Empty filter shows a greyed-out
    // hint so users discover the typed-input affordance (Stevie 2026-05-22
    // reported he didn't realise typing was supported).
    let prompt_line = if state.filter.is_empty() {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(
                "type to filter, or paste owner/repo · https://… · ssh://user@host · /local/path",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            ),
        ])
    } else {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(state.filter.clone(), Style::default().fg(SOFT_WHITE)),
        ])
    };
    let prompt = Paragraph::new(prompt_line).alignment(Alignment::Left);
    f.render_widget(prompt, chunks[0]);

    // List
    let items: Vec<ListItem> = state
        .filtered_indices
        .iter()
        .enumerate()
        .filter_map(|(visible_idx, &row_idx)| {
            let row = state.rows.get(row_idx)?;
            let is_selected = visible_idx == state.selected;
            let arrow = if is_selected {
                Span::styled(
                    "\u{25b8} ",
                    Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  ")
            };
            let marker = Span::styled(
                format!("{} ", row.kind.marker()),
                Style::default().fg(match row.kind {
                    RowKind::Favorite => GOLD,
                    RowKind::Recent => MUTED_GRAY,
                    RowKind::Local => CORNFLOWER_BLUE,
                }),
            );
            let label_style = if is_selected {
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(SOFT_WHITE)
            };
            let mut spans = vec![arrow, marker, Span::styled(row.label.clone(), label_style)];
            // Dimmed locator after the name so identically-named repos are
            // distinguishable (e.g. two `Rosetta` rows in different folders).
            if let Some(detail) = row_detail(&row.source) {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(detail, Style::default().fg(MUTED_GRAY)));
            }

            // Inline status shown directly under the highlighted row
            // by appending extra lines. Works inside ListItem::new(vec![…]).
            let mut lines = vec![Line::from(spans)];
            if is_selected {
                if let Some(auth) = &state.git_auth_status {
                    match auth {
                        GitAuthStatus::Checking => {
                            lines.push(Line::from(vec![
                                Span::raw("    "),
                                Span::styled(
                                    "\u{1f504} Checking GitHub auth...",
                                    Style::default().fg(Color::Rgb(100, 200, 230)),
                                ),
                            ]));
                        }
                        GitAuthStatus::NotAuthenticated => {
                            let amber = Color::Rgb(255, 191, 0);
                            lines.push(Line::from(vec![
                                Span::raw("    "),
                                Span::styled(
                                    "\u{1f511} GitHub auth required. In another terminal run:",
                                    Style::default().fg(amber),
                                ),
                            ]));
                            lines.push(Line::from(vec![
                                Span::raw("      "),
                                Span::styled(
                                    "gh auth login && gh auth setup-git",
                                    Style::default()
                                        .fg(SELECTION_GREEN)
                                        .add_modifier(Modifier::BOLD),
                                ),
                            ]));
                            lines.push(Line::from(vec![
                                Span::raw("    "),
                                Span::styled("Enter", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                                Span::styled("=Retry  ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("s", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                                Span::styled("=Skip auth  ", Style::default().fg(MUTED_GRAY)),
                                Span::styled("Esc", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
                                Span::styled("=Back", Style::default().fg(MUTED_GRAY)),
                            ]));
                        }
                        GitAuthStatus::Authenticated => {
                            lines.push(Line::from(vec![
                                Span::raw("    "),
                                Span::styled(
                                    "\u{2705} Authenticated",
                                    Style::default().fg(SELECTION_GREEN),
                                ),
                            ]));
                        }
                    }
                } else if let Some(progress) = &state.clone_progress {
                    if let Some(err) = &progress.error {
                        lines.push(Line::from(vec![
                            Span::raw("    "),
                            Span::styled(
                                format!("\u{2715} {err}"),
                                Style::default().fg(Color::Rgb(230, 100, 100)),
                            ),
                        ]));
                    } else {
                        let pct = progress
                            .bytes_done
                            .checked_mul(100)
                            .and_then(|n| n.checked_div(progress.bytes_total))
                            .map_or(0, |p| p.min(100));
                        lines.push(Line::from(vec![
                            Span::raw("    "),
                            Span::styled(
                                format!("\u{2299} cloning {} ({pct}%)", progress.url),
                                Style::default().fg(MUTED_GRAY),
                            ),
                        ]));
                    }
                }
            }
            Some(ListItem::new(lines))
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::NONE).style(Style::default().bg(DARK_BG)))
        .highlight_style(Style::default().bg(LIST_HIGHLIGHT_BG));

    let mut list_state = ListState::default();
    list_state.select(if state.filtered_indices.is_empty() {
        None
    } else {
        Some(state.selected)
    });
    f.render_stateful_widget(list, chunks[1], &mut list_state);

    // Help bar — gold keys + muted descriptions, single line.
    let help = Line::from(vec![
        Span::styled(
            "Enter",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("=Select  ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            "Esc",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("=Quit  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("^R", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled("=Reset  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("^F", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled("=Favorite", Style::default().fg(MUTED_GRAY)),
    ]);
    let help_p = Paragraph::new(help).alignment(Alignment::Center);
    f.render_widget(help_p, chunks[2]);
}

/// Handle a single key event. Mutates state in place; returns the outcome
/// the caller should act on (advance, return home, start clone, or stay).
///
/// Persistence is the dispatcher's job for navigation keys (finding #3 +
/// #11) — arrow/Esc/Enter only mutate the in-memory `state.defaults`
/// snapshot, and the dispatcher writes to
/// `~/.agents-in-a-box/session-defaults.yaml` when the screen exits
/// (AdvanceTo / StartClone / BackToHome). Two exceptions:
///   1. `^R` (reset) — a deliberate user-issued clear; we persist
///      synchronously here so the next Esc doesn't immediately re-record
///      a sticky-cursor highlight that the user just told us to wipe.
///   2. `^F` (favorite toggle) — already writes `favorites.yaml`
///      synchronously inside `toggle_favorite`.
pub fn handle_key(state: &mut PickRepoState, key: KeyEvent) -> PickRepoOutcome {
    // When an auth check is in flight or failed, intercept keys before
    // normal picker handling. Checking → only Esc; NotAuthenticated →
    // Enter retries, s skips, Esc clears.
    if let Some(ref auth) = state.git_auth_status {
        match auth {
            GitAuthStatus::Checking => {
                if matches!(key.code, KeyCode::Esc) {
                    state.git_auth_status = None;
                    state.pending_clone_source = None;
                }
                return PickRepoOutcome::Stay;
            }
            GitAuthStatus::NotAuthenticated => {
                match key.code {
                    KeyCode::Enter => {
                        state.git_auth_status = Some(GitAuthStatus::Checking);
                        return PickRepoOutcome::Stay; // dispatcher sees Checking → re-runs check
                    }
                    KeyCode::Char('s' | 'S') => {
                        let source = state.pending_clone_source.take();
                        state.git_auth_status = None;
                        if let Some(src) = source {
                            return PickRepoOutcome::StartClone(src);
                        }
                        return PickRepoOutcome::Stay;
                    }
                    KeyCode::Esc => {
                        state.git_auth_status = None;
                        state.pending_clone_source = None;
                        return PickRepoOutcome::Stay;
                    }
                    _ => return PickRepoOutcome::Stay,
                }
            }
            GitAuthStatus::Authenticated => {
                // Auto-advance handled by dispatcher; shouldn't linger here
                state.git_auth_status = None;
            }
        }
    }

    let defaults_path = SessionDefaults::default_path();
    // Ctrl-modified keys take precedence over plain chars so `^R` / `^F`
    // never get swallowed by the filter-input branch below.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('r' | 'R') => {
                tracing::debug!("pick_repo: ^R reset");
                state.filter.clear();
                state.defaults.reset_last_repo();
                state.refilter();
                state.selected = 0;
                // Persist the cleared state immediately — the next Esc
                // would otherwise re-stamp `last_repo` with the row 0 id
                // (sticky-cursor UX) and silently undo the ^R.
                if let Err(err) = state.defaults.save_to(&defaults_path) {
                    tracing::warn!(error = %err, "pick_repo: ^R persist failed");
                }
                return PickRepoOutcome::Stay;
            }
            KeyCode::Char('f' | 'F') => {
                tracing::debug!("pick_repo: ^F toggle favorite on highlighted row");
                if let Some(row) = state.highlighted().cloned() {
                    return match toggle_favorite(state, &row) {
                        FavoriteToggle::Refused(reason) => PickRepoOutcome::Notice {
                            message: format!("★ Can't favorite: {reason}"),
                            is_error: true,
                        },
                        FavoriteToggle::Added(display) => {
                            let local_repos = collect_local_repo_paths(state);
                            state.rebuild_rows(&local_repos);
                            PickRepoOutcome::Notice {
                                message: format!("⭐ Added '{display}' to favorites"),
                                is_error: false,
                            }
                        }
                        FavoriteToggle::Removed(display) => {
                            let local_repos = collect_local_repo_paths(state);
                            state.rebuild_rows(&local_repos);
                            PickRepoOutcome::Notice {
                                message: format!("★ Removed '{display}' from favorites"),
                                is_error: false,
                            }
                        }
                    };
                }
                return PickRepoOutcome::Stay;
            }
            KeyCode::Char('v' | 'V') => {
                // Ctrl+V: ask the caller to read the OS clipboard and append
                // it (Cmd+V / bracketed paste isn't delivered to this field
                // in some terminals — tmux, mouse-capture).
                tracing::debug!("pick_repo: ^V clipboard paste");
                return PickRepoOutcome::PasteFromClipboard;
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Esc => {
            // Esc with text typed → clear filter first; on second press
            // (empty filter) → return to home. Sticky-cursor highlight is
            // stored in-memory only; dispatcher persists on exit.
            if !state.filter.is_empty() {
                if let Some(row) = state.highlighted() {
                    state.defaults.last_repo = Some(row.id.clone());
                }
                state.filter.clear();
                state.refilter();
                return PickRepoOutcome::Stay;
            }
            if let Some(row) = state.highlighted() {
                state.defaults.last_repo = Some(row.id.clone());
            }
            PickRepoOutcome::BackToHome
        }
        KeyCode::Up => {
            if !state.filtered_indices.is_empty() {
                state.selected = if state.selected == 0 {
                    state.filtered_indices.len() - 1
                } else {
                    state.selected - 1
                };
                if let Some(row) = state.highlighted() {
                    state.defaults.last_repo = Some(row.id.clone());
                }
            }
            PickRepoOutcome::Stay
        }
        KeyCode::Down => {
            if !state.filtered_indices.is_empty() {
                state.selected = (state.selected + 1) % state.filtered_indices.len();
                if let Some(row) = state.highlighted() {
                    state.defaults.last_repo = Some(row.id.clone());
                }
            }
            PickRepoOutcome::Stay
        }
        KeyCode::Enter => {
            // Prefer a row hit; fall back to smart-parse on the filter text.
            if let Some(row) = state.highlighted().cloned() {
                tracing::debug!("pick_repo: Enter on row {}", row.id);
                state.defaults.last_repo = Some(row.id.clone());
                return resolve_outcome(row.source);
            }
            // No matches — smart-parse the filter as raw input.
            if state.filter.is_empty() {
                return PickRepoOutcome::Stay;
            }
            let parsed = parse_with(&state.filter, &RealFs);
            tracing::debug!("pick_repo: smart-parse {:?} -> {parsed:?}", state.filter);
            state.defaults.last_repo = Some(state.filter.clone());
            resolve_outcome(parsed)
        }
        KeyCode::Backspace => {
            state.filter.pop();
            state.refilter();
            PickRepoOutcome::Stay
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            state.filter.push(c);
            state.refilter();
            PickRepoOutcome::Stay
        }
        _ => PickRepoOutcome::Stay,
    }
}

/// Dispatch a resolved `RepoSource` into a `PickRepoOutcome` per spec table:
/// - `LocalPath` / pre-cloned `GithubShorthand` (local hit) → `AdvanceTo`
/// - `HttpsUrl` / `SshUrl` / `GithubShorthand` (remote) → `StartClone`
/// - `SshSession` → `AdvanceTo` (no clone needed)
/// - `Filter` → `Stay` (just an unparseable string)
fn resolve_outcome(source: RepoSource) -> PickRepoOutcome {
    match source {
        RepoSource::LocalPath(_) | RepoSource::SshSession(_) => PickRepoOutcome::AdvanceTo(source),
        RepoSource::HttpsUrl(_) | RepoSource::SshUrl(_) | RepoSource::GithubShorthand { .. } => {
            PickRepoOutcome::StartClone(source)
        }
        RepoSource::Filter(_) => PickRepoOutcome::Stay,
    }
}

/// Toggle the favorite status of a row. Updates both the in-memory store
/// (mutating `state.favorites`) AND the on-disk YAML so the change survives
/// across TUI restarts.
///
/// A favorite ALWAYS records a remote indicator. Remote rows store directly;
/// a `LocalPath` row is resolved to its `origin` remote via
/// [`favorite_from_local_repo`] (refused if it has none). `SshSession`
/// (interactive, not a repo) and `Filter` (unparseable text) rows are refused
/// outright — never persisted.
fn toggle_favorite(state: &mut PickRepoState, row: &PickRepoRow) -> FavoriteToggle {
    if state.favorites.has_alias(&row.id) {
        state.favorites.remove(&row.id);
        persist_favorites(state);
        return FavoriteToggle::Removed(row.label.clone());
    }

    let fav = match &row.source {
        RepoSource::HttpsUrl(u) => Favorite::new(row.id.clone(), u.clone(), SourceType::HttpsUrl),
        RepoSource::SshUrl(u) => Favorite::new(row.id.clone(), u.clone(), SourceType::SshUrl),
        RepoSource::GithubShorthand { owner, repo } => Favorite::new(
            row.id.clone(),
            format!("{owner}/{repo}"),
            SourceType::GithubShorthand,
        ),
        RepoSource::LocalPath(p) => {
            match crate::config::favorite_from_local_repo(row.id.clone(), p) {
                Ok(fav) => fav,
                Err(e) => {
                    tracing::warn!(
                        alias = %row.id,
                        path = %p.display(),
                        error = %e,
                        "pick_repo: refusing to favorite local row — no remote indicator",
                    );
                    return FavoriteToggle::Refused(e.to_string());
                }
            }
        }
        RepoSource::SshSession(s) | RepoSource::Filter(s) => {
            tracing::warn!(
                alias = %row.id,
                text = %s,
                "pick_repo: refusing to favorite — not a remote repository indicator",
            );
            return FavoriteToggle::Refused("not a remote repository".to_string());
        }
    };

    let display = fav.source.clone();
    state.favorites.set(fav);
    persist_favorites(state);
    FavoriteToggle::Added(display)
}

/// Persist the favorites store to disk, logging (but not failing) on error.
fn persist_favorites(state: &PickRepoState) {
    if let Err(err) = state.favorites.save() {
        tracing::warn!(error = %err, "pick_repo: failed to persist favorites");
    }
}

/// Pull current local-only paths out of state so the row list can be
/// rebuilt without losing the local-scan input.
fn collect_local_repo_paths(state: &PickRepoState) -> Vec<PathBuf> {
    state
        .rows
        .iter()
        .filter(|r| r.kind == RowKind::Local)
        .filter_map(|r| match &r.source {
            RepoSource::LocalPath(p) => Some(p.clone()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::favorites_store::SourceType;

    fn mk_state_with_rows(rows: Vec<PickRepoRow>) -> PickRepoState {
        let filtered_indices: Vec<usize> = (0..rows.len()).collect();
        PickRepoState {
            filter: String::new(),
            rows,
            filtered_indices,
            selected: 0,
            clone_progress: None,
            defaults: SessionDefaults::default(),
            favorites: FavoritesStore::default(),
        }
    }

    #[test]
    fn refilter_preserves_highlight_when_row_still_matches() {
        let mut s = mk_state_with_rows(vec![
            PickRepoRow {
                id: "ainb-tui".into(),
                label: "ainb-tui".into(),
                source: RepoSource::Filter("ainb-tui".into()),
                kind: RowKind::Favorite,
            },
            PickRepoRow {
                id: "agents".into(),
                label: "agents".into(),
                source: RepoSource::Filter("agents".into()),
                kind: RowKind::Favorite,
            },
        ]);
        s.selected = 1; // highlight agents
        s.filter = "agent".into();
        s.refilter();
        // After filter, only one row visible — that one is highlighted.
        assert_eq!(s.filtered_indices.len(), 1);
        assert_eq!(s.highlighted().unwrap().id, "agents");
    }

    #[test]
    fn resolve_outcome_dispatches_correctly() {
        assert_eq!(
            resolve_outcome(RepoSource::LocalPath(PathBuf::from("/x"))),
            PickRepoOutcome::AdvanceTo(RepoSource::LocalPath(PathBuf::from("/x")))
        );
        assert_eq!(
            resolve_outcome(RepoSource::Filter("x".into())),
            PickRepoOutcome::Stay
        );
        assert!(matches!(
            resolve_outcome(RepoSource::GithubShorthand {
                owner: "foo".into(),
                repo: "bar".into()
            }),
            PickRepoOutcome::StartClone(_)
        ));
        assert!(matches!(
            resolve_outcome(RepoSource::SshSession("ssh://x".into())),
            PickRepoOutcome::AdvanceTo(_)
        ));
    }

    #[test]
    fn build_rows_orders_favorites_then_recents_then_locals() {
        use chrono::Utc;
        let mut favorites = FavoritesStore::default();
        favorites
            .add(Favorite::new(
                "fav-a".into(),
                "owner/fav-a".into(),
                SourceType::GithubShorthand,
            ))
            .unwrap();
        let mut defaults = SessionDefaults::default();
        defaults.per_repo.insert(
            "recent-1".into(),
            crate::config::session_defaults::PerRepoDefaults {
                last_used_at: Utc::now(),
                ..Default::default()
            },
        );
        let locals = vec![PathBuf::from("/tmp/local-1")];

        let rows = build_rows(&favorites, &defaults, &locals);
        assert_eq!(rows[0].kind, RowKind::Favorite);
        assert_eq!(rows[0].id, "fav-a");
        assert_eq!(rows[1].kind, RowKind::Recent);
        assert_eq!(rows[1].id, "recent-1");
        assert_eq!(rows[2].kind, RowKind::Local);
    }

    #[test]
    fn pick_default_selection_finds_last_repo() {
        let rows = vec![
            PickRepoRow {
                id: "a".into(),
                label: "a".into(),
                source: RepoSource::Filter("a".into()),
                kind: RowKind::Favorite,
            },
            PickRepoRow {
                id: "b".into(),
                label: "b".into(),
                source: RepoSource::Filter("b".into()),
                kind: RowKind::Favorite,
            },
        ];
        let filtered = vec![0, 1];
        let defaults = SessionDefaults {
            last_repo: Some("b".into()),
            ..Default::default()
        };
        assert_eq!(pick_default_selection(&rows, &filtered, &defaults), 1);
    }

    #[test]
    fn pick_default_selection_falls_back_to_zero() {
        let rows = vec![PickRepoRow {
            id: "a".into(),
            label: "a".into(),
            source: RepoSource::Filter("a".into()),
            kind: RowKind::Favorite,
        }];
        let filtered = vec![0];
        let defaults = SessionDefaults {
            last_repo: Some("nonexistent".into()),
            ..Default::default()
        };
        assert_eq!(pick_default_selection(&rows, &filtered, &defaults), 0);
    }

    #[test]
    fn row_detail_renders_locator_per_source() {
        // A path outside home is shown unchanged (home collapsing is covered
        // separately so this stays independent of the test environment).
        assert_eq!(
            row_detail(&RepoSource::LocalPath(PathBuf::from("/opt/repos/Rosetta"))).as_deref(),
            Some("/opt/repos/Rosetta")
        );
        assert_eq!(
            row_detail(&RepoSource::HttpsUrl("https://github.com/o/r.git".into())).as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert_eq!(
            row_detail(&RepoSource::SshUrl("git@github.com:o/r.git".into())).as_deref(),
            Some("git@github.com:o/r.git")
        );
        assert_eq!(
            row_detail(&RepoSource::SshSession("ssh://deploy@prod-1".into())).as_deref(),
            Some("ssh://deploy@prod-1")
        );
        let shorthand = RepoSource::GithubShorthand {
            owner: "o".into(),
            repo: "r".into(),
        };
        assert_eq!(row_detail(&shorthand).as_deref(), Some("o/r"));
        // Unparseable filter text carries no real source — nothing to show.
        assert_eq!(
            row_detail(&RepoSource::Filter("rose".into())).as_deref(),
            None
        );
    }

    #[test]
    fn abbreviate_home_collapses_home_prefix() {
        let Some(home) = dirs::home_dir() else {
            return; // No home dir in this environment — nothing to assert.
        };
        let nested = home.join("Code-Zero").join("Rosetta");
        let expected = Path::new("~").join("Code-Zero").join("Rosetta");
        assert_eq!(abbreviate_home(&nested), expected.display().to_string());
        assert_eq!(abbreviate_home(&home), "~");
        let outside = Path::new("/opt/elsewhere/Rosetta");
        assert_eq!(abbreviate_home(outside), outside.display().to_string());
    }

    fn one_row() -> Vec<PickRepoRow> {
        vec![PickRepoRow {
            id: "shotclubhouse".into(),
            label: "shotclubhouse".into(),
            source: RepoSource::Filter("shotclubhouse".into()),
            kind: RowKind::Favorite,
        }]
    }

    #[test]
    fn ctrl_v_requests_clipboard_paste() {
        let mut s = mk_state_with_rows(one_row());
        let ctrl_v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL);
        assert_eq!(
            handle_key(&mut s, ctrl_v),
            PickRepoOutcome::PasteFromClipboard
        );
        // The keystroke must NOT also type a literal 'v' into the filter.
        assert_eq!(s.filter, "");
    }

    #[test]
    fn plain_v_still_types_into_filter() {
        let mut s = mk_state_with_rows(one_row());
        let v = KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE);
        assert_eq!(handle_key(&mut s, v), PickRepoOutcome::Stay);
        assert_eq!(s.filter, "v");
    }

    #[test]
    fn append_filter_strips_control_chars_and_refilters() {
        let mut s = mk_state_with_rows(one_row());
        // Simulate a pasted "owner/repo" with a trailing newline.
        s.append_filter("shotclub\n");
        assert_eq!(s.filter, "shotclub");
        // The single row still matches the prefix, so it stays visible.
        assert_eq!(s.filtered_indices.len(), 1);
        // A non-matching paste empties the filtered list.
        s.append_filter("zzz");
        assert_eq!(s.filter, "shotclubzzz");
        assert_eq!(s.filtered_indices.len(), 0);
    }
}
