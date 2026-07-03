//! P5 daemon-side agent profiles: the on-disk master store, the DB-index
//! reconciler + fs-watch, and compile-on-dispatch (D14-D16, T6).
//!
//! The pure format + the two down-compilers live in
//! [`ainb_hangar_core::profile`]; this module is the IO seam around them:
//!
//! - **Masters on disk.** Canonical Claude-subagent `.md` files under
//!   `{hangar_home}/profiles/<slug>.md` — the human-editable, git-able source of
//!   truth (T6). [`read_master`] / [`write_master`] / [`list_master_files`] are
//!   the file surface; the `profile/upsert` RPC writes through them.
//! - **DB index + fs-watch.** [`refresh_index`] reconciles the
//!   [`ProfileRepo`](ainb_hangar_store::repo::profile::ProfileRepo) index against
//!   the directory (upsert each master's `(slug, tier, mtime)`, prune a row whose
//!   file vanished). [`spawn_index_watch`] runs it once at boot and again on every
//!   filesystem change, so an edit-on-disk is picked up without an RPC (the P5
//!   acceptance "edit-on-disk picked up by watch").
//! - **Compile-on-dispatch.** [`materialise_profile`] writes the resolved
//!   tool-native files (Claude `.md` / Codex config+prompt) into a task's
//!   execution env before the provider spawns (D16), logging a visible WARN for
//!   every Claude-only field the Codex target drops.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ainb_hangar_core::profile::{ProfileMaster, ProfileParseError};
use ainb_hangar_store::repo::profile::ProfileRepo;
use sqlx::SqlitePool;

/// The masters directory under the Hangar home: `{hangar_home}/profiles`.
const PROFILES_SUBDIR: &str = "profiles";
/// The `.md` extension every master carries.
const MASTER_EXT: &str = "md";

/// Resolve the profiles directory (`{hangar_home}/profiles`), or `None` when the
/// Hangar home itself cannot be resolved.
///
/// Follows the same home contract as the rest of the daemon
/// ([`ainb_hangar_core::paths::hangar_home`]): `$AINB_HANGAR_HOME` verbatim when
/// set, else `~/.agents-in-a-box`. A test's isolated home therefore gets an
/// isolated profiles dir.
#[must_use]
pub fn profiles_dir() -> Option<PathBuf> {
    ainb_hangar_core::paths::hangar_home().map(|h| h.join(PROFILES_SUBDIR))
}

/// The absolute path of a master file: `{dir}/<slug>.md`.
#[must_use]
pub fn master_path(dir: &Path, slug: &str) -> PathBuf {
    dir.join(format!("{slug}.{MASTER_EXT}"))
}

/// A profile IO / parse failure.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// A filesystem read/write failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// A master file could not be parsed.
    #[error("profile {slug:?}: {source}")]
    Parse {
        /// The slug (file stem) whose master failed to parse.
        slug: String,
        /// The underlying parse error.
        #[source]
        source: ProfileParseError,
    },
}

/// Read + parse the master for `slug` from `dir`, or `Ok(None)` when the file
/// does not exist (a not-found is not an error — the `profile/get` RPC maps it to
/// its not-found result).
///
/// # Errors
///
/// [`ProfileError::Io`] on a read fault, [`ProfileError::Parse`] on a malformed
/// master.
pub fn read_master(dir: &Path, slug: &str) -> Result<Option<ProfileMaster>, ProfileError> {
    let path = master_path(dir, slug);
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ProfileError::Io(e)),
    };
    let master = ProfileMaster::parse(slug, &text).map_err(|source| ProfileError::Parse {
        slug: slug.to_string(),
        source,
    })?;
    Ok(Some(master))
}

/// Write `master` to `{dir}/<slug>.md` in canonical form, creating `dir` if
/// needed. Returns the absolute path written.
///
/// # Errors
///
/// [`ProfileError::Io`] on a mkdir/write fault.
pub fn write_master(dir: &Path, master: &ProfileMaster) -> Result<PathBuf, ProfileError> {
    fs::create_dir_all(dir)?;
    let path = master_path(dir, &master.slug);
    fs::write(&path, master.to_master_md())?;
    Ok(path)
}

