//! Class-C walker — orphan units across the 9 adapter tool homes.
//!
//! Walks every (tool, kind-subdir) pair the v1 adapters know about
//! and returns one `DiscoveredOrphanUnit` per on-disk candidate. The
//! walker is pure: no manifest reads, no writes, no network. It
//! deliberately mirrors the install-root resolution used by the
//! adapters themselves via `ainb_adapters_tool::install_root_for`,
//! so the same `AINB_TOOL_HOME_<TOOL>` env override that sandboxes
//! adapter writes also sandboxes the walker for tests.
//!
//! ## Layout assumptions (mirror of `convention.rs`)
//!
//! For each tool there are two unit-layout shapes:
//!
//! - **Directory** units: `<root>/<plural-kind>/<name>/...`
//!   (e.g. `skills/foo/SKILL.md`, `plugins/bar/plugin.json`,
//!   `mcp-servers/baz/...`). The walker looks for `SKILL.md`
//!   inside the unit dir; if present it parses YAML frontmatter
//!   best-effort. When absent or unparseable the walker still
//!   emits the unit with `frontmatter_valid = false` and the unit
//!   name falls back to the directory name.
//!
//! - **Flat-md** units: `<root>/<plural-kind>/<name>.md`
//!   (e.g. `agents/reviewer.md`, `commands/quick-help.md`). The
//!   file itself is parsed for frontmatter; the file-stem is the
//!   fallback name.
//!
//! Per-tool subdir support matches each adapter's `list_installed`
//! (see `ainb-adapters-tool/src/{claude,codex,…}.rs`). We keep the
//! mapping local instead of extending the `ToolAdapter` trait so
//! discovery stays decoupled from deploy mechanics.

use std::path::{Path, PathBuf};

use ainb_adapters_tool::read_root_for;
use ainb_skill_core::UnitKind;

/// One unit found on disk that may or may not already be in the
/// manifest. The reconciler (P3) decides which become
/// `local:` adoptees vs which are shadowed by class-A marketplace
/// plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredOrphanUnit {
    /// Tool name (`claude`, `codex`, …). Matches `ToolAdapter::name()`.
    pub tool: String,
    /// Best-effort unit kind. Comes from frontmatter `kind:` when
    /// valid; otherwise inferred from the subdir (e.g. `skills/`
    /// → `Skill`).
    pub kind: UnitKind,
    /// Unit name. Frontmatter `name:` wins; falls back to the
    /// directory name (dir-layout) or file stem (flat-md).
    pub name: String,
    /// Absolute path. For dir-layout units this is the unit
    /// directory; for flat-md it is the `.md` file itself. The
    /// reconciler uses this when synthesizing
    /// `local:~/.<tool>/<subdir>@head/<name>` URIs.
    pub path: PathBuf,
    /// Whether the SKILL.md (or flat `.md`) had a parseable YAML
    /// frontmatter block. `true` even when `name:` / `kind:` are
    /// absent, so long as the YAML mapping itself parsed. `false`
    /// when the file is missing, has no `---` frontmatter, or the
    /// YAML failed to parse.
    pub frontmatter_valid: bool,
}

/// Stable list of the 9 v1 adapter tools, matching
/// `ainb_adapters_tool::all_adapters()` order.
pub const ALL_TOOLS: &[&str] = &[
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

/// On-disk layout shape for one (tool, kind) pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Unit lives at `<root>/<plural-kind>/<name>/...` and may have
    /// a `SKILL.md` inside for frontmatter.
    Dir,
    /// Unit is a single `<root>/<plural-kind>/<name>.md` file.
    FlatMd,
}

