//! In-repo, feature-gated test-fixture builder for the SkillManager.
//!
//! Lives behind `feature = "test-fixtures"`; the default `cargo build`
//! omits this module entirely, so production binaries link zero fixture
//! seeding code and zero bytes of the seeded SKILL.md contents.
//! Integration tests (`tests/sandbox_fixture_smoke.rs` and similar) and
//! the bash launcher (`scripts/skill-manager-sandbox.sh`) consume the
//! exact same dir layout; the bash script re-implements seeding in
//! POSIX shell so it has no Rust runtime dependency.
//!
//! See `.agents/plans/skill-manager-sandbox-fixture-spec.md` for the
//! complete coverage matrix + design rationale.
//!
//! Bead `ai-e7t`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Sentinel file written into the sandbox root so the bash teardown
/// can refuse to `rm -rf` a directory that was not produced by this
/// fixture. The Rust API uses `tempfile::tempdir` for tests so the
/// marker is informational there — its load-bearing role is for
/// manual-mode bash teardown.
pub const SANDBOX_MARKER_FILE: &str = ".ainb-sandbox-marker";

/// How much to seed. `Minimal` covers what the existing
/// sync + drift integration tests need; `Full` adds every adapter
/// home, multi-marketplace plugin coverage, conflict pairs, the
/// banner skip-marker, and a real-schema `external-dependencies.yaml`
/// — enough for the upcoming provenance-matcher work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxTier {
    Minimal,
    Full,
}

/// Resolved paths after seeding. Every consumer (in-process test,
/// bash script, manual TUI launcher) speaks against the same shape,
/// so layout changes here are picked up uniformly.
#[derive(Debug, Clone)]
pub struct SandboxLayout {
    /// Root of the sandbox — what `HOME` resolves to when env_vars()
    /// is applied. Tests pass `tempdir.path()`; the bash script
    /// defaults to `~/.cache/ainb-sandbox`.
    pub root: PathBuf,
    /// `<root>/.agents-in-a-box/` — where ainb's `manifest.yaml`,
    /// `lock.yaml`, and `promote-cache/` live.
    pub ainb_home: PathBuf,
    /// `<root>/.claude/` — Claude tool home (skills, agents,
    /// commands, plugins cache).
    pub claude_home: PathBuf,
    /// `<root>/.codex/` — Codex tool home.
    pub codex_home: PathBuf,
    /// Bare git repo that plays the role of an upstream remote for
    /// sync round-trip tests. Has one seed commit on `main` so
    /// `git ls-remote` returns a tip.
    pub bare_remote: PathBuf,
    /// Which tier was used to seed.
    pub tier: SandboxTier,
}

impl SandboxLayout {
    /// Env-var pairs to set before launching `ainb` (CLI or TUI)
    /// against this sandbox. Matches what the bash script writes
    /// into `<root>/env.sh` so the two surfaces stay aligned.
    pub fn env_vars(&self) -> Vec<(&'static str, std::ffi::OsString)> {
        use std::ffi::OsString;
        vec![
            ("HOME", OsString::from(&self.root)),
            ("AINB_HOME", OsString::from(&self.ainb_home)),
            ("AINB_TOOL_HOME_CLAUDE", OsString::from(&self.claude_home)),
            ("AINB_TOOL_HOME_CODEX", OsString::from(&self.codex_home)),
            // Belt-and-braces — sync/drift code paths set these internally,
            // but if a subshell escapes the binary's env, we still want
            // zero credential prompts.
            ("GIT_TERMINAL_PROMPT", OsString::from("0")),
            ("GIT_ASKPASS", OsString::from("/bin/true")),
        ]
    }

    /// URI a manifest source entry would use to reach the bare
    /// remote. `git:file://` so `source_to_remote_url` passes the
    /// URL verbatim to `git ls-remote`.
    pub fn bare_remote_uri(&self) -> String {
        format!("git:file://{}", self.bare_remote.display())
    }

    /// Unit URI for the seed skill that already exists in the bare
    /// remote (`skills/initial-skill/SKILL.md` at the seed commit).
    /// Convenience for tests that need a known unit-URI to install.
    pub fn seed_unit_uri(&self) -> String {
        format!(
            "git:file://{}@main/skills/initial-skill",
            self.bare_remote.display()
        )
    }
}

