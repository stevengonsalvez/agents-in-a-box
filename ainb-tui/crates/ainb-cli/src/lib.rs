//! ainb-cli — clap surface for the `ainb` binary's non-TUI commands.
//!
//! P2 ships the `source` subcommand tree (with real fetch on `add`)
//! and the top-level `search` query. P3+ will wire up `skill`,
//! `plugin`, `agent`, `hook`, `mcp`, `migrate`, `cache`, and `doctor`
//! via the same dispatch entrypoint.

use std::io;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

pub mod discovery;
pub mod doctor;
pub mod library;
pub mod migrate;
pub mod promote;
pub mod scan;
pub mod search;
pub mod skill;
pub mod source;

/// Top-level CLI grammar.
#[derive(Parser, Debug)]
#[command(name = "ainb", about = "agents-in-a-box CLI", version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage source repositories declared in the manifest.
    Source {
        #[command(subcommand)]
        action: SourceCommand,
    },
    /// Search across all enabled sources for units matching a query.
    Search(SearchArgs),
    /// Install / remove units across one or more target tools.
    Skill {
        #[command(subcommand)]
        action: SkillCommand,
    },
    /// Day-1 migration helpers: scan existing tool homes, optionally
    /// wipe them with a backup, or seed the manifest from
    /// `toolkit/external-dependencies.yaml`.
    Migrate(MigrateArgs),
    /// Health-check the manifest, lockfile, deployed files, and
    /// configured sources. Exits non-zero when any problem is found.
    Doctor(DoctorArgs),
}

#[derive(Args, Debug, Default)]
pub struct DoctorArgs {
    /// Skip the source-reachability check (avoid hitting the
    /// network / re-running fetchers).
    #[arg(long)]
    pub offline: bool,
}

#[derive(Args, Debug, Default)]
pub struct MigrateArgs {
    /// Scan adapter install roots and report what would be wiped.
    /// Read-only; mutually exclusive with the other migrate modes.
    #[arg(long, conflicts_with_all = ["clean", "from_bootstrap", "discover", "upgrade_schema"])]
    pub check: bool,

    /// Wipe every adapter's install root (after optional backup) and
    /// then run `skill sync` to reconcile from the manifest.
    #[arg(long, conflicts_with_all = ["from_bootstrap", "discover", "upgrade_schema"])]
    pub clean: bool,

    /// Snapshot every adapter's install root to
    /// `$AINB_HOME/backups/<ts>/<tool>/` before clean wipes it.
    /// Only valid with --clean.
    #[arg(long, requires = "clean")]
    pub backup: bool,

    /// Parse `toolkit/external-dependencies.yaml` from the supplied
    /// path (or the working directory if omitted) and populate the
    /// manifest with one source + matching unit entries.
    #[arg(long = "from-bootstrap", conflicts_with_all = ["discover", "upgrade_schema"])]
    pub from_bootstrap: bool,

    /// Override the toolkit root used by --from-bootstrap. Defaults
    /// to the current working directory.
    #[arg(long, value_name = "PATH")]
    pub toolkit_root: Option<std::path::PathBuf>,

    /// Scan the Claude Code plugin cache + every adapter install
    /// root for units the manifest does not yet know about and
    /// merge them in (skill-manager v1.1 discovery flow). Honors
    /// `--dry-run`, `--legacy-yaml=<path>`, and `--force`.
    #[arg(long, conflicts_with = "upgrade_schema")]
    pub discover: bool,

    /// Fill in every source's empty `target_layout` with the
    /// canonical bootstrap defaults (`skills/*/SKILL.md → .claude/skills`,
    /// `agents/...`, `commands/*.md → .claude/commands`). Existing
    /// non-empty layouts are left alone. Idempotent: a second run is
    /// a no-op.
    #[arg(long = "upgrade-schema")]
    pub upgrade_schema: bool,

    /// Opt-in `external-dependencies.yaml` name-match. Only valid
    /// with `--discover`; orphan units whose name matches a
    /// `bundled-skills`/`agent-skills` entry get their URI rewritten
    /// from `local:` to `gh:<repo>@<ref>/...` so they're tracked as
    /// git-backed instead of local.
    #[arg(long = "legacy-yaml", value_name = "PATH", requires = "discover")]
    pub legacy_yaml: Option<std::path::PathBuf>,

    /// Override the empty-manifest guard on `--discover` so a
    /// populated manifest can be re-scanned. Only valid with
    /// `--discover`.
    #[arg(long, requires = "discover")]
    pub force: bool,

    /// Skip the interactive confirmation prompt.
    #[arg(long)]
    pub yes: bool,