/// (UnitKind, Layout, subdir-name) triples per tool. Mirrors each
/// adapter's `list_installed` calls in
/// `ainb-adapters-tool/src/<tool>.rs`. Subdirs the adapter does
/// not deploy to are deliberately omitted so we never surface a
/// "skill" found in a tool that doesn't accept skills.
fn tool_subdirs(tool: &str) -> &'static [(UnitKind, Layout, &'static str)] {
    match tool {
        "claude" => &[
            (UnitKind::Skill, Layout::Dir, "skills"),
            (UnitKind::Plugin, Layout::Dir, "plugins"),
            (UnitKind::Hook, Layout::Dir, "hooks"),
            (UnitKind::McpServer, Layout::Dir, "mcp-servers"),
            (UnitKind::Statusline, Layout::Dir, "statuslines"),
            (UnitKind::Agent, Layout::FlatMd, "agents"),
            (UnitKind::Command, Layout::FlatMd, "commands"),
        ],
        "codex" => &[
            (UnitKind::Skill, Layout::Dir, "skills"),
            (UnitKind::McpServer, Layout::Dir, "mcp-servers"),
            (UnitKind::Agent, Layout::FlatMd, "agents"),
            (UnitKind::Command, Layout::FlatMd, "commands"),
        ],
        "copilot" => &[
            (UnitKind::Skill, Layout::Dir, "skills"),
            (UnitKind::Agent, Layout::FlatMd, "agents"),
        ],
        "gemini" => &[
            (UnitKind::Skill, Layout::Dir, "skills"),
            (UnitKind::Agent, Layout::FlatMd, "agents"),
        ],
        "cursor" => &[
            (UnitKind::Skill, Layout::Dir, "skills"),
            (UnitKind::McpServer, Layout::Dir, "mcp-servers"),
            (UnitKind::Command, Layout::FlatMd, "commands"),
        ],
        "amazonq" => &[(UnitKind::Skill, Layout::Dir, "skills")],
        "claude-desktop" => &[(UnitKind::McpServer, Layout::Dir, "mcp-servers")],
        "cline" => &[
            (UnitKind::Skill, Layout::Dir, "skills"),
            (UnitKind::McpServer, Layout::Dir, "mcp-servers"),
        ],
        "roo" => &[
            (UnitKind::Skill, Layout::Dir, "skills"),
            (UnitKind::McpServer, Layout::Dir, "mcp-servers"),
        ],
        _ => &[],
    }
}

/// Walk every adapter tool's install root for orphan units. The
/// read root is resolved per the two-tier precedence in
/// `read_root_for` (env override → real home), so the SkillManager
/// discovery banner sees the user's actual `~/.claude/skills/...`
/// without `AINB_USE_REAL_HOMES=1`. Writes still go through
/// `install_root_for` (sandbox-by-default).
///
/// `AINB_TOOL_HOME_<TOOL>` env vars steer the walker at test or run
/// time without touching this function.
///
/// Output ordering is deterministic per tool (the `ALL_TOOLS`
/// order) but file-system enumeration inside each subdir mirrors
/// `read_dir`, which is platform-dependent. Callers that need
/// stable ordering (TUI rendering, snapshots) should sort the
/// returned vector.
pub fn walk_orphans() -> Vec<DiscoveredOrphanUnit> {
    let mut out = Vec::new();
    for tool in ALL_TOOLS {
        let root = read_root_for(tool);
        walk_one_tool(tool, &root, &mut out);
    }
    out
}

/// Test-friendly variant: walk a single tool given its already-
/// resolved root path. Doesn't touch env or the manifest. Used by
/// per-tool fixture tests so each test can stand up its own
/// tempdir without serializing on a global env lock.
pub fn walk_one_tool(tool: &str, root: &Path, out: &mut Vec<DiscoveredOrphanUnit>) {
    if !root.exists() {
        return;
    }
    for (kind, layout, subdir) in tool_subdirs(tool) {
        let dir = root.join(subdir);
        match layout {
            Layout::Dir => walk_dir_units(tool, *kind, &dir, out),
            Layout::FlatMd => walk_flat_md(tool, *kind, &dir, out),
        }
    }
}

fn walk_dir_units(
    tool: &str,
    fallback_kind: UnitKind,
    dir: &Path,
    out: &mut Vec<DiscoveredOrphanUnit>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_dir() {
            continue;
        }
        let Some(dir_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        // `~/.claude/plugins/cache/` is Claude Code's marketplace
        // plugin cache, owned by class-A discovery. A bare class-C
        // walk would otherwise surface `cache` as a synthetic orphan
        // plugin named "cache". Skip it for the claude tool only —
        // other tools have no such reserved subdir.
        if tool == "claude" && fallback_kind == UnitKind::Plugin && dir_name == "cache" {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        let (name, kind, valid) = if skill_md.is_file() {
            parse_frontmatter(&skill_md, dir_name, fallback_kind)
        } else {
            (dir_name.to_string(), fallback_kind, false)
        };
        out.push(DiscoveredOrphanUnit {
            tool: tool.to_string(),
            kind,
            name,
            path,
            frontmatter_valid: valid,
        });
    }
}

fn walk_flat_md(
    tool: &str,
    fallback_kind: UnitKind,
    dir: &Path,
    out: &mut Vec<DiscoveredOrphanUnit>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let (name, kind, valid) = parse_frontmatter(&path, stem, fallback_kind);
        out.push(DiscoveredOrphanUnit {
            tool: tool.to_string(),
            kind,
            name,
            path,
            frontmatter_valid: valid,
        });
    }
}