/// Build a sandbox at `root` for in-process tests. Returns the
/// resolved layout. The caller is responsible for the lifetime of
/// `root` (typically a `tempfile::TempDir`); this function does NOT
/// register a Drop guard.
///
/// Re-runs against an existing root wipe + reseed (idempotent for
/// test scaffolding). The function refuses to operate when `root`
/// equals the user's home directory — manual safety belt for the
/// human-typed call sites; in-process tests should never hit it.
///
/// # Errors
/// - `io::ErrorKind::InvalidInput` when `root == $HOME` (the
///   refuse-to-clobber rule).
/// - Any I/O error from `mkdir`, `write`, or `git`. The bare-remote
///   seed shell-out fails loudly if `git` is unavailable on PATH.
pub fn build_skill_manager_sandbox(
    root: &Path,
    tier: SandboxTier,
) -> std::io::Result<SandboxLayout> {
    refuse_real_home(root)?;

    // Idempotent: previous-run cruft is wiped before reseed.
    if root.exists() {
        fs::remove_dir_all(root)?;
    }
    fs::create_dir_all(root)?;

    let layout = SandboxLayout {
        root: root.to_path_buf(),
        ainb_home: root.join(".agents-in-a-box"),
        claude_home: root.join(".claude"),
        codex_home: root.join(".codex"),
        bare_remote: root.join("sandbox-remote.git"),
        tier,
    };

    // Sentinel written first so a teardown that crashes mid-seed
    // still recognises the dir as ours.
    fs::write(layout.root.join(SANDBOX_MARKER_FILE), b"")?;
    fs::create_dir_all(&layout.ainb_home)?;
    fs::create_dir_all(&layout.claude_home)?;
    fs::create_dir_all(&layout.codex_home)?;

    // Suppress the first-run ainb-hooks install nudge. It renders as a
    // confirmation modal on the home screen that intercepts keystrokes, so
    // every live TUI tripwire that navigates home -> SkillManager would
    // otherwise have its first key swallowed. `ainb_plugin_notifyd`'s
    // `prompt_state` reads `<home>/.agents-in-a-box/install.json`; a record
    // with `prompt_dismissed = true` makes it return `None`.
    fs::write(
        layout.ainb_home.join("install.json"),
        b"{\"agents\":[],\"hook_script\":\"\",\"prompt_dismissed\":true}\n",
    )?;

    seed_minimal(&layout)?;
    init_bare_remote_with_seed(&layout.bare_remote)?;

    if tier == SandboxTier::Full {
        seed_full(&layout)?;
    }

    Ok(layout)
}

/// Refuse to seed inside `$HOME` itself — would clobber real
/// `~/.claude` / `~/.codex` / `~/.agents-in-a-box`. The bash
/// script enforces the same rule externally; this is the in-process
/// belt.
fn refuse_real_home(root: &Path) -> std::io::Result<()> {
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if root == home.as_path() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "refusing to seed sandbox at real $HOME (`{}`) — \
                     pass a tempdir or a dedicated --root path",
                    home.display()
                ),
            ));
        }
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────
// Minimal tier
// ──────────────────────────────────────────────────────────────────

fn seed_minimal(layout: &SandboxLayout) -> std::io::Result<()> {
    // Hand-authored skill — truly local, no upstream.
    seed_dir_skill(
        &layout.claude_home,
        "commit",
        "Local hand-authored commit skill — no upstream provenance.",
    )?;

    // "External clone" skill — same on-disk shape as a Class-C
    // orphan today; the provenance matcher (future PR) will tag
    // this as `gh:` based on external-dependencies.yaml.
    seed_dir_skill(
        &layout.claude_home,
        "fireworks-tech-graph",
        "External clone skill — simulates `bootstrap` cloning from \
         gh:yizhiyanhua-ai/fireworks-tech-graph into the user's \
         claude home.",
    )?;

    // Flat-md agent — exercises Class-C's flat-md path.
    fs::create_dir_all(layout.claude_home.join("agents"))?;
    fs::write(
        layout.claude_home.join("agents/distinguished-engineer.md"),
        "---\nname: distinguished-engineer\n---\nSenior reviewer agent.\n",
    )?;

    // Codex-side skill — proves multi-tool walker coverage.
    seed_dir_skill(&layout.codex_home, "deploy", "Codex-side deploy skill.")?;

    // One marketplace plugin under plugins/cache — gives Class-A
    // walker something to find.
    seed_marketplace_plugin(
        &layout.claude_home,
        "sandbox-marketplace",
        "discord",
        "0.1.0",
        &["access", "configure"],
    )?;

    Ok(())
}

/// `<tool_home>/skills/<name>/SKILL.md` with frontmatter.
fn seed_dir_skill(tool_home: &Path, name: &str, body: &str) -> std::io::Result<()> {
    let dir = tool_home.join("skills").join(name);
    fs::create_dir_all(&dir)?;
    let mut content = String::new();
    content.push_str("---\nname: ");
    content.push_str(name);
    content.push_str("\nkind: skill\n---\n");
    content.push_str(body);
    content.push('\n');
    fs::write(dir.join("SKILL.md"), content)?;
    Ok(())
}