    /// Show the planned actions but don't apply.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Subcommand, Debug)]
pub enum SkillCommand {
    /// Install a unit by full URI to the accepting target tools.
    Install(InstallArgs),
    /// Remove a previously-installed unit from its target tools.
    Remove(RemoveSkillArgs),
    /// Refresh a unit (or all stale ones) — re-fetches the source and
    /// applies the diff. `--check` reports drift without changing
    /// anything; `--all` batches every locked unit.
    Update(UpdateArgs),
    /// Reconcile on-disk state with the manifest: install units that
    /// are declared but missing, uninstall units that the lockfile
    /// holds but the manifest no longer mentions.
    Sync(SyncArgs),
    /// Promote a `local:` orphan unit into a git-backed source repo,
    /// rewriting the manifest URI from `local:` to `gh:` / `git:`.
    Promote(PromoteArgs),
    /// Refresh per-unit usage telemetry (invocation count + last-used)
    /// by scanning each deploying tool's session logs, and record it in
    /// the lockfile. Without a unit name, every locked unit is refreshed.
    Usage(UsageArgs),
    /// Report drift between each locked unit's pinned SHA and its
    /// source's current upstream tip. Read-only; never mutates the
    /// lockfile or any on-disk unit.
    Check(CheckArgs),
    /// Walk every tool home + the Claude Code plugin cache and print a
    /// provenance tree (which real source each discovered unit came
    /// from: marketplace / external repo / toolkit / adopted / local).
    /// Read-only; never mutates the manifest, lockfile, or any unit.
    Scan(ScanArgs),
    /// Manage the own-skill library — skills the user authored locally,
    /// tracked in `library.yaml` (sibling to the manifest). `list` shows
    /// owned units, `add <path>` ingests an existing on-disk skill folder
    /// (must live under a tool home), `new <name>` scaffolds a fresh
    /// `SKILL.md` and registers it.
    Library {
        #[command(subcommand)]
        cmd: LibraryCmd,
    },
}

/// `ainb skill library ...` subcommand tree — bead ai-lgk.
#[derive(Subcommand, Debug)]
pub enum LibraryCmd {
    /// List every owned unit registered in `library.yaml`.
    List {
        /// Emit machine-readable JSON (`[{name, kind, path, created,
        /// promoted_uri?}, …]`) instead of the default table.
        #[arg(long)]
        json: bool,
    },
    /// Ingest an existing on-disk skill folder into the library. The
    /// path must live under one of the sandbox tool homes (refused
    /// otherwise — safety belt against registering arbitrary paths).
    Add {
        /// Path to the skill folder (e.g. `~/.claude/skills/my-skill`).
        path: std::path::PathBuf,

        /// Tool whose home the path is expected under (`claude`,
        /// `codex`, …). Defaults to `claude`.
        #[arg(long)]
        tool: Option<String>,
    },
    /// Scaffold a fresh `SKILL.md` under the tool's skills dir and
    /// register it as an owned unit.
    New {
        /// New skill name (used for the folder + frontmatter `name`).
        name: String,

        /// Tool whose home to scaffold under. Defaults to `claude`.
        #[arg(long)]
        tool: Option<String>,
    },
}

#[derive(Args, Debug)]
pub struct InstallArgs {
    /// Full unit URI (e.g. `gh:org/repo@main/skills/foo`).
    pub uri: String,

    /// Comma-separated list of target tools (defaults to every
    /// tool whose `accepts()` returns Yes for the unit's kind).
    #[arg(long)]
    pub targets: Option<String>,

    /// Show the planned diff but don't apply.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the interactive confirmation prompt.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct RemoveSkillArgs {
    /// Full unit URI to remove.
    pub uri: String,

    /// Comma-separated list of target tools (defaults to every tool
    /// this unit was deployed to per the lockfile).
    #[arg(long)]
    pub targets: Option<String>,

    /// Skip the interactive confirmation prompt.
    #[arg(long)]
    pub yes: bool,

    /// Show the planned deletions but don't apply.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    /// Optional unit URI to target. Without it, see `--all`.
    pub uri: Option<String>,

    /// Refresh every locked unit. Mutually exclusive with a positional URI.
    #[arg(long, conflicts_with = "uri")]
    pub all: bool,

    /// Re-fetch sources and report drift without changing anything.
    #[arg(long)]
    pub check: bool,

    /// Skip the interactive confirmation prompt.
    #[arg(long)]
    pub yes: bool,

    /// Show the planned diff but don't apply.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args, Debug)]
pub struct PromoteArgs {
    /// Unit name to promote (matches the trailing `/path` of the
    /// unit URI, e.g. `my-skill` in `local:~/.claude/skills@head/my-skill`).
    pub unit_name: String,

    /// Destination git URI. Supports `gh:user/repo[@branch]` for real
    /// GitHub remotes and `file://<path>[@<branch>]` for tests
    /// (and local bare repos). Required.
    #[arg(long)]
    pub to: String,

    /// Override the default commit message. Otherwise:
    /// `feat(<unit-name>): promote from local`.
    #[arg(long)]
    pub message: Option<String>,

