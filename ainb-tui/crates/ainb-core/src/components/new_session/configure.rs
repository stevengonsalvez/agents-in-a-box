// ABOUTME: Screen 2 of the new-session redesign — the consolidated Configure
// screen. Wizard-style row navigation. Key model (2026-05 refresh):
//   * Tab / Shift+Tab — cycle row focus (canonical).
//   * ↑ / ↓ — alias for Shift+Tab / Tab respectively (move row focus).
//   * ← / → — cycle the VALUE in the focused row.
//   * Enter — launch from any non-Prompt row. On Branch row, opens inline
//     edit. On Prompt row, inserts newline (Ctrl+Enter launches from there).
//
// The earlier prototype had ↑ / ↓ alias ←/→ (both cycled value). That
// conflated row-nav with value-cycling — Stevie flagged it as a UX bug.
// Split: ↑/↓ is now strictly row navigation; ←/→ stays as value cycling.
//
// **Preset ring with `Custom` sentinel.** Real presets are immutable from the
// Configure screen — Mode / Yolo / Agent / Model are display-only when a
// named preset is selected. Switching to `Custom` (the last entry in the
// preset ring) unlocks the fine-grained editor rows so the user can build
// an ad-hoc spec without having to first save a new preset to disk. `Custom`
// is NOT serialised on launch; the user must hit `^S` to save it under a
// chosen name.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use std::collections::HashMap;

use crate::app::state::TextEditor;
use crate::config::presets::{PresetManager, RepositoryPreset, SessionMode};
use crate::config::session_defaults::SessionDefaults;
use crate::git::branch_list::BranchEntry;
use crate::git::branch_namer::derive_branch_name;
use crate::git::repo_source::RepoSource;

// Palette — matches `pick_repo.rs` so the two screens feel like one app.
const CORNFLOWER_BLUE: Color = Color::Rgb(100, 149, 237);
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const DARK_BG: Color = Color::Rgb(25, 25, 35);
const ALERT_RED: Color = Color::Rgb(230, 90, 90);

/// Sentinel name used by the preset-ring `Custom` slot. Surfaces in the
/// `Preset:` row when the user has cycled past the last real preset.
pub const CUSTOM_PRESET_LABEL: &str = "Custom";

/// Which preset the user is currently targeting.
///
/// `Named(idx)` indexes into `available_presets`. `Custom` unlocks the
/// per-row editor rows (Agent / Model / Mode / Yolo). The Custom slot sits
/// at the end of the cycling ring, after the last named preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetSelection {
    Named(usize),
    Custom,
}

/// Overrides applied on top of the seed preset when `PresetSelection::Custom`
/// is active. Lazy-populated the first time the user cycles into Custom from
/// a named preset — the seed values come from whatever preset was selected
/// just before the switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomOverrides {
    pub agent_provider: String,
    pub agent_model: String,
    pub mode: SessionMode,
    pub skip_all: bool,
}

impl CustomOverrides {
    fn seed_from(preset: &RepositoryPreset) -> Self {
        Self {
            agent_provider: preset.agent_provider.clone(),
            agent_model: preset.agent_model.clone(),
            mode: preset.mode,
            skip_all: preset.permissions.skip_all,
        }
    }
}

/// Result of the remote-repo pre-flight (`git ls-remote` at Configure open).
///
/// Catches "repo doesn't exist" and "repo is empty" HERE, on the form, instead
/// of after Launch as a clone/worktree failure toast (Stevie 2026-07-04:
/// empty mysocialmedia died at `prepare_remote_worktree` with a cryptic
/// origin/HEAD error; a typo'd repo died with "Clone failed").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoCheck {
    /// Local path / SSH session — nothing to validate.
    NotApplicable,
    /// ls-remote in flight. Launch is held until it lands (sub-second).
    Checking,
    /// Remote exists and has at least one branch.
    Ok,
    /// Remote is unreachable, missing, or has zero branches. Blocks Launch;
    /// the message renders on the form.
    Failed(String),
}

impl RepoCheck {
    /// Fold a `list_remote_branches` result into a check verdict. Pure so the
    /// empty-repo rule is unit-testable without a network.
    #[must_use]
    pub fn from_branches(result: Result<usize, String>) -> Self {
        match result {
            Ok(0) => Self::Failed(
                "repository is empty (no branches) — push an initial commit first".to_string(),
            ),
            Ok(_) => Self::Ok,
            Err(msg) => Self::Failed(msg),
        }
    }

    /// True when Launch must be refused (check failed or still in flight).
    #[must_use]
    pub const fn blocks_launch(&self) -> bool {
        matches!(self, Self::Checking | Self::Failed(_))
    }
}

/// Which segment of the Branch row (`source → worktree`) is targeted when
/// the row is focused. ←/→ toggles; Enter acts on the targeted segment —
/// Source opens the base-branch picker popup, Worktree opens the inline
/// name edit (2026-06 base-picker feature).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchSegment {
    Source,
    Worktree,
}

/// How a picked base ref is applied at launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseMode {
    /// Cut a fresh `agents/xxx` branch off the picked ref (default).
    BaseOff,
    /// Check out the picked branch itself in the worktree (local tracking
    /// branch for remote picks). No generated branch name.
    Checkout,
}

/// Why the chosen worktree branch name would make launch fail — surfaced
/// inline on the Branch row so the user fixes it BEFORE pressing Launch
/// (Stevie 2026-06-07: feat/ota off main died only at launch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchProblem {
    /// Already checked out in a live worktree — `git worktree add` rejects it.
    InUse,
    /// Already exists as a branch (local or remote). Harmless in Checkout
    /// mode (that's the point), but in base-off mode we'd try to create a
    /// NEW branch with that name and fail (`worktree add -b` errors; the
    /// remote cache pre-check rejects "already exists in cache").
    Exists,
}

/// The user's pick from the base-branch popup. Threaded through `LaunchSpec`
/// into `create_session_from_configure`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseSelection {
    /// Display ref — `origin/feature-x` for remote entries, `feature-x` for
    /// local ones. Doubles as the git start-point (revparse-able).
    pub display: String,
    /// Local short name (`feature-x`) — the branch a Checkout selection
    /// creates / checks out.
    pub short_name: String,
    /// True when the pick came from the remote section.
    pub is_remote: bool,
    pub mode: BaseMode,
}

/// One row in the base-branch popup: the git entry plus the live-worktree
/// collision flag (drives the `⚠ in use` marker and blocks Checkout picks).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerBranchEntry {
    pub entry: BranchEntry,
    pub in_use: bool,
}

/// State for the base-branch popup. `None` on `ConfigureState.branch_picker`
/// when closed. Entries are seeded from cached refs at open (instant) and
/// replaced in place when the background fetch lands (`loading` spinner).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchPickerState {
    pub filter: String,
    pub entries: Vec<PickerBranchEntry>,
    /// Index into `filtered_indices()` — NOT into `entries`.
    pub selected: usize,
    /// True while the background fetch/ls-remote refresh is in flight.
    pub loading: bool,
    /// Inline error line (e.g. Checkout pick on an in-use branch).
    pub error: Option<String>,
    /// Action applied on Enter; Tab toggles.
    pub mode: BaseMode,
}

impl BranchPickerState {
    #[must_use]
    pub fn new(entries: Vec<PickerBranchEntry>, loading: bool) -> Self {
        Self {
            filter: String::new(),
            entries,
            selected: 0,
            loading,
            error: None,
            mode: BaseMode::BaseOff,
        }
    }

    /// Indices into `entries` that match the filter (case-insensitive
    /// substring on the display ref). Empty filter matches everything.
    #[must_use]
    pub fn filtered_indices(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| needle.is_empty() || e.entry.display.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    /// The entry currently under the selection cursor, if any.
    #[must_use]
    pub fn selected_entry(&self) -> Option<&PickerBranchEntry> {
        let filtered = self.filtered_indices();
        filtered.get(self.selected).map(|&i| &self.entries[i])
    }

    /// Re-clamp `selected` after the entry set or filter changed (also used
    /// by the app layer when the background refresh replaces `entries`).
    pub fn clamp_selection(&mut self) {
        let len = self.filtered_indices().len();
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
        }
    }
}

/// Identity of a logical row in the Configure form. The set of *visible*
/// rows depends on the active variant (SSH vs. local) and on whether
/// `PresetSelection::Custom` is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigureRow {
    Preset,
    Agent,
    Model,
    Mode,
    Yolo,
    HeadroomProxy,
    Rtk,
    Host,
    User,
    Port,
    Key,
    Branch,
    Prompt,
    /// Explicit submit row. Renders as a `[ Launch ]` button at the bottom of
    /// the form. Tab past Prompt lands here; Enter fires the launch. Avoids
    /// the Enter-on-Branch = edit ambiguity (Stevie 2026-05-27). Power users
    /// can still Ctrl+Enter from any row.
    Launch,
}

/// State for the Configure screen. Constructed once when the user advances
/// from `PickRepo`. Owned by `NewSessionState.configure_state`.
#[derive(Debug)]
pub struct ConfigureState {
    /// What the user selected on screen 1 — drives the layout variant.
    pub repo_source: RepoSource,
    /// Display label for the repo (e.g. "ainb-tui" or `host` for SSH).
    pub repo_label: String,
    /// Preset names ordered for ring cycling. Does NOT include the `Custom`
    /// sentinel — that's a separate variant on `PresetSelection`.
    pub available_presets: Vec<String>,
    /// Currently focused row in the form.
    pub focused_row: ConfigureRow,
    /// Active selection in the preset ring (Named or Custom).
    pub preset_selection: PresetSelection,
    /// The preset that was auto-loaded on entry. `• modified` badge fires
    /// when the effective config diverges from this baseline.
    pub current_preset: RepositoryPreset,
    /// Overrides layered on top of the seed preset when `Custom` is active.
    /// `None` until the user first cycles into Custom (at which point we
    /// seed from the previously-selected named preset).
    pub custom_overrides: Option<CustomOverrides>,
    /// HEAD branch of the source repo (or "main" placeholder).
    pub branch_source: String,
    /// Auto-derived worktree branch name; updated live as `prompt` changes.
    pub branch_worktree: String,
    /// Manual override for `branch_worktree` (Phase 7 — `E` affordance).
    pub branch_override: Option<String>,
    /// Inline branch edit buffer — `Some(_)` when the user pressed Enter on
    /// the Branch row. Esc cancels; Enter commits to `branch_override`.
    pub branch_edit: Option<String>,
    /// Multi-line prompt editor (Boss mode only).
    pub prompt: TextEditor,
    /// When `Some`, the save-preset modal is open and the contained string is
    /// the typed name buffer.
    pub save_preset_modal: Option<String>,
    /// Cached preset map — populated once in `from_pick_repo` so Tab cycling
    /// doesn't re-scan `~/.agents-in-a-box/presets/` on every keystroke
    /// (finding #4). Invalidated + reloaded only when `save_preset` writes a
    /// new file.
    pub presets_cache: HashMap<String, RepositoryPreset>,
    /// Branch prefix from `AppConfig.workspace_defaults.branch_prefix`,
    /// threaded through by the dispatcher (finding #5).
    pub branch_prefix: String,
    /// Snapshot of existing worktree branch names — passed to
    /// `derive_branch_name` so collision-disambiguation actually fires
    /// (finding #16). These are branches *in use by a worktree*.
    pub existing_branches: Vec<String>,
    /// All branch short names that exist in the repo (local heads +
    /// remote-tracking), regardless of whether a worktree holds them. Seeded
    /// for local repos at construction and refreshed from the base-branch
    /// picker (which lists/fetches them). Drives the base-off "⚠ exists"
    /// guard — creating a NEW branch over an existing name fails
    /// (Stevie 2026-06-07: feat/ota off main).
    pub repo_branch_names: Vec<String>,
    /// Which segment of the Branch row Enter acts on (←/→ toggles).
    pub branch_segment: BranchSegment,
    /// The user's base-branch pick, when they used the popup. `None` keeps
    /// the legacy behavior (HEAD for local repos, origin/HEAD for remote).
    pub base_selection: Option<BaseSelection>,
    /// Base-branch popup state — `Some` while the popup is open.
    pub branch_picker: Option<BranchPickerState>,
    /// Route this session's CLI through the local Headroom compression proxy.
    /// Only active for Claude and Codex agents.
    pub headroom_enabled: bool,
    /// Whether the `headroom` binary was found on PATH when this screen opened.
    /// Detected once at construction (cheap PATH lookup) — gates the toggle so
    /// we never offer routing through a proxy that can't run.
    pub headroom_available: bool,
    /// Wire RTK as a project-local Claude Code PreToolUse hook in the session's
    /// worktree. Claude only (Codex path is AGENTS.md prompt-injection, out of
    /// scope for this phase).
    pub rtk_enabled: bool,
    /// Whether the `rtk` binary was found on PATH when this screen opened.
    pub rtk_available: bool,
    /// Remote-repo pre-flight verdict. `Checking` for clonable remotes until
    /// the background ls-remote lands; `Failed` blocks Launch with an inline
    /// message; `NotApplicable` for local paths / SSH sessions.
    pub repo_check: RepoCheck,
}