/// `<claude_home>/plugins/cache/<mp>/<plugin>/<ver>/{plugin.json,skills/*}`.
fn seed_marketplace_plugin(
    claude_home: &Path,
    marketplace: &str,
    plugin: &str,
    version: &str,
    skills: &[&str],
) -> std::io::Result<()> {
    let plugin_root = claude_home
        .join("plugins")
        .join("cache")
        .join(marketplace)
        .join(plugin)
        .join(version);
    fs::create_dir_all(plugin_root.join(".claude-plugin"))?;
    fs::write(
        plugin_root.join(".claude-plugin/plugin.json"),
        format!(
            r#"{{"name":"{plugin}","version":"{version}","description":"sandbox fixture plugin"}}"#,
            plugin = plugin,
            version = version,
        ),
    )?;
    for skill in skills {
        seed_dir_skill(
            &plugin_root,
            skill,
            "Plugin-bundled skill seeded by sandbox fixture.",
        )?;
    }
    Ok(())
}

/// Init a bare git repo with one commit on `main` so
/// `git ls-remote` returns a tip and sync round-trips work.
fn init_bare_remote_with_seed(bare: &Path) -> std::io::Result<()> {
    let parent = bare.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "bare path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    git_must_succeed(
        &["init", "--bare", "-b", "main", bare.to_str().unwrap()],
        parent,
    )?;

    let work = parent.join(".sandbox-seed-work");
    if work.exists() {
        fs::remove_dir_all(&work)?;
    }
    fs::create_dir_all(&work)?;
    git_must_succeed(&["init", "-q", "-b", "main"], &work)?;
    git_must_succeed(&["config", "user.email", "sandbox@example.invalid"], &work)?;
    git_must_succeed(&["config", "user.name", "ainb-sandbox"], &work)?;

    let skills_dir = work.join("skills/initial-skill");
    fs::create_dir_all(&skills_dir)?;
    fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: initial-skill\nkind: skill\n---\n\
         Seed skill in the bare remote; gives drift + sync tests something to act on.\n",
    )?;
    git_must_succeed(&["add", "."], &work)?;
    git_must_succeed(&["commit", "-q", "-m", "seed: initial-skill"], &work)?;
    git_must_succeed(&["remote", "add", "origin", bare.to_str().unwrap()], &work)?;
    git_must_succeed(&["push", "-q", "origin", "main"], &work)?;

    fs::remove_dir_all(&work)?;
    Ok(())
}

fn git_must_succeed(args: &[&str], cwd: &Path) -> std::io::Result<()> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "/bin/true")
        .env("GIT_AUTHOR_NAME", "ainb-sandbox")
        .env("GIT_AUTHOR_EMAIL", "sandbox@example.invalid")
        .env("GIT_COMMITTER_NAME", "ainb-sandbox")
        .env("GIT_COMMITTER_EMAIL", "sandbox@example.invalid")
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git {:?} failed in {}: {}",
            args,
            cwd.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────
// Full tier (additive on top of Minimal)
// ──────────────────────────────────────────────────────────────────

/// Every v1 adapter tool name. Order intentionally matches
/// `ainb-cli::discovery::class_c::ALL_TOOLS`.
const ALL_TOOL_NAMES: &[&str] = &[
    "claude",
    "codex",
    "copilot",
    "gemini",
    "cursor",
    "amazonq",
    "claude-desktop",
    "cline",
    "roo",
];

fn seed_full(layout: &SandboxLayout) -> std::io::Result<()> {
    // Every adapter home gets at least one skill so the Class-C
    // walker has full multi-tool coverage. Claude + codex already
    // have content from Minimal — extend, don't replace.
    for tool in ALL_TOOL_NAMES {
        if *tool == "claude" || *tool == "codex" {
            continue;
        }
        let tool_home = layout.root.join(real_home_subpath_for(tool));
        seed_dir_skill(
            &tool_home,
            &format!("{tool}-side-skill"),
            &format!("Skill seeded under the {tool} adapter home."),
        )?;
    }

    // Two more marketplace plugins to exercise the multi-marketplace
    // path. plugins.json is intentionally absent so the walker
    // emits `marketplace = unknown` for the two community ones.
    seed_marketplace_plugin(
        &layout.claude_home,
        "official",
        "code-review",
        "0.2.0",
        &["review"],
    )?;
    seed_marketplace_plugin(
        &layout.claude_home,
        "community",
        "third-party-thing",
        "unknown",
        &["thing"],
    )?;

    // Two conflict pairs: each pair is two skills under the same
    // canonical name that ainb's ConflictFlip would resolve.
    // Authored as two manifest entries with shadowed_by; the
    // fixture WRITES the manifest for Full tier so tests can drive
    // the flip directly without first calling `ainb source add`.
    write_full_tier_manifest(&layout.ainb_home, &layout.bare_remote)?;

    // Banner skip-marker — proves the "don't re-show" path.
    fs::write(layout.ainb_home.join(".skip-banner"), b"")?;

    // external-dependencies.yaml — real-schema content lifted (and
    // trimmed) from external-dependencies.yaml. The bytes
    // here are the input for the future provenance matcher.
    fs::write(
        layout.root.join("external-dependencies.yaml"),
        EXTERNAL_DEPENDENCIES_YAML,
    )?;

    // Own-skill library (bead ai-lgk): a hand-authored skill on disk
    // under the claude home, registered in `library.yaml` so the
    // SkillManager's `[l]` Library view has a row to render. The skill
    // folder + a matching `library.yaml` entry together are the
    // tripwire's seed.
    seed_dir_skill(
        &layout.claude_home,
        OWN_SKILL_NAME,
        "Hand-authored own-skill seeded by the sandbox fixture — the \
         Library view ([l]) renders this row.",
    )?;
    fs::write(
        layout.ainb_home.join("library.yaml"),
        format!(
            "schema_version: 1\nowned:\n  \
             - name: {name}\n    \
             kind: skill\n    \
             path: .claude/skills/{name}\n    \
             created: \"2026-06-02T00:00:00+00:00\"\n",
            name = OWN_SKILL_NAME,
        ),
    )?;

    Ok(())
}

