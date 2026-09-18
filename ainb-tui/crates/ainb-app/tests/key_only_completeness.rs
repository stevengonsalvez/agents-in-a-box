//! Completeness fence for key-only commands (#1080).
//!
//! A command row whose action writes outside ainb must run only from its key:
//! `resolve_intent` refuses it by name, so no other surface (a desktop webview,
//! a mirror, a web client) can fire it. The flag is derived from the action
//! (`KeyAction::writes_outside_ainb`), and a few actions are judged by state
//! instead (`AppState::remote_command_refusal`). This test derives the same
//! set from the reducer source and fails when the two disagree.
//!
//! How the walk works, the same line-fence idea as `host_side_effects.rs`:
//! every keymap row is followed from its reducer arm (`process_event`,
//! `keymap_ui_event`, and the `AsyncAction` arms it queues) through precise
//! call edges (`Self::f(`, `state.f(`/`self.f(` on `AppState`, bare calls to
//! a function in the same file or brought in by a `use`, `crate::a::b::f(`,
//! and `x.f(` when only one type in the crate has a method `f`). Every function or arm it reaches that spawns a process,
//! writes a file or calls a known external writer is a sink, and every sink is
//! classified in [`SINKS`]: outside ainb, owned by ainb, or read-only.
//!
//! What the walk cannot see (a ratchet, not a proof):
//! - calls through closures passed elsewhere, trait objects or macros;
//! - what other `ainb_*` crates do internally, beyond [`EXTERNAL_WRITERS`];
//! - effects a command only arms, taking hold when a later command arrives;
//! - what the host's main loop does with an `AsyncAction` handed back to it,
//!   beyond the `host:` entries in [`SINKS`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ainb_app::AppState;
use ainb_app::app::keymap::{KeyAction, Keymap};
use ainb_app::app::state::ConfirmAction;

/// Where a sink writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Something ainb did not create: a third-party tool's config, the user's
    /// repository or shell, a tmux session ainb did not start, a package.
    Outside,
    /// ainb's own state: `~/.agents-in-a-box`, the sessions, worktrees, tmux
    /// sessions and daemons it started, the skill library it manages.
    Owned,
    /// A process spawned only to read (a probe) with nothing written.
    ReadOnly,
}

