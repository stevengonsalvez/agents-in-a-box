//! Install / uninstall / status for the `ainb-hooks` plugin.
//!
//! The bash hook script and Claude plugin manifest are embedded into
//! the `ainb-notifyd` binary at build time via `include_str!`. The
//! [`install`] verb extracts them to known on-disk locations and
//! wires Claude Code, Codex CLI, and GitHub Copilot CLI to call into `notify.sh`.
//!
//! For Claude: the `claude plugin` marketplace CLI installs
//! `ainb-hooks@agents-in-a-box`, so Claude resolves the plugin root and
//! bundled `hooks/notify.sh` itself.
//!
//! For Codex: a managed JSON block in `~/.codex/hooks.json`, since
//! Codex resolves hook commands as absolute paths. The block points
//! at the canonical `~/.agents-in-a-box/hooks/notify.sh` so the
//! plugin stays agent-agnostic per Stevie's constraint.
//!
//! For Copilot: a standalone drop-in at `~/.copilot/hooks/ainb.json`,
//! because Copilot combines every hook file in that directory.
//!
//! Install state is recorded at `~/.agents-in-a-box/install.json` so
//! [`uninstall`] is fully reversible.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::paths::Paths;

/// Atomically write `contents` to `path` via a sibling temp file + rename
/// (LOW-8). A bare `std::fs::write` truncates-then-writes in place, so a crash
/// mid-write leaves a TORN file — half-written `hooks.json` / `install.json`
/// is durable state that uninstall and re-install must parse. Writing to a temp
/// file in the SAME directory and `rename`-ing it over the target makes the
/// swap atomic on POSIX: a reader sees either the old file or the complete new
/// one, never a partial. Self-contained (no core `plumbing::atomic` dependency)
/// so the notifyd crate stays independent.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    // Unique-ish sibling temp name keyed by pid so concurrent writers don't
    // collide on the temp file before each renames over the target.
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp");
    let tmp = parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?; // durability: the bytes hit disk before the rename swap.
    }
    // Atomic same-dir swap; clean up the temp on rename failure.
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// The bash hook script, baked into the binary.
const HOOK_SCRIPT: &str = include_str!("../../../../plugins/ainb-hooks/hooks/notify.sh");

/// The Stop-hook stall guard, baked into the binary. Claude reaches it through
/// the plugin directory; Codex has no plugin runtime, so it needs the same
/// extract-and-point treatment as `notify.sh`.
const STALL_GUARD_SCRIPT: &str =
    include_str!("../../../../plugins/ainb-hooks/hooks/stall_guard.py");

/// The Claude plugin manifest, baked into the binary.
const CLAUDE_PLUGIN_JSON: &str =
    include_str!("../../../../plugins/ainb-hooks/.claude-plugin/plugin.json");

/// The Codex hooks.json merge template (with the `__AINB_HOOK_SCRIPT__`
/// placeholder that gets substituted at install time).
const CODEX_HOOKS_TEMPLATE: &str = include_str!("../../../../plugins/ainb-hooks/codex/hooks.json");

/// The Copilot drop-in template (with the `__AINB_HOOK_SCRIPT__`
/// placeholder substituted at install time). Unlike Codex, this is
/// written verbatim as a standalone file — Copilot loads + combines
/// every `*.json` in `~/.copilot/hooks/`, so ainb owns one file.
const COPILOT_HOOKS_TEMPLATE: &str =
    include_str!("../../../../plugins/ainb-hooks/copilot/hooks.json");

/// The Antigravity drop-in template (with the `__AINB_HOOK_SCRIPT__`
/// placeholder substituted at install time).
const ANTIGRAVITY_HOOKS_TEMPLATE: &str =
    include_str!("../../../../plugins/ainb-hooks/antigravity/hooks.json");

/// The host CLI agent being installed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    /// Anthropic Claude Code CLI.
    Claude,
    /// OpenAI Codex CLI.
    Codex,
    /// GitHub Copilot CLI. Hooks are installed as a standalone drop-in at
    /// `~/.copilot/hooks/ainb.json`; included in `ALL`.
    Copilot,
    /// Google Antigravity CLI. Hooks are installed as a standalone drop-in at
    /// `~/.gemini/antigravity-cli/hooks/ainb.json`; included in `ALL`.
    Antigravity,
    /// Catch-all for any agent written by a newer build that this binary
    /// doesn't know. Keeps `install.json` forward-compatible: an unknown
    /// variant no longer hard-fails `serde` deserialization — which used to
    /// reset the record to empty, nag the install prompt on every launch,
    /// and break the "Install" action with `parsing …/install.json`.
    #[serde(other)]
    Unknown,
}

impl Agent {
    /// All known agents this binary can install — used to derive `--all`.
    pub const ALL: &'static [Agent] = &[
        Agent::Claude,
        Agent::Codex,
        Agent::Copilot,
        Agent::Antigravity,
    ];

    /// Lowercase name for logs / status output.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Copilot => "copilot",
            Agent::Antigravity => "antigravity",
            Agent::Unknown => "unknown",
        }
    }
}

/// Persistent install record on disk. Lets `uninstall` reverse the
/// exact files it created, even if defaults change between versions.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    /// Agents currently installed for.
    pub agents: Vec<Agent>,
    /// Resolved absolute path to the canonical hook script.
    pub hook_script: PathBuf,
    /// Resolved per-agent plugin paths (Claude only).
    pub claude_plugin_dir: Option<PathBuf>,
    /// Resolved per-agent config path (Codex only).
    pub codex_hooks_json: Option<PathBuf>,
    /// Resolved path to ainb's standalone Copilot hooks drop-in
    /// (`~/.copilot/hooks/ainb.json`). Set on install; used by uninstall
    /// to remove only the managed file while leaving sibling drop-ins intact.
    #[serde(default)]
    pub copilot_hooks_json: Option<PathBuf>,
    /// Resolved path to ainb's standalone Antigravity hooks drop-in
    /// (`~/.gemini/antigravity-cli/hooks/ainb.json`). Set on install;
    /// used by uninstall to remove only the managed file while leaving
    /// sibling drop-ins intact.
    #[serde(default)]
    pub antigravity_hooks_json: Option<PathBuf>,
    /// The plugin manifest version that was written to disk at install
    /// time. Compared against [`embedded_plugin_version`] on startup to
    /// detect drift after an `ainb` upgrade. `None` for records written
    /// before this field existed (treated as "0.0.0" → always stale).
    #[serde(default)]
    pub plugin_version: Option<String>,
    /// Set when the user explicitly declined the first-run install
    /// prompt ("don't ask again"). Suppresses the offer-to-install
    /// prompt; does not affect the offer-to-update prompt (which only
    /// applies once something is actually installed).
    #[serde(default)]
    pub prompt_dismissed: bool,
}

impl InstallRecord {
    fn path(paths: &Paths) -> PathBuf {
        paths.base.join("install.json")
    }

    /// Load from disk, or return a default-empty record if absent.
    pub fn load(paths: &Paths) -> Result<Self> {
        let p = Self::path(paths);
        if !p.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", p.display()))
    }

    /// Persist to disk under the standard path.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        let p = Self::path(paths);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let text = serde_json::to_string_pretty(self)?;
        // Atomic write (LOW-8): a torn install.json would break uninstall's
        // reversibility, so swap it in via temp + rename.
        write_atomic(&p, &text).with_context(|| format!("writing {}", p.display()))?;
        Ok(())
    }
}

/// Where the canonical hook script lives on disk. Both Claude's
/// plugin dir and Codex's hooks.json point at this path so the
/// script is a single source of truth.
pub fn canonical_hook_script(paths: &Paths) -> PathBuf {
    paths.base.join("hooks").join("notify.sh")
}

/// Where the installer records the `ainb` launcher that hooks execute. Hook
/// processes do not inherit a developer shell's `AINB_BIN`, so resolving
/// `ainb` from PATH can silently select an older Homebrew build.
pub fn canonical_hook_bin(paths: &Paths) -> PathBuf {
    paths.base.join("hooks").join("ainb-bin")
}

/// Durable metadata beside [`canonical_hook_bin`].  The shell hook only needs
/// the one-line pointer, while this record lets health surfaces distinguish a
/// stable package launcher from an intentional dev build.
pub fn canonical_hook_bin_metadata(paths: &Paths) -> PathBuf {
    paths.base.join("hooks").join("ainb-bin.json")
}

/// How a hook reaches `ainb`.
///
/// `Release` deliberately means a package-manager launcher such as
/// `/opt/homebrew/bin/ainb`, never Homebrew's versioned Cellar binary. `Dev`
/// is intentionally exact: a removed worktree must be reported, not silently
/// replaced by an unrelated release binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookBinaryMode {
    /// Package-manager owned stable launcher, safe across upgrades.
    Release,
    /// Explicit local development build, expected to disappear with its tree.
    Dev,
    /// Absolute executable outside a recognised package or dev layout.
    Direct,
    /// Old one-line pointer without metadata; eligible only for safe migration.
    Legacy,
}

impl HookBinaryMode {
    /// Short user-facing policy label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Dev => "dev",
            Self::Direct => "direct",
            Self::Legacy => "legacy",
        }
    }
}

/// The executable policy persisted for the hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookBinaryTarget {
    /// Executable the shell hook invokes.
    pub path: PathBuf,
    /// Resolution policy for [`Self::path`].
    pub mode: HookBinaryMode,
}

fn is_dev_binary(path: &Path) -> bool {
    path.components().any(|component| component.as_os_str() == "target")
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn canonical_eq(left: &Path, right: &Path) -> bool {
    std::fs::canonicalize(left).ok() == std::fs::canonicalize(right).ok()
}

/// Return the unversioned Homebrew launcher for a Cellar executable.
fn homebrew_launcher(exe: &Path) -> Option<PathBuf> {
    let cellar = exe.ancestors().find(|path| path.file_name().is_some_and(|n| n == "Cellar"))?;
    let launcher = cellar.parent()?.join("bin").join("ainb");
    is_executable(&launcher).then_some(launcher)
}

/// Every prefix an installed `ainb` is expected to live under, most specific
/// first. Order is the preference order when several are populated.
fn launcher_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/opt/homebrew/bin/ainb"),
        PathBuf::from("/usr/local/bin/ainb"),
    ];
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/bin/ainb"));
        candidates.push(home.join(".cargo/bin/ainb"));
    }
    candidates
}

