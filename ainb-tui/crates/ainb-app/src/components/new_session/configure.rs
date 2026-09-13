// ABOUTME: Renderer-agnostic half of the `new_session::configure` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::new_session::configure`, which re-exports this module.

use crate::app::keymap::{Chord, Key, Mods};
use crate::config::presets::{PresetManager, RepositoryPreset, SessionMode};
use crate::config::session_defaults::SessionDefaults;
use crate::git::branch_list::BranchEntry;
use crate::git::branch_namer::derive_branch_name;
use crate::git::repo_source::RepoSource;
use crate::text_editor::TextEditor;
use std::collections::HashMap;

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
    pub fn seed_from(preset: &RepositoryPreset) -> Self {
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
    /// Remote exists but has zero branches (fresh GitHub repo, no initial
    /// commit). Blocks Launch, but offers `[i]` to initialize in place —
    /// README + initial commit + push — so the user never has to leave ainb
    /// (Stevie 2026-07-04: full-IDE-in-terminal experience).
    EmptyRemote,
    /// `[i]` accepted — README/commit/push in flight. Blocks Launch.
    Initializing,
    /// Remote is unreachable or missing. Blocks Launch; the message renders
    /// on the form.
    Failed(String),
}

impl RepoCheck {
    /// Fold a `list_remote_branches` result into a check verdict. Pure so the
    /// empty-repo rule is unit-testable without a network.
    #[must_use]
    pub fn from_branches(result: Result<usize, String>) -> Self {
        match result {
            Ok(0) => Self::EmptyRemote,
            Ok(_) => Self::Ok,
            Err(msg) => Self::Failed(msg),
        }
    }