/// Every sink the walk reaches, with where it writes and why. A reached sink
/// missing here fails, and so does an entry the walk no longer reaches.
///
/// Keys: `file::function` for a function under `src/`, `arm:Enum::Variant`
/// for a sink spelled in a reducer arm itself, `effect:…` for a host effect.
const SINKS: &[(&str, Reach, &str)] = &[
    // Outside ainb.
    (
        "app/events.rs::run_install_command",
        Reach::Outside,
        "runs a catalog install recipe with sh -c",
    ),
    (
        "arm:AppEvent::ConfirmationConfirm",
        Reach::Outside,
        "installs the Claude Code and Codex hooks (notifyd install_for)",
    ),
    (
        "effect:ToolTerminal::AbtopWithSetup",
        Reach::Outside,
        "runs abtop --setup, which edits Claude Code's statusline hook",
    ),
    (
        "app/state.rs::confirm_other_tmux_rename",
        Reach::Outside,
        "renames a tmux session ainb did not start",
    ),
    (
        "cli/statusline_install.rs::install_statusline_at",
        Reach::Outside,
        "writes ~/.claude/settings.json",
    ),
    (
        "cli/statusline_install.rs::atomic_write_json",
        Reach::Outside,
        "writes ~/.claude/settings.json",
    ),
    (
        "cli/statusline_install.rs::write_backup_from_bytes",
        Reach::Outside,
        "writes a settings.json backup beside it",
    ),
    (
        "cli/statusline_install.rs::prune_old_backups",
        Reach::Outside,
        "removes old settings.json backups",
    ),
    (
        "git/operations.rs::commit_and_push_cli",
        Reach::Outside,
        "git commit and git push in the user's repository",
    ),
    (
        "otel/mod.rs::ensure_settings_env",
        Reach::Outside,
        "writes Claude Code's settings env",
    ),
    (
        "otel/mod.rs::ensure_settings_env_at",
        Reach::Outside,
        "writes Claude Code's settings env",
    ),
    (
        "otel/mod.rs::ensure_shell_rc_at",
        Reach::Outside,
        "appends to the user's shell rc",
    ),
    (
        "otel/mod.rs::start_alloy",
        Reach::Outside,
        "starts the Grafana Alloy collector",
    ),
    (
        "setup/provision.rs::install_dep_capture",
        Reach::Outside,
        "installs a system dependency",
    ),
    (
        "setup/provision.rs::install_tmux_config",
        Reach::Outside,
        "writes ~/.tmux.conf",
    ),
    (
        "host:AsyncAction::KillOtherTmux",
        Reach::Outside,
        "the host kills a tmux session ainb did not start",
    ),
    (
        "host:AsyncAction::KillOtherTmuxSessions",
        Reach::Outside,
        "the host kills tmux sessions ainb did not start",
    ),
    (
        "app/events.rs::run_skill_cli",
        Reach::Outside,
        "ainb_cli skill commands write the tools' own skill dirs (~/.claude, ~/.codex, ~/.cursor)",
    ),
    (
        "app/events.rs::run_skill_cli_full",
        Reach::Outside,
        "ainb_cli skill commands write the tools' own skill dirs (~/.claude, ~/.codex, ~/.cursor)",
    ),
    // ainb's own state.
    (
        "components/log_history_viewer.rs::delete_all_logs",
        Reach::Owned,
        "deletes ainb's own log files",
    ),
    (
        "components/session_recovery.rs::archive_session_by_name",
        Reach::Owned,
        "archives an agent-session record in ~/.claude/agents, the registry the ainb toolkit hooks keep",
    ),
    (
        "components/session_recovery.rs::cleanup_single_worktree",
        Reach::Owned,
        "removes an orphaned worktree under ~/.agents-in-a-box/worktrees",
    ),
    (
        "components/session_recovery.rs::resume_single_session",
        Reach::Owned,
        "starts a tmux session to resume an orphaned ainb session",
    ),
    (
        "components/session_recovery.rs::resume_single_worktree",
        Reach::Owned,
        "starts a tmux session to resume an orphaned ainb worktree",
    ),
    (
        "git/worktree_manager.rs::remove_worktree",
        Reach::Owned,
        "removes the worktree of a session ainb created",
    ),
    (
        "headroom/mod.rs::ensure_proxy_running_under_process_lock",
        Reach::Owned,
        "starts ainb's headroom proxy under ~/.agents-in-a-box/headroom",
    ),
    (
        "headroom/mod.rs::live_users",
        Reach::Owned,
        "prunes stale pid files under ~/.agents-in-a-box/headroom/users",
    ),
    (
        "interactive/session_manager.rs::start_cli_in_tmux",
        Reach::Owned,
        "starts the agent CLI in the tmux session ainb created for it",
    ),
    (
        "interactive/session_manager.rs::write_atomic_config",
        Reach::Owned,
        "trusts the session's own worktree in Codex's config, part of every ainb launch",
    ),
    (
        "tmux/session.rs::cleanup",
        Reach::Owned,
        "kills the tmux session ainb created, by exact name",
    ),
    (
        "host:AsyncAction::KillWorkspaceShell",
        Reach::Owned,
        "the host kills a workspace shell ainb started",
    ),
    (
        "app/state.rs::cleanup_orphaned_containers",
        Reach::Owned,
        "prunes ainb's own worktrees and containers",
    ),
    (
        "app/state.rs::cleanup_orphaned_tmux_shells",
        Reach::Owned,
        "kills orphaned shells ainb started",
    ),
    (
        "app/state.rs::dismiss_abtop_setup",
        Reach::Owned,
        "a dismissal marker under ~/.agents-in-a-box",
    ),
    (
        "app/state.rs::downgrade_headroom_session",
        Reach::Owned,
        "respawns the pane of a session ainb started",
    ),
    (
        "app/state.rs::handle_reauthenticate",
        Reach::Owned,
        "credentials under ~/.agents-in-a-box/auth",
    ),
    (
        "app/state.rs::refresh_oauth_tokens",
        Reach::Owned,
        "ainb's auth container and its token files",
    ),
    (
        "app/state.rs::resume_interactive_session",
        Reach::Owned,
        "the tmux session of an ainb session",
    ),
    (
        "app/state.rs::run_oauth_setup",
        Reach::Owned,
        "ainb's auth container under ~/.agents-in-a-box/auth",
    ),
    (
        "app/state.rs::save_api_key",
        Reach::Owned,
        "~/.agents-in-a-box/.env",
    ),
    (
        "app/state.rs::stop_interactive_session",
        Reach::Owned,
        "kills the tmux session of an ainb session",
    ),
    (
        "cli/hangar.rs::clear_daemon_version_record",
        Reach::Owned,
        "the hangar daemon ainb runs",
    ),
    (
        "cli/hangar.rs::open_daemon_stderr_log",
        Reach::Owned,
        "the hangar daemon's log under ~/.agents-in-a-box",
    ),
    (
        "cli/hangar.rs::respawn_once",
        Reach::Owned,
        "the hangar daemon ainb runs",
    ),
    (
        "cli/hangar.rs::start_daemon_if_stopped",
        Reach::Owned,
        "the hangar daemon ainb runs",
    ),
    (
        "cli/hangar.rs::stop_daemon",
        Reach::Owned,
        "the hangar daemon ainb runs",
    ),
    (
        "components/skill_manager_screen.rs::apply_discovery_skip",
        Reach::Owned,
        "a skip marker under ainb's home",
    ),
    (
        "components/skill_manager_screen.rs::force_show_discovery_banner",
        Reach::Owned,
        "removes that skip marker",
    ),
    (
        "config/lock.rs::lock_for",
        Reach::Owned,
        "ainb's config lock file",
    ),
    (
        "mcp_pool/client.rs::ensure_daemon",
        Reach::Owned,
        "the MCP pool daemon ainb runs, and its log",
    ),
    (
        "otel/mod.rs::write_assets",
        Reach::Owned,
        "collector config under ~/.agents-in-a-box/otel",
    ),
    (
        "otel/mod.rs::write_env_file",
        Reach::Owned,
        "~/.agents-in-a-box/otel/grafana-cloud.env",
    ),
    (
        "otel/mod.rs::write_env_file_to",
        Reach::Owned,
        "~/.agents-in-a-box/otel/grafana-cloud.env",
    ),
    (
        "usage_cache/db.rs::open",
        Reach::Owned,
        "ainb's usage cache database",
    ),
    // Read-only probes.
    (
        "cli/deps.rs::run",
        Reach::ReadOnly,
        "runs a dependency's version probe",
    ),
    (
        "tmux/session.rs::does_session_exist",
        Reach::ReadOnly,
        "asks tmux whether a session exists",
    ),
    (
        "app/state.rs::auto_detect_workspace_shells",
        Reach::ReadOnly,
        "tmux list-sessions",
    ),
    (
        "app/state.rs::load_other_tmux_sessions",
        Reach::ReadOnly,
        "tmux list-sessions",
    ),
    (
        "app/state.rs::load_real_workspaces",
        Reach::ReadOnly,
        "tmux has-session",
    ),
    (
        "fleet/bridge/secrets.rs::resolve_keychain_via_security",
        Reach::ReadOnly,
        "reads a keychain item",
    ),
    (
        "git/branch_list.rs::checked_out_branches",
        Reach::ReadOnly,
        "git worktree list",
    ),
    (
        "git/operations.rs::get_git_credentials",
        Reach::ReadOnly,
        "git credential fill",
    ),
    (
        "interactive/session_manager.rs::capture_failed_launch_pane",
        Reach::ReadOnly,
        "tmux capture-pane",
    ),
    (
        "interactive/session_manager.rs::codex_launch_exit",
        Reach::ReadOnly,
        "tmux list-panes",
    ),
    (
        "otel/mod.rs::detect_host_name",
        Reach::ReadOnly,
        "hostname -s",
    ),
];