fn stable_launcher_for(exe: &Path) -> Option<PathBuf> {
    if let Some(launcher) = homebrew_launcher(exe) {
        return Some(launcher);
    }
    launcher_candidates()
        .into_iter()
        .find(|candidate| is_executable(candidate) && canonical_eq(candidate, exe))
}

/// The installed `ainb`, whatever binary happens to be running.
///
/// Unlike [`stable_launcher_for`] this does NOT require the launcher to be the
/// same file as `exe`: that is the whole point. Hooks installed from a
/// throwaway worktree build must point at the ainb that will still be there
/// tomorrow, not at the `target/debug` path the worktree took with it.
fn installed_launcher(exe: &Path) -> Option<PathBuf> {
    // A Homebrew install resolves to its OWN launcher first, so a machine with
    // two ainbs keeps the one the running binary belongs to.
    if let Some(launcher) = homebrew_launcher(exe) {
        return Some(launcher);
    }
    launcher_candidates().into_iter().find(|candidate| is_executable(candidate))
}

fn launcher_mode(path: &Path) -> HookBinaryMode {
    if is_dev_binary(path) {
        HookBinaryMode::Dev
    } else if path == Path::new("/opt/homebrew/bin/ainb")
        || path == Path::new("/usr/local/bin/ainb")
        || path
            .parent()
            .is_some_and(|parent| parent.ends_with(".cargo/bin") || parent.ends_with(".local/bin"))
    {
        HookBinaryMode::Release
    } else {
        HookBinaryMode::Direct
    }
}

/// Why a hook binary pointer is being written.
///
/// The two intents differ only in whether the INSTALLED ainb is preferred over
/// the one doing the writing. That is a policy choice, not a new kind of
/// binary, so it deliberately does not add a [`HookBinaryMode`] variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryIntent {
    /// Install / repair. Points hooks at the installed ainb so the pointer
    /// survives the deletion of whatever tree the running binary was built in.
    Install,
    /// Pin the binary running right now — deliberate dev testing.
    PinRunning,
}

fn hook_binary_target(exe: PathBuf, explicit: Option<&str>) -> HookBinaryTarget {
    resolve_hook_binary(exe, explicit, BinaryIntent::Install, &installed_launcher)
}

/// Resolve the executable a hook should invoke.
///
/// `find_installed` is injected so the search is unit-testable without a real
/// filesystem; production passes [`installed_launcher`].
fn resolve_hook_binary(
    exe: PathBuf,
    explicit: Option<&str>,
    intent: BinaryIntent,
    find_installed: &dyn Fn(&Path) -> Option<PathBuf>,
) -> HookBinaryTarget {
    if let Some(path) = explicit.filter(|value| !value.trim().is_empty()) {
        let path = PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            std::path::absolute(&path).unwrap_or(path)
        };
        // An explicit developer build is meaningful. An explicit Homebrew
        // Cellar path is only an old package implementation detail, so retain
        // the user's package choice but record its durable launcher instead.
        if let Some(launcher) = homebrew_launcher(&path) {
            return HookBinaryTarget {
                mode: HookBinaryMode::Release,
                path: launcher,
            };
        }
        return HookBinaryTarget {
            mode: launcher_mode(&path),
            path,
        };
    }
    // Install/repair prefers the installed ainb even when it is a DIFFERENT
    // file from the one writing the pointer. Pinning deliberately does not.
    if intent == BinaryIntent::Install {
        if let Some(path) = find_installed(&exe) {
            // An installed launcher is a stable one by construction — that is
            // the only kind `find_installed` yields — so it is Release
            // regardless of the prefix it was found under.
            return HookBinaryTarget {
                path,
                mode: HookBinaryMode::Release,
            };
        }
    }
    if let Some(path) = stable_launcher_for(&exe) {
        return HookBinaryTarget {
            path,
            mode: HookBinaryMode::Release,
        };
    }
    HookBinaryTarget {
        mode: launcher_mode(&exe),
        path: exe,
    }
}

fn write_hook_binary_target(paths: &Paths, target: &HookBinaryTarget) -> Result<PathBuf> {
    let dest = canonical_hook_bin(paths);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    write_atomic(&dest, &format!("{}\n", target.path.display()))
        .with_context(|| format!("writing {}", dest.display()))?;
    let metadata = canonical_hook_bin_metadata(paths);
    write_atomic(&metadata, &serde_json::to_string_pretty(target)?)
        .with_context(|| format!("writing {}", metadata.display()))?;
    Ok(dest)
}

/// Record an upgrade-safe executable for later hook invocations.
///
/// Homebrew's `current_exe()` lives in `Cellar/<version>`. Writing it here is
/// the regression this function avoids: use the stable `/opt/homebrew/bin`
/// launcher instead. A developer can deliberately pin their local build with
/// `AINB_BIN=/path/to/target/debug/ainb`; that intent is recorded as `dev`.
pub fn extract_hook_bin(paths: &Paths) -> Result<PathBuf> {
    let bin = std::env::current_exe().context("resolving installing ainb binary")?;
    let target = hook_binary_target(bin, std::env::var("AINB_BIN").ok().as_deref());
    write_hook_binary_target(paths, &target)
}

/// Point the hooks at the binary running RIGHT NOW, installed or not.
///
/// The deliberate opposite of [`extract_hook_bin`]: for testing a local build's
/// hooks. The pointer dies with the tree, which is the point — it is an
/// explicit choice rather than the accident of having installed from a
/// worktree.
pub fn pin_running_hook_binary(paths: &Paths) -> Result<HookBinaryTarget> {
    let bin = std::env::current_exe().context("resolving running ainb binary")?;
    let target = resolve_hook_binary(
        bin,
        std::env::var("AINB_BIN").ok().as_deref(),
        BinaryIntent::PinRunning,
        &installed_launcher,
    );
    write_hook_binary_target(paths, &target)?;
    Ok(target)
}

fn read_hook_binary_target(paths: &Paths) -> Option<HookBinaryTarget> {
    // `notify.sh` executes this one-line pointer, not the metadata. Read it
    // first so a stale or manually altered pointer can never be reported as
    // healthy because the JSON still names an executable release launcher.
    let raw = std::fs::read_to_string(canonical_hook_bin(paths)).ok().and_then(|text| {
        let path = PathBuf::from(text.trim());
        (!path.as_os_str().is_empty()).then_some(path)
    })?;
    let metadata = canonical_hook_bin_metadata(paths);
    if let Ok(text) = std::fs::read_to_string(metadata) {
        if let Ok(target) = serde_json::from_str::<HookBinaryTarget>(&text) {
            if target.path == raw {
                return Some(target);
            }
        }
    }
    Some(HookBinaryTarget {
        path: raw,
        mode: HookBinaryMode::Legacy,
    })
}

/// Safely migrate only old Homebrew Cellar pointers. Never replace a missing
/// dev/direct pointer: that would hide a developer's broken worktree by
/// unexpectedly switching their hooks to a global binary.
pub fn auto_repair_hook_binary(paths: &Paths) -> Result<bool> {
    let Some(target) = read_hook_binary_target(paths) else {
        return Ok(false);
    };
    if target.mode != HookBinaryMode::Legacy {
        return Ok(false);
    }
    let Some(launcher) = homebrew_launcher(&target.path) else {
        return Ok(false);
    };
    write_hook_binary_target(
        paths,
        &HookBinaryTarget {
            path: launcher,
            mode: HookBinaryMode::Release,
        },
    )?;
    Ok(true)
}

/// Extract the embedded `notify.sh` to its canonical location with
/// executable permissions. Idempotent — overwrites any existing
/// version of the script (so re-running install always picks up the
/// latest from the binary).
pub fn extract_hook_script(paths: &Paths) -> Result<PathBuf> {
    extract_script(canonical_hook_script(paths), HOOK_SCRIPT)
}

/// Canonical on-disk path of the extracted stall guard.
pub fn canonical_stall_guard(paths: &Paths) -> PathBuf {
    paths.base.join("hooks").join("stall_guard.py")
}

/// Extract the embedded `stall_guard.py` alongside `notify.sh`. Same
/// idempotent overwrite semantics, so re-running install picks up the latest.
pub fn extract_stall_guard(paths: &Paths) -> Result<PathBuf> {
    extract_script(canonical_stall_guard(paths), STALL_GUARD_SCRIPT)
}

fn extract_script(dest: PathBuf, body: &str) -> Result<PathBuf> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&dest, body).with_context(|| format!("writing {}", dest.display()))?;
    let mut perms = std::fs::metadata(&dest)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&dest, perms)?;
    Ok(dest)
}

/// Marketplace + plugin identifiers ainb-hooks is published under.
const CLAUDE_PLUGIN_REF: &str = "ainb-hooks@agents-in-a-box";
const CLAUDE_MARKETPLACE: &str = "agents-in-a-box";
/// GitHub source used to register the marketplace on machines that don't
/// already know it (e.g. a brew install with no repo checkout).
const CLAUDE_MARKETPLACE_SOURCE: &str = "stevengonsalvez/agents-in-a-box";

/// Outcome of registering the Claude plugin through the `claude` CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeRegister {
    /// `claude plugin install` succeeded, or the plugin was already installed.
    Registered,
    /// The `claude` CLI is not on `PATH` — Claude wiring was skipped.
    ClaudeCliMissing,
    /// The CLI ran but failed; carries a short single-line reason.
    Failed(String),
}

/// Report returned by [`install`]: the on-disk record, plus — when Claude
/// was among the requested agents — the outcome of registering its plugin
/// through the `claude` CLI (`None` when Claude was not requested).
#[derive(Debug, Clone)]
pub struct InstallReport {
    /// The persisted install record (canonical script, codex wiring, etc.).
    pub record: InstallRecord,
    /// Claude marketplace-registration outcome, if Claude was targeted.
    pub claude: Option<ClaudeRegister>,
}

/// First non-empty line of `stderr` (preferred) or `stdout`, trimmed and
/// length-capped — for a tidy one-line status message.
fn first_line(stderr: &[u8], stdout: &[u8]) -> String {
    let pick = |b: &[u8]| {
        String::from_utf8_lossy(b)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(|l| l.chars().take(160).collect::<String>())
    };
    pick(stderr).or_else(|| pick(stdout)).unwrap_or_else(|| "unknown error".into())
}

