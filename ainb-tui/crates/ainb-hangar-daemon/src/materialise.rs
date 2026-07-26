//! Dispatch-time materialisation of an agent's skills into its per-task env
//! (P6.4).
//!
//! When the daemon claims a task it must hand the provider subprocess the
//! agent's attached skills *on disk*, in the directory layout that provider
//! expects (Claude reads `.claude/skills/<name>/SKILL.md`, Codex reads
//! `.codex/skills/...`, and so on). This module is the seam that, after the git
//! worktree exists and before the provider is spawned, reads
//! [`SkillRepo::skills_for_agent`] and writes every skill bundle to the right
//! place.
//!
//! # Where skills land — and the no-pollute-git invariant
//!
//! The per-task tree is
//! `~/.agents-in-a-box/hangar/workspaces/{ws}/{shortID}/` with `workdir/` (the git
//! worktree) as one child. Provider "home" layouts (Claude/Codex/Cursor) write
//! their skills under the *task root* — a **sibling** of `workdir/` — so the
//! agent's git status stays clean: nothing is written inside the checked-out
//! tree. The provider is then pointed at that home via an env var
//! (`CLAUDE_HOME`, `CODEX_HOME`, `CURSOR_HOME`). Gemini/Default (and Copilot)
//! have no home env var, so their skills are written *inside* `workdir/` at a
//! conventional path; that is their documented layout.
//!
//! # Copy, never symlink
//!
//! Files are **copied**, matching the reference. Symlinks would point back into the
//! database-owned tree (or be dangling on cleanup) and break the provider
//! sandbox's path checks.
//!
//! # Disabled links never materialise (migration 0051, parity #24)
//!
//! There are two independent suppression levers, and this module is where BOTH
//! are honoured — hangar has no live tool registry to gate, so "the skill never
//! lands on disk for that agent" is the faithful equivalent (deviation D1):
//!
//! - **`agent_skill.enabled = 0`** — the link stays attached but stops being
//!   live. [`SkillRepo::skills_for_agent`] already filters it out, so it never
//!   reaches this loop.
//! - **`agent.disabled_runtime_skills`** — a per-agent by-NAME suppression list,
//!   applied here after the read. It can name a skill the agent is not attached
//!   to at all, which is why it cannot live in the junction query.
//!
//! An agent whose every skill is suppressed hits the same empty-skills
//! early-return as an agent with no attachments: **zero directories written**, an
//! empty report, and no stale `.claude/` tree left in a fresh task root.
//!
//! # Provider extensibility
//!
//! A new provider is one new [`ProviderSkillLayout`] arm (root + env var) and
//! one new test — no edits to the materialisation loop itself.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_core::skill::SkillWithFiles;
use ainb_hangar_store::repo::agent::AgentRepo;
use ainb_hangar_store::repo::skill::{SkillRepo, SkillRepoError};
use sqlx::SqlitePool;

/// The skills subdirectory name appended under a provider's layout root.
const SKILLS_DIR: &str = "skills";
/// The conventional `SKILL.md` filename a skill's top-level body lands in.
const SKILL_BODY_FILE: &str = "SKILL.md";
/// The relative-path prefix marking a skill file as an executable script. Files
/// under this directory get the unix executable bit (reference parity).
const SCRIPTS_PREFIX: &str = "scripts/";

/// Where a provider expects an agent's skills to live, plus the env var (if
/// any) that points the provider at that layout root.
///
/// This is the single extensibility point: adding a provider is one new arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSkillLayout {
    /// `claude`: `{task_root}/.claude/skills`, env `CLAUDE_HOME={task_root}`.
    Claude,
    /// `codex`: `{task_root}/.codex/skills`, env `CODEX_HOME={task_root}/.codex`.
    Codex,
    /// `cursor`: `{task_root}/.cursor/skills`, env `CURSOR_HOME={task_root}/.cursor`.
    Cursor,
    /// `copilot`: `{workdir}/.github/skills`, no env var.
    Copilot,
    /// `gemini` / anything unrecognised: `{workdir}/.agent_context/skills`, no
    /// env var. The catch-all keeps an unknown provider working (skills written
    /// to a neutral, in-workdir location) rather than silently materialising
    /// nothing.
    GeminiOrDefault,
}