/// Calls into other crates that write, spelled as they appear in this crate.
const EXTERNAL_WRITERS: &[&str] = &[
    "ainb_plugin_notifyd::install_for(",
    "ainb_cli::skill::dispatch(",
];

/// Host effects that run external code when the host executes them.
const OUTSIDE_EFFECTS: &[&str] = &["ToolTerminal::AbtopWithSetup"];

/// Line spellings of a process spawn or a file write.
const WRITE_SPELLINGS: &[&str] = &[
    "Command::new(",
    "CommandBuilder::new(",
    "fs::write(",
    "File::create(",
    "OpenOptions::new(",
    "fs::remove_file(",
    "fs::remove_dir(",
    "fs::remove_dir_all(",
    "fs::rename(",
    "fs::copy(",
    "fs::create_dir(",
    "fs::create_dir_all(",
];

/// Actions whose remote refusal depends on state, judged by
/// `AppState::remote_command_refusal` rather than by the row flag. Each must
/// write outside ainb by the walk, and be refused in a state that arms it
/// ([`armed`]), so this list cannot outlive the gate.
const STATE_GATED: &[&str] = &["ConfirmationConfirm", "OnboardingNext", "OnboardingFinish"];

/// Rows whose `AppEvent` has no arm in `process_event` (handled elsewhere, or
/// nowhere), so the walk has nothing to follow from them. A row joining or
/// leaving this set fails until it is looked at.
const SKIPPED_ROWS: &[&str] = &[];

/// Functions the walk never enters: the dispatchers themselves, which reach
/// every arm by name and would make every row reach every sink.
const NOT_WALKED: &[&str] = &[
    "process_event",
    "dispatch",
    "resolve_intent",
    "process_async_action",
    "keymap_ui_event",
    "apply_key_action",
];

// ---------------------------------------------------------------------------
// Source model
// ---------------------------------------------------------------------------

