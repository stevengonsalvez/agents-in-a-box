// ABOUTME: Renderer-agnostic half of the `new_session::pick_repo` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::new_session::pick_repo`, which re-exports this module.

use crate::app::keymap::{Chord, Key, Mods};
use crate::config::favorites_store::{Favorite, FavoritesStore, SourceType};
use crate::config::session_defaults::SessionDefaults;
use crate::git::repo_source::{RealFs, RepoSource, parse_with};
use std::path::PathBuf;

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
    pub const fn marker(self) -> &'static str {
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
    /// Exact, verbatim output of the failed `gh auth status` probe (stderr +
    /// stdout). Shown in the `NotAuthenticated` modal so the user sees the real
    /// reason instead of a generic "auth failed". `None` until a probe fails.
    pub git_auth_error: Option<String>,
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
            git_auth_error: None,
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
    pub fn refilter(&mut self) {
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
    pub fn rebuild_rows(&mut self, local_repos: &[PathBuf]) {
        self.rows = build_rows(&self.favorites, &self.defaults, local_repos);
        self.refilter();
    }
}

/// Pure helper: build the ordered row list from on-disk sources. Favorites
/// pinned first (in their stored order), then recents (most-recent first),
/// then any local-only repos not already represented above.
pub fn build_rows(
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
pub fn pick_default_selection(
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
/// Result of toggling a favorite — drives the `^F` notification.
enum FavoriteToggle {
    /// Newly favorited; carries the stored remote source for display.
    Added(String),
    /// Un-favorited; carries the row label for display.
    Removed(String),
    /// Refused because the row has no remote repository indicator.
    Refused(String),
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
pub fn handle_key(state: &mut PickRepoState, key: &Chord) -> PickRepoOutcome {
    // When an auth check is in flight or failed, intercept keys before
    // normal picker handling. Checking → only Esc; NotAuthenticated →
    // Enter retries, s skips, Esc clears.
    if let Some(ref auth) = state.git_auth_status {
        match auth {
            GitAuthStatus::Checking => {
                if matches!(key.code(), Key::Esc) {
                    state.git_auth_status = None;
                    state.pending_clone_source = None;
                    state.git_auth_error = None;
                }
                return PickRepoOutcome::Stay;
            }
            GitAuthStatus::NotAuthenticated => {
                match key.code() {
                    Key::Enter => {
                        // Re-probe: drop the stale error so the modal shows the
                        // spinner, not last attempt's failure, while it re-runs.
                        state.git_auth_error = None;
                        state.git_auth_status = Some(GitAuthStatus::Checking);
                        return PickRepoOutcome::Stay; // dispatcher sees Checking → re-runs check
                    }
                    Key::Char('s' | 'S') => {
                        let source = state.pending_clone_source.take();
                        state.git_auth_status = None;
                        state.git_auth_error = None;
                        if let Some(src) = source {
                            return PickRepoOutcome::StartClone(src);
                        }
                        return PickRepoOutcome::Stay;
                    }
                    Key::Esc => {
                        state.git_auth_status = None;
                        state.pending_clone_source = None;
                        state.git_auth_error = None;
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
    if key.modifiers().contains(Mods::CTRL) {
        match key.code() {
            Key::Char('r' | 'R') => {
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
            Key::Char('f' | 'F') => {
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
            Key::Char('v' | 'V') => {
                // Ctrl+V: ask the caller to read the OS clipboard and append
                // it (Cmd+V / bracketed paste isn't delivered to this field
                // in some terminals — tmux, mouse-capture).
                tracing::debug!("pick_repo: ^V clipboard paste");
                return PickRepoOutcome::PasteFromClipboard;
            }
            _ => {}
        }
    }

    match key.code() {
        Key::Esc => {
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
        Key::Up => {
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
        Key::Down => {
            if !state.filtered_indices.is_empty() {
                state.selected = (state.selected + 1) % state.filtered_indices.len();
                if let Some(row) = state.highlighted() {
                    state.defaults.last_repo = Some(row.id.clone());
                }
            }
            PickRepoOutcome::Stay
        }
        Key::Enter => {
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
        Key::Backspace => {
            state.filter.pop();
            state.refilter();
            PickRepoOutcome::Stay
        }
        Key::Char(c) if !key.modifiers().contains(Mods::CTRL) => {
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
pub fn resolve_outcome(source: RepoSource) -> PickRepoOutcome {
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