/// Best-effort frontmatter parse.
///
/// Returns `(name, kind, frontmatter_valid)`:
/// - `name` = frontmatter `name:` value if non-empty, else `fallback_name`.
/// - `kind` = frontmatter `kind:` parsed as `UnitKind`
///   (accepts `mcp` as an alias for `mcp-server` per spec
///   §Frontmatter validity), else `fallback_kind`.
/// - `frontmatter_valid` = `true` only if a `---\n...\n---` block
///   was present AND parsed as a YAML mapping. Missing files,
///   missing fences, and malformed YAML all yield `false`.
fn parse_frontmatter(
    path: &Path,
    fallback_name: &str,
    fallback_kind: UnitKind,
) -> (String, UnitKind, bool) {
    let body = match std::fs::read_to_string(path) {
        Ok(b) => b,
        Err(_) => return (fallback_name.to_string(), fallback_kind, false),
    };
    let Some(frontmatter) = extract_frontmatter(&body) else {
        return (fallback_name.to_string(), fallback_kind, false);
    };
    let parsed: serde_yaml_ng::Value = match serde_yaml_ng::from_str(frontmatter) {
        Ok(v) => v,
        Err(_) => return (fallback_name.to_string(), fallback_kind, false),
    };
    let Some(map) = parsed.as_mapping() else {
        // Frontmatter was present but didn't parse to a mapping
        // (e.g. a bare scalar). Treat as invalid per spec.
        return (fallback_name.to_string(), fallback_kind, false);
    };
    let name = map
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| fallback_name.to_string());
    let kind = map
        .get("kind")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .and_then(parse_kind_with_alias)
        .unwrap_or(fallback_kind);
    (name, kind, true)
}

/// Accept the spec's `mcp` short form alongside the canonical
/// `mcp-server` (matches `UnitKind::from_str` plus the
/// frontmatter-only alias). Returns `None` for anything else so
/// the caller falls back to the path-derived kind.
fn parse_kind_with_alias(raw: &str) -> Option<UnitKind> {
    let normalized = match raw {
        "mcp" => "mcp-server",
        other => other,
    };
    normalized.parse().ok()
}