struct Source {
    /// `path under src/` -> lines, with `#[cfg(test)] mod` blocks blanked.
    files: BTreeMap<String, Vec<String>>,
    /// `file` -> function name -> (first line, closing line), top level.
    free: BTreeMap<String, BTreeMap<String, (usize, usize)>>,
    /// `file` -> method name -> span, for methods of any `impl` in the file.
    methods: BTreeMap<String, BTreeMap<String, (usize, usize)>>,
    /// Method name -> spans, for `impl AppState` blocks in any file.
    app_state: BTreeMap<String, Vec<(String, usize, usize)>>,
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

fn blank_test_modules(lines: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(lines.len());
    let mut index = 0;
    while index < lines.len() {
        if lines[index].trim() == "#[cfg(test)]" {
            let mut next = index + 1;
            while next < lines.len()
                && (lines[next].trim().is_empty() || lines[next].trim().starts_with("#["))
            {
                next += 1;
            }
            if next < lines.len() {
                let head = lines[next].trim();
                let is_mod = head.ends_with('{')
                    && (head.starts_with("mod ")
                        || head.starts_with("pub mod ")
                        || head.starts_with("pub(crate) mod "));
                if is_mod {
                    let close = format!("{}}}", " ".repeat(indent(&lines[next])));
                    let mut end = next + 1;
                    while end < lines.len() && lines[end] != close {
                        end += 1;
                    }
                    out.extend(std::iter::repeat_n(String::new(), end + 1 - index));
                    index = end + 1;
                    continue;
                }
            }
        }
        out.push(lines[index].clone());
        index += 1;
    }
    out
}

fn fn_name(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    for prefix in ["pub(crate) ", "pub(super) ", "pub "] {
        rest = rest.strip_prefix(prefix).unwrap_or(rest);
    }
    for prefix in ["const ", "async ", "unsafe "] {
        rest = rest.strip_prefix(prefix).unwrap_or(rest);
    }
    let rest = rest.strip_prefix("fn ")?;
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
    let name = &rest[..end];
    (!name.is_empty() && name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_'))
        .then_some(name)
}

fn closing(lines: &[String], start: usize) -> usize {
    let close = format!("{}}}", " ".repeat(indent(&lines[start])));
    (start + 1..lines.len())
        .find(|&line| lines[line] == close)
        .unwrap_or(lines.len() - 1)
}

impl Source {
    fn load() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = BTreeMap::new();
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && !path.to_string_lossy().ends_with("_tests.rs")
                {
                    let rel = path
                        .strip_prefix(&root)
                        .expect("under src")
                        .to_string_lossy()
                        .replace('\\', "/");
                    let text = std::fs::read_to_string(&path).expect("read source");
                    let lines: Vec<String> = text.lines().map(str::to_string).collect();
                    files.insert(rel, blank_test_modules(&lines));
                }
            }
        }
        let mut free: BTreeMap<String, BTreeMap<String, (usize, usize)>> = BTreeMap::new();
        let mut methods: BTreeMap<String, BTreeMap<String, (usize, usize)>> = BTreeMap::new();
        let mut app_state: BTreeMap<String, Vec<(String, usize, usize)>> = BTreeMap::new();
        for (rel, lines) in &files {
            let mut impl_type: Option<(String, usize)> = None;
            for (index, line) in lines.iter().enumerate() {
                if line.starts_with("impl") && line.ends_with('{') {
                    let head = line.trim_end_matches('{').trim();
                    let target = head.rsplit(" for ").next().unwrap_or(head);
                    let target = target.trim_start_matches("impl").trim();
                    let target = target.rsplit('>').next().unwrap_or(target).trim();
                    let name: String = target
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    impl_type = Some((name, closing(lines, index)));
                }
                if impl_type.as_ref().is_some_and(|(_, end)| index > *end) {
                    impl_type = None;
                }
                let Some(name) = fn_name(line) else { continue };
                if line.trim_end().ends_with(';') {
                    continue;
                }
                let span = (index, closing(lines, index));
                match (indent(line), &impl_type) {
                    (0, _) => {
                        free.entry(rel.clone()).or_default().insert(name.to_string(), span);
                    }
                    (4, Some((owner, _))) => {
                        methods.entry(rel.clone()).or_default().insert(name.to_string(), span);
                        if owner == "AppState" {
                            app_state.entry(name.to_string()).or_default().push((
                                rel.clone(),
                                span.0,
                                span.1,
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        Self {
            files,
            free,
            methods,
            app_state,
        }
    }

    fn lines(&self, file: &str, (start, end): (usize, usize)) -> Vec<String> {
        self.files[file][start..=end]
            .iter()
            .filter(|line| !line.trim_start().starts_with("//"))
            .cloned()
            .collect()
    }

    /// The module file a `crate::a::b` path names, if it exists.
    fn module_file(&self, path: &str) -> Option<String> {
        let path = path.replace("::", "/");
        [format!("{path}.rs"), format!("{path}/mod.rs")]
            .into_iter()
            .find(|candidate| self.files.contains_key(candidate))
    }

    /// A free function `name`, looked for in `file` first and then as the one
    /// crate-wide definition of that name (a `pub use` re-export).
    fn free_fn(&self, file: Option<&str>, name: &str) -> Option<(String, (usize, usize))> {
        if let Some(file) = file {
            if let Some(span) = self.free.get(file).and_then(|fns| fns.get(name)) {
                return Some((file.to_string(), *span));
            }
        }
        let mut found = self
            .free
            .iter()
            .filter_map(|(file, fns)| fns.get(name).map(|span| (file.clone(), *span)));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// Match arms of `enum` inside the function span, at `arm_indent`. Bare
    /// variant names count when `bare` holds them (a `use Enum::{..}`).
    fn arms(
        &self,
        file: &str,
        (start, end): (usize, usize),
        enumeration: &str,
        arm_indent: usize,
        bare: &BTreeSet<String>,
    ) -> BTreeMap<String, Vec<String>> {
        let lines = &self.files[file];
        let prefix = format!("{enumeration}::");
        let mut arms: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut current: Vec<String> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for line in &lines[start + 1..end] {
            // A continued or-pattern (`| Enum::Y =>`) names the same arm.
            if indent(line) == arm_indent && line.trim_start().starts_with("| ") {
                let head = line.trim_start();
                names.extend(variant_names(
                    head.split("=>").next().unwrap_or(head),
                    enumeration,
                ));
                current.push(line.clone());
                continue;
            }
            let starts = if indent(line) == arm_indent {
                let head = line.trim_start();
                let pattern = head.split("=>").next().unwrap_or(head);
                // A binding (`action @ Enum::X`) names the arm after the `@`.
                let pattern = pattern.split_once(" @ ").map_or(pattern, |(_, rest)| rest);
                // The pattern's leading path, which may be qualified
                // (`crate::app::state::ConfirmAction::X`).
                let leading = pattern
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                    .next()
                    .unwrap_or_default();
                if leading.starts_with(&prefix) || leading.contains(&format!("::{prefix}")) {
                    Some(variant_names(pattern, enumeration))
                } else if let Some(word) = leading_word(head).filter(|word| bare.contains(*word)) {
                    Some(vec![word.to_string()])
                } else if head.starts_with("_ =>") || head.starts_with('}') {
                    Some(Vec::new())
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(starts) = starts {
                for name in &names {
                    arms.entry(name.clone()).or_default().extend(current.iter().cloned());
                }
                names = starts;
                current = vec![line.clone()];
            } else {
                current.push(line.clone());
            }
        }
        for name in &names {
            arms.entry(name.clone()).or_default().extend(current.iter().cloned());
        }
        arms
    }
}

fn leading_word(text: &str) -> Option<&str> {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(text.len());
    (end > 0).then(|| &text[..end])
}

fn variant_names(text: &str, enumeration: &str) -> Vec<String> {
    let prefix = format!("{enumeration}::");
    text.match_indices(&prefix)
        .filter_map(|(at, _)| leading_word(&text[at + prefix.len()..]))
        .filter(|name| name.starts_with(|c: char| c.is_ascii_uppercase()))
        .map(str::to_string)
        .collect()
}

/// Identifiers followed by `(` on a line, with what precedes each.
fn calls(line: &str) -> Vec<(String, String)> {
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    for (at, _) in line.match_indices('(') {
        let mut start = at;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            start -= 1;
        }
        if start == at {
            continue;
        }
        let name = &line[start..at];
        if !name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
            continue;
        }
        let mut path_start = start;
        while path_start > 0 {
            let c = bytes[path_start - 1];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b':' || c == b'.' {
                path_start -= 1;
            } else {
                break;
            }
        }
        out.push((line[path_start..start].to_string(), name.to_string()));
    }
    out
}

fn has_write(line: &str) -> Option<&'static str> {
    WRITE_SPELLINGS
        .iter()
        .chain(EXTERNAL_WRITERS)
        .chain(OUTSIDE_EFFECTS)
        .find(|spelling| line.contains(*spelling))
        .copied()
}

// ---------------------------------------------------------------------------
// The walk
// ---------------------------------------------------------------------------

struct Dispatch {
    app: BTreeMap<String, Vec<String>>,
    ui: BTreeMap<String, Vec<String>>,
    asynchronous: BTreeMap<String, Vec<String>>,
    confirm: BTreeMap<String, Vec<String>>,
}

const EVENTS: &str = "app/events.rs";
const STATE: &str = "app/state.rs";

impl Dispatch {
    fn new(source: &Source) -> Self {
        let none = BTreeSet::new();
        let process = source.methods[EVENTS]["process_event"];
        let app = source.arms(EVENTS, process, "AppEvent", 12, &none);
        let ui_span = source.methods[EVENTS]["keymap_ui_event"];
        // `keymap_ui_event` imports some variants bare with `use UiAction::{..}`.
        let imported: BTreeSet<String> = source.files[EVENTS][ui_span.0..ui_span.1]
            .iter()
            .take_while(|line| !line.trim_start().starts_with("match "))
            .flat_map(|line| {
                line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .filter(|word| word.starts_with(|c: char| c.is_ascii_uppercase()))
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
        let ui = source.arms(EVENTS, ui_span, "UiAction", 12, &imported);
        let async_span = source.methods[STATE]["process_async_action"];
        let mut asynchronous = BTreeMap::new();
        for arm_indent in (16..=32).step_by(4) {
            for (name, lines) in source.arms(STATE, async_span, "AsyncAction", arm_indent, &none) {
                asynchronous.entry(name).or_insert(lines);
            }
        }
        let confirm_lines = &app["ConfirmationConfirm"];
        let first = source.files[EVENTS]
            .iter()
            .position(|line| line == &confirm_lines[0])
            .expect("the Confirm arm is in events.rs");
        let span = (first, first + confirm_lines.len());
        let mut confirm = BTreeMap::new();
        for arm_indent in (20..=36).step_by(4) {
            for (name, lines) in source.arms(EVENTS, span, "ConfirmAction", arm_indent, &none) {
                confirm.entry(name).or_insert(lines);
            }
        }
        Self {
            app,
            ui,
            asynchronous,
            confirm,
        }
    }
}

/// Every sink reachable from `start` (lines of a reducer arm in `file`),
/// with the trail that reached it.
fn sinks_from(
    source: &Source,
    dispatch: &Dispatch,
    arm_key: &str,
    file: &str,
    start: &[String],
) -> BTreeMap<String, String> {
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    // (file, lines, is an arm, key, trail)
    let mut stack: Vec<(String, Vec<String>, bool, String, String)> = vec![(
        file.to_string(),
        start.to_vec(),
        true,
        arm_key.to_string(),
        arm_key.to_string(),
    )];
    while let Some((file, lines, is_arm, key, trail)) = stack.pop() {
        let uses = use_lines(&source.files[&file]);
        for line in &lines {
            if let Some(spelling) = has_write(line) {
                let sink = if OUTSIDE_EFFECTS.contains(&spelling) {
                    format!("effect:{spelling}")
                } else if is_arm {
                    key.clone()
                } else {
                    format!("{file}::{key}")
                };
                found.entry(sink).or_insert_with(|| trail.clone());
            }
            let mut push = |kind: &str, name: &str, lines: Option<&Vec<String>>, file: &str| {
                if let Some(lines) = lines {
                    let arm = format!("arm:{kind}::{name}");
                    if seen.insert(arm.clone()) {
                        stack.push((
                            file.to_string(),
                            lines.clone(),
                            true,
                            arm.clone(),
                            format!("{trail} > {arm}"),
                        ));
                    }
                }
            };
            for name in variant_names(line, "AppEvent") {
                push("AppEvent", &name, dispatch.app.get(&name), EVENTS);
            }
            for name in variant_names(line, "AsyncAction") {
                let arm = dispatch.asynchronous.get(&name);
                // An arm that only puts the action back is run by the host's
                // main loop (`ainb-core`'s `main.rs`), outside this crate.
                if arm.is_some_and(|lines| {
                    lines.iter().any(|line| line.contains("pending_async_action = Some(action)"))
                }) {
                    found
                        .entry(format!("host:AsyncAction::{name}"))
                        .or_insert_with(|| format!("{trail} > host:{name}"));
                }
                push("AsyncAction", &name, arm, STATE);
            }
            for (path, name) in calls(line) {
                if NOT_WALKED.contains(&name.as_str()) {
                    continue;
                }
                let targets = call_targets(source, &file, &uses, &path, &name);
                for (target_file, span) in targets {
                    let fn_key = format!("{target_file}::{name}");
                    if !seen.insert(fn_key) {
                        continue;
                    }
                    stack.push((
                        target_file.clone(),
                        source.lines(&target_file, span),
                        false,
                        name.clone(),
                        format!("{trail} > {name}"),
                    ));
                }
            }
        }
    }
    found
}

/// Where a call spelled `{path}{name}(` in `file` goes, by the edges the module
/// doc lists. Empty when the call is not one the walk follows.
fn call_targets(
    source: &Source,
    file: &str,
    uses: &BTreeSet<String>,
    path: &str,
    name: &str,
) -> Vec<(String, (usize, usize))> {
    if path.ends_with("Self::") {
        return source
            .methods
            .get(file)
            .and_then(|methods| methods.get(name))
            .map(|span| vec![(file.to_string(), *span)])
            .unwrap_or_default();
    }
    if ["state.", "self."].contains(&path) || path.ends_with(".state.") {
        return source
            .app_state
            .get(name)
            .into_iter()
            .flatten()
            .map(|(method_file, start, end)| (method_file.clone(), (*start, *end)))
            .collect();
    }
    if path.ends_with('.') {
        // A method on some other receiver (`git_state.commit_and_push()`):
        // followed only when one file in the crate defines that name, since
        // the receiver's type is not in the text.
        let mut owners = source.methods.iter().filter_map(|(method_file, methods)| {
            methods.get(name).map(|span| (method_file.clone(), *span))
        });
        return match (owners.next(), owners.next()) {
            (Some(target), None) => vec![target],
            _ => Vec::new(),
        };
    }
    if let Some(module) = path.strip_prefix("crate::").and_then(|p| p.strip_suffix("::")) {
        let module_file = source.module_file(module);
        return source.free_fn(module_file.as_deref(), name).into_iter().collect();
    }
    let local = source.free.get(file).is_some_and(|fns| fns.contains_key(name));
    if path.is_empty() && (local || uses.contains(name)) {
        return source.free_fn(Some(file), name).into_iter().collect();
    }
    Vec::new()
}

/// Names a file brings in with `use crate::…`, anywhere in it.
fn use_lines(lines: &[String]) -> BTreeSet<String> {
    lines
        .iter()
        .filter(|line| line.trim_start().starts_with("use crate::"))
        .flat_map(|line| {
            line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|word| word.starts_with(|c: char| c.is_ascii_lowercase()))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn action_variant(action: &KeyAction) -> (&'static str, String) {
    let debug = format!("{action:?}");
    let (kind, rest) = debug.split_once('(').unwrap_or((debug.as_str(), ""));
    let kind = match kind {
        "App" => "App",
        "Ui" => "Ui",
        _ => "",
    };
    (kind, leading_word(rest).unwrap_or_default().to_string())
}

fn reach_of(key: &str) -> Option<Reach> {
    SINKS.iter().find(|(sink, _, _)| *sink == key).map(|(_, reach, _)| *reach)
}

// ---------------------------------------------------------------------------
// The checks
// ---------------------------------------------------------------------------

#[test]
fn every_row_that_writes_outside_ainb_runs_only_from_its_key() {
    let source = Source::load();
    let dispatch = Dispatch::new(&source);
    assert!(dispatch.app.len() > 100, "the AppEvent arms were not found");
    assert!(
        dispatch.asynchronous.len() > 10,
        "the AsyncAction arms were not found"
    );

    let keymap = Keymap::defaults();
    let mut reached: BTreeMap<String, String> = BTreeMap::new();
    let mut outside_rows: BTreeSet<String> = BTreeSet::new();
    let mut key_only_rows: BTreeSet<String> = BTreeSet::new();
    let mut skipped: BTreeSet<String> = BTreeSet::new();
    let mut gated_outside: BTreeSet<String> = BTreeSet::new();
    for (id, binding) in keymap.commands() {
        let (kind, variant) = action_variant(&binding.action);
        let arms = match kind {
            "App" => &dispatch.app,
            "Ui" => &dispatch.ui,
            _ => continue,
        };
        let Some(lines) = arms.get(&variant) else {
            skipped.insert(id.as_str().to_string());
            continue;
        };
        let arm_key = format!(
            "arm:{}::{variant}",
            if kind == "App" {
                "AppEvent"
            } else {
                "UiAction"
            }
        );
        let sinks = sinks_from(&source, &dispatch, &arm_key, EVENTS, lines);
        let writes_outside = sinks.keys().any(|sink| reach_of(sink) == Some(Reach::Outside));
        if writes_outside && STATE_GATED.contains(&variant.as_str()) {
            gated_outside.insert(variant.clone());
        } else if writes_outside {
            outside_rows.insert(id.as_str().to_string());
        }
        if binding.key_only() {
            key_only_rows.insert(id.as_str().to_string());
        }
        for (sink, trail) in sinks {
            reached.entry(sink).or_insert(trail);
        }
    }
    // The Confirm arm's sub-arms, for the dialog check. A sink spelled in a
    // sub-arm is the Confirm arm's own sink.
    let mut outside_confirms: BTreeSet<String> = BTreeSet::new();
    for (variant, lines) in &dispatch.confirm {
        let key = "arm:AppEvent::ConfirmationConfirm";
        let sinks = sinks_from(&source, &dispatch, key, EVENTS, lines);
        if sinks.keys().any(|sink| reach_of(sink) == Some(Reach::Outside)) {
            outside_confirms.insert(variant.clone());
        }
        for (sink, trail) in sinks {
            reached.entry(sink).or_insert(trail);
        }
    }
    let key_only_confirms: BTreeSet<String> = confirm_actions()
        .into_iter()
        .filter(ConfirmAction::runs_only_from_key)
        .map(|action| leading_word(&format!("{action:?}")).unwrap_or_default().to_string())
        .collect();
    let named: BTreeSet<String> = confirm_actions()
        .iter()
        .map(|action| leading_word(&format!("{action:?}")).unwrap_or_default().to_string())
        .collect();
    let arms: BTreeSet<String> = dispatch.confirm.keys().cloned().collect();
    assert_eq!(
        arms, named,
        "confirm_actions() must list one of every ConfirmAction the Confirm arm handles"
    );
    assert_eq!(
        outside_confirms, key_only_confirms,
        "the ConfirmActions that write outside ainb must be exactly those \
         ConfirmAction::runs_only_from_key accepts"
    );

    assert_sinks_classified(&reached);
    assert_walk_baselines(&skipped, &gated_outside);

    let unflagged: Vec<&String> = outside_rows.difference(&key_only_rows).collect();
    let overflagged: Vec<&String> = key_only_rows.difference(&outside_rows).collect();
    assert!(
        unflagged.is_empty() && overflagged.is_empty(),
        "rows that write outside ainb must be exactly the key-only rows \
         (KeyAction::writes_outside_ainb):\nwrite outside but not key-only: \
         {unflagged:#?}\nkey-only but reach nothing outside: {overflagged:#?}"
    );
}

/// Every sink the walk reached is in [`SINKS`], and every entry there is
/// still reached.
fn assert_sinks_classified(reached: &BTreeMap<String, String>) {
    let unclassified: Vec<String> = reached
        .iter()
        .filter(|(sink, _)| reach_of(sink).is_none())
        .map(|(sink, trail)| format!("{sink}\n      via {trail}"))
        .collect();
    let stale: Vec<&str> = SINKS
        .iter()
        .map(|(sink, _, _)| *sink)
        .filter(|sink| !reached.contains_key(*sink))
        .collect();
    assert!(
        unclassified.is_empty() && stale.is_empty(),
        "a command reaches a sink SINKS does not classify, or SINKS names one no \
         command reaches:\nunclassified:\n  {}\nstale: {stale:#?}",
        unclassified.join("\n  ")
    );
}

/// The rows the walk skipped match [`SKIPPED_ROWS`], and the state-gated
/// actions it found writing outside ainb match [`STATE_GATED`].
fn assert_walk_baselines(skipped: &BTreeSet<String>, gated_outside: &BTreeSet<String>) {
    let baseline: BTreeSet<String> = SKIPPED_ROWS.iter().map(|id| (*id).to_string()).collect();
    assert_eq!(
        *skipped, baseline,
        "rows the walk cannot follow (no arm for their action) changed; check each \
         new one by hand, then update SKIPPED_ROWS"
    );
    let gated: BTreeSet<String> = STATE_GATED.iter().map(|name| (*name).to_string()).collect();
    assert_eq!(
        *gated_outside, gated,
        "every STATE_GATED action must write outside ainb by the walk; drop one \
         that no longer does"
    );
}

/// Each [`STATE_GATED`] action is refused from a remote surface in a state
/// that makes it write outside ainb, and allowed in a fresh state.
#[test]
fn every_state_gated_action_is_refused_in_the_state_that_arms_it() {
    for name in STATE_GATED {
        let (action, state) = armed(name);
        assert!(
            state.remote_command_refusal(&action).is_some(),
            "{name} runs from a remote surface in the state that arms it"
        );
        assert_eq!(
            AppState::new().remote_command_refusal(&action),
            None,
            "{name} is refused even in a fresh state"
        );
    }
}

/// The action a [`STATE_GATED`] name spells, with a state in which it would
/// write outside ainb.
fn armed(name: &str) -> (KeyAction, AppState) {
    use ainb_app::app::state::{ConfirmationDialog, DialogOption};
    use ainb_app::components::onboarding::OnboardingState;

    let mut state = AppState::new();
    match name {
        "ConfirmationConfirm" => {
            // The hook install dialog, "Install" selected.
            state.shell.confirmation_dialog = Some(ConfirmationDialog {
                title: "Get notified when a session needs you?".to_string(),
                message: String::new(),
                confirm_action: ConfirmAction::InstallNotifyHooks,
                selected_option: false,
                warning: None,
                options: Some(vec![
                    DialogOption {
                        label: "Install".to_string(),
                        action: ConfirmAction::InstallNotifyHooks,
                    },
                    DialogOption {
                        label: "Not now".to_string(),
                        action: ConfirmAction::Cancel,
                    },
                ]),
                selected_index: 0,
            });
        }
        "OnboardingNext" | "OnboardingFinish" => {
            // Telemetry opted into, with every credential typed.
            state.onboarding.onboarding_state = Some(OnboardingState {
                otel_skip: false,
                otel_otlp_endpoint: "https://otlp.example.test".to_string(),
                otel_instance_id: "123".to_string(),
                otel_api_token: "token".to_string(),
                ..OnboardingState::default()
            });
        }
        other => panic!("STATE_GATED names {other}; give it an arming state here"),
    }
    // The action as a keymap row spells it.
    let action = Keymap::defaults()
        .commands()
        .map(|(_, row)| row.action.clone())
        .find(|action| action_variant(action) == ("App", name.to_string()))
        .unwrap_or_else(|| panic!("no keymap row runs {name}"));
    (action, state)
}

/// One of every `ConfirmAction`, for the dialog check.
fn confirm_actions() -> Vec<ConfirmAction> {
    let id = uuid::Uuid::nil();
    vec![
        ConfirmAction::DeleteSession(id),
        ConfirmAction::StopSession(id),
        ConfirmAction::BulkDeleteSessions(vec![id]),
        ConfirmAction::BulkStopSessions(vec![id]),
        ConfirmAction::KillOtherTmux("scratch".to_string()),
        ConfirmAction::KillOtherTmuxSessions(vec!["scratch".to_string()]),
        ConfirmAction::KillWorkspaceShell(0),
        ConfirmAction::InstallNotifyHooks,
        ConfirmAction::DismissNotifyPrompt,
        ConfirmAction::McpStopServer("github".to_string()),
        ConfirmAction::McpStopDaemon,
        ConfirmAction::SetupAbtopRateLimits,
        ConfirmAction::OpenAbtopSkipSetup,
        ConfirmAction::DismissAbtopSetup,
        ConfirmAction::Cancel,
    ]
}