    /// True when Launch must be refused (check failed or still in flight).
    #[must_use]
    pub const fn blocks_launch(&self) -> bool {
        matches!(
            self,
            Self::Checking | Self::EmptyRemote | Self::Initializing | Self::Failed(_)
        )
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
    /// Per-session prefix for generated worktree branch names. This is an
    /// ephemeral override of the configured Workspace default.
    Prefix,
    Branch,
    /// Optional durable label rendered ahead of the Git branch in the session
    /// list. Unlike [`Self::Prefix`], this never changes a branch name.
    SessionPrefix,
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
    /// Inline prefix edit buffer. The committed value applies only to this
    /// launch; it never changes the Workspace default in `config.toml`.
    pub branch_prefix_edit: Option<String>,
    /// Optional human prefix for the session row, persisted after a successful
    /// tmux-backed launch through `SessionLabelStore`.
    pub session_prefix: String,
    /// Inline edit buffer for [`Self::session_prefix`].
    pub session_prefix_edit: Option<String>,
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
        // background ls-remote and flips this to Ok / Failed. Same
        // `is_remote()` predicate as the kick site — if they disagreed, a
        // form could open in Checking with no check ever spawned (Launch
        // bricked behind a permanent spinner).
        let repo_check = if repo_source.is_remote() {
            RepoCheck::Checking
        } else {
            RepoCheck::NotApplicable
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
            branch_prefix_edit: None,
            session_prefix: String::new(),
            session_prefix_edit: None,
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

    /// Apply a per-session branch prefix without mutating the Workspace
    /// default. Keep the generated suffix stable when possible, so editing
    /// `agents/` to `ci/` turns `agents/abcd1234` into `ci/abcd1234`.
    pub fn set_branch_prefix(&mut self, prefix: String) {
        let previous_prefix = std::mem::replace(&mut self.branch_prefix, prefix);
        if self.branch_override.is_some() {
            return;
        }

        let suffix = self
            .branch_worktree
            .strip_prefix(&previous_prefix)
            .unwrap_or(&self.branch_worktree);
        let candidate = format!("{}{}", self.branch_prefix, suffix);
        let collides = self.existing_branches.iter().any(|branch| branch == &candidate)
            || self.repo_branch_names.iter().any(|branch| branch == &candidate);
        self.branch_worktree = if collides {
            let mut unavailable = self.existing_branches.clone();
            unavailable.extend(self.repo_branch_names.iter().cloned());
            derive_branch_name(&self.branch_prefix, &unavailable)
        } else {
            candidate
        };
    }

    /// The list of rows visible for the current variant + preset selection.
    /// Ordering matches the render layout and Tab cycle order.
    pub fn visible_rows(&self) -> Vec<ConfigureRow> {
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
            if preset.agent_provider == "claude"
                || preset.agent_provider == "codex"
                || preset.agent_provider == "antigravity"
            {
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
        // Prefix and Branch rows are visible for everything that isn't SSH.
        // Prefix is per-session only. Global defaults remain editable from
        // Settings → Workspace.
        rows.push(ConfigureRow::Prefix);
        rows.push(ConfigureRow::Branch);
        rows.push(ConfigureRow::SessionPrefix);
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
    pub fn cycle_focus(&mut self, delta: i32) {
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
        if self.focused_row != ConfigureRow::Prefix {
            self.branch_prefix_edit = None;
        }
        if self.focused_row != ConfigureRow::SessionPrefix {
            self.session_prefix_edit = None;
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
    /// `[i]` on an `EmptyRemote` verdict — the app layer must commit a README
    /// to the clone cache and push it, then flip `repo_check` to Ok (git
    /// stays out of components/).
    InitializeRemote,
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
    /// Optional durable label displayed before the branch in the sidebar.
    pub session_prefix: String,
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
pub fn handle_key(state: &mut ConfigureState, key: &Chord) -> ConfigureOutcome {
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

    // Prefix editing has the same priority. Keeping this separate from the
    // branch editor makes the Prefix row a true per-session control instead
    // of overloading the branch-name textarea.
    if state.branch_prefix_edit.is_some() {
        return handle_branch_prefix_edit_key(state, key);
    }
    if state.session_prefix_edit.is_some() {
        return handle_session_prefix_edit_key(state, key);
    }

    // Ctrl shortcuts.
    if key.modifiers().contains(Mods::CTRL) {
        match key.code() {
            Key::Char('s' | 'S') => {
                state.save_preset_modal = Some(String::new());
                return ConfigureOutcome::Stay;
            }
            Key::Char('p' | 'P') => {
                tracing::warn!("configure: ^P preset manager — stub until Phase 7 polish");
                return ConfigureOutcome::OpenPresetManager;
            }
            // Ctrl+Enter from anywhere = Launch. Many terminals don't deliver
            // Ctrl+Enter distinctly (they collapse to Enter), but where they
            // do we honour it as the prompt-textarea escape hatch.
            Key::Enter => return launch_outcome(state),
            _ => {}
        }
    }

    match key.code() {
        Key::Esc => ConfigureOutcome::BackToPickRepo,
        // Shift+Tab: a terminal's BackTab arrives as Tab with Shift held.
        Key::Tab if key.modifiers().contains(Mods::SHIFT) => {
            state.cycle_focus(-1);
            ConfigureOutcome::Stay
        }
        Key::Tab => {
            state.cycle_focus(1);
            ConfigureOutcome::Stay
        }
        Key::Enter => match state.focused_row {
            ConfigureRow::Prefix => {
                state.branch_prefix_edit = Some(state.branch_prefix.clone());
                ConfigureOutcome::Stay
            }
            ConfigureRow::SessionPrefix => {
                state.session_prefix_edit = Some(state.session_prefix.clone());
                ConfigureOutcome::Stay
            }
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
        Key::Left => {
            cycle_value_in_focused_row(state, -1);
            ConfigureOutcome::Stay
        }
        Key::Right => {
            cycle_value_in_focused_row(state, 1);
            ConfigureOutcome::Stay
        }
        // ↑/↓ are row navigation (alias for Shift+Tab / Tab respectively).
        // EXCEPT inside the Prompt textarea — there they're absorbed by the
        // textarea so vertical cursor movement still works. The textarea
        // doesn't itself implement arrow-key cursor moves today, but
        // forwarding the keystroke leaves room for that without retraining
        // muscle memory later.
        Key::Up => {
            if state.focused_row == ConfigureRow::Prompt {
                // No-op for now (TextEditor has no vertical move API yet);
                // intentionally NOT row-nav so Stevie's "↑/↓ behave normally
                // inside the textarea" rule holds.
                return ConfigureOutcome::Stay;
            }
            state.cycle_focus(-1);
            ConfigureOutcome::Stay
        }
        Key::Down => {
            if state.focused_row == ConfigureRow::Prompt {
                return ConfigureOutcome::Stay;
            }
            state.cycle_focus(1);
            ConfigureOutcome::Stay
        }
        Key::Backspace => {
            if state.focused_row == ConfigureRow::Prompt {
                state.prompt.backspace();
            }
            ConfigureOutcome::Stay
        }
        Key::Char(c) if !key.modifiers().contains(Mods::CTRL) => {
            if state.focused_row == ConfigureRow::Prompt {
                state.prompt.insert_char(c);
                return ConfigureOutcome::Stay;
            }
            // `[i]` initializes an empty remote (README + commit + push) so
            // the user never leaves ainb. Only offered while the pre-flight
            // verdict is EmptyRemote; the Prompt row keeps plain chars.
            if matches!(c, 'i' | 'I') && state.repo_check == RepoCheck::EmptyRemote {
                state.repo_check = RepoCheck::Initializing;
                return ConfigureOutcome::InitializeRemote;
            }
            // No other bare-char shortcuts — Tab + arrows is the entire
            // navigation surface for non-Prompt rows.
            ConfigureOutcome::Stay
        }
        _ => ConfigureOutcome::Stay,
    }
}

/// Build a `Launch` outcome from the current state.
pub fn launch_outcome(state: &mut ConfigureState) -> ConfigureOutcome {
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
        session_prefix: state.session_prefix.trim().to_string(),
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
pub fn cycle_value_in_focused_row(state: &mut ConfigureState, delta: i32) {
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
        ConfigureRow::Prefix => {
            // Prefix is an editable text row, not a value ring.
        }
        ConfigureRow::SessionPrefix => {
            // Session prefix is an editable text row, not a value ring.
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
pub fn cycle_preset_ring(state: &mut ConfigureState, delta: i32) {
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

pub const AGENTS: &[&str] = &["claude", "codex", "antigravity", "copilot", "shell", "ssh"];

/// Agent providers shown in the Agent row but greyed-out / non-selectable:
/// kept OUT of the `AGENTS` cycle ring AND refused at launch. Single source of
/// truth so the greyed pill and the launch guard never disagree.
pub const DISABLED_AGENTS: &[&str] = &["gemini"];

/// Cycle agent for Custom selection: rotates through claude -> codex -> antigravity -> copilot -> shell -> ssh.
/// Gemini is intentionally excluded: it renders greyed-out (non-selectable) in the Agent row.
pub fn cycle_agent(state: &mut ConfigureState, delta: i32) {
    let prev_provider = {
        let overrides = ensure_overrides_seed(state);
        let cur = AGENTS.iter().position(|a| *a == overrides.agent_provider).unwrap_or(0);
        let len = AGENTS.len() as i32;
        let next = ((cur as i32) + delta).rem_euclid(len) as usize;
        let prev = overrides.agent_provider.clone();
        overrides.agent_provider = AGENTS[next].to_string();
        prev
    };
    // Crossing the model-supporting provider boundary directly: reset the model field to
    // `"default"` so a Claude/Codex/Antigravity-flavoured id doesn't linger on another agent.
    // Non-adjacent paths skip this, but model parsers map any stale/unknown id to SystemDefault
    // and omit `--model`, so it stays safe either way.
    {
        let overrides = state.custom_overrides.as_mut().expect("just seeded");
        let crossed = matches!(
            (prev_provider.as_str(), overrides.agent_provider.as_str()),
            ("claude", "codex")
                | ("codex", "claude")
                | ("claude", "antigravity")
                | ("antigravity", "claude")
                | ("codex", "antigravity")
                | ("antigravity", "codex")
        );
        if crossed {
            overrides.agent_model = "default".to_string();
        }
    }
    // Swapping agents may strand focus on a Model row that's no longer visible
    // (Model exists only for Claude / Codex / Antigravity). Re-anchor.
    let rows = state.visible_rows();
    if !rows.contains(&state.focused_row) {
        state.focused_row = ConfigureRow::Agent;
    }
}

/// Cycle the Model row's value. Provider-aware: walks the `ClaudeModel::all()`
/// ring for Claude, `CodexModel::all()` for Codex, `AntigravityModel::all()` for Antigravity.
/// The cycled-to variant's full canonical id (or `"default"` for `SystemDefault`) is written
/// back into `overrides.agent_model` so TOML serialization stays the same
/// String shape it always was.
pub fn cycle_model(state: &mut ConfigureState, delta: i32) {
    use crate::models::{AntigravityModel, ClaudeModel, CodexModel};
    let provider = state.effective_preset().agent_provider.clone();
    let overrides = ensure_overrides_seed(state);
    overrides.agent_model = match provider.as_str() {
        "claude" => {
            let ring = ClaudeModel::all();
            let current = ClaudeModel::parse(&overrides.agent_model);
            let cur_idx = ring.iter().position(|m| *m == current).unwrap_or(0);
            let len = ring.len() as i32;
            let next = ((cur_idx as i32) + delta).rem_euclid(len) as usize;
            // SystemDefault -> "default"; real variants -> canonical CLI id.
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
        "antigravity" => {
            let ring = AntigravityModel::all();
            let current = AntigravityModel::parse(&overrides.agent_model);
            let cur_idx = ring.iter().position(|m| *m == current).unwrap_or(0);
            let len = ring.len() as i32;
            let next = ((cur_idx as i32) + delta).rem_euclid(len) as usize;
            ring[next].cli_value().unwrap_or("default").to_string()
        }
        // Shell / SSH never reach this code path (Model row hidden), but
        // belt-and-braces: leave the field unchanged.
        _ => overrides.agent_model.clone(),
    };
}

pub fn cycle_mode(state: &mut ConfigureState) {
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
fn handle_branch_edit_key(state: &mut ConfigureState, key: &Chord) -> ConfigureOutcome {
    let buf = state.branch_edit.as_mut().expect("guard checked");
    match key.code() {
        Key::Esc => {
            state.branch_edit = None;
            ConfigureOutcome::Stay
        }
        Key::Enter => {
            let new_branch = buf.trim().to_string();
            state.branch_edit = None;
            if !new_branch.is_empty() && new_branch != state.branch_worktree {
                state.branch_override = Some(new_branch);
            } else if new_branch.is_empty() {
                state.branch_override = None;
            }
            ConfigureOutcome::Stay
        }
        Key::Backspace => {
            buf.pop();
            ConfigureOutcome::Stay
        }
        Key::Char(c) if !key.modifiers().contains(Mods::CTRL) => {
            buf.push(c);
            ConfigureOutcome::Stay
        }
        _ => ConfigureOutcome::Stay,
    }
}

/// Inline prefix-edit key handler. An empty prefix is valid and creates a
/// generated branch with no leading namespace.
fn handle_branch_prefix_edit_key(state: &mut ConfigureState, key: &Chord) -> ConfigureOutcome {
    let buffer = state.branch_prefix_edit.as_mut().expect("guard checked");
    match key.code() {
        Key::Esc => {
            state.branch_prefix_edit = None;
            ConfigureOutcome::Stay
        }
        Key::Enter => {
            let prefix = buffer.trim().to_string();
            state.branch_prefix_edit = None;
            state.set_branch_prefix(prefix);
            ConfigureOutcome::Stay
        }
        Key::Backspace => {
            buffer.pop();
            ConfigureOutcome::Stay
        }
        Key::Char(c) if !key.modifiers().contains(Mods::CTRL) => {
            buffer.push(c);
            ConfigureOutcome::Stay
        }
        _ => ConfigureOutcome::Stay,
    }
}

/// Inline edit for the durable session prefix. Empty deliberately clears it.
fn handle_session_prefix_edit_key(state: &mut ConfigureState, key: &Chord) -> ConfigureOutcome {
    let buffer = state.session_prefix_edit.as_mut().expect("guard checked");
    match key.code() {
        Key::Esc => {
            state.session_prefix_edit = None;
            ConfigureOutcome::Stay
        }
        Key::Enter => {
            state.session_prefix = buffer.trim().to_string();
            state.session_prefix_edit = None;
            ConfigureOutcome::Stay
        }
        Key::Backspace => {
            buffer.pop();
            ConfigureOutcome::Stay
        }
        Key::Char(c) if !key.modifiers().contains(Mods::CTRL) => {
            buffer.push(c);
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
fn handle_branch_picker_key(state: &mut ConfigureState, key: &Chord) -> ConfigureOutcome {
    let picker = state.branch_picker.as_mut().expect("guard checked");
    match key.code() {
        Key::Esc => {
            state.branch_picker = None;
            ConfigureOutcome::Stay
        }
        Key::Tab => {
            picker.mode = match picker.mode {
                BaseMode::BaseOff => BaseMode::Checkout,
                BaseMode::Checkout => BaseMode::BaseOff,
            };
            picker.error = None;
            ConfigureOutcome::Stay
        }
        Key::Up => {
            picker.selected = picker.selected.saturating_sub(1);
            ConfigureOutcome::Stay
        }
        Key::Down => {
            let len = picker.filtered_indices().len();
            if len > 0 && picker.selected + 1 < len {
                picker.selected += 1;
            }
            ConfigureOutcome::Stay
        }
        Key::Backspace => {
            picker.filter.pop();
            picker.clamp_selection();
            picker.error = None;
            ConfigureOutcome::Stay
        }
        Key::Enter => {
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
        Key::Char(c) if !key.modifiers().contains(Mods::CTRL) => {
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
fn handle_modal_key(state: &mut ConfigureState, key: &Chord) -> ConfigureOutcome {
    let buf = state.save_preset_modal.as_mut().expect("modal guard checked");
    match key.code() {
        Key::Esc => {
            state.save_preset_modal = None;
            ConfigureOutcome::Stay
        }
        Key::Enter => {
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
        Key::Backspace => {
            buf.pop();
            ConfigureOutcome::Stay
        }
        Key::Char(c) if !key.modifiers().contains(Mods::CTRL) => {
            buf.push(c);
            ConfigureOutcome::Stay
        }
        _ => ConfigureOutcome::Stay,
    }
}