/// Name of the own-skill the Full tier seeds into `library.yaml`. The
/// `[l]` Library-view tripwire asserts this row renders. Distinct from
/// every Minimal/Full marketplace + orphan name so substring checks on
/// the captured pane are unambiguous.
pub const OWN_SKILL_NAME: &str = "my-own-skill";

/// Path under `<root>` for each tool, matching what
/// `read_root_for()` falls back to when no env override is set.
fn real_home_subpath_for(tool: &str) -> PathBuf {
    match tool {
        "claude" => PathBuf::from(".claude"),
        "codex" => PathBuf::from(".codex"),
        "copilot" => PathBuf::from(".copilot"),
        "gemini" => PathBuf::from(".gemini"),
        "cursor" => PathBuf::from(".cursor"),
        "amazonq" => PathBuf::from(".aws").join("amazonq"),
        "claude-desktop" => {
            // Always the Linux/XDG path inside the sandbox — `HOME`
            // override means we never reach the macOS Library
            // location. Keeps cross-platform tests reproducible.
            PathBuf::from(".config").join("Claude")
        }
        "cline" => PathBuf::from(".cline"),
        "roo" => PathBuf::from(".roo"),
        _ => PathBuf::from(format!(".{tool}")),
    }
}

fn write_full_tier_manifest(ainb_home: &Path, bare: &Path) -> std::io::Result<()> {
    let manifest = format!(
        r#"schema_version: 1
sources:
  - name: sandbox
    type: git
    uri: git:file://{bare}
    ref: main
    enabled: true
    target_layout:
      - glob: "skills/*/SKILL.md"
        home: ".claude/skills"
        repo: "skills"
units:
  - uri: git:file://{bare}@main/skills/initial-skill
    targets: [claude]
  - uri: local:.claude/skills/commit
    targets: [claude]
    shadowed_by: git:file://{bare}@main/skills/initial-skill
"#,
        bare = bare.display()
    );
    fs::write(ainb_home.join("manifest.yaml"), manifest)?;
    Ok(())
}

/// Trimmed copy of `external-dependencies.yaml` (real schema).
/// Includes one `agent-skills` entry that matches a Minimal-tier
/// seeded skill name (`fireworks-tech-graph`), so the future
/// provenance matcher can resolve that skill to its `gh:` upstream.
const EXTERNAL_DEPENDENCIES_YAML: &str = r#"# Sandbox fixture — mirrors external-dependencies.yaml schema.
# Seeded by ainb-skill-core::fixtures (feature = "test-fixtures").
version: "1.0.0"
updated: "2026-06-01"

external-packages:
  - name: reflect-kb
    source: https://github.com/stevengonsalvez/reflect-kb.git
    install: "uv tool install --force '{source}[graph]'"
    cli: reflect
    version-pin: main

bundled-skills:
  - name: coding-agent
    path: packages/skills/coding-agent
    purpose: "Delegate coding tasks to subagents"

  - name: interview
    path: packages/skills/interview
    purpose: "Plan interview + specification generation"

agent-skills:
  - name: fireworks-tech-graph
    repo: https://github.com/yizhiyanhua-ai/fireworks-tech-graph
    applies-to: [claude, codex, copilot, gemini]
    purpose: "SVG technical-diagram generator"

  - name: ui-ux-pro-max
    repo: https://github.com/nextlevelbuilder/ui-ux-pro-max-skill
    version: "2.2.1"
    applies-to: [claude, codex, copilot, gemini]
    purpose: "UI/UX design intelligence"
"#;
