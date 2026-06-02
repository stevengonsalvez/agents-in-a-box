//! `ainb skill library {list,add,new}` — the own-skill library
//! (bead ai-lgk).
//!
//! A first-class home for skills the user authored locally. State lives
//! in `library.yaml` (sibling to the manifest) — **no SQLite**. Every
//! mutating command resolves the path under a tool home via
//! `read_root_for(tool)` (honours the `AINB_TOOL_HOME_<TOOL>` sandbox
//! override) and stores a canonical tool-home-relative path
//! (`.claude/skills/<name>`) so the entry stays portable across
//! machines.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use ainb_adapters_tool::read_root_for;
use ainb_skill_core::library::{library_path_in, Library, OwnedUnit};
use ainb_skill_core::UnitKind;

use crate::LibraryCmd;

/// Default tool when none is supplied on the command line.
const DEFAULT_TOOL: &str = "claude";

pub fn dispatch(home: &Path, cmd: LibraryCmd, out: &mut dyn io::Write) -> Result<()> {
    match cmd {
        LibraryCmd::List { json } => list(home, json, out),
        LibraryCmd::Add { path, tool } => add(home, &path, tool.as_deref(), out),
        LibraryCmd::New { name, tool } => new(home, &name, tool.as_deref(), out),
    }
}

/// `ainb skill library list [--json]` — print every owned unit.
fn list(home: &Path, json: bool, out: &mut dyn io::Write) -> Result<()> {
    let lib = Library::load_from(&library_path_in(home))?;

    if json {
        let rows: Vec<serde_json::Value> = lib
            .owned
            .iter()
            .map(|u| {
                serde_json::json!({
                    "name": u.name,
                    "kind": u.kind.as_str(),
                    "path": u.path,
                    "created": u.created,
                    "promoted_uri": u.promoted_uri,
                })
            })
            .collect();
        writeln!(out, "{}", serde_json::to_string_pretty(&rows)?)?;
        return Ok(());
    }

    if lib.owned.is_empty() {
        writeln!(out, "# no owned skills — `ainb skill library new <name>` to author one")?;
        return Ok(());
    }

    writeln!(out, "{:<28}  {:<8}  {:<32}  deploy", "name", "kind", "path")?;
    writeln!(out, "{:-<28}  {:-<8}  {:-<32}  {:-<8}", "", "", "", "")?;
    for u in &lib.owned {
        let deploy = if u.promoted_uri.is_some() {
            "promoted"
        } else {
            "local"
        };
        writeln!(out, "{:<28}  {:<8}  {:<32}  {}", u.name, u.kind.as_str(), u.path, deploy)?;
    }
    writeln!(out, "# {} owned skill(s)", lib.owned.len())?;
    Ok(())
}

/// `ainb skill library add <path> [--tool t]` — ingest an existing
/// on-disk skill folder. Refuses paths outside the tool home.
fn add(home: &Path, path: &Path, tool: Option<&str>, out: &mut dyn io::Write) -> Result<()> {
    let tool = tool.unwrap_or(DEFAULT_TOOL);
    let tool_root = read_root_for(tool);

    // Canonicalise both sides so symlinks / `..` segments don't dodge
    // the containment check. Fall back to the raw path when canonicalise
    // fails (e.g. the path doesn't exist) — `bail` below catches that.
    let abs = canonical_or_self(path);
    let root_abs = canonical_or_self(&tool_root);
    if !abs.starts_with(&root_abs) {
        bail!(
            "refusing to add `{}` — it is outside the `{}` tool home (`{}`). \
             Only skills under a tool home can be registered.",
            path.display(),
            tool,
            tool_root.display()
        );
    }

    if !abs.is_dir() {
        bail!("`{}` is not a directory — point at a skill folder", path.display());
    }

    let name = abs
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow!("cannot derive a skill name from `{}`", path.display()))?
        .to_string();

    let rel = tool_home_relative_path(tool, "skills", &name);
    register_owned(home, &name, UnitKind::Skill, &rel, out)
}