    /// Print the plan; do not clone, commit, push, or write any
    /// manifest/lockfile mutations.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the interactive confirmation prompt.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct UsageArgs {
    /// Optional unit name (the trailing path segment, e.g. `commit`).
    /// Without it, every locked unit is refreshed.
    pub unit_name: Option<String>,

    /// Print each unit's invocation count + last-used as it is refreshed.
    #[arg(long, short)]
    pub verbose: bool,
}

#[derive(Args, Debug)]
pub struct CheckArgs {
    /// Optional source name to scope the report to a single source.
    /// Without it, every locked unit's source is checked.
    pub source: Option<String>,

    /// Emit machine-readable JSON (`[{unit, status, ahead?, behind?}, …]`)
    /// instead of the default tabular output. Useful for scripting.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args, Debug, Default)]
pub struct ScanArgs {
    /// Restrict the tree to one provenance kind:
    /// `marketplace` | `external` | `toolkit` | `adopted` | `local`.
    #[arg(long)]
    pub provenance: Option<String>,

    /// Restrict the tree to units discovered under one tool home
    /// (`claude`, `codex`, …). Marketplace plugins are claude-deployed.
    #[arg(long)]
    pub tool: Option<String>,

    /// Emit machine-readable JSON instead of the default tree.
    #[arg(long)]
    pub json: bool,

    /// Override the `external-dependencies.yaml` path used to resolve
    /// external-clone provenance. Defaults to `$HOME/external-dependencies.yaml`
    /// then the current working directory. Mainly for tests.
    #[arg(long = "ext-deps", value_name = "PATH")]
    pub ext_deps: Option<std::path::PathBuf>,
}

#[derive(Args, Debug, Default)]
pub struct SyncArgs {
    /// Optional source name OR unit URI to scope the sync. Without it,
    /// every unit eligible for content sync is considered.
    pub source_or_unit: Option<String>,

    /// Skip the interactive confirmation prompt.
    #[arg(long)]
    pub yes: bool,

    /// Show the planned mutations but don't apply.
    #[arg(long)]
    pub dry_run: bool,

    /// Restrict bidirectional content sync to the upstream-pull
    /// direction (`ToHome` only). Mutually compatible with
    /// `--to-repo`: passing both is the same as the default
    /// bidirectional behaviour.
    #[arg(long)]
    pub to_home: bool,

    /// Restrict bidirectional content sync to the publish direction
    /// (`ToRepo` only). See [`Self::to_home`] for combined-flag
    /// behaviour.
    #[arg(long)]
    pub to_repo: bool,
}

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Substring matched (case-insensitive) against unit names,
    /// descriptions, and tags. Pass an empty string to list every
    /// unit.
    pub query: String,

    /// Restrict results to one unit kind (e.g. `skill`, `plugin`,
    /// `agent`, `command`, `hook`, `mcp-server`, `statusline`).
    #[arg(long)]
    pub kind: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum SourceCommand {
    /// Add a new source by URI.
    Add(AddArgs),
    /// List configured sources.
    List,
    /// Remove a source by name.
    Remove(RemoveArgs),
    /// Enable a source.
    Enable(NameArg),
    /// Disable a source.
    Disable(NameArg),
}

#[derive(Args, Debug)]
pub struct AddArgs {
    /// Source URI without unit path (e.g. `gh:org/repo` or `gh:org/repo@v1.2`).
    pub uri: String,

    /// Override the derived source name slug.
    #[arg(long)]
    pub name: Option<String>,

    /// Source kind hint (`marketplace`, `manifest`, `raw`, `single`).
    /// Optional; adapter auto-detection in P2 will fill it when omitted.
    #[arg(long = "type", value_parser = source::parse_kind_hint)]
    pub kind: Option<String>,
}

#[derive(Args, Debug)]
pub struct RemoveArgs {
    /// Source name to remove.
    pub name: String,

    /// Skip the confirmation prompt.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args, Debug)]
pub struct NameArg {
    /// Source name.
    pub name: String,
}

/// Run the CLI using process argv and the default `$AINB_HOME`.
pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let home = ainb_skill_core::paths::default_ainb_home();
    dispatch(&home, cli.command, &mut io::stdout())
}

/// Dispatch a parsed command against an explicit ainb home. Used by
/// integration tests so they can isolate state in a tempdir without
/// touching the process env.
pub fn dispatch(home: &std::path::Path, command: Command, out: &mut dyn io::Write) -> Result<()> {
    match command {
        Command::Source { action } => source::dispatch(home, action, out),
        Command::Search(args) => search::dispatch(home, args, out),
        Command::Skill { action } => skill::dispatch(home, action, out),
        Command::Migrate(args) => migrate::dispatch(home, args, out),
        Command::Doctor(args) => doctor::dispatch(home, args, out),
    }
}