/// List every `<slug>.md` master in `dir` with its last-modified time (epoch
/// millis), slug-ordered. A missing directory yields an empty list (no masters
/// authored yet) rather than an error.
///
/// Non-`.md` files and sub-directories are ignored.
///
/// # Errors
///
/// [`io::Error`] on a directory-read fault (other than not-found).
pub fn list_master_files(dir: &Path) -> io::Result<Vec<(String, i64)>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(MASTER_EXT) {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let mtime = mtime_millis(&entry.metadata()?);
        out.push((slug.to_string(), mtime));
    }
    out.sort();
    Ok(out)
}

/// The last-modified time of `meta` in epoch milliseconds, saturating to `0`
/// before the epoch / on an unavailable mtime (defensive; every real file has
/// one).
fn mtime_millis(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// Reconcile the DB profile index against the masters on disk (the fs-watch
/// refresh + the boot pass).
///
/// For every `<slug>.md` in `dir`: parse it and upsert its `(slug, tier, mtime)`
/// index row (a malformed master is logged and skipped — one bad file never
/// wedges the index). Then prune every indexed slug whose master file no longer
/// exists. Returns the number of rows upserted.
///
/// # Errors
///
/// A store fault (repo read/write) propagates; a per-file parse fault does not.
pub async fn refresh_index(pool: &SqlitePool, dir: &Path) -> anyhow::Result<usize> {
    let files = list_master_files(dir)?;
    let mut on_disk: Vec<String> = Vec::with_capacity(files.len());
    let mut upserted = 0usize;

    for (slug, mtime) in &files {
        on_disk.push(slug.clone());
        match read_master(dir, slug) {
            Ok(Some(master)) => {
                ProfileRepo::upsert(pool, &master.slug, master.tier.as_str(), *mtime).await?;
                upserted += 1;
            }
            Ok(None) => {
                // Raced with a delete between listing and reading — the prune
                // pass below drops any stale row.
            }
            Err(e) => {
                tracing::warn!(slug = %slug, error = %e, "profile index: skipping unparseable master");
            }
        }
    }

    // Prune rows whose master file has disappeared.
    for indexed in ProfileRepo::list_slugs(pool).await? {
        if !on_disk.iter().any(|s| s == &indexed) {
            ProfileRepo::delete(pool, &indexed).await?;
            tracing::info!(slug = %indexed, "profile index: pruned (master file removed)");
        }
    }

    Ok(upserted)
}

/// Run an initial [`refresh_index`] and spawn an fs-watch that re-runs it on
/// every change under the profiles directory (T6 "fs-watch refresh").
///
/// Best-effort: a watcher-setup or watch fault is logged and swallowed — the
/// RPC-driven `profile/upsert` still refreshes the index directly, so a missing
/// watcher only means edits made outside ainb are not auto-indexed, never a
/// dispatch failure. The returned [`notify::RecommendedWatcher`] MUST be held for
/// the watch to stay live (dropping it stops the watch); the daemon keeps it for
/// the process lifetime.
///
/// Returns `None` when the profiles dir cannot be resolved or the watcher cannot
/// be constructed.
#[must_use]
pub fn spawn_index_watch(pool: SqlitePool, dir: PathBuf) -> Option<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};

    // Ensure the dir exists so the watch has something to attach to, and run the
    // initial reconcile so the index reflects whatever masters already exist.
    if let Err(e) = fs::create_dir_all(&dir) {
        tracing::warn!(dir = %dir.display(), error = %e, "profile watch: cannot create profiles dir");
        return None;
    }
    {
        let pool = pool.clone();
        let dir = dir.clone();
        tokio::spawn(async move {
            if let Err(e) = refresh_index(&pool, &dir).await {
                tracing::warn!(error = %e, "profile index: initial reconcile failed");
            }
        });
    }

    // A change under the dir schedules a debounced reconcile on the tokio
    // runtime (the notify callback runs on notify's own thread, so it hands the
    // async reconcile off via the runtime handle captured here).
    let handle = tokio::runtime::Handle::current();
    let watch_pool = pool.clone();
    let watch_dir = dir.clone();
    let mut watcher = match notify::recommended_watcher(
        move |res: notify::Result<notify::Event>| match res {
            Ok(_event) => {
                let pool = watch_pool.clone();
                let dir = watch_dir.clone();
                handle.spawn(async move {
                    if let Err(e) = refresh_index(&pool, &dir).await {
                        tracing::warn!(error = %e, "profile index: watch reconcile failed");
                    }
                });
            }
            Err(e) => tracing::warn!(error = %e, "profile watch error"),
        },
    ) {
        Ok(w) => w,
        Err(e) => {
            tracing::warn!(error = %e, "profile watch: watcher construction failed");
            return None;
        }
    };

    if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
        tracing::warn!(dir = %dir.display(), error = %e, "profile watch: watch() failed");
        return None;
    }
    tracing::info!(dir = %dir.display(), "profile index watch active");
    Some(watcher)
}