/// Register the ainb-hooks plugin with Claude Code via its `claude plugin`
/// CLI. Modern Claude only loads plugins from a registered marketplace, so
/// dropping files under `~/.claude/plugins/` is inert — registration must
/// go through the CLI. Best-effort + non-interactive; never panics.
fn register_claude_plugin() -> ClaudeRegister {
    // Probe for the CLI by listing marketplaces. A spawn error means
    // `claude` is not on PATH.
    let list = match Command::new("claude").args(["plugin", "marketplace", "list"]).output() {
        Ok(o) => o,
        Err(_) => return ClaudeRegister::ClaudeCliMissing,
    };
    // Register the marketplace only when Claude doesn't already know it —
    // a local-directory source (a dev's repo checkout) must not be
    // clobbered by adding the GitHub one.
    let known = String::from_utf8_lossy(&list.stdout);
    if !known.contains(CLAUDE_MARKETPLACE) {
        let _ = Command::new("claude")
            .args(["plugin", "marketplace", "add", CLAUDE_MARKETPLACE_SOURCE])
            .output();
    }
    // Install, then update. `install` is idempotent but does not refresh an
    // existing cached marketplace plugin, leaving hook code behind `ainb`.
    let installed =
        match Command::new("claude").args(["plugin", "install", CLAUDE_PLUGIN_REF]).output() {
            Ok(o) if o.status.success() => true,
            Ok(o) => {
                let msg = first_line(&o.stderr, &o.stdout);
                if msg.to_lowercase().contains("already") {
                    true
                } else {
                    return ClaudeRegister::Failed(msg);
                }
            }
            Err(e) => return ClaudeRegister::Failed(e.to_string()),
        };
    if !installed {
        return ClaudeRegister::Failed("plugin install did not complete".into());
    }
    match Command::new("claude").args(["plugin", "update", CLAUDE_PLUGIN_REF]).output() {
        Ok(o) if o.status.success() => ClaudeRegister::Registered,
        Ok(o) => {
            let msg = first_line(&o.stderr, &o.stdout);
            if msg.to_lowercase().contains("already") || msg.to_lowercase().contains("latest") {
                ClaudeRegister::Registered
            } else {
                ClaudeRegister::Failed(msg)
            }
        }
        Err(e) => ClaudeRegister::Failed(e.to_string()),
    }
}

/// Unregister the ainb-hooks plugin from Claude (mirror of
/// [`register_claude_plugin`]). Best-effort; ignores all errors.
fn unregister_claude_plugin() {
    let _ = Command::new("claude").args(["plugin", "uninstall", CLAUDE_PLUGIN_REF]).output();
}

/// Install for one or more agents under the user's real `$HOME`.
///
/// Does the file-based wiring (canonical hook script + Codex managed
/// block + install record) via [`install_under_home`], then — if Claude
/// was requested — registers the Claude plugin through the `claude` CLI
/// and reports the outcome. Idempotent.
pub fn install(paths: &Paths, agents: &[Agent]) -> Result<InstallReport> {
    let home = dirs::home_dir().context("resolving home dir")?;
    let record = install_under_home(paths, &home, agents)?;
    let claude = agents.contains(&Agent::Claude).then(register_claude_plugin);
    Ok(InstallReport { record, claude })
}

/// Rebuild every installed hook surface against this running Ainb binary.
///
/// Unlike `fleet runtime install`, this touches hooks only: canonical scripts,
/// executable resolver, Codex/Copilot wiring, and Claude marketplace plugin.
/// It refuses an empty record rather than turning a diagnostic command into a
/// surprise all-agent install.
pub fn repair_hooks(paths: &Paths) -> Result<InstallReport> {
    let record = InstallRecord::load(paths)?;
    if record.agents.is_empty() {
        bail!("hooks are not installed; run `ainb notifyd install --all`");
    }
    install(paths, &record.agents)
}

/// Repair installed hooks, or perform the first install for every agent when
/// nothing is installed yet. This backs the TUI's repair keypress: unlike the
/// diagnostic CLI path above, a keypress on the Daemons screen is explicit
/// operator intent, so an empty record means "install", not "refuse".
pub fn repair_or_install_hooks(paths: &Paths) -> Result<InstallReport> {
    let record = InstallRecord::load(paths)?;
    if record.agents.is_empty() {
        return install(paths, Agent::ALL);
    }
    install(paths, &record.agents)
}

/// Install variant that takes an explicit `$HOME` root. Lets tests
/// stay isolated without mutating the process-wide environment.
pub fn install_under_home(paths: &Paths, home: &Path, agents: &[Agent]) -> Result<InstallRecord> {
    if agents.is_empty() {
        bail!("install: must specify at least one agent");
    }
    paths
        .ensure_base()
        .with_context(|| format!("creating {}", paths.base.display()))?;
    let hook_script = extract_hook_script(paths)?;
    extract_hook_bin(paths)?;
    let stall_guard = extract_stall_guard(paths)?;

    let mut record = InstallRecord::load(paths)?;
    record.hook_script = hook_script.clone();
    // Stamp the manifest version we're writing so a later `ainb`
    // upgrade can detect drift (see `prompt_state`). A fresh install
    // also clears any prior "don't ask again" — the user is opting in.
    record.plugin_version = Some(embedded_plugin_version());
    record.prompt_dismissed = false;
    // Per-agent install failures are isolated: one agent's filesystem error
    // (e.g. an unwritable `~/.copilot`) must NOT abort the others or prevent
    // the Claude plugin registration that runs after this returns. We collect
    // failures, persist whatever succeeded, and surface warnings — never let a
    // single `--all` target take the whole step down.
    let mut failures: Vec<(Agent, anyhow::Error)> = Vec::new();
    for agent in agents {
        match agent {
            Agent::Claude => {
                // Claude is wired through its plugin marketplace (see
                // `register_claude_plugin`, invoked by `install`), NOT by
                // dropping a plugin directory — modern Claude Code ignores
                // unregistered dirs. Here we only record the agent and
                // clear any legacy hand-dropped dir reference.
                record.claude_plugin_dir = None;
                push_unique(&mut record.agents, Agent::Claude);
            }
            Agent::Codex => match install_codex(home, &hook_script, &stall_guard) {
                Ok(hooks_json) => {
                    record.codex_hooks_json = Some(hooks_json);
                    push_unique(&mut record.agents, Agent::Codex);
                    // Writing hooks.json is only half a Codex install. Codex
                    // pins every hook in config.toml by `trusted_hash` and
                    // SILENTLY skips any entry it has not trusted — no error,
                    // no log line, the hook simply never runs. Say so, or the
                    // install looks complete while doing nothing.
                    eprintln!(
                        "note: Codex only runs hooks it has trusted. Start `codex` once and \
                         approve the startup hooks review, otherwise the ainb hooks (including \
                         the stall guard) are skipped silently. Non-interactive automation can \
                         pass --dangerously-bypass-hook-trust instead."
                    );
                }
                Err(e) => failures.push((Agent::Codex, e)),
            },
            Agent::Copilot => match install_copilot(home, &hook_script) {
                Ok(hooks_json) => {
                    record.copilot_hooks_json = Some(hooks_json);
                    push_unique(&mut record.agents, Agent::Copilot);
                }
                Err(e) => failures.push((Agent::Copilot, e)),
            },
            Agent::Antigravity => match install_antigravity(home, &hook_script) {
                Ok(hooks_json) => {
                    record.antigravity_hooks_json = Some(hooks_json);
                    push_unique(&mut record.agents, Agent::Antigravity);
                }
                Err(e) => failures.push((Agent::Antigravity, e)),
            },
            // Unknown agents are owned by a newer build; skip without
            // disturbing an existing record entry.
            Agent::Unknown => {}
        }
    }
    record.save(paths)?;
    for (agent, e) in &failures {
        eprintln!(
            "warning: notifyd hook install for {agent:?} failed (other agents unaffected): {e:#}"
        );
    }
    Ok(record)
}

/// The plugin manifest version baked into this binary — parsed from
/// the embedded `plugin.json`. Single source of truth: bump the
/// version in `plugins/ainb-hooks/.claude-plugin/plugin.json` and both
/// the install record and the drift check pick it up automatically.
pub fn embedded_plugin_version() -> String {
    serde_json::from_str::<serde_json::Value>(CLAUDE_PLUGIN_JSON)
        .ok()
        .and_then(|v| v.get("version").and_then(|s| s.as_str()).map(str::to_string))
        .unwrap_or_else(|| "0.0.0".to_string())
}

/// Parse a `major.minor.patch` string into a comparable tuple. Any
/// non-numeric suffix on a component (e.g. `-rc1`) is ignored; missing
/// components default to 0. Robust to the `v` prefix.
fn parse_semver(v: &str) -> (u64, u64, u64) {
    let mut parts = v.trim_start_matches('v').split('.').map(|p| {
        p.split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("0")
            .parse::<u64>()
            .unwrap_or(0)
    });
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// What the TUI should do about the notification hooks on startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPrompt {
    /// Nothing installed and the user hasn't declined — offer to
    /// install.
    OfferInstall,
    /// Installed, but this binary embeds a newer manifest — offer to
    /// re-install (drift after an `ainb` upgrade).
    OfferUpdate {
        /// Version currently on disk.
        installed: String,
        /// Version this binary would write.
        embedded: String,
    },
    /// Up to date, or the user declined — show nothing.
    None,
}

/// Decide whether the TUI should prompt the user about notification
/// hooks. This is the single entry point the host calls on startup;
/// it reads the install record and compares versions. Never errors —
/// a missing/corrupt record is treated as "not installed".
pub fn prompt_state(paths: &Paths) -> InstallPrompt {
    let record = InstallRecord::load(paths).unwrap_or_default();
    let embedded = embedded_plugin_version();
    if record.agents.is_empty() {
        if record.prompt_dismissed {
            InstallPrompt::None
        } else {
            InstallPrompt::OfferInstall
        }
    } else {
        let installed = record.plugin_version.clone().unwrap_or_else(|| "0.0.0".to_string());
        if parse_semver(&installed) < parse_semver(&embedded) {
            InstallPrompt::OfferUpdate {
                installed,
                embedded,
            }
        } else {
            InstallPrompt::None
        }
    }
}