impl ProviderSkillLayout {
    /// Resolve a provider's wire name (`"claude"`, `"codex"`, …) to its layout.
    ///
    /// Matching is case-insensitive. An unrecognised provider maps to
    /// [`Self::GeminiOrDefault`] (the neutral in-workdir layout).
    #[must_use]
    pub fn from_provider(provider: &str) -> Self {
        match provider.to_ascii_lowercase().as_str() {
            "claude" => Self::Claude,
            "codex" => Self::Codex,
            "cursor" => Self::Cursor,
            "copilot" => Self::Copilot,
            _ => Self::GeminiOrDefault,
        }
    }

    /// The directory the skills tree is rooted under, given the task root
    /// (sibling of `workdir`) and the `workdir` itself.
    ///
    /// Home-style providers (Claude/Codex/Cursor) root outside the git worktree
    /// (under `task_root`) to keep the worktree's git status clean; the others
    /// root inside `workdir` at their documented path.
    #[must_use]
    pub fn skills_root(self, task_root: &Path, workdir: &Path) -> PathBuf {
        match self {
            Self::Claude => task_root.join(".claude").join(SKILLS_DIR),
            Self::Codex => task_root.join(".codex").join(SKILLS_DIR),
            Self::Cursor => task_root.join(".cursor").join(SKILLS_DIR),
            Self::Copilot => workdir.join(".github").join(SKILLS_DIR),
            Self::GeminiOrDefault => workdir.join(".agent_context").join(SKILLS_DIR),
        }
    }

    /// The environment variable (name, value) the provider needs to discover the
    /// materialised skills, or `None` when the provider uses an in-workdir
    /// convention and needs no pointer.
    ///
    /// Wired into the dispatch `Command::env(...)` so the spawned provider reads
    /// from the task-isolated home rather than the operator's real `$HOME`.
    #[must_use]
    pub fn home_env(self, task_root: &Path) -> Option<(String, PathBuf)> {
        match self {
            Self::Claude => Some(("CLAUDE_HOME".to_string(), task_root.to_path_buf())),
            Self::Codex => Some(("CODEX_HOME".to_string(), task_root.join(".codex"))),
            Self::Cursor => Some(("CURSOR_HOME".to_string(), task_root.join(".cursor"))),
            Self::Copilot | Self::GeminiOrDefault => None,
        }
    }
}

/// A summary of what one materialisation pass wrote — fed to P8 observability and
/// returned so the caller can log/assert the outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaterialiseReport {
    /// Total number of files written (skill bodies + supporting files).
    pub files_written: usize,
    /// Total bytes written across all files.
    pub total_bytes: u64,
    /// The (kebab-case) names of the skills materialised, in skill order.
    pub skill_names: Vec<String>,
    /// The home env var to set on the provider command, or `None`.
    pub home_env: Option<(String, PathBuf)>,
}

/// Where to materialise — the per-task paths plus the provider name.
#[derive(Debug, Clone)]
pub struct MaterialiseTarget {
    /// The workspace the task's agent belongs to. The skill read is scoped to
    /// it so a task can never materialise another tenant's skills.
    pub workspace: WorkspaceId,
    /// The per-task root (`ExecEnv::root()`): sibling of `workdir`, outside the
    /// git worktree. Home-style provider layouts root here.
    pub task_root: PathBuf,
    /// The agent's working directory (the git worktree for issue tasks).
    /// In-workdir provider layouts root here.
    pub workdir: PathBuf,
    /// The provider wire name (`agent_runtime.provider`).
    pub provider: String,
}