impl ConfigureState {
    /// Construct a Configure state for the given `repo_source` + `repo_label`,
    /// auto-loading the preset per spec rule (repo override -> session-defaults
    /// last_preset -> first installed default).
    pub fn from_pick_repo(
        repo_source: RepoSource,
        repo_label: String,
        defaults: &SessionDefaults,
        branch_source: Option<String>,
        branch_prefix: &str,
        existing_branches: Vec<String>,
        repo_branch_names: Vec<String>,
    ) -> Self {
        // Build the presets cache ONCE here (finding #4). Tab/Shift-Tab
        // cycling consults the cache, not the disk.
        let presets_cache: HashMap<String, RepositoryPreset> = PresetManager::new()
            .ok()
            .map(|m| m.all().iter().map(|p| (p.name.clone(), (*p).clone())).collect())
            .unwrap_or_default();

        // Step 1: collect available preset names. Sorted for stable cycling.
        let mut available_presets: Vec<String> = presets_cache.keys().cloned().collect();
        available_presets.sort();
        if available_presets.is_empty() {
            // Defensive: always have at least one entry so cycling never panics.
            available_presets.push("default".to_string());
        }

        // Step 2: pick the autoload preset following spec precedence.
        let mut autoload: Option<RepositoryPreset> = None;
        if let RepoSource::LocalPath(p) = &repo_source {
            if let Ok(Some(pr)) = PresetManager::load_repo_preset(p) {
                autoload = Some(pr);
            }
        }
        if autoload.is_none() {
            if let Some(per) = defaults.per_repo.get(&repo_label) {
                if let Some(name) = per.last_preset.as_deref() {
                    if let Some(pr) = presets_cache.get(name).cloned() {
                        autoload = Some(pr);
                    }
                }
            }
        }
        let current_preset = autoload
            .or_else(|| available_presets.iter().find_map(|n| presets_cache.get(n).cloned()))
            .unwrap_or_default();

        let selected_idx =
            available_presets.iter().position(|n| n == &current_preset.name).unwrap_or(0);

        // Pre-populate prompt from per-repo persisted state when present.
        let prompt = defaults
            .per_repo
            .get(&repo_label)
            .and_then(|per| per.last_prompt.as_deref())
            .map(TextEditor::from_string)
            .unwrap_or_else(TextEditor::new);

        // Branch line. Stable for the lifetime of the Configure session —
        // we generate the random 8-hex suffix once at open and don't re-roll
        // on prompt edits (was jittery before; Stevie 2026-05-27).
        let branch_source = branch_source.unwrap_or_else(|| "main".to_string());
        let branch_worktree = derive_branch_name(branch_prefix, &existing_branches);

        // Initial focus: the Preset row — matches a fresh-form expectation.
        let focused_row = ConfigureRow::Preset;

        // Clonable remotes start in `Checking`; the app layer kicks the
        // background ls-remote and flips this to Ok / Failed.
        let repo_check = match &repo_source {
            RepoSource::HttpsUrl(_)
            | RepoSource::SshUrl(_)
            | RepoSource::GithubShorthand { .. } => RepoCheck::Checking,
            _ => RepoCheck::NotApplicable,
        };

        Self {
            repo_source,
            repo_label,
            available_presets,
            focused_row,
            preset_selection: PresetSelection::Named(selected_idx),
            current_preset,
            custom_overrides: None,
            branch_source,
            branch_worktree,
            branch_override: None,
            branch_edit: None,
            prompt,
            save_preset_modal: None,
            presets_cache,
            branch_prefix: branch_prefix.to_string(),
            existing_branches,
            repo_branch_names,
            branch_segment: BranchSegment::Source,
            base_selection: None,
            branch_picker: None,
            headroom_enabled: false,
            headroom_available: crate::headroom::is_installed(),
            rtk_enabled: false,
            rtk_available: crate::rtk::is_installed(),
            repo_check,
        }
    }

    /// The preset that the user is *currently* targeting — Custom overrides
    /// the seed; Named returns the cached preset (defaults to current_preset
    /// on a cache miss).
    #[must_use]
    pub fn effective_preset(&self) -> RepositoryPreset {
        match self.preset_selection {
            PresetSelection::Named(idx) => {
                let name = self
                    .available_presets
                    .get(idx)
                    .cloned()
                    .unwrap_or_else(|| self.current_preset.name.clone());
                self.presets_cache
                    .get(&name)
                    .cloned()
                    .unwrap_or_else(|| self.current_preset.clone())
            }
            PresetSelection::Custom => {
                // Custom needs a seed; if we never populated overrides we
                // fall back to the current (last-named) preset.
                let mut p = self.seed_preset_for_custom();
                if let Some(o) = self.custom_overrides.as_ref() {
                    p.agent_provider = o.agent_provider.clone();
                    p.agent_model = o.agent_model.clone();
                    p.mode = o.mode;
                    p.permissions.skip_all = o.skip_all;
                }
                p.name = CUSTOM_PRESET_LABEL.to_string();
                p
            }
        }
    }

    /// Pick the seed preset for the `Custom` slot. Whatever preset is closest
    /// to the user's last "real" selection wins: if they just cycled in from
    /// Named(n), that's the seed; otherwise fall back to the autoloaded one.
    fn seed_preset_for_custom(&self) -> RepositoryPreset {
        self.current_preset.clone()
    }

    /// True when the effective config diverges from the autoloaded baseline.
    /// Drives the `• modified` badge.
    ///
    /// Two paths:
    ///   1. `Custom` is selected and either has overrides OR doesn't byte-match
    ///      the autoloaded preset.
    ///   2. `Named(idx)` is selected and the named preset != current_preset.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        match self.preset_selection {
            PresetSelection::Custom => {
                // Custom always counts as modified unless its effective spec
                // byte-matches a known preset baseline. For the wizard UX we
                // treat Custom as "always modified" — the user explicitly
                // opted into the editor, so the badge is informative.
                let effective = self.effective_preset();
                effective.agent_provider != self.current_preset.agent_provider
                    || effective.agent_model != self.current_preset.agent_model
                    || effective.mode != self.current_preset.mode
                    || effective.permissions.skip_all != self.current_preset.permissions.skip_all
                    || self.custom_overrides.is_some()
            }
            PresetSelection::Named(idx) => self
                .available_presets
                .get(idx)
                .map(|n| n != &self.current_preset.name)
                .unwrap_or(false),
        }
    }

    /// True when the user picked "checkout the branch itself" in the base
    /// popup — the worktree lands ON the picked branch, no generated name.
    #[must_use]
    pub fn is_checkout(&self) -> bool {
        self.base_selection.as_ref().is_some_and(|b| b.mode == BaseMode::Checkout)
    }

    /// The branch name that will actually be used for the worktree. Priority:
    ///   0. checkout-direct pick — the picked branch IS the session branch;
    ///   1. in-progress inline edit buffer (so the collision warning updates
    ///      live as the user types — Stevie 2026-05-27);
    ///   2. committed manual override;
    ///   3. auto-derived random name.
    #[must_use]
    pub fn effective_branch(&self) -> String {
        if let Some(base) = self.base_selection.as_ref() {
            if base.mode == BaseMode::Checkout {
                return base.short_name.clone();
            }
        }
        if let Some(ref buf) = self.branch_edit {
            return buf.clone();
        }
        self.branch_override.clone().unwrap_or_else(|| self.branch_worktree.clone())
    }

    /// Why the effective worktree branch name would make launch fail, if at
    /// all. Drives the inline Branch-row warning and the pre-launch block.
    /// Only reachable via a manual override / picked name — the auto default
    /// is a fresh random 8-hex that avoids every existing branch.
    ///
    /// `InUse` (checked out by a live worktree) applies in BOTH modes — git
    /// rejects a second worktree on the same branch. `Exists` (the name is a
    /// branch but not in a worktree) applies ONLY in base-off mode, where we
    /// create a NEW branch off the base; in Checkout mode an existing branch
    /// is exactly what's wanted (Stevie 2026-06-07: feat/ota off main).
    #[must_use]
    pub fn branch_problem(&self) -> Option<BranchProblem> {
        let b = self.effective_branch();
        if self.existing_branches.iter().any(|x| x == &b) {
            return Some(BranchProblem::InUse);
        }
        if !self.is_checkout() && self.repo_branch_names.iter().any(|x| x == &b) {
            return Some(BranchProblem::Exists);
        }
        None
    }

    /// True when the chosen branch name would fail at `git worktree add` —
    /// the pre-launch chokepoint reads this to block + refocus the Branch row.
    #[must_use]
    pub fn branch_collision(&self) -> bool {
        self.branch_problem().is_some()
    }

    /// Recompute `branch_worktree`. After the 2026-05-27 refactor branch
    /// names are random (8-hex), independent of prompt text, so this is now
    /// only called on explicit user reset (re-roll). Kept as a method so the
    /// `^R`-style flows have a hook, but NOT wired to prompt edits.
    #[allow(dead_code)]
    fn refresh_branch_name(&mut self) {
        if self.branch_override.is_some() {
            return;
        }
        self.branch_worktree = derive_branch_name(&self.branch_prefix, &self.existing_branches);
    }

    /// The list of rows visible for the current variant + preset selection.
    /// Ordering matches the render layout and Tab cycle order.
    fn visible_rows(&self) -> Vec<ConfigureRow> {
        if matches!(self.repo_source, RepoSource::SshSession(_)) {
            return vec![
                ConfigureRow::Preset,
                ConfigureRow::Host,
                ConfigureRow::User,
                ConfigureRow::Port,
                ConfigureRow::Key,
                ConfigureRow::Launch,
            ];
        }
        let preset = self.effective_preset();
        let is_custom = self.preset_selection == PresetSelection::Custom;
        let mut rows = vec![ConfigureRow::Preset];

        if is_custom {
            rows.push(ConfigureRow::Agent);
            // Model row is shown for both Claude and Codex (2026-05 refresh).
            // Shell / SSH agents have no model concept — keep the row hidden.
            if preset.agent_provider == "claude" || preset.agent_provider == "codex" {
                rows.push(ConfigureRow::Model);
            }
            // Shell agent: no Mode/Yolo/Prompt.
            if preset.agent_provider != "shell" {
                rows.push(ConfigureRow::Mode);
                rows.push(ConfigureRow::Yolo);
                if preset.agent_provider == "claude" || preset.agent_provider == "codex" {
                    rows.push(ConfigureRow::HeadroomProxy);
                }
                // RTK is a Claude Code hook (`.claude/settings.json`) — Claude
                // only. Codex/Gemini/Copilot never read it, so don't offer it.
                if preset.agent_provider == "claude" {
                    rows.push(ConfigureRow::Rtk);
                }
            }
        } else {
            // Real preset — Mode/Yolo are shown locked, but only when the
            // preset's agent runtime supports them. Shell preset: no Mode/Yolo.
            if preset.agent_provider != "shell" {
                rows.push(ConfigureRow::Mode);
                rows.push(ConfigureRow::Yolo);
                if preset.agent_provider == "claude" || preset.agent_provider == "codex" {
                    rows.push(ConfigureRow::HeadroomProxy);
                }
                // RTK is a Claude Code hook (`.claude/settings.json`) — Claude
                // only. Codex/Gemini/Copilot never read it, so don't offer it.
                if preset.agent_provider == "claude" {
                    rows.push(ConfigureRow::Rtk);
                }
            }
        }
        // Branch row visible for everything that isn't SSH.
        rows.push(ConfigureRow::Branch);
        // Prompt row visible only in Boss mode for non-shell agents.
        if preset.mode == SessionMode::Boss && preset.agent_provider != "shell" {
            rows.push(ConfigureRow::Prompt);
        }
        // Explicit Launch row — always last. Tab past Prompt lands here;
        // Enter on this row fires the launch. (Stevie 2026-05-27 — replaces
        // the Enter-anywhere semantics that conflicted with Enter-on-Branch
        // opening inline edit.)
        rows.push(ConfigureRow::Launch);
        rows
    }

    /// Cycle focus through `visible_rows` by `delta` (+1 forward, -1 back).
    /// Wraps. Silently ignores when the row set is empty (defensive).
    fn cycle_focus(&mut self, delta: i32) {
        let rows = self.visible_rows();
        if rows.is_empty() {
            return;
        }
        let cur = rows.iter().position(|r| *r == self.focused_row).unwrap_or(0);
        let len = rows.len() as i32;
        let next = ((cur as i32) + delta).rem_euclid(len) as usize;
        self.focused_row = rows[next];
        // Leaving the Prompt row: cancel branch_edit, no-op for prompt
        // contents (the textarea state is sticky).
        if self.focused_row != ConfigureRow::Branch {
            self.branch_edit = None;
        }
    }
}

/// What the dispatcher should do after a key press on Configure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigureOutcome {
    /// Re-render same state (most input).
    Stay,
    /// Esc pressed — return to PickRepo. The dispatcher must persist any
    /// half-typed prompt to session-defaults BEFORE transitioning.
    BackToPickRepo,
    /// User confirmed launch — build a session with the given spec.
    Launch(LaunchSpec),
    /// `^P` — open the preset manager overlay (stub for Phase 5; Phase 7).
    OpenPresetManager,
    /// Enter on the Branch row's Source segment — the dispatcher must list
    /// branches (git stays out of components/ — finding #9), seed
    /// `branch_picker`, and kick the background refresh.
    OpenBranchPicker,
}

/// Launch payload built by the Configure component and threaded all the way
/// through to `create_session_from_configure` (finding #7). Carries enough
/// state for both the session-defaults persistence step and the async
/// session-creation step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub repo_label: String,
    pub repo_source: RepoSource,
    pub preset: RepositoryPreset,
    pub preset_name: String,
    pub branch_worktree: String,
    pub branch_source: String,
    /// When set, the user manually overrode the auto-derived branch name.
    /// Persisted to `session-defaults.per_repo[].last_branch_override` so the
    /// next launch can pre-fill the textarea.
    pub branch_override: Option<String>,
    /// The base-branch popup pick, when used. `None` = legacy base policy
    /// (HEAD for local repos, origin/HEAD for remote/star launches).
    pub base: Option<BaseSelection>,
    pub prompt: Option<String>,
    pub headroom_enabled: bool,
    /// Wire RTK project-local PreToolUse hook in this session's worktree.
    pub rtk_enabled: bool,
}