/// Persist the user's "don't ask again" choice so the offer-to-install
/// prompt never fires again on this machine. Writes a record with no
/// agents and `prompt_dismissed = true`.
pub fn dismiss_prompt(paths: &Paths) -> Result<()> {
    let mut record = InstallRecord::load(paths).unwrap_or_default();
    record.prompt_dismissed = true;
    record.save(paths)
}

fn push_unique<T: PartialEq>(v: &mut Vec<T>, item: T) {
    if !v.contains(&item) {
        v.push(item);
    }
}

/// Install Codex hooks: merge our managed block into
/// `<home>/.codex/hooks.json`. Substitutes the `__AINB_HOOK_SCRIPT__`
/// placeholder with the canonical script path.
fn install_codex(
    home: &Path,
    hook_script_canonical: &Path,
    stall_guard_canonical: &Path,
) -> Result<PathBuf> {
    let codex_dir = home.join(".codex");
    std::fs::create_dir_all(&codex_dir)?;
    let hooks_json = codex_dir.join("hooks.json");

    // We're going to maintain a managed block bracketed by stable
    // markers. The simplest implementation: produce a JSON object that
    // combines the user's existing hooks with our block.
    let existing: serde_json::Value = if hooks_json.exists() {
        let text = std::fs::read_to_string(&hooks_json)?;
        if text.trim().is_empty() {
            serde_json::Value::Object(serde_json::Map::new())
        } else {
            // The user's hooks.json may include `//` line comments
            // (e.g. the existing reflect block). Strip them before
            // parsing.
            let cleaned = strip_line_comments(&text);
            serde_json::from_str(&cleaned)
                .with_context(|| format!("parsing {}", hooks_json.display()))?
        }
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    // Resolve our template against the actual hook script path.
    let our_block_text = CODEX_HOOKS_TEMPLATE
        .replace(
            "__AINB_HOOK_SCRIPT__",
            &hook_script_canonical.to_string_lossy(),
        )
        .replace(
            "__AINB_STALL_GUARD__",
            &stall_guard_canonical.to_string_lossy(),
        );
    let our_block: serde_json::Value = serde_json::from_str(&strip_line_comments(&our_block_text))
        .context("parsing embedded codex hooks template")?;
    let our_hooks = our_block
        .get("hooks")
        .and_then(|v| v.as_object())
        .cloned()
        .context("codex hooks template missing 'hooks' object")?;

    // Build the merged JSON. For each event in our_hooks, if the user
    // already has entries for that event, we append a single managed
    // entry; we never replace user-authored hooks.
    let mut merged = existing.as_object().cloned().unwrap_or_default();
    let mut merged_hooks =
        merged.get("hooks").and_then(|v| v.as_object()).cloned().unwrap_or_default();

    for (event, our_entries) in &our_hooks {
        // Drop any pre-existing ainb-managed entries for this event.
        let kept: Vec<serde_json::Value> = merged_hooks
            .get(event)
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter(|entry| !is_ainb_managed_entry(entry)).cloned().collect())
            .unwrap_or_default();
        let mut new_entries = kept;
        if let Some(arr) = our_entries.as_array() {
            for entry in arr {
                let mut tagged = entry.clone();
                if let Some(obj) = tagged.as_object_mut() {
                    obj.insert("_ainb_managed".to_string(), serde_json::Value::Bool(true));
                }
                new_entries.push(tagged);
            }
        }
        merged_hooks.insert(event.clone(), serde_json::Value::Array(new_entries));
    }
    merged.insert("hooks".to_string(), serde_json::Value::Object(merged_hooks));

    let text = serde_json::to_string_pretty(&serde_json::Value::Object(merged))?;
    // Atomic write (LOW-8): hooks.json is durable state Codex reads on every
    // invocation; a torn file would break hook dispatch. Swap via temp + rename.
    write_atomic(&hooks_json, &text)
        .with_context(|| format!("writing {}", hooks_json.display()))?;
    info!(hooks_json = %hooks_json.display(), "installed codex hooks");
    Ok(hooks_json)
}

/// Path to the ainb-owned Copilot drop-in file under an explicit home
/// root. Copilot loads every `*.json` in `~/.copilot/hooks/` and
/// combines them, so ainb writes one standalone file here rather than
/// merging into a shared config (the Codex strategy). Parameterised on
/// `home` — never `dirs::home_dir()` — so the temp-HOME tests can
/// redirect it.
fn copilot_dropin(home: &Path) -> PathBuf {
    home.join(".copilot").join("hooks").join("ainb.json")
}

/// Install Copilot hooks: write our drop-in verbatim to
/// `<home>/.copilot/hooks/ainb.json`. Unlike Codex, this is a whole-file
/// write of a standalone drop-in — we never merge into a shared config,
/// and uninstall deletes just this one file (sibling drop-ins from the
/// user or other tools are untouched). Substitutes the
/// `__AINB_HOOK_SCRIPT__` placeholder with the canonical script path.
fn install_copilot(home: &Path, hook_script_canonical: &Path) -> Result<PathBuf> {
    let dropin = copilot_dropin(home);
    if let Some(parent) = dropin.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    // JSON-encode the path to escape backslashes, quotes, etc., then strip
    // the surrounding `"…"` to get just the escaped interior for substitution.
    // Round-trip through serde_json afterwards to normalise formatting and
    // prove the final JSON is valid before it lands on disk.
    let path_json = serde_json::to_string(&hook_script_canonical.to_string_lossy())?;
    let path_escaped = path_json.trim_matches('"');
    let resolved = COPILOT_HOOKS_TEMPLATE.replace("__AINB_HOOK_SCRIPT__", path_escaped);
    let value: serde_json::Value = serde_json::from_str(&strip_line_comments(&resolved))
        .context("parsing embedded copilot hooks template")?;
    let text = serde_json::to_string_pretty(&value)?;
    std::fs::write(&dropin, text).with_context(|| format!("writing {}", dropin.display()))?;
    info!(dropin = %dropin.display(), "installed copilot hooks");
    Ok(dropin)
}

/// Path to the ainb-owned Antigravity drop-in file under an explicit home root.
fn antigravity_dropin(home: &Path) -> PathBuf {
    home.join(".gemini").join("antigravity-cli").join("hooks").join("ainb.json")
}

/// Install Antigravity hooks: write our drop-in verbatim to
/// `<home>/.gemini/antigravity-cli/hooks/ainb.json`. Whole-file write of a standalone
/// drop-in — we never merge into a shared config, and uninstall deletes just this one
/// file. Substitutes `__AINB_HOOK_SCRIPT__` placeholder with canonical script path.
fn install_antigravity(home: &Path, hook_script_canonical: &Path) -> Result<PathBuf> {
    let dropin = antigravity_dropin(home);
    if let Some(parent) = dropin.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }

    let path_json = serde_json::to_string(&hook_script_canonical.to_string_lossy())?;
    let path_escaped = path_json.trim_matches('"');
    let resolved = ANTIGRAVITY_HOOKS_TEMPLATE.replace("__AINB_HOOK_SCRIPT__", path_escaped);
    let value: serde_json::Value = serde_json::from_str(&strip_line_comments(&resolved))
        .context("parsing embedded antigravity hooks template")?;
    let text = serde_json::to_string_pretty(&value)?;
    std::fs::write(&dropin, text).with_context(|| format!("writing {}", dropin.display()))?;
    info!(dropin = %dropin.display(), "installed antigravity hooks");
    Ok(dropin)
}

fn is_ainb_managed_entry(entry: &serde_json::Value) -> bool {
    entry.get("_ainb_managed").and_then(|v| v.as_bool()).unwrap_or(false)
        || entry
            .get("hooks")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|s| s.contains("AINB_AGENT=") && s.contains("notify.sh"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
}

fn strip_line_comments(s: &str) -> String {
    // Strip lines that are pure `//` comments (with optional leading
    // whitespace). JSON doesn't formally support them, but Codex's
    // own `~/.codex/hooks.json` sometimes carries explanatory `//`
    // markers — and we ourselves embed one in the template's `"//"`
    // key. That's a valid JSON key so it survives parse anyway; this
    // routine only strips line-comment lines.
    s.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Uninstall for one or more agents. Removes plugin files, strips
/// the codex managed block, and clears the install record. The
/// canonical hook script is removed only when no agents remain.
pub fn uninstall(paths: &Paths, agents: &[Agent]) -> Result<()> {
    let mut record = InstallRecord::load(paths)?;
    for agent in agents {
        match agent {
            Agent::Claude => {
                // Mirror of install: unregister the marketplace plugin via
                // the `claude` CLI (best-effort).
                unregister_claude_plugin();
                // Legacy cleanup: remove a hand-dropped plugin dir left by
                // older installs, if one is still recorded / present.
                if let Some(dir) = record.claude_plugin_dir.take() {
                    if dir.exists() {
                        std::fs::remove_dir_all(&dir).with_context(|| {
                            format!("removing legacy claude plugin dir {}", dir.display())
                        })?;
                    }
                }
                record.agents.retain(|a| *a != Agent::Claude);
            }
            Agent::Codex => {
                if let Some(hooks_json) = record.codex_hooks_json.take() {
                    if hooks_json.exists() {
                        strip_codex_managed_entries(&hooks_json)?;
                    }
                }
                record.agents.retain(|a| *a != Agent::Codex);
            }
            Agent::Copilot => {
                // We own a whole standalone drop-in file, so uninstall
                // is a single-file delete — never `remove_dir_all`, which
                // would clobber sibling drop-ins from the user or other
                // tools in `~/.copilot/hooks/`.
                if let Some(dropin) = record.copilot_hooks_json.take() {
                    if dropin.exists() {
                        std::fs::remove_file(&dropin).with_context(|| {
                            format!("removing copilot drop-in {}", dropin.display())
                        })?;
                    }
                }
                record.agents.retain(|a| *a != Agent::Copilot);
            }
            Agent::Antigravity => {
                if let Some(dropin) = record.antigravity_hooks_json.take() {
                    if dropin.exists() {
                        std::fs::remove_file(&dropin).with_context(|| {
                            format!("removing antigravity drop-in {}", dropin.display())
                        })?;
                    }
                }
                record.agents.retain(|a| *a != Agent::Antigravity);
            }
            // Unknown agents are owned by a newer build; leave record intact.
            Agent::Unknown => {}
        }
    }
    record.save(paths)?;
    // If no agents remain installed, the canonical hook script + the
    // daemon's runtime files are scaffolding for nothing — leave them
    // alone (the daemon may still be useful for direct UnixStream
    // producers), but warn if a daemon is still running.
    if record.agents.is_empty() {
        warn!("all agents uninstalled; ainb-notifyd may still be running");
    }
    Ok(())
}

fn strip_codex_managed_entries(hooks_json: &Path) -> Result<()> {
    let text = std::fs::read_to_string(hooks_json)?;
    let mut value: serde_json::Value = serde_json::from_str(&strip_line_comments(&text))
        .with_context(|| format!("parsing {}", hooks_json.display()))?;
    if let Some(hooks_obj) = value
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(|v| v.as_object_mut())
    {
        for (_event, entries) in hooks_obj.iter_mut() {
            if let Some(arr) = entries.as_array_mut() {
                arr.retain(|e| !is_ainb_managed_entry(e));
            }
        }
        // Drop now-empty event arrays so we don't leave noise.
        hooks_obj.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(false));
    }
    let out = serde_json::to_string_pretty(&value)?;
    // Atomic write (LOW-8): same durability concern as install_codex — a torn
    // hooks.json after a strip would leave Codex with an unparseable file.
    write_atomic(hooks_json, &out)?;
    Ok(())
}

/// One row in `ainb-notifyd status` output.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StatusRow {
    /// Agent name (`claude` / `codex`).
    pub agent: String,
    /// Whether the install record lists this agent.
    pub installed: bool,
    /// Whether the canonical hook script is present and executable.
    pub hook_script_ok: bool,
    /// Whether the daemon socket appears live.
    pub socket_ok: bool,
    /// Last event ts (string) — empty if no events yet.
    pub last_event: String,
}