/// Failure modes of a materialisation pass.
#[derive(Debug, thiserror::Error)]
pub enum MaterialiseError {
    /// Reading the agent's skills from the store failed.
    #[error(transparent)]
    Repo(#[from] SkillRepoError),
    /// A filesystem write (mkdir, copy, chmod) failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Materialise every skill attached to `agent` into the provider's layout under
/// `target`.
///
/// Reads [`SkillRepo::skills_for_agent`] — which returns the **enabled** links
/// only — then drops anything named in the agent's `disabled_runtime_skills`,
/// picks the [`ProviderSkillLayout`] for `target.provider`, and for each
/// surviving skill writes its body to `SKILL.md` and its supporting files at
/// their relative paths under `{skills_root}/{skill_name}/`. Files under
/// `scripts/` get the unix executable bit. Files are **copied** (written), never
/// symlinked.
///
/// An agent with no attached skills — or whose every skill is suppressed by
/// either lever — writes nothing (no directories created) and returns an empty
/// [`MaterialiseReport`]; dispatch must still succeed.
///
/// # Errors
///
/// Returns [`MaterialiseError::Repo`] if the skill read fails, or
/// [`MaterialiseError::Io`] on any filesystem failure.
pub async fn materialise_for_agent(
    pool: &SqlitePool,
    agent: &AgentId,
    target: &MaterialiseTarget,
) -> Result<MaterialiseReport, MaterialiseError> {
    let skills = SkillRepo::skills_for_agent(pool, &target.workspace, agent).await?;

    // D1: the by-name runtime suppression list. Applied after the junction read
    // because it can name a skill this agent was never attached to. A missing
    // agent row suppresses nothing (the read below already scoped the skills, so
    // there is no leak in degrading open here).
    let suppressed = AgentRepo::get(pool, agent.as_str())
        .await
        .map_err(SkillRepoError::from)?
        .map(|a| a.disabled_runtime_skills)
        .unwrap_or_default();
    let skills: Vec<_> = skills
        .into_iter()
        .filter(|s| !suppressed.iter().any(|n| n == s.name.as_str()))
        .collect();

    let layout = ProviderSkillLayout::from_provider(&target.provider);

    // No skills → no dirs, no env pointer. Dispatch proceeds with an empty
    // report (the empty-skill no-op invariant).
    if skills.is_empty() {
        return Ok(MaterialiseReport::default());
    }

    let skills_root = layout.skills_root(&target.task_root, &target.workdir);
    let mut report = MaterialiseReport {
        home_env: layout.home_env(&target.task_root),
        ..MaterialiseReport::default()
    };

    for skill in &skills {
        write_skill(&skills_root, skill, &mut report)?;
        report.skill_names.push(skill.name.as_str().to_string());
    }

    Ok(report)
}

/// Write one skill bundle under `{skills_root}/{name}/`: its `SKILL.md` body (if
/// any) plus every supporting file at its relative path, accumulating counts
/// into `report`.
fn write_skill(
    skills_root: &Path,
    skill: &SkillWithFiles,
    report: &mut MaterialiseReport,
) -> io::Result<()> {
    let skill_dir = skills_root.join(skill.name.as_str());

    if let Some(body) = &skill.content {
        write_file(
            &skill_dir.join(SKILL_BODY_FILE),
            body.as_bytes(),
            false,
            report,
        )?;
    }

    for file in &skill.files {
        // Containment guard (defense-in-depth): a skill file path that escapes
        // the skill dir (`..`, an absolute path, a Windows drive prefix) is
        // skipped rather than allowed to write outside the materialisation root.
        // Skill content is operator-curated today, but a single malformed import
        // must never let a write land outside `{skills_root}/{name}/`.
        if escapes_skill_dir(&file.path) {
            tracing::warn!(
                skill = skill.name.as_str(),
                path = file.path.as_str(),
                "skipping skill file with escaping path"
            );
            continue;
        }
        let dest = skill_dir.join(&file.path);
        let executable = file.path.starts_with(SCRIPTS_PREFIX);
        write_file(&dest, file.content.as_bytes(), executable, report)?;
    }
    Ok(())
}

/// Whether a relative skill-file path would escape its skill directory.
///
/// Rejects absolute paths and any component that is a `..` parent reference (or
/// a path that `Path::join` would treat as a root, e.g. a Windows drive
/// prefix). A normal `references/x.md` / `scripts/y.sh` passes.
fn escapes_skill_dir(rel: &str) -> bool {
    use std::path::Component;
    let p = Path::new(rel);
    p.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

/// Write `bytes` to `dest` (creating parent dirs), optionally setting the unix
/// executable bit, and fold the byte/file counts into `report`.
fn write_file(
    dest: &Path,
    bytes: &[u8],
    executable: bool,
    report: &mut MaterialiseReport,
) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dest, bytes)?;
    if executable {
        set_executable(dest)?;
    }
    report.files_written += 1;
    report.total_bytes += bytes.len() as u64;
    Ok(())
}

/// Set the unix executable bit (`0o755`) on `path`. A no-op on non-unix targets
/// (Hangar is unix-only, but the cfg keeps the build honest).
#[cfg(unix)]
fn set_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
}

/// No-op on non-unix platforms (no executable bit to set).
#[cfg(not(unix))]
fn set_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}