impl LaunchSpec {
    /// Surface the manual override (when set) so the dispatcher can persist
    /// it as `last_branch_override` without re-reading `configure_state`.
    #[must_use]
    pub fn branch_override(&self) -> Option<String> {
        self.branch_override.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    Local,
    Ssh,
}

fn pick_variant(state: &ConfigureState) -> Variant {
    if matches!(state.repo_source, RepoSource::SshSession(_)) {
        Variant::Ssh
    } else {
        Variant::Local
    }
}

/// Render the Configure screen into `area`. Layout morphs based on the
/// active variant (SSH session has fixed host/user/port lines; local picks
/// row visibility from the active preset).
#[allow(clippy::too_many_lines)]
pub fn render(f: &mut Frame, state: &ConfigureState, area: Rect) {
    let title = format!(" {} → new session ", state.repo_label);
    let outer = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(CORNFLOWER_BLUE))
        .title(Span::styled(
            title,
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(DARK_BG));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let variant = pick_variant(state);
    let rows = state.visible_rows();
    // Constraints: 1 line per row (Prompt gets Min(3) for the textarea).
    // SSH variant keeps the static 5-row layout it always had.
    let mut constraints: Vec<Constraint> = Vec::new();
    for row in &rows {
        match row {
            ConfigureRow::Prompt => constraints.push(Constraint::Min(3)),
            ConfigureRow::Preset => constraints.push(Constraint::Length(2)),
            // Branch row grows to 2 lines when it shows the collision guide.
            ConfigureRow::Branch if state.branch_collision() => {
                constraints.push(Constraint::Length(2))
            }
            _ => constraints.push(Constraint::Length(1)),
        }
    }
    constraints.push(Constraint::Min(1)); // filler
    constraints.push(Constraint::Length(2)); // help bar

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(constraints)
        .split(inner);

    // Render each visible row in order.
    for (i, row) in rows.iter().enumerate() {
        let area_for_row = chunks[i];
        let focused = state.focused_row == *row;
        match row {
            ConfigureRow::Preset => render_preset_row(f, state, area_for_row, focused),
            ConfigureRow::Agent => render_agent_row(f, state, area_for_row, focused),
            ConfigureRow::Model => render_model_row(f, state, area_for_row, focused),
            ConfigureRow::Mode => {
                render_mode_row(f, state, area_for_row, focused);
            }
            ConfigureRow::Yolo => {
                render_yolo_row(f, state, area_for_row, focused);
            }
            ConfigureRow::HeadroomProxy => {
                render_headroom_row(f, state, area_for_row, focused);
            }
            ConfigureRow::Rtk => {
                render_rtk_row(f, state, area_for_row, focused);
            }
            ConfigureRow::Host | ConfigureRow::User | ConfigureRow::Port | ConfigureRow::Key => {
                render_ssh_field(f, state, area_for_row, *row);
            }
            ConfigureRow::Branch => render_branch_row(f, state, area_for_row, focused),
            ConfigureRow::Prompt => render_prompt_row(f, state, area_for_row, focused),
            ConfigureRow::Launch => render_launch_row(f, area_for_row, focused),
        }
    }

    // Contextual help in the filler space (the Min(1) chunk between the rows
    // and the help bar), keyed to the focused row. Headroom card for now — the
    // pattern extends to other rows when they need it.
    let filler_chunk = chunks[rows.len()];
    // The remote pre-flight verdict outranks the focus-contextual guides —
    // a blocked Launch must always be explained on screen.
    if state.repo_check.blocks_launch() {
        render_repo_check(f, &state.repo_check, filler_chunk);
    } else if state.focused_row == ConfigureRow::HeadroomProxy && state.headroom_available {
        render_headroom_guide(f, filler_chunk);
    } else if state.focused_row == ConfigureRow::Rtk && state.rtk_available {
        render_rtk_guide(f, filler_chunk);
    }

    // Help bar — always last chunk.
    let help_chunk = *chunks.last().expect("layout always emits help row");
    let in_prompt =
        state.focused_row == ConfigureRow::Prompt && rows.contains(&ConfigureRow::Prompt);
    let help = render_help_bar(variant, in_prompt, state.branch_edit.is_some());
    f.render_widget(
        Paragraph::new(help).alignment(Alignment::Center),
        help_chunk,
    );

    // Modal overlay for save-preset, if open.
    if let Some(ref name_buf) = state.save_preset_modal {
        render_save_preset_modal(f, area, name_buf);
    }

    // Base-branch popup, if open. Rendered last so it overlays the form.
    if let Some(ref picker) = state.branch_picker {
        render_branch_picker_modal(f, area, picker);
    }
}

/// Build the bottom help bar. Shape switches based on whether the prompt
/// textarea is the focused row (which captures plain chars).
fn render_help_bar(variant: Variant, in_prompt: bool, branch_editing: bool) -> Line<'static> {
    if branch_editing {
        return Line::from(vec![
            Span::styled(
                "Enter",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("=Commit  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "Esc",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("=Cancel branch edit", Style::default().fg(MUTED_GRAY)),
        ]);
    }
    if in_prompt {
        return Line::from(vec![
            Span::styled(
                "Ctrl+Enter",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("=Launch  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "Esc",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("=Back  ", Style::default().fg(MUTED_GRAY)),
            Span::styled(
                "Tab",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled("=Leave  ", Style::default().fg(MUTED_GRAY)),
            Span::styled("^S", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled("=Save preset", Style::default().fg(MUTED_GRAY)),
        ]);
    }
    let mut spans = vec![
        Span::styled(
            "\u{2190}/\u{2192}",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("=Change  ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            "\u{2191}/\u{2193}",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("=Next field  ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            "Enter",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "=Launch (on [Launch] row)  ",
            Style::default().fg(MUTED_GRAY),
        ),
        Span::styled(
            "^Enter",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("=Quick launch  ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            "Esc",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("=Back  ", Style::default().fg(MUTED_GRAY)),
    ];
    if variant != Variant::Ssh {
        spans.extend([
            Span::styled("^S", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled("=Save preset", Style::default().fg(MUTED_GRAY)),
        ]);
    }
    Line::from(spans)
}

fn render_preset_row(f: &mut Frame, state: &ConfigureState, area: Rect, focused: bool) {
    let preset = state.effective_preset();
    let modified = state.is_modified();
    let current = preset.name.clone();

    // Build the pill list: every available preset, then Custom at the end.
    let mut options: Vec<String> = state.available_presets.clone();
    options.push(CUSTOM_PRESET_LABEL.to_string());

    let line = build_pills_line("Preset:  ", &options, &current, focused, &[], area.width);

    // Tack on the modified badge to the same line (after the pills).
    let line = if modified {
        let mut spans: Vec<Span<'static>> = line.spans;
        spans.push(Span::styled(
            "  \u{2022} modified",
            Style::default().fg(SELECTION_GREEN),
        ));
        Line::from(spans)
    } else {
        line
    };

    // Two-line block: name line + a contextual sub-line.
    //
    // When Custom is selected, swap the generic description bullet for an
    // actionable hint pointing at `^S` to save the current effective
    // configuration as a named preset — discoverability fix for Stevie's
    // "save it as a preset" ask (2026-05-27). Highlighted in SELECTION_GREEN
    // so it actually catches the eye, not muted.
    let is_custom = state.preset_selection == PresetSelection::Custom;
    let sub_line = if is_custom {
        Line::from(vec![
            Span::raw("           "),
            Span::styled("\u{2514} press ", Style::default().fg(MUTED_GRAY)),
            Span::styled("^S", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(
                " to save this as a named preset",
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::ITALIC),
            ),
        ])
    } else {
        let desc = describe_preset(&preset);
        Line::from(vec![
            Span::raw("           "),
            Span::styled(format!("\u{2514} {desc}"), Style::default().fg(MUTED_GRAY)),
        ])
    };
    let para = Paragraph::new(vec![line, sub_line]);
    f.render_widget(para, area);
}

fn render_agent_row(f: &mut Frame, state: &ConfigureState, area: Rect, focused: bool) {
    let preset = state.effective_preset();
    let current = agent_label(preset.agent_provider.as_str()).to_string();
    // Gemini is shown but greyed-out / non-selectable for now (kept out of the
    // `AGENTS` cycle ring) — `build_pills_line` renders it muted with a
    // `[soon]` tag. Copilot is a real, selectable option. `DISABLED_AGENTS` is
    // the single source of truth shared with the launch guard.
    let options: Vec<String> = ["Claude", "Codex", "Gemini", "Copilot", "Shell", "SSH"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let disabled: Vec<&str> = DISABLED_AGENTS.iter().map(|p| agent_label(p)).collect();

    // Width-fit gate (mirrors render_model_row): the row grew to six pills, so
    // on narrow terminals fall back to the single `◀ value ▶` cycle display
    // rather than overflowing and truncating pills off the right edge.
    let pill_width = estimate_pill_width("Agent:   ", &options, &disabled, focused);
    if pill_width > area.width as usize {
        let spans = vec![
            focus_indicator(focused),
            label_span("Agent:   "),
            cyclable_arrow_left(focused),
            Span::styled(
                current,
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            cyclable_arrow_right(focused),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let line = build_pills_line(
        "Agent:   ",
        &options,
        &current,
        focused,
        &disabled,
        area.width,
    );
    f.render_widget(Paragraph::new(line), area);
}

fn render_model_row(f: &mut Frame, state: &ConfigureState, area: Rect, focused: bool) {
    use crate::models::{ClaudeModel, CodexModel};
    let preset = state.effective_preset();
    // Resolve raw `agent_model` (a free-form String for TOML stability) into
    // the appropriate enum's `display_label()` for the active provider.
    let (current, options): (String, Vec<String>) = match preset.agent_provider.as_str() {
        "claude" => {
            let cur = ClaudeModel::parse(&preset.agent_model);
            (
                cur.display_label().to_string(),
                ClaudeModel::all().into_iter().map(|m| m.display_label().to_string()).collect(),
            )
        }
        "codex" => {
            let cur = CodexModel::parse(&preset.agent_model);
            (
                cur.display_label().to_string(),
                CodexModel::all().into_iter().map(|m| m.display_label().to_string()).collect(),
            )
        }
        _ => (preset.agent_model.clone(), vec![preset.agent_model.clone()]),
    };

    // Width-fit gate: if the pill row would overflow, fall back to the
    // single-value cycle display. The Model row's labels include ctx hints
    // like "[1M]" so they grow fast; on narrow terminals the cycle form is
    // more readable.
    let pill_width = estimate_pill_width("Model:   ", &options, &[], focused);
    if pill_width > area.width as usize {
        // Mute "system default" so the user can tell at a glance.
        let is_default = current == "system default";
        let value_style = if is_default {
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC)
        } else {
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD)
        };
        let spans = vec![
            focus_indicator(focused),
            label_span("Model:   "),
            cyclable_arrow_left(focused),
            Span::styled(current, value_style),
            cyclable_arrow_right(focused),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }

    let line = build_pills_line("Model:   ", &options, &current, focused, &[], area.width);
    f.render_widget(Paragraph::new(line), area);
}

fn render_mode_row(f: &mut Frame, state: &ConfigureState, area: Rect, focused: bool) {
    let preset = state.effective_preset();
    let is_boss = preset.mode == SessionMode::Boss;
    let cyclable = state.preset_selection == PresetSelection::Custom;

    // Mode is locked when a real preset is selected — render the bare value
    // (no pills, no arrows) when Custom isn't active. Boss carries an
    // `[alpha]` tag in muted styling per Stevie 2026-05-27 — the autonomous
    // Boss-mode path is not yet production-ready; the tag signals "don't
    // expect this to fully work yet".
    if !cyclable {
        // Locked display (real preset selected). For Boss, render muted +
        // italic with the [alpha] tag inline. For Interactive, fall through
        // to the standard locked-value renderer.
        if is_boss {
            let spans = vec![
                focus_indicator(focused),
                label_span("Mode:    "),
                Span::styled(
                    "Boss [alpha]",
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                ),
            ];
            f.render_widget(Paragraph::new(Line::from(spans)), area);
            return;
        }
        f.render_widget(
            Paragraph::new(value_row_locked("Mode:    ", "Interactive", focused, false)),
            area,
        );
        return;
    }

    // ponytail: Boss/container mode is hidden for now — the only mode is
    // Interactive, so even the Custom path renders a fixed value (no pills, no
    // arrows). Restore the Interactive/Boss pill picker when the container
    // session path is wired up again.
    f.render_widget(
        Paragraph::new(value_row_locked("Mode:    ", "Interactive", focused, false)),
        area,
    );
}

fn render_yolo_row(f: &mut Frame, state: &ConfigureState, area: Rect, focused: bool) {
    let preset = state.effective_preset();
    let current = if preset.permissions.skip_all {
        "ON".to_string()
    } else {
        "OFF".to_string()
    };
    let cyclable = state.preset_selection == PresetSelection::Custom;
    if !cyclable {
        f.render_widget(
            Paragraph::new(value_row_locked("Yolo:    ", &current, focused, false)),
            area,
        );
        return;
    }
    let options = vec!["ON".to_string(), "OFF".to_string()];
    let line = build_pills_line("Yolo:    ", &options, &current, focused, &[], area.width);
    f.render_widget(Paragraph::new(line), area);
}

fn render_headroom_row(f: &mut Frame, state: &ConfigureState, area: Rect, focused: bool) {
    // Gate: if the headroom binary isn't on PATH, the toggle can't work — show
    // a muted, non-interactive row with the install command instead of pills.
    if !state.headroom_available {
        let line = Line::from(vec![
            focus_indicator(focused),
            label_span("Headroom: "),
            Span::styled(
                "unavailable \u{2014} install: uv tool install 'headroom-ai[proxy]'",
                Style::default().fg(MUTED_GRAY),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    let current = if state.headroom_enabled {
        "on".to_string()
    } else {
        "off".to_string()
    };
    let options = vec!["on".to_string(), "off".to_string()];
    let mut line = build_pills_line("Headroom: ", &options, &current, focused, &[], area.width);
    // Brief muted explainer + link. Terminals auto-linkify the bare URL, so a
    // cmd/ctrl-click opens it — no OSC-8 escape juggling needed.
    line.spans.push(Span::styled(
        "  \u{2014} proxy that trims token usage \u{00b7} github.com/chopratejas/headroom",
        Style::default().fg(MUTED_GRAY),
    ));
    f.render_widget(Paragraph::new(line), area);
}

/// Contextual "when to use Headroom" card, shown in the new-session filler
/// space while the Headroom row is focused. Honest pros/cons at the point of
/// choice — token savings vs. latency + a proxy dependency that auto-degrades.
fn render_headroom_guide(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            "  Headroom \u{00b7} local compression proxy",
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  Use when token budget matters more than speed.",
            Style::default().fg(MUTED_GRAY),
        )),
        Line::from(Span::styled(
            "    \u{2713} trims context \u{2192} fewer tokens billed",
            Style::default().fg(SELECTION_GREEN),
        )),
        Line::from(Span::styled(
            "    \u{2717} ~100ms latency per call",
            Style::default().fg(MUTED_GRAY),
        )),
        Line::from(Span::styled(
            "    \u{2717} proxy dependency \u{2014} auto-degrades to direct on failure",
            Style::default().fg(MUTED_GRAY),
        )),
        Line::from(Span::styled(
            "  Off = straight to the provider \u{00b7} fastest \u{00b7} no savings",
            Style::default().fg(MUTED_GRAY),
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

fn render_rtk_row(f: &mut Frame, state: &ConfigureState, area: Rect, focused: bool) {
    if !state.rtk_available {
        let line = Line::from(vec![
            focus_indicator(focused),
            label_span("RTK:      "),
            Span::styled(
                "unavailable \u{2014} install: brew install rtk",
                Style::default().fg(MUTED_GRAY),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    let current = if state.rtk_enabled {
        "on".to_string()
    } else {
        "off".to_string()
    };
    let options = vec!["on".to_string(), "off".to_string()];
    let mut line = build_pills_line("RTK:      ", &options, &current, focused, &[], area.width);
    line.spans.push(Span::styled(
        "  \u{2014} compress tool output via hooks \u{00b7} Claude only \u{00b7} github.com/rtk-ai/rtk",
        Style::default().fg(MUTED_GRAY),
    ));
    f.render_widget(Paragraph::new(line), area);
}

/// Remote pre-flight status card, shown in the filler space while the check
/// is in flight or has failed. A failure blocks Launch, so it must be loud.
fn render_repo_check(f: &mut Frame, check: &RepoCheck, area: Rect) {
    let lines = match check {
        RepoCheck::Checking => vec![Line::from(Span::styled(
            "  \u{23f3} validating remote repository\u{2026}",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        ))],
        RepoCheck::Failed(msg) => vec![
            Line::from(Span::styled(
                format!("  \u{2716} {msg}"),
                Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "  Launch is disabled \u{2014} Esc to pick another repository",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            )),
        ],
        RepoCheck::NotApplicable | RepoCheck::Ok => return,
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Contextual RTK guide card, shown while the RTK row is focused.
fn render_rtk_guide(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            "  RTK \u{00b7} project-local Claude Code hook",
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  Wires a PreToolUse hook into this session's worktree .claude/settings.json.",
            Style::default().fg(MUTED_GRAY),
        )),
        Line::from(Span::styled(
            "    \u{2713} compresses Bash/test/diff output \u{2192} fewer tokens",
            Style::default().fg(SELECTION_GREEN),
        )),
        Line::from(Span::styled(
            "    \u{2713} hook-based \u{2014} zero latency overhead",
            Style::default().fg(SELECTION_GREEN),
        )),
        Line::from(Span::styled(
            "    \u{2717} Claude only \u{2014} Codex hook path out of scope for this phase",
            Style::default().fg(MUTED_GRAY),
        )),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

/// Inline marker + guidance sub-line for a Branch-row problem. Returns
/// `(trailing marker, guidance text)`; the caller styles them red / muted.
const fn branch_problem_text(problem: BranchProblem) -> (&'static str, &'static str) {
    match problem {
        BranchProblem::InUse => (
            "   \u{26a0} in use",
            "\u{2514} already checked out by a session \u{2014} pick another name, or Esc \u{2192} menu \u{2192} Recovery to respawn it",
        ),
        BranchProblem::Exists => (
            "   \u{26a0} exists",
            "\u{2514} a branch with this name already exists \u{2014} pick another name, or Enter on Branch \u{2192} check it out as the base",
        ),
    }
}

fn render_branch_row(f: &mut Frame, state: &ConfigureState, area: Rect, focused: bool) {
    if let Some(ref buf) = state.branch_edit {
        // Inline edit mode. The problem evaluates live against the edit buffer
        // (effective_branch() prefers branch_edit), so the ⚠ warning appears
        // as the user types a name that's already in use or already exists.
        let problem = state.branch_problem();
        let buf_style = if problem.is_some() {
            Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
        };
        let edit_line = Line::from(vec![
            focus_indicator(focused),
            label_span("Branch:  "),
            Span::styled(state.branch_source.clone(), Style::default().fg(SOFT_WHITE)),
            Span::styled(" \u{2192} ", Style::default().fg(MUTED_GRAY)),
            Span::styled(buf.clone(), buf_style),
            Span::styled("_", Style::default().fg(MUTED_GRAY)),
            problem.map_or_else(
                || Span::raw(""),
                |p| {
                    Span::styled(
                        branch_problem_text(p).0,
                        Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD),
                    )
                },
            ),
        ]);
        if let Some(p) = problem {
            let guide = Line::from(vec![
                Span::raw("           "),
                Span::styled(
                    branch_problem_text(p).1,
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                ),
            ]);
            f.render_widget(Paragraph::new(vec![edit_line, guide]), area);
        } else {
            f.render_widget(Paragraph::new(edit_line), area);
        }
        return;
    }
    // Checkout-direct pick: the picked branch IS the session branch — no
    // `source → worktree` arrow, no generated name.
    if state.is_checkout() {
        let line = Line::from(vec![
            focus_indicator(focused),
            label_span("Branch:  "),
            Span::styled(
                state.effective_branch(),
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  (checkout)",
                Style::default().fg(GOLD).add_modifier(Modifier::ITALIC),
            ),
            Span::styled(
                "   [Enter to pick base]",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }

    let worktree = state.effective_branch();
    let problem = state.branch_problem();

    // Segment targeting (2026-06 base picker): when the row is focused the
    // targeted segment renders underlined; ←/→ toggles, Enter acts on it.
    let source_targeted = focused && state.branch_segment == BranchSegment::Source;
    let worktree_targeted = focused && state.branch_segment == BranchSegment::Worktree;

    let mut source_style = Style::default().fg(SOFT_WHITE);
    if source_targeted {
        source_style = source_style.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
    }

    // Branch worktree name renders red on a problem (in-use OR an existing
    // base-off name), green otherwise. Only reachable via a manual override.
    let mut worktree_style = if problem.is_some() {
        Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
    };
    if worktree_targeted {
        worktree_style = worktree_style.add_modifier(Modifier::UNDERLINED);
    }
    let trailing = problem.map_or_else(
        || {
            // No problem: show the contextual targeting hint instead.
            let hint = if source_targeted {
                "   [Enter to pick base \u{00b7} \u{2192} name]"
            } else if worktree_targeted {
                "   [Enter to edit \u{00b7} \u{2190} base]"
            } else {
                "   [Enter to edit]"
            };
            Span::styled(
                hint,
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            )
        },
        |p| {
            Span::styled(
                branch_problem_text(p).0,
                Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD),
            )
        },
    );
    let branch_line = Line::from(vec![
        focus_indicator(focused),
        label_span("Branch:  "),
        Span::styled(state.branch_source.clone(), source_style),
        Span::styled(" \u{2192} ", Style::default().fg(MUTED_GRAY)),
        Span::styled(worktree, worktree_style),
        trailing,
    ]);

    if let Some(p) = problem {
        // Two-line block: the worktree-name problem + the guidance sub-line.
        let guide = Line::from(vec![
            Span::raw("           "),
            Span::styled(
                branch_problem_text(p).1,
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            ),
        ]);
        f.render_widget(Paragraph::new(vec![branch_line, guide]), area);
    } else {
        f.render_widget(Paragraph::new(branch_line), area);
    }
}

fn render_prompt_row(f: &mut Frame, state: &ConfigureState, area: Rect, focused: bool) {
    let border_color = if focused { GOLD } else { MUTED_GRAY };
    let prompt_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            " Prompt: ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
    let prompt_text: String = state.prompt.get_lines().join("\n");
    let prompt_para = Paragraph::new(prompt_text)
        .style(Style::default().fg(SOFT_WHITE))
        .wrap(Wrap { trim: false })
        .block(prompt_block);
    f.render_widget(prompt_para, area);
}

/// Render the explicit `[ Launch ]` button row at the bottom of the form.
/// Focused state: GOLD bold brackets + green-on-dark label, drawing the eye.
/// Unfocused: muted bordered-button visual hinting at submit-ability.
fn render_launch_row(f: &mut Frame, area: Rect, focused: bool) {
    let arrow = focus_indicator(focused);
    let (bracket_l, bracket_r, label_style) = if focused {
        (
            Span::styled("[ ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Span::styled(" ]", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
        )
    } else {
        (
            Span::styled("[ ", Style::default().fg(MUTED_GRAY)),
            Span::styled(" ]", Style::default().fg(MUTED_GRAY)),
            Style::default().fg(SOFT_WHITE),
        )
    };
    let hint = if focused {
        Span::styled(
            "   press Enter",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )
    } else {
        Span::styled(
            "   Tab to here, then Enter",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )
    };
    let line = Line::from(vec![
        arrow,
        bracket_l,
        Span::styled("Launch", label_style),
        bracket_r,
        hint,
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_ssh_field(f: &mut Frame, state: &ConfigureState, area: Rect, row: ConfigureRow) {
    let url = match &state.repo_source {
        RepoSource::SshSession(s) => s.as_str(),
        _ => "",
    };
    let (user, host, port) = parse_ssh_session(url);
    let (label, value) = match row {
        ConfigureRow::Host => ("Host:    ", host),
        ConfigureRow::User => ("User:    ", user),
        ConfigureRow::Port => ("Port:    ", port),
        ConfigureRow::Key => ("Key:     ", "~/.ssh/id_ed25519".to_string()),
        _ => return,
    };
    let line = Line::from(vec![
        Span::styled(
            label,
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, Style::default().fg(SOFT_WHITE)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

// --- render helpers -------------------------------------------------------

fn focus_indicator(focused: bool) -> Span<'static> {
    if focused {
        Span::styled(
            "\u{25b8} ",
            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw("  ")
    }
}

fn label_span(label: &'static str) -> Span<'static> {
    Span::styled(
        label,
        Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
    )
}

fn cyclable_arrow_left(focused: bool) -> Span<'static> {
    if focused {
        Span::styled("\u{25c0} ", Style::default().fg(MUTED_GRAY))
    } else {
        Span::raw("  ")
    }
}

fn cyclable_arrow_right(focused: bool) -> Span<'static> {
    if focused {
        Span::styled(" \u{25b6}", Style::default().fg(MUTED_GRAY))
    } else {
        Span::raw("  ")
    }
}

/// Build a pill row: every option rendered inline, with the current one
/// highlighted in SELECTION_GREEN + bold + `[…]` markers. Separator is
/// ` · ` in MUTED_GRAY. When the row is focused, a "←/→ to change" hint is
/// appended in MUTED_GRAY italic.
///
/// Width-aware: if the rendered pill row would exceed `available_width`, the
/// caller is expected to have already gated on `estimate_pill_width` and
/// fallen back to the `◀ value ▶` single-cycle display. This function still
/// builds the line — the gate is a render-time decision in the row fn.
fn build_pills_line(
    label: &'static str,
    options: &[String],
    current: &str,
    focused: bool,
    disabled: &[&str],
    _available_width: u16,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(focus_indicator(focused));
    spans.push(label_span(label));

    for (i, opt) in options.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" \u{00b7} ", Style::default().fg(MUTED_GRAY)));
        }
        let is_disabled = disabled.contains(&opt.as_str());
        let is_current = opt == current;
        if is_disabled {
            // Greyed-out, non-selectable option (e.g. Gemini): muted + italic
            // with a `[soon]` tag so it reads as unavailable — distinct from a
            // merely-not-current option, which is plain muted with no tag. If a
            // disabled option is somehow also the current one (a hand-authored
            // preset), still bracket it — muted — so the row always shows a
            // selection rather than nothing.
            let style = Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC);
            if is_current {
                spans.push(Span::styled("[", style));
                spans.push(Span::styled(opt.clone(), style));
                spans.push(Span::styled("]", style));
            } else {
                spans.push(Span::styled(opt.clone(), style));
            }
            spans.push(Span::styled(" [soon]", style));
        } else if is_current {
            spans.push(Span::styled(
                "[",
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                opt.clone(),
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                "]",
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(opt.clone(), Style::default().fg(MUTED_GRAY)));
        }
    }

    if focused {
        spans.push(Span::styled(
            "   \u{2190}/\u{2192} to change",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        ));
    }

    Line::from(spans)
}

/// Estimate the visual width (in monospaced cells) the pill row will take.
/// Used to gate fallback to the `◀ value ▶` single-cycle display on narrow
/// terminals. Slightly over-approximates: counts char_indices (Unicode) but
/// charges 1 cell per char (good enough for ASCII + the few `·` separators).
fn estimate_pill_width(label: &str, options: &[String], disabled: &[&str], focused: bool) -> usize {
    // 2 chars for focus indicator ("▸ " or "  "), then label, then pills.
    let mut w = 2 + label.chars().count();
    for (i, opt) in options.iter().enumerate() {
        if i > 0 {
            w += " \u{00b7} ".chars().count();
        }
        // Plus 2 for the [ ] around the current item — over-counts for
        // non-current options, but we want the gate to fire generously.
        w += opt.chars().count() + 2;
        // Disabled options carry a trailing " [soon]" tag.
        if disabled.contains(&opt.as_str()) {
            w += " [soon]".chars().count();
        }
    }
    if focused {
        w += "   ←/→ to change".chars().count();
    }
    w
}

/// Build a single Line for a cyclable value row (always cyclable).
#[allow(dead_code)]
fn value_row(label: &'static str, value: &str, focused: bool) -> Line<'static> {
    let spans = vec![
        focus_indicator(focused),
        label_span(label),
        cyclable_arrow_left(focused),
        Span::styled(
            value.to_string(),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
        cyclable_arrow_right(focused),
    ];
    Line::from(spans)
}

/// Build a row that's only conditionally cyclable. When `cyclable` is false
/// the arrows are omitted (the value reads as locked / display-only).
fn value_row_locked(
    label: &'static str,
    value: &str,
    focused: bool,
    cyclable: bool,
) -> Line<'static> {
    if cyclable {
        return value_row(label, value, focused);
    }
    let spans = vec![
        focus_indicator(focused),
        label_span(label),
        Span::styled(
            value.to_string(),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
    ];
    Line::from(spans)
}

/// One-line description of a preset for the secondary preset row.
fn describe_preset(p: &RepositoryPreset) -> String {
    let model = if p.agent_model.is_empty() {
        "?".to_string()
    } else {
        p.agent_model.clone()
    };
    let mode = match p.mode {
        SessionMode::Boss => "Boss",
        SessionMode::Interactive => "Interactive",
    };
    let perms = if p.permissions.skip_all {
        "Yolo"
    } else {
        "Safe"
    };
    format!("{model} \u{00b7} {mode} \u{00b7} {perms}")
}

/// Centered save-preset modal. Rendered last so it overlays the form.
fn render_save_preset_modal(f: &mut Frame, area: Rect, name_buf: &str) {
    let width = 50.min(area.width.saturating_sub(4));
    let height = 5;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect::new(x, y, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(
            " Save preset as ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(DARK_BG));
    let inner = block.inner(modal);
    f.render_widget(ratatui::widgets::Clear, modal);
    f.render_widget(block, modal);

    let line = Line::from(vec![
        Span::styled("> ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled(name_buf.to_string(), Style::default().fg(SOFT_WHITE)),
        Span::styled("_", Style::default().fg(MUTED_GRAY)),
    ]);
    let help = Line::from(vec![
        Span::styled("Enter", Style::default().fg(GOLD)),
        Span::styled("=Save  ", Style::default().fg(MUTED_GRAY)),
        Span::styled("Esc", Style::default().fg(GOLD)),
        Span::styled("=Cancel", Style::default().fg(MUTED_GRAY)),
    ]);
    let para = Paragraph::new(vec![line, Line::raw(""), help]);
    f.render_widget(para, inner);
}

/// Centered base-branch popup. Filter line, sectioned scrollable list
/// (remote first, default on top, `⚠ in use` markers), mode-aware footer.
fn render_branch_picker_modal(f: &mut Frame, area: Rect, picker: &BranchPickerState) {
    let width = 62.min(area.width.saturating_sub(4));
    let height = 16.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let modal = Rect::new(x, y, width, height);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(GOLD))
        .title(Span::styled(
            " Pick base branch ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Center)
        .style(Style::default().bg(DARK_BG));
    let inner = block.inner(modal);
    f.render_widget(ratatui::widgets::Clear, modal);
    f.render_widget(block, modal);

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Filter line, with the refresh spinner while the background fetch runs.
    let mut filter_spans = vec![
        Span::styled("> ", Style::default().fg(GOLD).add_modifier(Modifier::BOLD)),
        Span::styled(picker.filter.clone(), Style::default().fg(SOFT_WHITE)),
        Span::styled("_", Style::default().fg(MUTED_GRAY)),
    ];
    if picker.loading {
        filter_spans.push(Span::styled(
            "   \u{27f3} refreshing\u{2026}",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        ));
    }
    lines.push(Line::from(filter_spans));

    // Error line (e.g. checkout pick on an in-use branch), else spacer.
    if let Some(ref err) = picker.error {
        lines.push(Line::from(Span::styled(
            format!("\u{26a0} {err}"),
            Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::raw(""));
    }

    // Build the display list: section headers interleaved with entries.
    enum Item {
        Header(&'static str),
        Entry(usize, usize), // (entries idx, filtered position)
    }
    let filtered = picker.filtered_indices();
    let mut items: Vec<Item> = Vec::new();
    let mut last_remote: Option<bool> = None;
    for (pos, &idx) in filtered.iter().enumerate() {
        let is_remote = picker.entries[idx].entry.is_remote;
        if last_remote != Some(is_remote) {
            items.push(Item::Header(if is_remote { "remote" } else { "local" }));
            last_remote = Some(is_remote);
        }
        items.push(Item::Entry(idx, pos));
    }

    // 2 lines used above + 1 footer line below.
    let list_height = (inner.height as usize).saturating_sub(3).max(1);

    if items.is_empty() {
        let msg = if picker.entries.is_empty() && picker.loading {
            "loading branches\u{2026}"
        } else {
            "no branches match"
        };
        lines.push(Line::from(Span::styled(
            format!("  {msg}"),
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )));
    } else {
        // Scroll the window so the selected entry stays visible.
        let sel_display = items
            .iter()
            .position(|i| matches!(i, Item::Entry(_, pos) if *pos == picker.selected))
            .unwrap_or(0);
        let start = sel_display.saturating_sub(list_height.saturating_sub(1));
        for item in items.iter().skip(start).take(list_height) {
            match item {
                Item::Header(name) => lines.push(Line::from(Span::styled(
                    format!("\u{2500}\u{2500} {name} \u{2500}\u{2500}"),
                    Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                ))),
                Item::Entry(idx, pos) => {
                    let e = &picker.entries[*idx];
                    let selected = *pos == picker.selected;
                    let mut spans: Vec<Span<'static>> = Vec::new();
                    if selected {
                        spans.push(Span::styled(
                            "\u{25b8} ",
                            Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
                        ));
                    } else {
                        spans.push(Span::raw("  "));
                    }
                    let name_style = if selected {
                        Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(SOFT_WHITE)
                    };
                    spans.push(Span::styled(e.entry.display.clone(), name_style));
                    if e.entry.is_default {
                        spans.push(Span::styled(
                            "  (default)",
                            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
                        ));
                    }
                    if e.in_use {
                        spans.push(Span::styled(
                            "  \u{26a0} in use",
                            Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD),
                        ));
                    }
                    lines.push(Line::from(spans));
                }
            }
        }
    }

    // Pad so the footer sits on the last inner line.
    while (lines.len() as u16) < inner.height.saturating_sub(1) {
        lines.push(Line::raw(""));
    }

    // Mode-aware footer: Enter's action follows the Tab-toggled mode.
    let (enter_action, tab_action) = match picker.mode {
        BaseMode::BaseOff => ("=New branch off pick  ", "=Checkout mode  "),
        BaseMode::Checkout => ("=Checkout branch  ", "=Base-off mode  "),
    };
    lines.push(Line::from(vec![
        Span::styled(
            "Enter",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(enter_action, Style::default().fg(MUTED_GRAY)),
        Span::styled(
            "Tab",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(tab_action, Style::default().fg(MUTED_GRAY)),
        Span::styled(
            "Esc",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled("=Close", Style::default().fg(MUTED_GRAY)),
    ]));

    f.render_widget(Paragraph::new(lines), inner);
}

/// Render-side adapter: parse `ssh://user@host:port` into (user, host, port)
/// strings for the SSH variant.
fn parse_ssh_session(url: &str) -> (String, String, String) {
    match crate::git::repo_source::parse_ssh_session_url(url) {
        Some(target) => (
            target.user.unwrap_or_default(),
            target.host,
            target.port.to_string(),
        ),
        None => (String::new(), String::new(), "22".to_string()),
    }
}

// --- key handling ---------------------------------------------------------

/// Handle a single key event for the Configure screen.
///
/// Returns the outcome the dispatcher should act on. Mutates `state` in place
/// for the common "type a char" path.
///
/// Key model (2026-05 split — ↑/↓ are row-nav, NOT value cycling):
///   * Tab / Shift+Tab — cycle focus through visible rows (canonical).
///   * ↑ / ↓ — alias for Shift+Tab / Tab respectively. The earlier prototype
///     had these double as value cycling; Stevie flagged that as a UX bug.
///     Now strictly row-nav, EXCEPT when the focused row is `Prompt` — there
///     the arrow keys are absorbed by the textarea for cursor movement.
///   * ← / → — cycle the VALUE in the focused row. No effect on Prompt row.
///   * Enter — Launch from any non-Prompt row. On the Branch row Enter opens
///     inline edit. On the Prompt row Enter inserts a newline; Ctrl+Enter
///     launches.
///   * Esc — back to PickRepo (or cancel the active branch edit / save-preset
///     modal).
///   * Ctrl+S / Ctrl+P — save preset / open preset manager.
pub fn handle_key(state: &mut ConfigureState, key: KeyEvent) -> ConfigureOutcome {
    // Modal interception — every key goes to the modal until it closes.
    if state.save_preset_modal.is_some() {
        return handle_modal_key(state, key);
    }

    // Base-branch popup interception — mirrors the save-preset modal.
    // INVARIANT: the two modals are mutually exclusive (the picker handler
    // exposes no ^S path and vice versa). If that ever changes, align this
    // precedence with the render order in `render()` — the picker draws on
    // top, so it must also win the key race.
    if state.branch_picker.is_some() {
        return handle_branch_picker_key(state, key);
    }

    // Inline branch edit takes priority over the row machinery — every key
    // either commits / cancels the edit or extends the buffer.
    if state.branch_edit.is_some() {
        return handle_branch_edit_key(state, key);
    }

    // Ctrl shortcuts.
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('s' | 'S') => {
                state.save_preset_modal = Some(String::new());
                return ConfigureOutcome::Stay;
            }
            KeyCode::Char('p' | 'P') => {
                tracing::warn!("configure: ^P preset manager — stub until Phase 7 polish");
                return ConfigureOutcome::OpenPresetManager;
            }
            // Ctrl+Enter from anywhere = Launch. Many terminals don't deliver
            // Ctrl+Enter distinctly (they collapse to Enter), but where they
            // do we honour it as the prompt-textarea escape hatch.
            KeyCode::Enter => return launch_outcome(state),
            _ => {}
        }
    }

    match key.code {
        KeyCode::Esc => ConfigureOutcome::BackToPickRepo,
        KeyCode::Tab => {
            state.cycle_focus(1);
            ConfigureOutcome::Stay
        }
        KeyCode::BackTab => {
            state.cycle_focus(-1);
            ConfigureOutcome::Stay
        }
        KeyCode::Enter => match state.focused_row {
            ConfigureRow::Branch => match state.branch_segment {
                // Source segment: open the base-branch picker popup. The
                // dispatcher lists branches (git stays out of components/).
                BranchSegment::Source => ConfigureOutcome::OpenBranchPicker,
                BranchSegment::Worktree => {
                    // Checkout-direct pick: no generated name to edit — route
                    // to the picker instead so Enter never dead-ends.
                    if state.is_checkout() {
                        return ConfigureOutcome::OpenBranchPicker;
                    }
                    // Open inline branch edit. Seed buffer from override or auto.
                    let buf = state
                        .branch_override
                        .clone()
                        .unwrap_or_else(|| state.branch_worktree.clone());
                    state.branch_edit = Some(buf);
                    ConfigureOutcome::Stay
                }
            },
            ConfigureRow::Prompt => {
                // Inside Prompt textarea — Enter = newline. Ctrl+Enter is
                // the launch shortcut from anywhere.
                state.prompt.insert_newline();
                ConfigureOutcome::Stay
            }
            ConfigureRow::Launch => launch_outcome(state),
            // Enter on a non-Launch row no longer fires the launch — Stevie
            // 2026-05-27 wants the explicit Launch row to be the only canonical
            // way to commit the form. Ctrl+Enter still works as the quick-
            // launch shortcut from any row (handled higher in this match).
            _ => ConfigureOutcome::Stay,
        },
        KeyCode::Left => {
            cycle_value_in_focused_row(state, -1);
            ConfigureOutcome::Stay
        }
        KeyCode::Right => {
            cycle_value_in_focused_row(state, 1);
            ConfigureOutcome::Stay
        }
        // ↑/↓ are row navigation (alias for Shift+Tab / Tab respectively).
        // EXCEPT inside the Prompt textarea — there they're absorbed by the
        // textarea so vertical cursor movement still works. The textarea
        // doesn't itself implement arrow-key cursor moves today, but
        // forwarding the keystroke leaves room for that without retraining
        // muscle memory later.
        KeyCode::Up => {
            if state.focused_row == ConfigureRow::Prompt {
                // No-op for now (TextEditor has no vertical move API yet);
                // intentionally NOT row-nav so Stevie's "↑/↓ behave normally
                // inside the textarea" rule holds.
                return ConfigureOutcome::Stay;
            }
            state.cycle_focus(-1);
            ConfigureOutcome::Stay
        }
        KeyCode::Down => {
            if state.focused_row == ConfigureRow::Prompt {
                return ConfigureOutcome::Stay;
            }
            state.cycle_focus(1);
            ConfigureOutcome::Stay
        }
        KeyCode::Backspace => {
            if state.focused_row == ConfigureRow::Prompt {
                state.prompt.backspace();
            }
            ConfigureOutcome::Stay
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            if state.focused_row == ConfigureRow::Prompt {
                state.prompt.insert_char(c);
            }
            // No bare-char shortcuts elsewhere — Tab + arrows is the entire
            // navigation surface for non-Prompt rows.
            ConfigureOutcome::Stay
        }
        _ => ConfigureOutcome::Stay,
    }
}

/// Build a `Launch` outcome from the current state.
fn launch_outcome(state: &mut ConfigureState) -> ConfigureOutcome {
    // Pre-flight: refuse to launch onto a branch already checked out in a
    // live worktree (git would reject `worktree add` anyway). Move focus to
    // the Branch row so the inline "⚠ in use" guidance is unmissable, and
    // stay on Configure. Single chokepoint — covers both the [Launch] row
    // and the Ctrl+Enter quick-launch (Stevie 2026-05-27).
    if state.branch_collision() {
        state.focused_row = ConfigureRow::Branch;
        return ConfigureOutcome::Stay;
    }
    // Remote pre-flight gate: a missing / empty / unreachable remote can never
    // launch — the clone or worktree step is guaranteed to fail. Refuse here
    // and let the inline message (rendered in the filler space) explain.
    // `Checking` also blocks: ls-remote lands sub-second, and launching before
    // the verdict would just re-open the old fail-after-Launch hole.
    if state.repo_check.blocks_launch() {
        return ConfigureOutcome::Stay;
    }
    // Defense-in-depth: a greyed-out agent (e.g. Gemini) is never selectable in
    // the UI, but a hand-authored preset could still carry one. Refuse to launch
    // a disabled provider and refocus the Agent row, mirroring the collision
    // guard above. `DISABLED_AGENTS` is the shared source of truth with the
    // Agent-row greying, so the two never disagree.
    if DISABLED_AGENTS.contains(&state.effective_preset().agent_provider.as_str()) {
        state.focused_row = ConfigureRow::Agent;
        return ConfigureOutcome::Stay;
    }
    let preset = state.effective_preset();
    let prompt = state.prompt.to_non_empty_string();
    // Checkout-direct pick: the session branch IS the picked branch — the
    // generated `agents/xxx` name (and any manual override) doesn't apply.
    let branch_worktree = if state.is_checkout() {
        state.effective_branch()
    } else {
        state.branch_override.clone().unwrap_or_else(|| state.branch_worktree.clone())
    };
    ConfigureOutcome::Launch(LaunchSpec {
        repo_label: state.repo_label.clone(),
        repo_source: state.repo_source.clone(),
        preset_name: preset.name.clone(),
        preset,
        branch_worktree,
        branch_source: state.branch_source.clone(),
        branch_override: state.branch_override.clone(),
        base: state.base_selection.clone(),
        prompt,
        // Defensive: never launch with Headroom on if the binary isn't there,
        // even if some stale state slipped through.
        headroom_enabled: state.headroom_enabled && state.headroom_available,
        // Same guard for RTK.
        rtk_enabled: state.rtk_enabled && state.rtk_available,
    })
}

/// Cycle the value in the focused row by `delta` (+1 / -1). For locked rows
/// (Mode / Yolo when a real preset is active), no-op.
fn cycle_value_in_focused_row(state: &mut ConfigureState, delta: i32) {
    match state.focused_row {
        ConfigureRow::Preset => cycle_preset_ring(state, delta),
        ConfigureRow::Agent => {
            if state.preset_selection == PresetSelection::Custom {
                cycle_agent(state, delta);
            }
        }
        ConfigureRow::Model => {
            if state.preset_selection == PresetSelection::Custom {
                cycle_model(state, delta);
            }
        }
        ConfigureRow::Mode => {
            if state.preset_selection == PresetSelection::Custom {
                cycle_mode(state);
            }
        }
        ConfigureRow::Yolo => {
            if state.preset_selection == PresetSelection::Custom {
                cycle_yolo(state);
            }
        }
        ConfigureRow::HeadroomProxy => {
            // No-op when headroom isn't installed — the row is informational only.
            if state.headroom_available {
                state.headroom_enabled = !state.headroom_enabled;
            }
        }
        ConfigureRow::Rtk => {
            // No-op when rtk isn't installed — the row is informational only.
            if state.rtk_available {
                state.rtk_enabled = !state.rtk_enabled;
            }
        }
        ConfigureRow::Branch => {
            // ←/→ on the Branch row toggles the targeted segment
            // (source ⇄ worktree). Checkout mode pins Source — there's no
            // editable worktree name to target.
            if !state.is_checkout() {
                state.branch_segment = match state.branch_segment {
                    BranchSegment::Source => BranchSegment::Worktree,
                    BranchSegment::Worktree => BranchSegment::Source,
                };
            }
        }
        ConfigureRow::Prompt
        | ConfigureRow::Host
        | ConfigureRow::User
        | ConfigureRow::Port
        | ConfigureRow::Key
        | ConfigureRow::Launch => {
            // No cyclable value — silently ignore.
        }
    }
}

/// Cycle the preset selection ring: Named(0)..Named(n-1) → Custom → Named(0).
fn cycle_preset_ring(state: &mut ConfigureState, delta: i32) {
    if state.available_presets.is_empty() {
        return;
    }
    let n = state.available_presets.len() as i32;
    // Ring length = named count + 1 (the Custom slot).
    let ring_len = n + 1;
    let cur = match state.preset_selection {
        PresetSelection::Named(idx) => idx as i32,
        PresetSelection::Custom => n,
    };
    let next = (cur + delta).rem_euclid(ring_len);
    if next == n {
        // Stepping into Custom — seed overrides from the previously-selected
        // named preset so the editor starts at a known baseline.
        let seed = match state.preset_selection {
            PresetSelection::Named(idx) => state
                .available_presets
                .get(idx)
                .and_then(|n| state.presets_cache.get(n).cloned())
                .unwrap_or_else(|| state.current_preset.clone()),
            PresetSelection::Custom => state.current_preset.clone(),
        };
        if state.custom_overrides.is_none() {
            state.custom_overrides = Some(CustomOverrides::seed_from(&seed));
        }
        state.preset_selection = PresetSelection::Custom;
    } else {
        state.preset_selection = PresetSelection::Named(next as usize);
        // Leaving Custom → clear the override layer so the named preset
        // displays exactly as it lives on disk.
        state.custom_overrides = None;
    }
    // Focus stays on the Preset row; row visibility may have changed
    // (e.g. Boss preset reveals Prompt row).
    // Re-anchor if the previously focused row vanished.
    let rows = state.visible_rows();
    if !rows.contains(&state.focused_row) {
        state.focused_row = ConfigureRow::Preset;
    }
}

fn ensure_overrides_seed(state: &mut ConfigureState) -> &mut CustomOverrides {
    if state.custom_overrides.is_none() {
        let seed = state.current_preset.clone();
        state.custom_overrides = Some(CustomOverrides::seed_from(&seed));
    }
    state.custom_overrides.as_mut().expect("just seeded")
}

const AGENTS: &[&str] = &["claude", "codex", "copilot", "shell", "ssh"];

/// Agent providers shown in the Agent row but greyed-out / non-selectable:
/// kept OUT of the `AGENTS` cycle ring AND refused at launch. Single source of
/// truth so the greyed pill and the launch guard never disagree.
const DISABLED_AGENTS: &[&str] = &["gemini"];

/// Map an `agent_provider` id to its Agent-row display label.
fn agent_label(provider: &str) -> &str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "gemini" => "Gemini",
        "copilot" => "Copilot",
        "shell" => "Shell",
        "ssh" => "SSH",
        other => other,
    }
}

/// Cycle agent for Custom selection: rotates through claude → codex → copilot → shell → ssh.
/// Gemini is intentionally excluded — it renders greyed-out (non-selectable) in the Agent row.
fn cycle_agent(state: &mut ConfigureState, delta: i32) {
    let prev_provider = {
        let overrides = ensure_overrides_seed(state);
        let cur = AGENTS.iter().position(|a| *a == overrides.agent_provider).unwrap_or(0);
        let len = AGENTS.len() as i32;
        let next = ((cur as i32) + delta).rem_euclid(len) as usize;
        let prev = overrides.agent_provider.clone();
        overrides.agent_provider = AGENTS[next].to_string();
        prev
    };
    // Crossing the Claude/Codex boundary directly: reset the model field to
    // `"default"` so a Claude-flavoured id doesn't linger on a Codex agent (or
    // vice versa). Non-adjacent paths (e.g. codex → copilot → … → claude) skip
    // this, but `ClaudeModel::parse` / `CodexModel::parse` map any stale/unknown
    // id to SystemDefault and omit `--model`, so it stays safe either way.
    {
        let overrides = state.custom_overrides.as_mut().expect("just seeded");
        let crossed = matches!(
            (prev_provider.as_str(), overrides.agent_provider.as_str()),
            ("claude", "codex") | ("codex", "claude")
        );
        if crossed {
            overrides.agent_model = "default".to_string();
        }
    }
    // Swapping agents may strand focus on a Model row that's no longer visible
    // (Model exists only for Claude / Codex). Re-anchor.
    let rows = state.visible_rows();
    if !rows.contains(&state.focused_row) {
        state.focused_row = ConfigureRow::Agent;
    }
}

/// Cycle the Model row's value. Provider-aware: walks the `ClaudeModel::all()`
/// ring for Claude, the `CodexModel::all()` ring for Codex. The cycled-to
/// variant's full canonical id (or `"default"` for `SystemDefault`) is written
/// back into `overrides.agent_model` so TOML serialization stays the same
/// String shape it always was.
fn cycle_model(state: &mut ConfigureState, delta: i32) {
    use crate::models::{ClaudeModel, CodexModel};
    let provider = state.effective_preset().agent_provider.clone();
    let overrides = ensure_overrides_seed(state);
    overrides.agent_model = match provider.as_str() {
        "claude" => {
            let ring = ClaudeModel::all();
            let current = ClaudeModel::parse(&overrides.agent_model);
            let cur_idx = ring.iter().position(|m| *m == current).unwrap_or(0);
            let len = ring.len() as i32;
            let next = ((cur_idx as i32) + delta).rem_euclid(len) as usize;
            // SystemDefault → "default"; real variants → canonical CLI id.
            ring[next].cli_value().unwrap_or("default").to_string()
        }
        "codex" => {
            let ring = CodexModel::all();
            let current = CodexModel::parse(&overrides.agent_model);
            let cur_idx = ring.iter().position(|m| *m == current).unwrap_or(0);
            let len = ring.len() as i32;
            let next = ((cur_idx as i32) + delta).rem_euclid(len) as usize;
            ring[next].cli_value().unwrap_or("default").to_string()
        }
        // Shell / SSH never reach this code path (Model row hidden), but
        // belt-and-braces — leave the field unchanged.
        _ => overrides.agent_model.clone(),
    };
}

fn cycle_mode(state: &mut ConfigureState) {
    // ponytail: Boss/container mode is hidden for now, so cycling pins the
    // mode to Interactive. Restore the Boss<->Interactive toggle (and the
    // Prompt-row reveal it drove) when the container path is wired up again.
    let overrides = ensure_overrides_seed(state);
    overrides.mode = SessionMode::Interactive;
}

fn cycle_yolo(state: &mut ConfigureState) {
    let overrides = ensure_overrides_seed(state);
    overrides.skip_all = !overrides.skip_all;
}

/// Inline branch-edit key handler.
fn handle_branch_edit_key(state: &mut ConfigureState, key: KeyEvent) -> ConfigureOutcome {
    let buf = state.branch_edit.as_mut().expect("guard checked");
    match key.code {
        KeyCode::Esc => {
            state.branch_edit = None;
            ConfigureOutcome::Stay
        }
        KeyCode::Enter => {
            let new_branch = buf.trim().to_string();
            state.branch_edit = None;
            if !new_branch.is_empty() && new_branch != state.branch_worktree {
                state.branch_override = Some(new_branch);
            } else if new_branch.is_empty() {
                state.branch_override = None;
            }
            ConfigureOutcome::Stay
        }
        KeyCode::Backspace => {
            buf.pop();
            ConfigureOutcome::Stay
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            buf.push(c);
            ConfigureOutcome::Stay
        }
        _ => ConfigureOutcome::Stay,
    }
}

/// Base-branch popup key handler. Chars/Backspace edit the fuzzy filter,
/// ↑/↓ move the selection, Tab toggles the action mode (base-off ⇄ checkout),
/// Enter commits the pick, Esc closes without changes.
///
/// `c` from the interview mock was dropped as the checkout shortcut — plain
/// chars feed the filter, so a bare-letter action key would corrupt typing.
/// Tab-toggle + Enter keeps per-branch action choice without the conflict.
fn handle_branch_picker_key(state: &mut ConfigureState, key: KeyEvent) -> ConfigureOutcome {
    let picker = state.branch_picker.as_mut().expect("guard checked");
    match key.code {
        KeyCode::Esc => {
            state.branch_picker = None;
            ConfigureOutcome::Stay
        }
        KeyCode::Tab | KeyCode::BackTab => {
            picker.mode = match picker.mode {
                BaseMode::BaseOff => BaseMode::Checkout,
                BaseMode::Checkout => BaseMode::BaseOff,
            };
            picker.error = None;
            ConfigureOutcome::Stay
        }
        KeyCode::Up => {
            picker.selected = picker.selected.saturating_sub(1);
            ConfigureOutcome::Stay
        }
        KeyCode::Down => {
            let len = picker.filtered_indices().len();
            if len > 0 && picker.selected + 1 < len {
                picker.selected += 1;
            }
            ConfigureOutcome::Stay
        }
        KeyCode::Backspace => {
            picker.filter.pop();
            picker.clamp_selection();
            picker.error = None;
            ConfigureOutcome::Stay
        }
        KeyCode::Enter => {
            let Some(picked) = picker.selected_entry().cloned() else {
                return ConfigureOutcome::Stay;
            };
            let mode = picker.mode;
            // Checkout of an in-use branch is a hard `git worktree add`
            // failure — block here with the inline error (interview pick:
            // mark + block, never silently degrade to base-off).
            if mode == BaseMode::Checkout && picked.in_use {
                picker.error = Some(
                    "checked out by a live session — base a new branch off it instead".to_string(),
                );
                return ConfigureOutcome::Stay;
            }
            state.base_selection = Some(BaseSelection {
                display: picked.entry.display.clone(),
                short_name: picked.entry.short_name.clone(),
                is_remote: picked.entry.is_remote,
                mode,
            });
            state.branch_source = picked.entry.display;
            if state.is_checkout() {
                // No generated name in checkout mode — drop the stale edit
                // buffer and pin the segment back on Source.
                state.branch_edit = None;
                state.branch_segment = BranchSegment::Source;
            }
            state.branch_picker = None;
            ConfigureOutcome::Stay
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            picker.filter.push(c);
            picker.clamp_selection();
            picker.error = None;
            ConfigureOutcome::Stay
        }
        _ => ConfigureOutcome::Stay,
    }
}

/// Save-preset modal key handler. Backspace removes from the name buffer;
/// Enter calls `PresetManager::save_preset` and closes the modal; Esc cancels.
fn handle_modal_key(state: &mut ConfigureState, key: KeyEvent) -> ConfigureOutcome {
    let buf = state.save_preset_modal.as_mut().expect("modal guard checked");
    match key.code {
        KeyCode::Esc => {
            state.save_preset_modal = None;
            ConfigureOutcome::Stay
        }
        KeyCode::Enter => {
            let new_name = buf.trim().to_string();
            if new_name.is_empty() {
                state.save_preset_modal = None;
                return ConfigureOutcome::Stay;
            }
            let mut to_save = state.effective_preset();
            to_save.name = new_name.clone();
            if let Ok(mut manager) = PresetManager::new() {
                if let Err(err) = manager.save_preset(&to_save) {
                    tracing::warn!(error = %err, "save_preset failed");
                }
            }
            state.save_preset_modal = None;
            // Refresh the preset list and select the new entry.
            if !state.available_presets.iter().any(|n| n == &new_name) {
                state.available_presets.push(new_name.clone());
                state.available_presets.sort();
            }
            if let Some(idx) = state.available_presets.iter().position(|n| n == &new_name) {
                state.preset_selection = PresetSelection::Named(idx);
            }
            // Invalidate and reload the in-memory cache so subsequent Tab
            // cycles see the newly saved preset.
            state.presets_cache.insert(new_name.clone(), to_save.clone());
            // New preset becomes the baseline — clear overrides so the
            // `• modified` badge disappears.
            state.current_preset = to_save;
            state.custom_overrides = None;
            ConfigureOutcome::Stay
        }
        KeyCode::Backspace => {
            buf.pop();
            ConfigureOutcome::Stay
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            buf.push(c);
            ConfigureOutcome::Stay
        }
        _ => ConfigureOutcome::Stay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn mk_state() -> ConfigureState {
        let presets_cache: HashMap<String, RepositoryPreset> = ["a", "b", "c"]
            .into_iter()
            .map(|n| {
                (
                    n.to_string(),
                    RepositoryPreset {
                        name: n.to_string(),
                        ..Default::default()
                    },
                )
            })
            .collect();
        ConfigureState {
            repo_source: RepoSource::LocalPath(PathBuf::from("/tmp/repo")),
            repo_label: "repo".into(),
            available_presets: vec!["a".into(), "b".into(), "c".into()],
            focused_row: ConfigureRow::Preset,
            preset_selection: PresetSelection::Named(0),
            current_preset: RepositoryPreset {
                name: "a".into(),
                ..Default::default()
            },
            custom_overrides: None,
            branch_source: "main".into(),
            branch_worktree: "agents/auto".into(),
            branch_override: None,
            branch_edit: None,
            prompt: TextEditor::new(),
            save_preset_modal: None,
            presets_cache,
            branch_prefix: "agents/".into(),
            existing_branches: Vec::new(),
            repo_branch_names: Vec::new(),
            branch_segment: BranchSegment::Source,
            base_selection: None,
            branch_picker: None,
            headroom_enabled: false,
            headroom_available: true,
            rtk_enabled: false,
            rtk_available: true,
            repo_check: RepoCheck::NotApplicable,
        }
    }

    #[test]
    fn repo_check_from_branches_folds_verdicts() {
        // Zero branches = empty repo → explicit, actionable failure.
        assert!(matches!(
            RepoCheck::from_branches(Ok(0)),
            RepoCheck::Failed(msg) if msg.contains("empty")
        ));
        assert_eq!(RepoCheck::from_branches(Ok(3)), RepoCheck::Ok);
        // ls-remote error (not found / auth / network) carries through verbatim.
        assert!(matches!(
            RepoCheck::from_branches(Err("Repository not found: x".into())),
            RepoCheck::Failed(msg) if msg == "Repository not found: x"
        ));
    }

    #[test]
    fn launch_blocked_while_repo_check_pending_or_failed() {
        let mut s = mk_state();
        s.repo_check = RepoCheck::Checking;
        assert!(matches!(launch_outcome(&mut s), ConfigureOutcome::Stay));
        s.repo_check = RepoCheck::Failed("Repository not found".into());
        assert!(matches!(launch_outcome(&mut s), ConfigureOutcome::Stay));
        // Verdict lands → same keypress launches.
        s.repo_check = RepoCheck::Ok;
        assert!(matches!(
            launch_outcome(&mut s),
            ConfigureOutcome::Launch(_)
        ));
    }

    #[test]
    fn remote_source_starts_in_checking_local_not_applicable() {
        use crate::config::session_defaults::SessionDefaults;
        let defaults = SessionDefaults::default();
        let remote = ConfigureState::from_pick_repo(
            RepoSource::GithubShorthand {
                owner: "o".into(),
                repo: "r".into(),
            },
            "r".into(),
            &defaults,
            None,
            "agents/",
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(remote.repo_check, RepoCheck::Checking);
        let local = ConfigureState::from_pick_repo(
            RepoSource::LocalPath(PathBuf::from("/tmp/repo")),
            "repo".into(),
            &defaults,
            None,
            "agents/",
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(local.repo_check, RepoCheck::NotApplicable);
    }

    #[test]
    fn agent_pills_gemini_greyed_copilot_selectable() {
        // The Agent row shows Gemini greyed-out (non-selectable, `[soon]` tag)
        // and Copilot as a real, selectable pill. Current pill stays green/bold.
        let options: Vec<String> = ["Claude", "Codex", "Gemini", "Copilot", "Shell", "SSH"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let line = build_pills_line("Agent:   ", &options, "Claude", false, &["Gemini"], 200);

        let find = |needle: &str| {
            line.spans.iter().find(|s| s.content.as_ref() == needle).unwrap_or_else(|| {
                panic!(
                    "no span with content {needle:?}; spans: {:?}",
                    line.spans.iter().map(|s| s.content.as_ref()).collect::<Vec<_>>()
                )
            })
        };

        // Current pill (Claude): green + bold, bracketed.
        let claude = find("Claude");
        assert_eq!(
            claude.style.fg,
            Some(SELECTION_GREEN),
            "current pill must be green"
        );
        assert!(
            claude.style.add_modifier.contains(Modifier::BOLD),
            "current pill must be bold"
        );
        assert!(
            line.spans
                .iter()
                .any(|s| s.content.as_ref() == "[" && s.style.fg == Some(SELECTION_GREEN)),
            "current pill must be bracketed in green"
        );

        // Gemini: greyed-out (muted + italic) with a ` [soon]` tag, never green/bold.
        let gemini = find("Gemini");
        assert_eq!(
            gemini.style.fg,
            Some(MUTED_GRAY),
            "Gemini must be muted grey"
        );
        assert!(
            gemini.style.add_modifier.contains(Modifier::ITALIC),
            "Gemini must be italic (disabled)"
        );
        assert!(
            !gemini.style.add_modifier.contains(Modifier::BOLD),
            "Gemini must not be bold"
        );
        let soon = find(" [soon]");
        assert_eq!(soon.style.fg, Some(MUTED_GRAY));
        assert!(soon.style.add_modifier.contains(Modifier::ITALIC));
        assert!(
            !line
                .spans
                .iter()
                .any(|s| s.content.as_ref() == " [soon]" && s.style.fg == Some(SELECTION_GREEN)),
            "the [soon] tag must never render as the green current pill"
        );

        // Copilot: a real, selectable (not current) pill — plain muted, no italic, no tag.
        let copilot = find("Copilot");
        assert_eq!(
            copilot.style.fg,
            Some(MUTED_GRAY),
            "Copilot must be muted grey"
        );
        assert!(
            !copilot.style.add_modifier.contains(Modifier::ITALIC),
            "Copilot must not be italic (it is selectable, not disabled)"
        );
        assert!(!copilot.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn agent_cycle_ring_excludes_gemini_includes_copilot() {
        // Copilot is selectable (in the cycle ring); Gemini is not.
        assert!(
            AGENTS.contains(&"copilot"),
            "copilot must be a selectable agent"
        );
        assert!(
            !AGENTS.contains(&"gemini"),
            "gemini stays out of the cycle ring (greyed-out)"
        );
    }

    #[test]
    fn render_agent_row_shows_gemini_greyed_and_copilot() {
        use ratatui::{Terminal, backend::TestBackend};
        let state = mk_state();
        let mut terminal = Terminal::new(TestBackend::new(120, 3)).unwrap();
        terminal.draw(|f| render_agent_row(f, &state, f.size(), true)).unwrap();
        let buf = terminal.backend().buffer();
        let rendered: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            rendered.contains("Agent:"),
            "agent row label missing: {rendered:?}"
        );
        assert!(
            rendered.contains("Gemini"),
            "Gemini pill missing: {rendered:?}"
        );
        assert!(
            rendered.contains("[soon]"),
            "Gemini greyed [soon] tag missing: {rendered:?}"
        );
        assert!(
            rendered.contains("Copilot"),
            "Copilot pill missing: {rendered:?}"
        );
    }

    #[test]
    fn agents_picker_gemini_disabled_copilot_available() {
        use crate::app::state::{AgentProvider, ProviderStatus};
        assert_eq!(
            AgentProvider::gemini().status,
            ProviderStatus::Disabled,
            "Gemini must be greyed-out / non-launchable in the Agents picker"
        );
        assert_eq!(
            AgentProvider::copilot().status,
            ProviderStatus::Available,
            "Copilot stays selectable in the Agents picker"
        );
    }

    #[test]
    fn disabled_current_pill_still_shows_selection() {
        // A hand-authored preset could make a disabled agent the current one.
        // The row must still bracket it (muted) so a selection always reads,
        // and must never render it in the green current style.
        let options: Vec<String> = ["Claude", "Gemini"].iter().map(|s| (*s).to_string()).collect();
        let line = build_pills_line("Agent:   ", &options, "Gemini", false, &["Gemini"], 200);
        assert!(
            line.spans.iter().any(|s| s.content.as_ref() == "["),
            "disabled-current must still show a bracket; otherwise nothing reads selected"
        );
        assert!(
            !line.spans.iter().any(|s| s.style.fg == Some(SELECTION_GREEN)),
            "disabled-current must never use the green current style"
        );
        assert!(
            line.spans.iter().any(|s| s.content.as_ref() == " [soon]"),
            "disabled-current must still carry the [soon] tag"
        );
    }

    #[test]
    fn launch_refused_for_disabled_agent_preset() {
        // Gemini is non-selectable in the UI, but a TOML preset could carry it.
        // Launch must be refused and focus moved to the Agent row.
        let mut s = mk_state();
        s.presets_cache.get_mut("a").unwrap().agent_provider = "gemini".into();
        let outcome = launch_outcome(&mut s);
        assert!(
            matches!(outcome, ConfigureOutcome::Stay),
            "a disabled-agent preset must not launch"
        );
        assert_eq!(
            s.focused_row,
            ConfigureRow::Agent,
            "launch refusal should refocus the Agent row"
        );
    }

    #[test]
    fn branch_collision_false_for_random_default() {
        let s = mk_state();
        // Default auto branch, no existing worktrees → no collision.
        assert!(!s.branch_collision());
    }

    #[test]
    fn branch_collision_true_when_override_matches_live_worktree() {
        let mut s = mk_state();
        s.existing_branches = vec!["feat/blog".into(), "agents/abc123".into()];
        s.branch_override = Some("feat/blog".into());
        assert!(
            s.branch_collision(),
            "override matching a live worktree must collide"
        );
    }

    #[test]
    fn branch_collision_false_when_override_is_unique() {
        let mut s = mk_state();
        s.existing_branches = vec!["feat/blog".into()];
        s.branch_override = Some("feat/something-else".into());
        assert!(!s.branch_collision());
    }

    #[test]
    fn branch_problem_exists_when_baseoff_name_already_a_branch() {
        // feat/ota exists as a branch but is NOT in a worktree. Base-off would
        // try to create it anew off main and fail → block at selection
        // (Stevie 2026-06-07).
        let mut s = mk_state();
        s.repo_branch_names = vec!["main".into(), "feat/ota".into()];
        s.branch_override = Some("feat/ota".into());
        assert_eq!(s.branch_problem(), Some(BranchProblem::Exists));
        assert!(
            s.branch_collision(),
            "existing base-off name must block launch"
        );
    }

    #[test]
    fn branch_problem_none_for_existing_name_in_checkout_mode() {
        // In Checkout mode an existing branch is exactly the point — never a
        // problem (the picker separately blocks checking out an in-use branch).
        let mut s = mk_state();
        s.repo_branch_names = vec!["feat/ota".into()];
        s.base_selection = Some(BaseSelection {
            display: "feat/ota".into(),
            short_name: "feat/ota".into(),
            is_remote: false,
            mode: BaseMode::Checkout,
        });
        assert_eq!(s.branch_problem(), None);
        assert!(!s.branch_collision());
    }

    #[test]
    fn branch_problem_inuse_takes_precedence_over_exists() {
        // A name that is both a branch AND in a worktree reports InUse.
        let mut s = mk_state();
        s.existing_branches = vec!["feat/ota".into()];
        s.repo_branch_names = vec!["feat/ota".into()];
        s.branch_override = Some("feat/ota".into());
        assert_eq!(s.branch_problem(), Some(BranchProblem::InUse));
    }

    #[test]
    fn launch_blocked_and_refocuses_branch_on_collision() {
        let mut s = mk_state();
        s.existing_branches = vec!["feat/blog".into()];
        s.branch_override = Some("feat/blog".into());
        s.focused_row = ConfigureRow::Launch;
        let outcome = launch_outcome(&mut s);
        assert!(
            matches!(outcome, ConfigureOutcome::Stay),
            "collision must block launch"
        );
        assert_eq!(
            s.focused_row,
            ConfigureRow::Branch,
            "focus must move to Branch row so the warning is visible"
        );
    }

    #[test]
    fn launch_proceeds_when_no_collision() {
        let mut s = mk_state();
        s.focused_row = ConfigureRow::Launch;
        let outcome = launch_outcome(&mut s);
        assert!(
            matches!(outcome, ConfigureOutcome::Launch(_)),
            "no collision → launch proceeds"
        );
    }

    #[test]
    fn cycling_preset_named_sets_modified_flag() {
        let mut s = mk_state();
        cycle_preset_ring(&mut s, 1);
        assert!(matches!(s.preset_selection, PresetSelection::Named(1)));
        assert!(s.is_modified());
        cycle_preset_ring(&mut s, -1);
        assert!(matches!(s.preset_selection, PresetSelection::Named(0)));
        assert!(!s.is_modified());
    }

    #[test]
    fn preset_ring_includes_custom_at_end() {
        let mut s = mk_state();
        // a -> b -> c -> Custom
        cycle_preset_ring(&mut s, 1);
        cycle_preset_ring(&mut s, 1);
        cycle_preset_ring(&mut s, 1);
        assert_eq!(s.preset_selection, PresetSelection::Custom);
        assert!(s.is_modified());
        // Custom -> a (wraps)
        cycle_preset_ring(&mut s, 1);
        assert!(matches!(s.preset_selection, PresetSelection::Named(0)));
    }

    #[test]
    fn custom_seeds_overrides_from_previous_named() {
        let mut s = mk_state();
        s.current_preset.agent_provider = "claude".into();
        s.current_preset.agent_model = "opus".into();
        s.presets_cache.insert(
            "a".into(),
            RepositoryPreset {
                name: "a".into(),
                agent_provider: "claude".into(),
                agent_model: "opus".into(),
                ..Default::default()
            },
        );
        // Step into Custom.
        cycle_preset_ring(&mut s, -1); // wrap to Custom
        assert_eq!(s.preset_selection, PresetSelection::Custom);
        let o = s.custom_overrides.clone().unwrap();
        assert_eq!(o.agent_provider, "claude");
        assert_eq!(o.agent_model, "opus");
    }

    #[test]
    fn custom_unlocks_mode_yolo_editing() {
        let mut s = mk_state();
        // Switch to Custom via Right-arrow on Preset row (delta=+1 thrice).
        cycle_preset_ring(&mut s, -1); // wrap to Custom
        s.focused_row = ConfigureRow::Mode;
        let before = s.effective_preset().mode;
        cycle_mode(&mut s);
        let after = s.effective_preset().mode;
        assert_ne!(before, after);
    }

    #[test]
    fn locked_mode_no_op_when_named_selected() {
        let mut s = mk_state();
        // Default selection = Named(0), no overrides — cycle_value should
        // no-op on Mode row.
        s.focused_row = ConfigureRow::Mode;
        cycle_value_in_focused_row(&mut s, 1);
        // Still no overrides, still on Named(0).
        assert!(s.custom_overrides.is_none());
        assert!(matches!(s.preset_selection, PresetSelection::Named(0)));
    }

    #[test]
    fn headroom_toggle_gated_when_unavailable() {
        let mut s = mk_state();
        s.headroom_available = false;
        s.focused_row = ConfigureRow::HeadroomProxy;
        // Cycling the row must NOT enable Headroom when the binary is absent.
        cycle_value_in_focused_row(&mut s, 1);
        assert!(
            !s.headroom_enabled,
            "toggle must not flip when headroom unavailable"
        );
        cycle_value_in_focused_row(&mut s, -1);
        assert!(!s.headroom_enabled);

        // And when available it flips normally.
        s.headroom_available = true;
        cycle_value_in_focused_row(&mut s, 1);
        assert!(
            s.headroom_enabled,
            "toggle flips when headroom is available"
        );
    }

    #[test]
    fn rtk_toggle_gated_when_unavailable() {
        let mut s = mk_state();
        s.rtk_available = false;
        s.focused_row = ConfigureRow::Rtk;
        // Cycling the row must NOT enable RTK when the binary is absent.
        cycle_value_in_focused_row(&mut s, 1);
        assert!(!s.rtk_enabled, "toggle must not flip when rtk unavailable");
        cycle_value_in_focused_row(&mut s, -1);
        assert!(!s.rtk_enabled);

        // And when available it flips normally.
        s.rtk_available = true;
        cycle_value_in_focused_row(&mut s, 1);
        assert!(s.rtk_enabled, "toggle flips when rtk is available");
    }

    #[test]
    fn tab_cycles_focus_through_visible_rows() {
        let mut s = mk_state();
        // Named preset, default mode = Boss, Claude/Codex provider → rows =
        // [Preset, Mode, Yolo, HeadroomProxy, Rtk, Branch, Prompt, Launch].
        assert_eq!(s.focused_row, ConfigureRow::Preset);
        s.cycle_focus(1);
        assert_eq!(s.focused_row, ConfigureRow::Mode);
        s.cycle_focus(1);
        assert_eq!(s.focused_row, ConfigureRow::Yolo);
        s.cycle_focus(1);
        assert_eq!(s.focused_row, ConfigureRow::HeadroomProxy);
        s.cycle_focus(1);
        assert_eq!(s.focused_row, ConfigureRow::Rtk);
        s.cycle_focus(1);
        assert_eq!(s.focused_row, ConfigureRow::Branch);
        s.cycle_focus(1);
        assert_eq!(s.focused_row, ConfigureRow::Prompt);
        s.cycle_focus(1);
        assert_eq!(s.focused_row, ConfigureRow::Launch);
        // Wraps.
        s.cycle_focus(1);
        assert_eq!(s.focused_row, ConfigureRow::Preset);
    }

    #[test]
    fn ssh_variant_visible_rows() {
        let mut s = mk_state();
        s.repo_source = RepoSource::SshSession("ssh://x@y".into());
        let rows = s.visible_rows();
        assert_eq!(
            rows,
            vec![
                ConfigureRow::Preset,
                ConfigureRow::Host,
                ConfigureRow::User,
                ConfigureRow::Port,
                ConfigureRow::Key,
                ConfigureRow::Launch,
            ]
        );
    }

    #[test]
    fn interactive_preset_hides_prompt_row() {
        let mut s = mk_state();
        s.current_preset.mode = SessionMode::Interactive;
        s.presets_cache.insert(
            "a".into(),
            RepositoryPreset {
                name: "a".into(),
                mode: SessionMode::Interactive,
                ..Default::default()
            },
        );
        let rows = s.visible_rows();
        assert!(!rows.contains(&ConfigureRow::Prompt));
    }

    #[test]
    fn boss_preset_shows_prompt_row() {
        let s = mk_state();
        let rows = s.visible_rows();
        assert!(rows.contains(&ConfigureRow::Prompt));
    }

    #[test]
    fn parse_ssh_session_extracts_user_host_port() {
        let (u, h, p) = parse_ssh_session("ssh://deploy@prod-1.internal");
        assert_eq!(u, "deploy");
        assert_eq!(h, "prod-1.internal");
        assert_eq!(p, "22");
    }

    // --- 2026-05 refresh: ↑/↓ is row-nav, ←/→ stays value-cycling --------

    #[test]
    fn arrow_up_down_now_moves_row_focus_not_value() {
        // Spec: ↑/↓ are aliases for Shift+Tab / Tab. ←/→ continue to cycle
        // the focused row's VALUE.
        let mut s = mk_state();
        s.preset_selection = PresetSelection::Custom;
        s.custom_overrides = Some(CustomOverrides::seed_from(&s.current_preset));
        // Start on Preset; Down should move to the next visible row.
        assert_eq!(s.focused_row, ConfigureRow::Preset);

        // Simulate the Down key handler. We can't import KeyCode here without
        // a frame, so call cycle_focus(1) which is what the handler delegates
        // to — and trust the handler test to cover the dispatch.
        s.cycle_focus(1);
        // The next row depends on visibility — Custom default seed has agent
        // = claude → Agent row is next.
        assert_ne!(s.focused_row, ConfigureRow::Preset);
    }

    #[test]
    fn model_row_visible_for_codex_too() {
        // Spec: Model row appears for BOTH Claude and Codex when in Custom.
        // Shell / SSH stay hidden.
        let mut s = mk_state();
        s.preset_selection = PresetSelection::Custom;
        let mut overrides = CustomOverrides::seed_from(&s.current_preset);
        overrides.agent_provider = "codex".to_string();
        s.custom_overrides = Some(overrides);

        let rows = s.visible_rows();
        assert!(
            rows.contains(&ConfigureRow::Model),
            "Codex agent must show Model row in Custom mode, got: {rows:?}"
        );
    }

    #[test]
    fn model_row_hidden_for_shell_and_ssh() {
        let mut s = mk_state();
        s.preset_selection = PresetSelection::Custom;
        for prov in ["shell", "ssh"] {
            let mut overrides = CustomOverrides::seed_from(&s.current_preset);
            overrides.agent_provider = prov.to_string();
            s.custom_overrides = Some(overrides);
            let rows = s.visible_rows();
            assert!(
                !rows.contains(&ConfigureRow::Model),
                "{prov} agent must NOT show Model row, got: {rows:?}"
            );
        }
    }

    #[test]
    fn cycle_model_ring_for_claude_uses_new_canonical_ids() {
        // Custom + claude agent. Start at "default" → cycle once forward →
        // canonical Fable id ("claude-fable-5"). The Configure render reads
        // this field through ClaudeModel::parse for label rendering, but the
        // stored string is the canonical id.
        let mut s = mk_state();
        s.preset_selection = PresetSelection::Custom;
        let mut overrides = CustomOverrides::seed_from(&s.current_preset);
        overrides.agent_provider = "claude".to_string();
        overrides.agent_model = "default".to_string();
        s.custom_overrides = Some(overrides);

        s.focused_row = ConfigureRow::Model;
        cycle_value_in_focused_row(&mut s, 1);
        assert_eq!(
            s.custom_overrides.as_ref().unwrap().agent_model,
            "claude-fable-5"
        );
    }

    #[test]
    fn cycle_model_ring_for_codex_uses_gpt_ids() {
        let mut s = mk_state();
        s.preset_selection = PresetSelection::Custom;
        let mut overrides = CustomOverrides::seed_from(&s.current_preset);
        overrides.agent_provider = "codex".to_string();
        overrides.agent_model = "default".to_string();
        s.custom_overrides = Some(overrides);

        s.focused_row = ConfigureRow::Model;
        cycle_value_in_focused_row(&mut s, 1);
        // First step from SystemDefault → Gpt55 → "gpt-5.5".
        assert_eq!(s.custom_overrides.as_ref().unwrap().agent_model, "gpt-5.5");
    }

    // --- 2026-06: base-branch picker ---------------------------------------

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn mk_entries() -> Vec<PickerBranchEntry> {
        let mk = |display: &str, short: &str, remote: bool, default: bool, in_use: bool| {
            PickerBranchEntry {
                entry: BranchEntry {
                    display: display.into(),
                    short_name: short.into(),
                    is_remote: remote,
                    is_default: default,
                },
                in_use,
            }
        };
        vec![
            mk("origin/main", "main", true, true, false),
            mk("origin/feature-x", "feature-x", true, false, false),
            mk("origin/fix/login", "fix/login", true, false, true),
            mk("local-only", "local-only", false, false, false),
        ]
    }

    #[test]
    fn enter_on_source_segment_opens_picker() {
        let mut s = mk_state();
        s.focused_row = ConfigureRow::Branch;
        assert_eq!(s.branch_segment, BranchSegment::Source, "Source is default");
        let outcome = handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(outcome, ConfigureOutcome::OpenBranchPicker);
    }

    #[test]
    fn arrows_toggle_segment_and_enter_edits_worktree_name() {
        let mut s = mk_state();
        s.focused_row = ConfigureRow::Branch;
        handle_key(&mut s, key(KeyCode::Right));
        assert_eq!(s.branch_segment, BranchSegment::Worktree);
        let outcome = handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(outcome, ConfigureOutcome::Stay);
        assert!(
            s.branch_edit.is_some(),
            "Worktree segment Enter = inline edit"
        );
    }

    #[test]
    fn picker_enter_commits_base_off_pick() {
        let mut s = mk_state();
        s.branch_picker = Some(BranchPickerState::new(mk_entries(), false));
        // Move to origin/feature-x and commit.
        handle_key(&mut s, key(KeyCode::Down));
        handle_key(&mut s, key(KeyCode::Enter));
        assert!(s.branch_picker.is_none(), "popup closes on commit");
        let base = s.base_selection.clone().expect("pick recorded");
        assert_eq!(base.display, "origin/feature-x");
        assert_eq!(base.short_name, "feature-x");
        assert_eq!(base.mode, BaseMode::BaseOff);
        assert_eq!(s.branch_source, "origin/feature-x", "row shows the pick");
        // Worktree name still the generated one — base-off keeps it.
        assert_eq!(s.effective_branch(), s.branch_worktree);
    }

    #[test]
    fn picker_tab_toggles_to_checkout_and_picked_branch_becomes_session_branch() {
        let mut s = mk_state();
        s.branch_picker = Some(BranchPickerState::new(mk_entries(), false));
        handle_key(&mut s, key(KeyCode::Down)); // origin/feature-x
        handle_key(&mut s, key(KeyCode::Tab)); // checkout mode
        handle_key(&mut s, key(KeyCode::Enter));
        assert!(s.is_checkout());
        assert_eq!(s.effective_branch(), "feature-x");
        // Launch spec carries the pick and uses the picked branch name.
        s.focused_row = ConfigureRow::Launch;
        let outcome = launch_outcome(&mut s);
        let ConfigureOutcome::Launch(spec) = outcome else {
            panic!("expected launch");
        };
        assert_eq!(spec.branch_worktree, "feature-x");
        assert_eq!(spec.base.unwrap().mode, BaseMode::Checkout);
    }

    #[test]
    fn picker_blocks_checkout_of_in_use_branch() {
        let mut s = mk_state();
        s.branch_picker = Some(BranchPickerState::new(mk_entries(), false));
        handle_key(&mut s, key(KeyCode::Down));
        handle_key(&mut s, key(KeyCode::Down)); // origin/fix/login (in use)
        handle_key(&mut s, key(KeyCode::Tab)); // checkout mode
        handle_key(&mut s, key(KeyCode::Enter));
        let picker = s.branch_picker.as_ref().expect("popup stays open");
        assert!(picker.error.is_some(), "inline error shown");
        assert!(s.base_selection.is_none(), "no pick recorded");
        // Base-off of the same branch is fine — Tab back and commit.
        handle_key(&mut s, key(KeyCode::Tab));
        handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(s.base_selection.unwrap().mode, BaseMode::BaseOff);
    }

    #[test]
    fn picker_filter_narrows_and_esc_closes_without_pick() {
        let mut s = mk_state();
        s.branch_picker = Some(BranchPickerState::new(mk_entries(), false));
        for c in "feat".chars() {
            handle_key(&mut s, key(KeyCode::Char(c)));
        }
        {
            let picker = s.branch_picker.as_ref().unwrap();
            assert_eq!(picker.filtered_indices().len(), 1);
            assert_eq!(
                picker.selected_entry().unwrap().entry.display,
                "origin/feature-x"
            );
        }
        handle_key(&mut s, key(KeyCode::Esc));
        assert!(s.branch_picker.is_none());
        assert!(s.base_selection.is_none());
        assert_eq!(s.branch_source, "main", "Esc leaves the source untouched");
    }

    #[test]
    fn checkout_pick_pins_segment_and_reroutes_enter_to_picker() {
        let mut s = mk_state();
        s.branch_segment = BranchSegment::Worktree;
        s.branch_picker = Some(BranchPickerState::new(mk_entries(), false));
        handle_key(&mut s, key(KeyCode::Tab)); // checkout mode
        handle_key(&mut s, key(KeyCode::Enter)); // pick origin/main
        assert_eq!(s.branch_segment, BranchSegment::Source, "segment pinned");
        // ←/→ must not move the segment off Source in checkout mode.
        s.focused_row = ConfigureRow::Branch;
        handle_key(&mut s, key(KeyCode::Right));
        assert_eq!(s.branch_segment, BranchSegment::Source);
        // Enter re-opens the picker (no generated name to edit).
        let outcome = handle_key(&mut s, key(KeyCode::Enter));
        assert_eq!(outcome, ConfigureOutcome::OpenBranchPicker);
    }

    #[test]
    fn agent_switch_claude_to_codex_resets_model_to_default() {
        // Crossing the Claude↔Codex provider boundary must reset agent_model
        // so a Claude id doesn't linger on a Codex agent (and vice versa).
        let mut s = mk_state();
        s.preset_selection = PresetSelection::Custom;
        let mut overrides = CustomOverrides::seed_from(&s.current_preset);
        overrides.agent_provider = "claude".to_string();
        overrides.agent_model = "claude-opus-4-7".to_string();
        s.custom_overrides = Some(overrides);

        s.focused_row = ConfigureRow::Agent;
        // Forward from claude → codex (AGENTS = [claude, codex, shell, ssh]).
        cycle_agent(&mut s, 1);
        let o = s.custom_overrides.as_ref().unwrap();
        assert_eq!(o.agent_provider, "codex");
        assert_eq!(o.agent_model, "default");
    }
}
