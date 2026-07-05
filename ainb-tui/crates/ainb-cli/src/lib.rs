//! ainb-cli — clap surface for the `ainb` binary's non-TUI commands.
//!
//! P2 ships the `source` subcommand tree (with real fetch on `add`)
//! and the top-level `search` query. P3+ will wire up `skill`,
//! `plugin`, `agent`, `hook`, `mcp`, `cache`, and `doctor`
//! via the same dispatch entrypoint.

use std::io;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

pub mod catalog_curated;
pub mod catalog_http;
pub mod discovery;
pub mod doctor;
pub mod library;
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
    /// Health-check the manifest, lockfile, deployed files, and
    /// configured sources. Exits non-zero when any problem is found.
    Doctor(DoctorArgs),
}

#[derive(Args, Debug, Default)]
#[command(after_help = "\
EXAMPLES:
  ainb doctor                      Health-check skill manifest/lockfile/deployed files
  ainb doctor --offline            Skip source-reachability network checks")]
pub struct DoctorArgs {
    /// Skip the source-reachability check (avoid hitting the
    /// network / re-running fetchers).
    #[arg(long)]
    pub offline: bool,
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
    /// Browse a remote skill catalog (skills.sh) before installing.
    /// Prints ranked hits (name, repo, stars, install-uri). Read-only.
    /// The API key is optional — read from `[skills].api_key` in
    /// config.toml or the `AINB_SKILLS_API_KEY` env var.
    Browse(BrowseArgs),
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
    /// Copy a unit from any configured source into the user's library.
    /// Resolves the unit like `install` does, deploys it under the
    /// tool's skills dir (skipped if already deployed there), and
    /// registers it as an owned unit — same as `add`/`new`.
    Copy {
        /// Full unit URI to copy in (e.g. `gh:org/repo@main/skills/foo`).
        uri: String,

        /// Tool whose home to deploy under. Defaults to `claude`.
        #[arg(long)]
        tool: Option<String>,
    },
    /// Mark a manifest source as a "library source", enabling
    /// `push`/`pull` git-native two-way sync against it.
    MarkSource {
        /// Source name, as declared in the manifest.
        name: String,
    },
    /// Unmark a source as a library source.
    UnmarkSource {
        /// Source name, as declared in the manifest.
        name: String,
    },
    /// Publish local library edits to a library source's git remote:
    /// sync tool-home content into the source's fetched checkout, then
    /// commit + push. Only valid for a source already marked via
    /// `mark-source`.
    Push {
        /// Library source name.
        source: String,
    },
    /// Pull a library source's git remote into local library edits:
    /// `git pull --rebase` the source's fetched checkout, then sync its
    /// content back down into the tool home. Only valid for a source
    /// already marked via `mark-source`.
    Pull {
        /// Library source name.
        source: String,
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

/// Which catalog `ainb skill browse` targets. A `ValueEnum` (not a free
/// string) so clap rejects a typo at parse time instead of silently routing
/// to skills.sh.
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CatalogChoice {
    /// The public skills.sh catalog (the default). Aliased `skills.sh`.
    #[default]
    #[value(name = "skills", alias = "skills.sh")]
    Skills,
    /// The toolkit's curated shelf — owned skills + vetted external — from the
    /// pinned GitHub release index. Aliased `curated`.
    #[value(name = "ainb", alias = "curated")]
    Ainb,
}

#[derive(Args, Debug)]
pub struct BrowseArgs {
    /// Catalog search query (matched against unit names / descriptions).
    /// An empty / whitespace-only query is a no-op hint for the `skills`
    /// catalog, but lists the WHOLE shelf for the `ainb` curated catalog.
    pub query: String,

    /// Which catalog to browse: `skills` (skills.sh, the default) or `ainb`
    /// (the toolkit's curated shelf).
    #[arg(long, value_enum, default_value_t = CatalogChoice::Skills)]
    pub catalog: CatalogChoice,

    /// Emit machine-readable JSON (`[{name, repo, stars, install_uri,
    /// description}, …]`) instead of the default ranked table.
    #[arg(long)]
    pub json: bool,
}

impl BrowseArgs {
    /// True when this targets the `ainb` curated catalog (vs skills.sh).
    pub fn is_curated(&self) -> bool {
        matches!(self.catalog, CatalogChoice::Ainb)
    }
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
        Command::Doctor(args) => doctor::dispatch(home, args, out),
    }
}
