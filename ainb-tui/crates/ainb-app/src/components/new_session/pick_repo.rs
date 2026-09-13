// ABOUTME: Renderer-agnostic half of the `new_session::pick_repo` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::new_session::pick_repo`, which re-exports this module.

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