/// `ainb skill library new <name> [--tool t]` — scaffold a fresh
/// `SKILL.md` under the tool's skills dir and register it.
fn new(home: &Path, name: &str, tool: Option<&str>, out: &mut dyn io::Write) -> Result<()> {
    let tool = tool.unwrap_or(DEFAULT_TOOL);
    let name = name.trim();
    if name.is_empty() {
        bail!("skill name must not be empty");
    }

    let tool_root = read_root_for(tool);
    let dir = tool_root.join("skills").join(name);
    if dir.exists() {
        bail!("`{}` already exists — pick a fresh name or use `library add`", dir.display());
    }
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating skill folder `{}`", dir.display()))?;

    let skill_md = scaffold_skill_md(name);
    std::fs::write(dir.join("SKILL.md"), skill_md)
        .with_context(|| format!("writing SKILL.md for `{name}`"))?;

    let rel = tool_home_relative_path(tool, "skills", name);
    register_owned(home, name, UnitKind::Skill, &rel, out)
}

/// Shared registration tail: load `library.yaml`, register (dedup by
/// name), save, and print a one-line summary.
fn register_owned(
    home: &Path,
    name: &str,
    kind: UnitKind,
    rel_path: &str,
    out: &mut dyn io::Write,
) -> Result<()> {
    let lib_path = library_path_in(home);
    let mut lib = Library::load_from(&lib_path)?;
    let inserted = lib.register(OwnedUnit {
        name: name.to_string(),
        kind,
        path: rel_path.to_string(),
        created: now_rfc3339(),
        promoted_uri: None,
    });
    lib.save_to(&lib_path)
        .with_context(|| format!("saving library at `{}`", lib_path.display()))?;
    let verb = if inserted { "registered" } else { "updated" };
    writeln!(out, "{verb} own-skill `{name}` → {rel_path}")?;
    Ok(())
}

/// Scaffold the bytes of a minimal, parseable `SKILL.md` with the
/// frontmatter the discovery walkers + adapters expect.
fn scaffold_skill_md(name: &str) -> String {
    format!(
        "---\nname: {name}\nkind: skill\ndescription: \"TODO: describe {name}.\"\n---\n\n\
         # {name}\n\n\
         <!-- Authored locally via `ainb skill library new`. Fill in the body. -->\n"
    )
}

/// Canonical tool-home-relative display path
/// (e.g. `.claude/skills/my-skill`). The dotdir is derived from a stable
/// tool→dotdir map so the stored path is portable regardless of where
/// the tool home actually resolves on this machine (real home, sandbox
/// override, …).
fn tool_home_relative_path(tool: &str, subdir: &str, name: &str) -> String {
    format!("{}/{subdir}/{name}", tool_dotdir(tool))
}

/// Stable tool → home-dotdir mapping. Matches `real_home_for` in
/// `ainb-adapters-tool` so the canonical relative path lines up with the
/// real on-disk layout.
fn tool_dotdir(tool: &str) -> &'static str {
    match tool {
        "claude" => ".claude",
        "codex" => ".codex",
        "copilot" => ".copilot",
        "gemini" => ".gemini",
        "cursor" => ".cursor",
        "amazonq" => ".aws/amazonq",
        "cline" => ".cline",
        "roo" => ".roo",
        // Unknown tools fall back to a dotted name; keeps the path
        // informative without a hard failure.
        _ => leak_dotted(tool),
    }
}

/// Leak a `".{tool}"` string for the unknown-tool fallback so the return
/// type stays `&'static str`. Only hit for tools outside the known set,
/// so the (tiny, bounded-by-tool-name-variety) leak is acceptable.
fn leak_dotted(tool: &str) -> &'static str {
    Box::leak(format!(".{tool}").into_boxed_str())
}

fn canonical_or_self(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Current wall-clock time as an RFC 3339 string. Uses
/// `std::time::SystemTime` formatted as a UTC instant without pulling in
/// `chrono` at the CLI layer.
fn now_rfc3339() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Minimal RFC 3339 rendering from a Unix timestamp (UTC). Avoids a
    // chrono dependency for a single timestamp; the value is only used
    // for display + round-trip, not parsed back by ainb.
    format_unix_utc(secs)
}

/// Render a Unix timestamp (seconds, UTC) as `YYYY-MM-DDTHH:MM:SSZ`.
fn format_unix_utc(secs: u64) -> String {
    // Days since epoch + seconds-of-day.
    let days = secs / 86_400;
    let sod = secs % 86_400;
    let (hh, mm, ss) = (sod / 3600, (sod % 3600) / 60, sod % 60);
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Howard Hinnant's `civil_from_days` — convert days-since-epoch to a
/// proleptic Gregorian (year, month, day). Public-domain algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