/// Build status rows for every known agent.
pub fn status(paths: &Paths) -> Result<Vec<StatusRow>> {
    let record = InstallRecord::load(paths)?;
    let hook = canonical_hook_script(paths);
    let hook_script_ok = hook.exists()
        && std::fs::metadata(&hook)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    let socket_ok = paths.socket.exists();
    // Latest event across all agents (we report it on every row for
    // ease of grep).
    let last_event = if paths.db.exists() {
        crate::store::Store::open(&paths.db)
            .ok()
            .and_then(|s| s.latest().ok().flatten())
            .map(|r| format!("{} ({})", r.raw_event, r.agent))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let mut rows = Vec::new();
    for agent in Agent::ALL {
        rows.push(StatusRow {
            agent: agent.name().to_string(),
            installed: record.agents.contains(agent),
            hook_script_ok,
            socket_ok,
            last_event: last_event.clone(),
        });
    }
    Ok(rows)
}

/// One installed agent's hook wiring state.
#[derive(Debug, Clone, Serialize)]
pub struct HookAgentHealth {
    /// Agent name (`claude`, `codex`, or `copilot`).
    pub agent: String,
    /// Whether the persistent install record lists this agent.
    pub installed: bool,
    /// Whether the agent's on-disk wiring still points at the canonical hook.
    /// Claude's marketplace registration is performed by its CLI and has no
    /// stable local config file to inspect, so its recorded installation is the
    /// available local proof.
    pub wiring_ready: bool,
    /// Short explanation suitable for a status surface.
    pub detail: String,
}

/// One actionable hook-health problem.
#[derive(Debug, Clone, Serialize)]
pub struct HookHealthIssue {
    /// Component with the problem, for example `hook script` or `Codex`.
    pub component: String,
    /// Plain-language description of what was detected.
    pub message: String,
    /// Exact safe repair command.
    pub repair: String,
}

/// Complete local health report for the shared `ainb-hooks` runtime.
///
/// This deliberately checks the files that make a hook executable, rather
/// than trusting a historical install record alone. It does not spawn agent
/// CLIs: callers may run it repeatedly from a TUI background collector.
#[derive(Debug, Clone, Serialize)]
pub struct HookHealth {
    /// Plugin version embedded in the running `ainb` binary.
    pub bundled_version: String,
    /// Version recorded when hooks were last installed, if any.
    pub installed_version: Option<String>,
    /// Whether installed hooks are at least as new as this binary's hooks.
    pub version_current: bool,
    /// Canonical extracted hook script location.
    pub script_path: PathBuf,
    /// Whether the canonical hook script exists and is executable.
    pub script_ready: bool,
    /// Actual `ainb` executable named by the extracted hook binary pointer.
    pub hook_binary: Option<PathBuf>,
    /// Whether the hook follows a package-manager launcher, an exact dev
    /// target, or a pre-1.21 legacy pointer.
    pub hook_binary_mode: Option<HookBinaryMode>,
    /// Whether the hook binary pointer resolves to an executable file.
    pub hook_binary_ready: bool,
    /// The `ainb` doing the reporting. Shown beside [`Self::hook_binary`] so a
    /// pointer aimed somewhere other than the binary you are looking at is
    /// visible rather than something you have to go and read off disk.
    pub running_binary: Option<PathBuf>,
    /// Per-agent installation and wiring state.
    pub agents: Vec<HookAgentHealth>,
    /// Whether notifyd's delivery socket accepted a connection now.
    pub notify_socket_live: bool,
    /// Whether approval broker's socket accepted a connection now.
    pub approve_socket_live: bool,
    /// Most-recent persisted hook event, when one exists.
    pub last_event: Option<String>,
    /// Problems that need action. Idle sockets are reported above, not as a
    /// failure: notifyd intentionally starts lazily on the first hook event.
    pub issues: Vec<HookHealthIssue>,
}

/// Inspect local hook wiring without changing it.
#[must_use]
pub fn hook_health(paths: &Paths) -> HookHealth {
    let bundled_version = embedded_plugin_version();
    let record = InstallRecord::load(paths);
    let (record, record_error) = match record {
        Ok(record) => (record, None),
        Err(error) => (InstallRecord::default(), Some(error.to_string())),
    };
    let script_path = canonical_hook_script(paths);
    let script_ready = is_executable(&script_path);
    let hook_target = read_hook_binary_target(paths);
    let hook_binary = hook_target.as_ref().map(|target| target.path.clone());
    let hook_binary_mode = hook_target.as_ref().map(|target| target.mode);
    // The metadata is diagnostic only. The shell hook executes the one-line
    // `ainb-bin` pointer, so a healthy metadata file cannot mask a missing
    // pointer file.
    let hook_binary_ready =
        canonical_hook_bin(paths).is_file() && hook_binary.as_deref().is_some_and(is_executable);
    let installed_any = !record.agents.is_empty();
    let version_current = record
        .plugin_version
        .as_deref()
        .is_some_and(|installed| parse_semver(installed) >= parse_semver(&bundled_version));

    let agents = Agent::ALL
        .iter()
        .map(|agent| agent_health(*agent, &record, &script_path))
        .collect::<Vec<_>>();

    let mut issues = Vec::new();
    if let Some(error) = record_error {
        issues.push(HookHealthIssue {
            component: "install record".to_string(),
            message: format!("cannot read install.json: {error}"),
            repair: "ainb doctor --fix-hooks".to_string(),
        });
    } else if !installed_any {
        issues.push(HookHealthIssue {
            component: "hooks".to_string(),
            message: "not installed for any agent".to_string(),
            repair: "ainb notifyd install --all".to_string(),
        });
    } else {
        if !version_current {
            let installed = record.plugin_version.as_deref().unwrap_or("unknown");
            issues.push(HookHealthIssue {
                component: "version".to_string(),
                message: format!("installed {installed}; ainb bundles {bundled_version}"),
                repair: "ainb doctor --fix-hooks".to_string(),
            });
        }
        if !script_ready {
            issues.push(HookHealthIssue {
                component: "hook script".to_string(),
                message: format!("{} is missing or not executable", script_path.display()),
                repair: "ainb doctor --fix-hooks".to_string(),
            });
        }
        if !hook_binary_ready {
            // A dead dev pointer is the common case (the build tree it named
            // was deleted) and repair now reaches past it to the installed
            // ainb, so the fix is the same keypress as every other pointer
            // fault — not a lecture about rebuilding a tree that is gone.
            let repair =
                "ainb doctor --fix-hooks, or press I in Daemons, to repoint at the installed ainb"
                    .to_string();
            issues.push(HookHealthIssue {
                component: "hook binary".to_string(),
                message: hook_binary.as_ref().map_or_else(
                    || "ainb-bin pointer is missing or empty".to_string(),
                    |target| format!("{} is missing or not executable", target.display()),
                ),
                repair,
            });
        }
        if hook_binary_mode == Some(HookBinaryMode::Legacy) {
            issues.push(HookHealthIssue {
                component: "hook binary".to_string(),
                message: "legacy binary pointer; migrate to stable launcher".to_string(),
                repair: "ainb doctor --fix-hooks".to_string(),
            });
        }
        for agent in agents.iter().filter(|agent| agent.installed && !agent.wiring_ready) {
            issues.push(HookHealthIssue {
                component: agent.agent.clone(),
                message: agent.detail.clone(),
                repair: "ainb doctor --fix-hooks".to_string(),
            });
        }
    }

    HookHealth {
        bundled_version,
        installed_version: record.plugin_version,
        version_current,
        script_path,
        script_ready,
        hook_binary,
        hook_binary_mode,
        hook_binary_ready,
        running_binary: std::env::current_exe().ok(),
        agents,
        notify_socket_live: socket_live(&paths.socket),
        approve_socket_live: socket_live(&paths.approve_socket),
        last_event: latest_event(paths),
        issues,
    }
}

fn agent_health(agent: Agent, record: &InstallRecord, script: &Path) -> HookAgentHealth {
    let installed = record.agents.contains(&agent);
    let (wiring_ready, detail) = match agent {
        // Claude owns marketplace state. Its CLI confirms registration during
        // install, but does not expose a stable config file that this fast,
        // local-only probe could safely inspect every two seconds.
        Agent::Claude => (
            installed,
            if installed {
                "marketplace install recorded".to_string()
            } else {
                "not installed".to_string()
            },
        ),
        Agent::Codex => {
            config_references_script(record.codex_hooks_json.as_deref(), script, "hooks.json")
        }
        Agent::Copilot => {
            config_references_script(record.copilot_hooks_json.as_deref(), script, "drop-in")
        }
        Agent::Antigravity => config_references_script(
            record.antigravity_hooks_json.as_deref(),
            script,
            "antigravity drop-in",
        ),
        Agent::Unknown => (false, "unknown agent".to_string()),
    };
    HookAgentHealth {
        agent: agent.name().to_string(),
        installed,
        wiring_ready,
        detail,
    }
}

fn config_references_script(
    config: Option<&Path>,
    script: &Path,
    config_name: &str,
) -> (bool, String) {
    let Some(config) = config else {
        return (false, format!("{config_name} path is not recorded"));
    };
    let Ok(text) = std::fs::read_to_string(config) else {
        return (
            false,
            format!("{} is missing or unreadable", config.display()),
        );
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&strip_line_comments(&text)) else {
        return (false, format!("{} is not valid JSON", config.display()));
    };
    let expected = script.to_string_lossy();
    if json_has_hook_command(&value, &expected) {
        (true, format!("{} points at shared hook", config.display()))
    } else {
        (
            false,
            format!("{} does not point at shared hook", config.display()),
        )
    }
}

fn json_has_hook_command(value: &serde_json::Value, expected: &str) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object
                .get("command")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|command| command.contains(expected))
                || object.values().any(|value| json_has_hook_command(value, expected))
        }
        serde_json::Value::Array(values) => {
            values.iter().any(|value| json_has_hook_command(value, expected))
        }
        _ => false,
    }
}

