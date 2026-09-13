// ABOUTME: Renderer-agnostic half of the `new_session::configure` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::new_session::configure`, which re-exports this module.

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