/// Strip a leading `---\n...\n---` YAML block and return its
/// inner text. Supports both `\n` and `\r\n` line endings. A file
/// without a leading fence yields `None`.
fn extract_frontmatter(body: &str) -> Option<&str> {
    let after_open = body.strip_prefix("---\n").or_else(|| body.strip_prefix("---\r\n"))?;
    // Look for a closing fence — `\n---\n`, `\n---\r\n`, or `\n---`
    // at EOF (no trailing newline).
    if let Some(end) = after_open.find("\n---\n") {
        return Some(&after_open[..end]);
    }
    if let Some(end) = after_open.find("\n---\r\n") {
        return Some(&after_open[..end]);
    }
    if let Some(stripped) = after_open.strip_suffix("\n---") {
        return Some(stripped);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use tempfile::TempDir;

    /// Build a `~/.<tool>` tree under `root` and write one unit.
    /// Returns the absolute path the walker should report.
    fn write_dir_unit(root: &Path, subdir: &str, name: &str, skill_md: Option<&str>) -> PathBuf {
        let unit = root.join(subdir).join(name);
        fs::create_dir_all(&unit).unwrap();
        if let Some(body) = skill_md {
            fs::write(unit.join("SKILL.md"), body).unwrap();
        }
        unit
    }

    fn write_flat_md(root: &Path, subdir: &str, name: &str, body: &str) -> PathBuf {
        let dir = root.join(subdir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.md"));
        fs::write(&path, body).unwrap();
        path
    }

    fn find<'a>(
        out: &'a [DiscoveredOrphanUnit],
        tool: &str,
        name: &str,
    ) -> &'a DiscoveredOrphanUnit {
        out.iter()
            .find(|u| u.tool == tool && u.name == name)
            .unwrap_or_else(|| panic!("missing unit `{tool}/{name}` in {out:?}"))
    }

    // ---- frontmatter parsing edge cases -------------------------

    #[test]
    fn frontmatter_full_overrides_name_and_kind() {
        let tmp = TempDir::new().unwrap();
        let body = "---\nname: real-name\nkind: agent\n---\nbody\n";
        let path = tmp.path().join("x.md");
        fs::write(&path, body).unwrap();
        let (n, k, v) = parse_frontmatter(&path, "fallback", UnitKind::Skill);
        assert_eq!(n, "real-name");
        assert_eq!(k, UnitKind::Agent);
        assert!(v);
    }

    #[test]
    fn frontmatter_name_only_keeps_fallback_kind() {
        let tmp = TempDir::new().unwrap();
        let body = "---\nname: only-name\n---\n";
        let path = tmp.path().join("x.md");
        fs::write(&path, body).unwrap();
        let (n, k, v) = parse_frontmatter(&path, "fallback", UnitKind::Command);
        assert_eq!(n, "only-name");
        assert_eq!(k, UnitKind::Command);
        assert!(v, "name-only frontmatter still counts as valid");
    }

    #[test]
    fn frontmatter_kind_only_keeps_fallback_name() {
        let tmp = TempDir::new().unwrap();
        let body = "---\nkind: hook\n---\n";
        let path = tmp.path().join("x.md");
        fs::write(&path, body).unwrap();
        let (n, k, v) = parse_frontmatter(&path, "fallback", UnitKind::Skill);
        assert_eq!(n, "fallback");
        assert_eq!(k, UnitKind::Hook);
        assert!(v, "kind-only frontmatter still counts as valid");
    }

    #[test]
    fn frontmatter_missing_falls_back_to_dir_name() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("x.md");
        fs::write(&path, "no frontmatter here\n").unwrap();
        let (n, k, v) = parse_frontmatter(&path, "dir-name", UnitKind::Plugin);
        assert_eq!(n, "dir-name");
        assert_eq!(k, UnitKind::Plugin);
        assert!(!v);
    }

    #[test]
    fn frontmatter_malformed_falls_back_to_dir_name() {
        let tmp = TempDir::new().unwrap();
        // Open fence, garbage YAML, close fence.
        let body = "---\n  : : oops:\n  - this is not a map\n---\nx\n";
        let path = tmp.path().join("x.md");
        fs::write(&path, body).unwrap();
        let (n, k, v) = parse_frontmatter(&path, "dir-name", UnitKind::Skill);
        assert_eq!(n, "dir-name");
        assert_eq!(k, UnitKind::Skill);
        assert!(!v, "malformed YAML must not be reported as valid");
    }

    #[test]
    fn frontmatter_unknown_kind_falls_back_to_path_inferred_kind() {
        let tmp = TempDir::new().unwrap();
        let body = "---\nname: x\nkind: nonsense\n---\n";
        let path = tmp.path().join("x.md");
        fs::write(&path, body).unwrap();
        let (n, k, v) = parse_frontmatter(&path, "dir-name", UnitKind::Agent);
        assert_eq!(n, "x");
        assert_eq!(
            k,
            UnitKind::Agent,
            "unknown kind falls back to path-inferred"
        );
        assert!(v, "frontmatter still parses; only kind is rejected");
    }

    #[test]
    fn frontmatter_accepts_mcp_alias_for_mcp_server() {
        let tmp = TempDir::new().unwrap();
        let body = "---\nname: srv\nkind: mcp\n---\n";
        let path = tmp.path().join("x.md");
        fs::write(&path, body).unwrap();
        let (_, k, v) = parse_frontmatter(&path, "fallback", UnitKind::Skill);
        assert_eq!(k, UnitKind::McpServer);
        assert!(v);
    }

    #[test]
    fn frontmatter_empty_name_keeps_fallback() {
        let tmp = TempDir::new().unwrap();
        let body = "---\nname: \"\"\nkind: skill\n---\n";
        let path = tmp.path().join("x.md");
        fs::write(&path, body).unwrap();
        let (n, _, v) = parse_frontmatter(&path, "fallback", UnitKind::Skill);
        assert_eq!(n, "fallback", "empty `name:` must fall back");
        assert!(v);
    }

    #[test]
    fn frontmatter_no_close_fence_is_invalid() {
        let tmp = TempDir::new().unwrap();
        let body = "---\nname: dangling\nkind: skill\n";
        let path = tmp.path().join("x.md");
        fs::write(&path, body).unwrap();
        let (n, k, v) = parse_frontmatter(&path, "fallback", UnitKind::Skill);
        assert_eq!(n, "fallback");
        assert_eq!(k, UnitKind::Skill);
        assert!(!v, "unclosed frontmatter is invalid");
    }

    #[test]
    fn frontmatter_crlf_line_endings_are_supported() {
        let tmp = TempDir::new().unwrap();
        let body = "---\r\nname: crlf\r\nkind: agent\r\n---\r\nbody\r\n";
        let path = tmp.path().join("x.md");
        fs::write(&path, body).unwrap();
        let (n, k, v) = parse_frontmatter(&path, "fallback", UnitKind::Skill);
        assert_eq!(n, "crlf");
        assert_eq!(k, UnitKind::Agent);
        assert!(v);
    }

    // ---- per-tool walker coverage --------------------------------

    #[test]
    fn walks_claude_skills_with_valid_frontmatter() {
        let tmp = TempDir::new().unwrap();
        let unit_path = write_dir_unit(
            tmp.path(),
            "skills",
            "commit",
            Some("---\nname: commit\nkind: skill\n---\nbody\n"),
        );
        let mut out = Vec::new();
        walk_one_tool("claude", tmp.path(), &mut out);
        let u = find(&out, "claude", "commit");
        assert_eq!(u.kind, UnitKind::Skill);
        assert_eq!(u.path, unit_path);
        assert!(u.frontmatter_valid);
    }

    #[test]
    fn walks_claude_skills_without_skill_md_uses_dir_name() {
        let tmp = TempDir::new().unwrap();
        let unit_path = write_dir_unit(tmp.path(), "skills", "no-frontmatter", None);
        let mut out = Vec::new();
        walk_one_tool("claude", tmp.path(), &mut out);
        let u = find(&out, "claude", "no-frontmatter");
        assert_eq!(u.kind, UnitKind::Skill);
        assert_eq!(u.path, unit_path);
        assert!(!u.frontmatter_valid);
    }

    #[test]
    fn walks_claude_agents_as_flat_md() {
        let tmp = TempDir::new().unwrap();
        let path = write_flat_md(
            tmp.path(),
            "agents",
            "reviewer",
            "---\nname: reviewer\nkind: agent\n---\nbody\n",
        );
        let mut out = Vec::new();
        walk_one_tool("claude", tmp.path(), &mut out);
        let u = find(&out, "claude", "reviewer");
        assert_eq!(u.kind, UnitKind::Agent);
        assert_eq!(u.path, path);
        assert!(u.frontmatter_valid);
    }

    #[test]
    fn walks_claude_commands_with_no_frontmatter_uses_file_stem() {
        let tmp = TempDir::new().unwrap();
        let path = write_flat_md(tmp.path(), "commands", "quick", "just a body\n");
        let mut out = Vec::new();
        walk_one_tool("claude", tmp.path(), &mut out);
        let u = find(&out, "claude", "quick");
        assert_eq!(u.kind, UnitKind::Command);
        assert_eq!(u.path, path);
        assert!(!u.frontmatter_valid);
    }

    #[test]
    fn walks_claude_all_supported_kinds() {
        let tmp = TempDir::new().unwrap();
        write_dir_unit(tmp.path(), "skills", "s1", None);
        write_dir_unit(tmp.path(), "plugins", "p1", None);
        write_dir_unit(tmp.path(), "hooks", "h1", None);
        write_dir_unit(tmp.path(), "mcp-servers", "m1", None);
        write_dir_unit(tmp.path(), "statuslines", "sl1", None);
        write_flat_md(tmp.path(), "agents", "a1", "");
        write_flat_md(tmp.path(), "commands", "c1", "");
        let mut out = Vec::new();
        walk_one_tool("claude", tmp.path(), &mut out);
        assert_eq!(out.len(), 7, "got: {out:#?}");
        assert_eq!(find(&out, "claude", "s1").kind, UnitKind::Skill);
        assert_eq!(find(&out, "claude", "p1").kind, UnitKind::Plugin);
        assert_eq!(find(&out, "claude", "h1").kind, UnitKind::Hook);
        assert_eq!(find(&out, "claude", "m1").kind, UnitKind::McpServer);
        assert_eq!(find(&out, "claude", "sl1").kind, UnitKind::Statusline);
        assert_eq!(find(&out, "claude", "a1").kind, UnitKind::Agent);
        assert_eq!(find(&out, "claude", "c1").kind, UnitKind::Command);
    }

    #[test]
    fn walks_codex_supported_kinds_only() {
        let tmp = TempDir::new().unwrap();
        write_dir_unit(tmp.path(), "skills", "s1", None);
        write_dir_unit(tmp.path(), "mcp-servers", "m1", None);
        write_flat_md(tmp.path(), "agents", "a1", "");
        write_flat_md(tmp.path(), "commands", "c1", "");
        // Decoy subdirs codex doesn't accept — must NOT appear.
        write_dir_unit(tmp.path(), "plugins", "p1", None);
        write_dir_unit(tmp.path(), "hooks", "h1", None);
        let mut out = Vec::new();
        walk_one_tool("codex", tmp.path(), &mut out);
        let names: Vec<&str> = out.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(out.len(), 4, "got names {names:?}");
        assert!(names.contains(&"s1"));
        assert!(names.contains(&"m1"));
        assert!(names.contains(&"a1"));
        assert!(names.contains(&"c1"));
        assert!(
            !names.contains(&"p1"),
            "plugins must not be discovered for codex"
        );
        assert!(
            !names.contains(&"h1"),
            "hooks must not be discovered for codex"
        );
    }

    #[test]
    fn walks_amazonq_only_skills() {
        let tmp = TempDir::new().unwrap();
        write_dir_unit(tmp.path(), "skills", "only-skill", None);
        write_flat_md(tmp.path(), "agents", "nope", "");
        let mut out = Vec::new();
        walk_one_tool("amazonq", tmp.path(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "only-skill");
        assert_eq!(out[0].kind, UnitKind::Skill);
    }

    #[test]
    fn walks_claude_desktop_only_mcp_servers() {
        let tmp = TempDir::new().unwrap();
        write_dir_unit(tmp.path(), "mcp-servers", "filesystem", None);
        write_dir_unit(tmp.path(), "skills", "nope", None);
        let mut out = Vec::new();
        walk_one_tool("claude-desktop", tmp.path(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "filesystem");
        assert_eq!(out[0].kind, UnitKind::McpServer);
    }

    #[test]
    fn walks_cursor_skills_mcp_and_commands() {
        let tmp = TempDir::new().unwrap();
        write_dir_unit(tmp.path(), "skills", "s1", None);
        write_dir_unit(tmp.path(), "mcp-servers", "m1", None);
        write_flat_md(tmp.path(), "commands", "c1", "");
        // Decoys.
        write_flat_md(tmp.path(), "agents", "nope", "");
        let mut out = Vec::new();
        walk_one_tool("cursor", tmp.path(), &mut out);
        assert_eq!(out.len(), 3);
        let names: Vec<&str> = out.iter().map(|u| u.name.as_str()).collect();
        assert!(names.contains(&"s1"));
        assert!(names.contains(&"m1"));
        assert!(names.contains(&"c1"));
        assert!(!names.contains(&"nope"));
    }

    #[test]
    fn missing_root_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let nonexistent = tmp.path().join("never-created");
        let mut out = Vec::new();
        walk_one_tool("claude", &nonexistent, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn empty_subdir_returns_no_units_for_that_kind() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join("skills")).unwrap();
        let mut out = Vec::new();
        walk_one_tool("claude", tmp.path(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn ignores_non_md_files_in_flat_md_subdir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("agents");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("real.md"), "").unwrap();
        fs::write(dir.join("README.txt"), "").unwrap();
        fs::write(dir.join(".DS_Store"), "").unwrap();
        let mut out = Vec::new();
        walk_one_tool("claude", tmp.path(), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "real");
    }

    #[test]
    fn ignores_files_in_dir_layout_subdir() {
        // A stray file under `skills/` (not a directory) must not
        // be reported as a unit.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("skills");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("stray.md"), "").unwrap();
        let mut out = Vec::new();
        walk_one_tool("claude", tmp.path(), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn unknown_tool_yields_no_units() {
        let tmp = TempDir::new().unwrap();
        write_dir_unit(tmp.path(), "skills", "x", None);
        let mut out = Vec::new();
        walk_one_tool("definitely-not-a-real-tool", tmp.path(), &mut out);
        assert!(out.is_empty());
    }

    // ---- walker is pure: never writes or reads outside `root` ---

    #[test]
    fn does_not_mutate_filesystem() {
        let tmp = TempDir::new().unwrap();
        write_dir_unit(
            tmp.path(),
            "skills",
            "noop",
            Some("---\nname: noop\nkind: skill\n---\n"),
        );
        let before: Vec<_> = walkdir_collect(tmp.path());
        let mut out = Vec::new();
        walk_one_tool("claude", tmp.path(), &mut out);
        let after: Vec<_> = walkdir_collect(tmp.path());
        assert_eq!(before, after, "walker must not create/delete files");
        assert_eq!(out.len(), 1);
    }

    fn walkdir_collect(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in walkdir_iter(root) {
            out.push(entry);
        }
        out.sort();
        out
    }

    /// Minimal recursive directory walk so we don't pull in
    /// walkdir just for this assertion.
    fn walkdir_iter(root: &Path) -> Vec<PathBuf> {
        let mut stack = vec![root.to_path_buf()];
        let mut out = Vec::new();
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p.clone());
                }
                out.push(p);
            }
        }
        out
    }

    // ---- spec §Walker performance budget ------------------------

    /// 100 dir-layout units spread across the 9 tool homes must
    /// walk in under 500ms on a debug build (spec §Walker
    /// performance budget). Sister bead .1 (ClassA) is expected
    /// to add its own slice of the same budget; together the two
    /// walkers must stay under the 500ms ceiling.
    ///
    /// The fixture uses real frontmatter so the YAML parser path
    /// is exercised, not just `read_dir`. Each unit is placed in
    /// the first dir-layout subdir the tool actually deploys to
    /// (most are `skills/`; `claude-desktop` only takes
    /// `mcp-servers/`), so the walker enumerates every placed unit.
    #[test]
    fn perf_budget_under_500ms_for_100_units() {
        let tmp = TempDir::new().unwrap();
        let tool_setup: Vec<(&'static str, PathBuf, &'static str)> = ALL_TOOLS
            .iter()
            .map(|tool| {
                let subdir = tool_subdirs(tool)
                    .iter()
                    .find(|(_, layout, _)| *layout == Layout::Dir)
                    .map(|(_, _, sub)| *sub)
                    .expect("every adapter has at least one dir-layout subdir");
                (*tool, tmp.path().join(tool), subdir)
            })
            .collect();
        let total_units: usize = 100;
        let mut placed = 0usize;
        for (i, (_, root, subdir)) in tool_setup.iter().enumerate() {
            let mut per_tool = total_units / ALL_TOOLS.len();
            if i < total_units % ALL_TOOLS.len() {
                per_tool += 1;
            }
            for n in 0..per_tool {
                let unit = root.join(subdir).join(format!("unit-{i}-{n}"));
                fs::create_dir_all(&unit).unwrap();
                fs::write(
                    unit.join("SKILL.md"),
                    format!("---\nname: unit-{i}-{n}\nkind: skill\n---\nbody\n"),
                )
                .unwrap();
                placed += 1;
            }
        }
        assert_eq!(placed, total_units);

        let start = std::time::Instant::now();
        let mut out = Vec::new();
        for (tool, root, _) in &tool_setup {
            walk_one_tool(tool, root, &mut out);
        }
        let elapsed = start.elapsed();

        assert_eq!(out.len(), total_units, "walker dropped units");
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "walker took {elapsed:?} for {total_units} units; \
             spec §Walker performance budget is <500ms"
        );
    }
}