fn socket_live(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

fn latest_event(paths: &Paths) -> Option<String> {
    paths.db.exists().then_some(())?;
    crate::store::Store::open(&paths.db)
        .ok()?
        .latest()
        .ok()?
        .map(|row| format!("{} ({})", row.raw_event, row.agent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fake_home() -> TempDir {
        let dir = TempDir::new().unwrap();
        dir
    }

    fn paths_under_home(home: &Path) -> Paths {
        Paths::under(home.join(".agents-in-a-box"))
    }

    #[test]
    fn write_atomic_replaces_existing_file_completely() {
        // LOW-8: a second atomic write fully replaces prior content (no torn
        // overlay), and the destination ends up with exactly the new bytes.
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("hooks.json");
        write_atomic(&target, "{\"v\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "{\"v\":1}");
        write_atomic(&target, "{\"v\":2,\"longer\":true}").unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "{\"v\":2,\"longer\":true}"
        );
    }

    #[test]
    fn write_atomic_leaves_no_temp_file_behind() {
        // The temp sibling must be renamed away (or cleaned up), never left as
        // litter next to the target.
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("install.json");
        write_atomic(&target, "payload").unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
    }

    #[test]
    fn install_json_with_copilot_and_unknown_agents_parses() {
        // Regression: an install.json written by a newer, Copilot-capable
        // build (or any future agent) must not hard-fail deserialization.
        // A failed parse used to reset the record to empty -> re-offer the
        // install prompt on every launch, and break "Install" with
        // `parsing .../install.json`.
        let json = r#"{
            "agents": ["claude", "codex", "copilot", "gemini"],
            "hook_script": "/home/u/.agents-in-a-box/hooks/ainb-notify.sh",
            "codex_hooks_json": "/home/u/.codex/hooks.json",
            "copilot_hooks_json": "/home/u/.copilot/hooks/ainb.json",
            "plugin_version": "0.2.0",
            "prompt_dismissed": false
        }"#;
        let rec: InstallRecord = serde_json::from_str(json).expect("record must parse");
        assert!(rec.agents.contains(&Agent::Claude));
        assert!(rec.agents.contains(&Agent::Codex));
        assert!(rec.agents.contains(&Agent::Copilot));
        // The unknown "gemini" maps to the catch-all instead of erroring.
        assert!(rec.agents.contains(&Agent::Unknown));
        // Non-empty agents => prompt_state does NOT offer install every launch.
        assert!(!rec.agents.is_empty());
        // Copilot path is preserved on the record.
        assert_eq!(
            rec.copilot_hooks_json.as_deref(),
            Some(Path::new("/home/u/.copilot/hooks/ainb.json"))
        );
        // Copilot survives a save/load round-trip (no data loss).
        let reser = serde_json::to_string(&rec).unwrap();
        let again: InstallRecord = serde_json::from_str(&reser).unwrap();
        assert!(again.agents.contains(&Agent::Copilot));
    }

    #[test]
    fn extract_hook_script_writes_executable_file() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let dest = extract_hook_script(&p).unwrap();
        assert!(dest.exists());
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "script must be executable: mode={mode:o}"
        );
        let content = std::fs::read_to_string(&dest).unwrap();
        assert!(content.contains("ainb-hooks"));
    }

    #[test]
    fn extract_hook_bin_records_mode_and_executable_target() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let dest = extract_hook_bin(&p).unwrap();
        let target = read_hook_binary_target(&p).expect("pointer metadata");
        assert_eq!(
            std::fs::read_to_string(dest).unwrap().trim(),
            target.path.display().to_string()
        );
        assert!(target.path.is_absolute());
        assert_ne!(target.mode, HookBinaryMode::Legacy);
    }

    #[test]
    fn homebrew_cellar_binary_uses_stable_bin_launcher() {
        let dir = fake_home();
        let root = dir.path().join("homebrew");
        let cellar_bin = root.join("Cellar/ainb/1.20.8/libexec/ainb");
        let launcher = root.join("bin/ainb");
        std::fs::create_dir_all(cellar_bin.parent().unwrap()).unwrap();
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::write(&cellar_bin, "#!/bin/sh\n").unwrap();
        std::fs::write(&launcher, "#!/bin/sh\n").unwrap();
        for path in [&cellar_bin, &launcher] {
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
        }

        let target = hook_binary_target(cellar_bin, None);
        assert_eq!(target.mode, HookBinaryMode::Release);
        assert_eq!(target.path, launcher);
    }

    #[test]
    fn explicit_dev_binary_stays_exact_but_explicit_cellar_is_normalised() {
        let dir = fake_home();
        let dev = dir.path().join("checkout/target/debug/ainb");
        let dev_target = hook_binary_target(
            PathBuf::from("/unused/ainb"),
            Some(dev.to_str().expect("UTF-8 temporary path")),
        );
        assert_eq!(dev_target.mode, HookBinaryMode::Dev);
        assert_eq!(dev_target.path, dev);

        let root = dir.path().join("homebrew");
        let cellar = root.join("Cellar/ainb/1.21.0/libexec/ainb");
        let launcher = root.join("bin/ainb");
        std::fs::create_dir_all(cellar.parent().unwrap()).unwrap();
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::write(&cellar, "#!/bin/sh\\n").unwrap();
        std::fs::write(&launcher, "#!/bin/sh\\n").unwrap();
        for path in [&cellar, &launcher] {
            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
        }
        let cellar_target = hook_binary_target(
            PathBuf::from("/unused/ainb"),
            Some(cellar.to_str().expect("UTF-8 temporary path")),
        );
        assert_eq!(cellar_target.mode, HookBinaryMode::Release);
        assert_eq!(cellar_target.path, launcher);
    }

    /// The reported bug: hooks installed while running a worktree
    /// `target/debug/ainb` stamped that doomed path into the pointer, and it
    /// stayed there after the worktree was deleted. Install intent must reach
    /// past the running binary to the installed one.
    #[test]
    fn install_prefers_the_installed_launcher_over_a_dev_build() {
        let dev = PathBuf::from("/w/tree/target/debug/ainb");
        let installed = PathBuf::from("/home/u/.local/bin/ainb");
        let found = installed.clone();

        let target = resolve_hook_binary(dev.clone(), None, BinaryIntent::Install, &|_| {
            Some(found.clone())
        });

        assert_eq!(target.path, installed);
        assert_eq!(target.mode, HookBinaryMode::Release);
    }

    /// Pinning is the escape hatch, so it must NOT consult the installed
    /// search — otherwise there would be no way to test a local build's hooks.
    #[test]
    fn pinning_keeps_the_running_binary_even_when_one_is_installed() {
        let dev = PathBuf::from("/w/tree/target/debug/ainb");
        let installed = PathBuf::from("/home/u/.local/bin/ainb");

        let target = resolve_hook_binary(dev.clone(), None, BinaryIntent::PinRunning, &|_| {
            Some(installed.clone())
        });

        assert_eq!(target.path, dev);
        assert_eq!(target.mode, HookBinaryMode::Dev);
    }

    /// With nothing installed there is nothing better to point at, so install
    /// falls back to the running binary rather than writing a dead path.
    #[test]
    fn install_falls_back_to_the_running_binary_when_nothing_is_installed() {
        let dev = PathBuf::from("/w/tree/target/debug/ainb");

        let target = resolve_hook_binary(dev.clone(), None, BinaryIntent::Install, &|_| None);

        assert_eq!(target.path, dev);
        assert_eq!(target.mode, HookBinaryMode::Dev);
    }

    /// An explicit `AINB_BIN` is the operator's stated intent and outranks both
    /// policies.
    #[test]
    fn explicit_binary_wins_under_either_intent() {
        let dev = PathBuf::from("/w/tree/target/debug/ainb");
        let installed = PathBuf::from("/home/u/.local/bin/ainb");
        let chosen = PathBuf::from("/opt/custom/ainb");

        for intent in [BinaryIntent::Install, BinaryIntent::PinRunning] {
            let target =
                resolve_hook_binary(dev.clone(), Some("/opt/custom/ainb"), intent, &|_| {
                    Some(installed.clone())
                });
            assert_eq!(target.path, chosen, "intent {intent:?} ignored AINB_BIN");
        }
    }

    /// `~/.local/bin` is a first-class install prefix (it is where `ainb`
    /// installs itself on a machine with no Homebrew), so a binary there is a
    /// stable launcher — not an unclassified `Direct` path that repair treats
    /// as no better than a worktree build.
    #[test]
    fn local_bin_launcher_is_release_mode() {
        let dir = fake_home();
        let launcher = dir.path().join(".local/bin/ainb");
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::write(&launcher, "#!/bin/sh\n").unwrap();
        let mut permissions = std::fs::metadata(&launcher).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&launcher, permissions).unwrap();

        let target = hook_binary_target(
            PathBuf::from("/unused/ainb"),
            Some(launcher.to_str().expect("UTF-8 temporary path")),
        );
        assert_eq!(target.mode, HookBinaryMode::Release);
        assert_eq!(target.path, launcher);
    }

    #[test]
    fn legacy_cellar_pointer_migrates_but_missing_dev_pointer_does_not() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let root = dir.path().join("homebrew");
        let old = root.join("Cellar/ainb/1.20.8/libexec/ainb");
        let launcher = root.join("bin/ainb");
        std::fs::create_dir_all(launcher.parent().unwrap()).unwrap();
        std::fs::write(&launcher, "#!/bin/sh\n").unwrap();
        let mut permissions = std::fs::metadata(&launcher).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&launcher, permissions).unwrap();
        std::fs::create_dir_all(canonical_hook_bin(&p).parent().unwrap()).unwrap();
        std::fs::write(canonical_hook_bin(&p), format!("{}\n", old.display())).unwrap();

        assert!(auto_repair_hook_binary(&p).unwrap());
        let migrated = read_hook_binary_target(&p).unwrap();
        assert_eq!(migrated.mode, HookBinaryMode::Release);
        assert_eq!(migrated.path, launcher);

        let dev = dir.path().join("project/target/debug/ainb");
        std::fs::write(canonical_hook_bin(&p), format!("{}\n", dev.display())).unwrap();
        std::fs::remove_file(canonical_hook_bin_metadata(&p)).unwrap();
        assert!(!auto_repair_hook_binary(&p).unwrap());
        assert_eq!(
            read_hook_binary_target(&p).unwrap().mode,
            HookBinaryMode::Legacy
        );
    }

    #[test]
    fn install_under_home_records_claude_without_dropping_a_dir() {
        // Claude is wired via the marketplace CLI (done in `install`),
        // not by dropping a plugin dir — so install_under_home must record
        // the agent but leave `claude_plugin_dir` unset and create nothing
        // under ~/.claude/plugins/.
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let record = install_under_home(&p, dir.path(), &[Agent::Claude]).unwrap();
        assert!(record.agents.contains(&Agent::Claude));
        assert!(
            record.claude_plugin_dir.is_none(),
            "must not drop a plugin dir"
        );
        assert!(
            !dir.path().join(".claude/plugins/ainb-hooks").exists(),
            "no hand-dropped plugin dir should be created"
        );
    }

    #[test]
    fn install_codex_creates_managed_block_in_hooks_json() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let hooks_json_path = codex_dir.join("hooks.json");
        std::fs::write(
            &hooks_json_path,
            r#"{
                "hooks": {
                    "SessionStart": [
                        {"hooks": [{"type": "command", "command": "echo user-hook"}]}
                    ]
                }
            }"#,
        )
        .unwrap();
        let record = install_under_home(&p, dir.path(), &[Agent::Codex]).unwrap();
        assert!(record.codex_hooks_json.is_some());
        let text = std::fs::read_to_string(&hooks_json_path).unwrap();
        assert!(text.contains("echo user-hook"), "user hook lost: {text}");
        assert!(
            text.contains("AINB_AGENT=codex"),
            "managed block missing: {text}"
        );
        assert!(
            text.contains("notify.sh"),
            "managed block lacks script: {text}"
        );
        assert!(
            text.contains("\"PermissionRequest\""),
            "Codex managed block should use Codex's native permission hook: {text}"
        );
        assert!(
            !text.contains("\"Notification\""),
            "Codex managed block should not use Claude/Copilot Notification hook: {text}"
        );
        assert!(
            !text.contains("__AINB_STALL_GUARD__"),
            "stall guard placeholder left unresolved: {text}"
        );
        assert!(
            text.contains("stall_guard.py"),
            "Codex Stop must also run the stall guard: {text}"
        );
        assert!(
            canonical_stall_guard(&p).exists(),
            "stall guard must be extracted next to notify.sh"
        );
    }

    #[test]
    fn codex_template_registers_every_documented_cli_hook() {
        let template: serde_json::Value = serde_json::from_str(CODEX_HOOKS_TEMPLATE).unwrap();
        let hooks = template["hooks"].as_object().expect("hooks object");
        let events = hooks.keys().map(String::as_str).collect::<std::collections::BTreeSet<_>>();
        let expected = [
            "PermissionRequest",
            "PostCompact",
            "PostToolUse",
            "PreCompact",
            "PreToolUse",
            "SessionEnd",
            "SessionStart",
            "Stop",
            "SubagentStart",
            "SubagentStop",
            "UserPromptSubmit",
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(events, expected);
    }

    #[test]
    fn codex_template_respects_codex_hook_timeout_caps() {
        // Codex hard-clamps the SessionEnd hook to 3s and surfaces the clamp
        // as an error item on every session start. Anything above the cap is a
        // user-visible startup error, not a silent adjustment.
        let template: serde_json::Value = serde_json::from_str(CODEX_HOOKS_TEMPLATE).unwrap();
        let caps = [("SessionEnd", 3u64)];
        for (event, cap) in caps {
            let entries = template["hooks"][event].as_array().expect("event entries");
            for entry in entries {
                for hook in entry["hooks"].as_array().expect("hook list") {
                    let timeout = hook["timeout"].as_u64().expect("timeout");
                    assert!(
                        timeout <= cap,
                        "{event} hook timeout {timeout}s exceeds Codex's {cap}s cap; \
                         Codex clamps it and prints a startup error"
                    );
                }
            }
        }
    }

    #[test]
    fn claude_manifest_registers_every_documented_hook() {
        let manifest: serde_json::Value = serde_json::from_str(CLAUDE_PLUGIN_JSON).unwrap();
        let hooks = manifest["hooks"].as_object().expect("hooks object");
        let events = hooks.keys().map(String::as_str).collect::<std::collections::BTreeSet<_>>();
        let expected = [
            "SessionStart",
            "Setup",
            "InstructionsLoaded",
            "UserPromptSubmit",
            "UserPromptExpansion",
            "MessageDisplay",
            "PreToolUse",
            "PermissionRequest",
            "PostToolUse",
            "PostToolUseFailure",
            "PostToolBatch",
            "PermissionDenied",
            "Notification",
            "SubagentStart",
            "SubagentStop",
            "TaskCreated",
            "TaskCompleted",
            "Stop",
            "StopFailure",
            "TeammateIdle",
            "ConfigChange",
            "CwdChanged",
            "FileChanged",
            "WorktreeCreate",
            "WorktreeRemove",
            "PreCompact",
            "PostCompact",
            "SessionEnd",
            "Elicitation",
            "ElicitationResult",
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(events, expected);
        for entries in hooks.values() {
            assert!(
                entries.as_array().is_some_and(|entries| entries.iter().all(|entry| {
                    entry["hooks"].as_array().is_some_and(|hooks| {
                        hooks.iter().all(|hook| {
                            hook["command"].as_str().is_some_and(|command| {
                                // Every telemetry hook routes through notify.sh
                                // carrying the agent tag. The stall guard is the
                                // one entry that is not telemetry: it reads the
                                // Stop payload and may answer with a block.
                                command.contains("AINB_AGENT=claude")
                                    || command.contains("stall_guard.py")
                            })
                        })
                    })
                }))
            );
        }
    }

    #[test]
    fn uninstall_strips_codex_managed_block_but_keeps_user_hooks() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let codex_dir = dir.path().join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        std::fs::write(
            codex_dir.join("hooks.json"),
            r#"{
                "hooks": {
                    "SessionStart": [
                        {"hooks": [{"type": "command", "command": "echo user-hook"}]}
                    ]
                }
            }"#,
        )
        .unwrap();
        install_under_home(&p, dir.path(), &[Agent::Codex]).unwrap();
        uninstall(&p, &[Agent::Codex]).unwrap();
        let text = std::fs::read_to_string(codex_dir.join("hooks.json")).unwrap();
        assert!(text.contains("echo user-hook"));
        assert!(!text.contains("AINB_AGENT=codex"));
    }

    #[test]
    fn install_copilot_writes_native_dropin_file() {
        // `--copilot` writes a STANDALONE drop-in at
        // ~/.copilot/hooks/ainb.json in Copilot's native format: a flat
        // per-event array (NOT Codex's two-level {matcher, hooks:[]}),
        // camelCase events, top-level version:1, timeoutSec (not timeout),
        // and AINB_AGENT=copilot with the placeholder substituted away.
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let record = install_under_home(&p, dir.path(), &[Agent::Copilot]).unwrap();

        let dropin = dir.path().join(".copilot/hooks/ainb.json");
        assert_eq!(record.copilot_hooks_json.as_deref(), Some(dropin.as_path()));
        assert!(
            dropin.exists(),
            "drop-in file must exist at {}",
            dropin.display()
        );

        let raw = std::fs::read_to_string(&dropin).unwrap();
        assert!(
            !raw.contains("__AINB_HOOK_SCRIPT__"),
            "placeholder survived: {raw}"
        );
        assert!(
            raw.contains(&record.hook_script.to_string_lossy().to_string()),
            "drop-in lacks resolved script path: {raw}"
        );

        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            v["version"],
            serde_json::json!(1),
            "missing version:1: {raw}"
        );
        let hooks = v["hooks"].as_object().expect("hooks object");

        for event in ["notification", "agentStop"] {
            let arr = hooks
                .get(event)
                .and_then(|e| e.as_array())
                .unwrap_or_else(|| panic!("event {event} missing or not an array: {raw}"));
            assert_eq!(arr.len(), 1, "expected 1 entry for {event}: {raw}");
            let entry = &arr[0];
            // FLAT entry: type/command/timeoutSec live directly on the
            // entry (not nested under a `hooks` array with a `matcher`).
            assert_eq!(entry["type"], serde_json::json!("command"), "{raw}");
            assert!(
                entry.get("hooks").is_none(),
                "copilot entries must be flat, not nested {{matcher, hooks:[]}}: {raw}"
            );
            assert!(
                entry.get("matcher").is_none(),
                "copilot entries carry no matcher: {raw}"
            );
            assert_eq!(entry["timeoutSec"], serde_json::json!(5), "{raw}");
            assert!(
                entry.get("timeout").is_none(),
                "copilot uses timeoutSec, not timeout: {raw}"
            );
            let cmd = entry["command"].as_str().expect("command string");
            assert!(
                cmd.contains("AINB_AGENT=copilot"),
                "command not tagged for copilot: {cmd}"
            );
            assert!(cmd.contains("notify.sh"), "command lacks script: {cmd}");
        }
        assert!(
            hooks.get("Notification").is_none(),
            "leaked Codex event: {raw}"
        );
        assert!(hooks.get("Stop").is_none(), "leaked Codex event: {raw}");
    }

    #[test]
    fn uninstall_copilot_removes_dropin_only() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let hooks_dir = dir.path().join(".copilot/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let sibling = hooks_dir.join("other.json");
        std::fs::write(&sibling, r#"{"version":1,"hooks":{}}"#).unwrap();

        install_under_home(&p, dir.path(), &[Agent::Copilot]).unwrap();
        let dropin = hooks_dir.join("ainb.json");
        assert!(dropin.exists());
        assert!(sibling.exists(), "install must not touch sibling drop-ins");

        uninstall(&p, &[Agent::Copilot]).unwrap();
        assert!(!dropin.exists(), "our drop-in must be removed");
        assert!(
            sibling.exists(),
            "uninstall must not touch sibling drop-ins"
        );
        assert!(
            hooks_dir.exists(),
            "uninstall must not remove the hooks dir"
        );

        let record = InstallRecord::load(&p).unwrap();
        assert!(!record.agents.contains(&Agent::Copilot));
    }

    #[test]
    fn install_copilot_overwrites_its_own_dropin_idempotently() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        install_under_home(&p, dir.path(), &[Agent::Copilot]).unwrap();
        install_under_home(&p, dir.path(), &[Agent::Copilot]).unwrap();
        let dropin = dir.path().join(".copilot/hooks/ainb.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dropin).unwrap()).unwrap();
        assert_eq!(v["hooks"]["notification"].as_array().unwrap().len(), 1);
        assert_eq!(v["hooks"]["agentStop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn install_record_roundtrip() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        let mut rec = InstallRecord::default();
        rec.agents.push(Agent::Claude);
        rec.hook_script = PathBuf::from("/x/y/notify.sh");
        rec.save(&p).unwrap();
        let loaded = InstallRecord::load(&p).unwrap();
        assert_eq!(loaded.agents, vec![Agent::Claude]);
        assert_eq!(loaded.hook_script, PathBuf::from("/x/y/notify.sh"));
    }

    #[test]
    fn install_antigravity_writes_native_dropin_file() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let record = install_under_home(&p, dir.path(), &[Agent::Antigravity]).unwrap();

        let dropin = dir.path().join(".gemini/antigravity-cli/hooks/ainb.json");
        assert_eq!(
            record.antigravity_hooks_json.as_deref(),
            Some(dropin.as_path())
        );
        assert!(
            dropin.exists(),
            "drop-in file must exist at {}",
            dropin.display()
        );

        let raw = std::fs::read_to_string(&dropin).unwrap();
        assert!(
            !raw.contains("__AINB_HOOK_SCRIPT__"),
            "placeholder survived: {raw}"
        );
        assert!(
            raw.contains(&record.hook_script.to_string_lossy().to_string()),
            "drop-in lacks resolved script path: {raw}"
        );

        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            v["version"],
            serde_json::json!(1),
            "missing version:1: {raw}"
        );
        let hooks = v["hooks"].as_object().expect("hooks object");

        for event in [
            "SessionStart",
            "SessionEnd",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "Notification",
        ] {
            let arr = hooks
                .get(event)
                .and_then(|e| e.as_array())
                .unwrap_or_else(|| panic!("event {event} missing or not an array: {raw}"));
            assert_eq!(arr.len(), 1, "expected 1 entry for {event}: {raw}");
            let entry = &arr[0];
            assert_eq!(entry["type"], serde_json::json!("command"), "{raw}");
            let cmd = entry["command"].as_str().expect("command string");
            assert!(
                cmd.contains("AINB_AGENT=antigravity"),
                "command not tagged for antigravity: {cmd}"
            );
            assert!(cmd.contains("notify.sh"), "command lacks script: {cmd}");
        }
    }

    #[test]
    fn uninstall_antigravity_removes_dropin_only() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        let hooks_dir = dir.path().join(".gemini/antigravity-cli/hooks");
        std::fs::create_dir_all(&hooks_dir).unwrap();
        let sibling = hooks_dir.join("other.json");
        std::fs::write(&sibling, r#"{"version":1,"hooks":{}}"#).unwrap();

        install_under_home(&p, dir.path(), &[Agent::Antigravity]).unwrap();
        let dropin = hooks_dir.join("ainb.json");
        assert!(dropin.exists());
        assert!(sibling.exists(), "install must not touch sibling drop-ins");

        uninstall(&p, &[Agent::Antigravity]).unwrap();
        assert!(!dropin.exists(), "our drop-in must be removed");
        assert!(
            sibling.exists(),
            "uninstall must not touch sibling drop-ins"
        );
        assert!(
            hooks_dir.exists(),
            "uninstall must not remove the hooks dir"
        );

        let record = InstallRecord::load(&p).unwrap();
        assert!(!record.agents.contains(&Agent::Antigravity));
    }

    #[test]
    fn install_antigravity_overwrites_its_own_dropin_idempotently() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        install_under_home(&p, dir.path(), &[Agent::Antigravity]).unwrap();
        install_under_home(&p, dir.path(), &[Agent::Antigravity]).unwrap();
        let dropin = dir.path().join(".gemini/antigravity-cli/hooks/ainb.json");
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&dropin).unwrap()).unwrap();
        assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert_eq!(v["hooks"]["Stop"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn status_reports_each_known_agent() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        let rows = status(&p).unwrap();
        let names: Vec<_> = rows.iter().map(|r| r.agent.as_str()).collect();
        assert!(names.contains(&"claude"));
        assert!(names.contains(&"codex"));
        assert!(names.contains(&"copilot"));
        assert!(names.contains(&"antigravity"));
        for r in &rows {
            assert!(!r.installed);
            assert!(!r.socket_ok);
        }
    }

    #[test]
    fn hook_health_checks_version_script_binary_and_agent_wiring() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        install_under_home(&p, dir.path(), &[Agent::Codex]).unwrap();

        let health = hook_health(&p);
        assert!(health.version_current);
        assert!(health.script_ready);
        assert!(health.hook_binary_ready);
        let codex = health.agents.iter().find(|agent| agent.agent == "codex").unwrap();
        assert!(codex.installed);
        assert!(codex.wiring_ready, "{}", codex.detail);
        assert!(health.issues.is_empty(), "{:?}", health.issues);

        let altered = dir.path().join("manually-altered-ainb");
        std::fs::write(canonical_hook_bin(&p), format!("{}\n", altered.display())).unwrap();
        let broken = hook_health(&p);
        assert_eq!(broken.hook_binary.as_deref(), Some(altered.as_path()));
        assert_eq!(broken.hook_binary_mode, Some(HookBinaryMode::Legacy));
        assert!(!broken.hook_binary_ready);
        assert!(broken.issues.iter().any(|issue| issue.component == "hook binary"));
    }

    #[test]
    fn embedded_plugin_version_is_parsed_from_manifest() {
        // Must match the version in
        // plugins/ainb-hooks/.claude-plugin/plugin.json.
        let v = embedded_plugin_version();
        assert!(!v.is_empty() && v != "0.0.0", "got {v:?}");
        // It parses as a semver tuple.
        assert!(parse_semver(&v) > (0, 0, 0));
    }

    #[test]
    fn parse_semver_handles_components_and_suffixes() {
        assert_eq!(parse_semver("0.2.0"), (0, 2, 0));
        assert_eq!(parse_semver("v1.2.3"), (1, 2, 3));
        assert_eq!(parse_semver("0.10.0"), (0, 10, 0));
        assert!(
            parse_semver("0.9.0") < parse_semver("0.10.0"),
            "numeric, not lexical"
        );
        assert_eq!(parse_semver("1.0.0-rc1"), (1, 0, 0));
        assert_eq!(parse_semver("garbage"), (0, 0, 0));
    }

    #[test]
    fn prompt_state_offers_install_when_nothing_installed() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        assert_eq!(prompt_state(&p), InstallPrompt::OfferInstall);
    }

    #[test]
    fn prompt_state_silent_after_dismiss() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        dismiss_prompt(&p).unwrap();
        assert_eq!(prompt_state(&p), InstallPrompt::None);
    }

    #[test]
    fn prompt_state_silent_when_up_to_date() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        let mut rec = InstallRecord::default();
        rec.agents.push(Agent::Claude);
        rec.plugin_version = Some(embedded_plugin_version());
        rec.save(&p).unwrap();
        assert_eq!(prompt_state(&p), InstallPrompt::None);
    }

    #[test]
    fn prompt_state_offers_update_on_version_drift() {
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        // Installed an older version than the binary embeds.
        let mut rec = InstallRecord::default();
        rec.agents.push(Agent::Claude);
        rec.plugin_version = Some("0.0.1".to_string());
        rec.save(&p).unwrap();
        match prompt_state(&p) {
            InstallPrompt::OfferUpdate {
                installed,
                embedded,
            } => {
                assert_eq!(installed, "0.0.1");
                assert_eq!(embedded, embedded_plugin_version());
            }
            other => panic!("expected OfferUpdate, got {other:?}"),
        }
    }

    #[test]
    fn prompt_state_treats_missing_version_as_stale() {
        // Records written before the version field existed parse with
        // plugin_version = None → treated as 0.0.0 → offer update.
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        std::fs::create_dir_all(&p.base).unwrap();
        std::fs::write(
            p.base.join("install.json"),
            r#"{"agents":["claude"],"hook_script":"/x/notify.sh"}"#,
        )
        .unwrap();
        assert!(matches!(
            prompt_state(&p),
            InstallPrompt::OfferUpdate { .. }
        ));
    }

    #[test]
    fn install_then_prompt_state_is_none() {
        // End-to-end: install stamps the current version, so an
        // immediate prompt_state is silent.
        let dir = fake_home();
        let p = paths_under_home(dir.path());
        install_under_home(&p, dir.path(), &[Agent::Claude]).unwrap();
        assert_eq!(prompt_state(&p), InstallPrompt::None);
    }
}