/// The outcome of a compile-on-dispatch materialise pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileMaterialiseReport {
    /// The slug materialised (empty when the provider had no profile target).
    pub slug: String,
    /// Number of files written.
    pub files_written: usize,
    /// The dropped-field warnings from the Codex compile (empty for Claude / when
    /// nothing was dropped).
    pub warnings: Vec<String>,
    /// The `*_HOME` env var (name, path) the runner must forward so the provider
    /// discovers the materialised profile, or `None` for a provider with no
    /// profile target.
    pub home_env: Option<(String, PathBuf)>,
}

/// Materialise `master`'s tool-native files into `task_root` for `provider`
/// (D16 compile-on-dispatch).
///
/// - `claude`: writes the lossless subagent `.md` to
///   `{task_root}/.claude/agents/<slug>.md`; env `CLAUDE_HOME={task_root}`.
/// - `codex`: writes the `[profiles.<slug>]` fragment to
///   `{task_root}/.codex/config.toml` and the prompt to
///   `{task_root}/.codex/prompts/<slug>.md`; env `CODEX_HOME={task_root}/.codex`.
///   Every Claude-only field the compile dropped is returned as a warning.
/// - any other provider (v1 supports Claude + Codex only, D7): a no-op empty
///   report.
///
/// Paths mirror the skills-materialise layout ([`crate::materialise`]) so the
/// home env agrees with any skills written for the same task.
///
/// # Errors
///
/// [`io::Error`] on a mkdir/write fault.
pub fn materialise_profile(
    master: &ProfileMaster,
    provider: &str,
    task_root: &Path,
) -> io::Result<ProfileMaterialiseReport> {
    match provider.to_ascii_lowercase().as_str() {
        "claude" => {
            let compile = master.compile_claude();
            let agents_dir = task_root.join(".claude").join("agents");
            fs::create_dir_all(&agents_dir)?;
            fs::write(agents_dir.join(&compile.filename), &compile.contents)?;
            Ok(ProfileMaterialiseReport {
                slug: master.slug.clone(),
                files_written: 1,
                warnings: Vec::new(),
                home_env: Some(("CLAUDE_HOME".to_string(), task_root.to_path_buf())),
            })
        }
        "codex" => {
            let compile = master.compile_codex();
            let codex_home = task_root.join(".codex");
            let prompts_dir = codex_home.join("prompts");
            fs::create_dir_all(&prompts_dir)?;
            // One agent per task ⇒ one profile fragment; write (overwrite) the
            // config so a resumed task's re-materialise stays a single section.
            fs::write(codex_home.join("config.toml"), &compile.config_fragment)?;
            fs::write(
                prompts_dir.join(&compile.prompt_filename),
                &compile.prompt_contents,
            )?;
            for w in &compile.warnings {
                tracing::warn!(target: "profile_compile", "{w}");
            }
            Ok(ProfileMaterialiseReport {
                slug: master.slug.clone(),
                files_written: 2,
                warnings: compile.warnings,
                home_env: Some(("CODEX_HOME".to_string(), codex_home)),
            })
        }
        _ => Ok(ProfileMaterialiseReport::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::Store;

    const SAMPLE: &str = "---\n\
name: reviewer\n\
description: Reviews a diff\n\
model: premium\n\
tools: Read, Grep\n\
color: cyan\n\
---\n\
You are a reviewer.\n";

    fn master() -> ProfileMaster {
        ProfileMaster::parse("reviewer", SAMPLE).unwrap()
    }

    #[test]
    fn write_then_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let m = master();
        let path = write_master(dir.path(), &m).unwrap();
        assert!(path.ends_with("reviewer.md"));
        let back = read_master(dir.path(), "reviewer").unwrap().unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn read_missing_master_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_master(dir.path(), "nope").unwrap().is_none());
    }

    #[test]
    fn list_master_files_only_md_slug_ordered() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("b.md"), SAMPLE).unwrap();
        fs::write(dir.path().join("a.md"), SAMPLE).unwrap();
        fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();
        let files = list_master_files(dir.path()).unwrap();
        let slugs: Vec<_> = files.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(slugs, ["a", "b"]);
    }

    #[test]
    fn list_master_files_missing_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(list_master_files(&missing).unwrap().is_empty());
    }

    #[tokio::test]
    async fn refresh_index_upserts_updates_and_prunes() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let profiles = dir.path().join("profiles");
        fs::create_dir_all(&profiles).unwrap();

        // Two masters on disk.
        write_master(&profiles, &master()).unwrap();
        let author = ProfileMaster::parse(
            "author",
            "---\nname: author\ndescription: d\nmodel: fast\n---\nb\n",
        )
        .unwrap();
        write_master(&profiles, &author).unwrap();

        let n = refresh_index(store.pool(), &profiles).await.unwrap();
        assert_eq!(n, 2);
        let rows = ProfileRepo::list(store.pool()).await.unwrap();
        let by_slug: Vec<_> = rows.iter().map(|r| (r.slug.as_str(), r.tier.as_str())).collect();
        assert_eq!(by_slug, [("author", "fast"), ("reviewer", "premium")]);

        // Edit reviewer's tier on disk → reconcile refreshes the index row.
        let edited = ProfileMaster::parse(
            "reviewer",
            "---\nname: reviewer\ndescription: d\nmodel: balanced\n---\nb\n",
        )
        .unwrap();
        write_master(&profiles, &edited).unwrap();
        refresh_index(store.pool(), &profiles).await.unwrap();
        assert_eq!(
            ProfileRepo::get(store.pool(), "reviewer").await.unwrap().unwrap().tier,
            "balanced",
            "edit-on-disk picked up by reconcile"
        );

        // Delete author's file → reconcile prunes the row.
        fs::remove_file(master_path(&profiles, "author")).unwrap();
        refresh_index(store.pool(), &profiles).await.unwrap();
        assert!(ProfileRepo::get(store.pool(), "author").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn refresh_index_skips_unparseable_master() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let profiles = dir.path().join("profiles");
        fs::create_dir_all(&profiles).unwrap();
        write_master(&profiles, &master()).unwrap();
        // A malformed master (no frontmatter) must not wedge the reconcile.
        fs::write(profiles.join("broken.md"), "no frontmatter here").unwrap();

        let n = refresh_index(store.pool(), &profiles).await.unwrap();
        assert_eq!(n, 1, "only the valid master indexes");
        assert!(ProfileRepo::get(store.pool(), "reviewer").await.unwrap().is_some());
        assert!(ProfileRepo::get(store.pool(), "broken").await.unwrap().is_none());
    }

    #[test]
    fn materialise_claude_writes_lossless_subagent() {
        let root = tempfile::tempdir().unwrap();
        let report = materialise_profile(&master(), "claude", root.path()).unwrap();
        assert_eq!(report.files_written, 1);
        assert!(report.warnings.is_empty());
        assert_eq!(
            report.home_env,
            Some(("CLAUDE_HOME".to_string(), root.path().to_path_buf()))
        );
        let written = fs::read_to_string(
            root.path().join(".claude").join("agents").join("reviewer.md"),
        )
        .unwrap();
        // Tier resolved to a Claude model; every field preserved.
        assert!(written.contains("model: opus"));
        assert!(written.contains("tools: Read, Grep"));
        assert!(written.contains("color: cyan"));
        assert!(written.contains("You are a reviewer."));
    }

    #[test]
    fn materialise_codex_writes_config_prompt_and_warns() {
        let root = tempfile::tempdir().unwrap();
        let report = materialise_profile(&master(), "codex", root.path()).unwrap();
        assert_eq!(report.files_written, 2);
        assert_eq!(report.warnings.len(), 2, "tools + color dropped");
        assert_eq!(
            report.home_env,
            Some(("CODEX_HOME".to_string(), root.path().join(".codex")))
        );
        let config =
            fs::read_to_string(root.path().join(".codex").join("config.toml")).unwrap();
        assert_eq!(config, "[profiles.reviewer]\nmodel = \"gpt-5\"\n");
        let prompt = fs::read_to_string(
            root.path().join(".codex").join("prompts").join("reviewer.md"),
        )
        .unwrap();
        assert!(prompt.contains("You are a reviewer."));
    }

    #[test]
    fn materialise_unknown_provider_is_noop() {
        let root = tempfile::tempdir().unwrap();
        let report = materialise_profile(&master(), "gemini", root.path()).unwrap();
        assert_eq!(report, ProfileMaterialiseReport::default());
        assert!(report.home_env.is_none());
    }
}
