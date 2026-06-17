//! `ainb skill library {list,add,new}` integration tests — bead ai-lgk.
//!
//! The own-skill library is YAML-backed (`library.yaml`), no SQLite.
//! Every test isolates the tool home via `AINB_TOOL_HOME_CLAUDE`
//! pointed at a per-test tempdir, so `library add` / `new` never touch
//! the user's real `~/.claude`. Env mutation is serialised through
//! `ENV_LOCK` because cargo runs tests in parallel within a binary.

use std::fs;
use std::path::{Path, PathBuf};

use ainb_cli::{Command as CliCommand, LibraryCmd, SkillCommand, dispatch};
use ainb_skill_core::library::{Library, library_path_in};

/// Serialises `AINB_TOOL_HOME_CLAUDE` mutation across these tests so a
/// parallel run doesn't trample another test's tool home.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Sandbox {
    _tmp: tempfile::TempDir,
    /// `$AINB_HOME` — where `library.yaml` lands.
    home: PathBuf,
    /// The isolated claude tool home (pointed at by AINB_TOOL_HOME_CLAUDE).
    claude_home: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = tempfile::Builder::new().prefix("ainb-library-cli-").tempdir().expect("tempdir");
        let home = tmp.path().join("ainb");
        fs::create_dir_all(&home).unwrap();
        let claude_home = tmp.path().join("claude-home");
        fs::create_dir_all(&claude_home).unwrap();
        Self {
            _tmp: tmp,
            home,
            claude_home,
        }
    }

    fn library(&self) -> Library {
        Library::load_from(&library_path_in(&self.home)).expect("load library")
    }
}

/// Run a `skill library` subcommand against the sandbox with the claude
/// tool home isolated to `sandbox.claude_home`. Returns the captured
/// stdout. Holds `ENV_LOCK` for the duration so the env override is
/// race-free.
fn run_library(sandbox: &Sandbox, cmd: LibraryCmd) -> (anyhow::Result<()>, String) {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_CLAUDE", &sandbox.claude_home);
    let mut buf: Vec<u8> = Vec::new();
    let res = dispatch(
        &sandbox.home,
        CliCommand::Skill {
            action: SkillCommand::Library { cmd },
        },
        &mut buf,
    );
    std::env::remove_var("AINB_TOOL_HOME_CLAUDE");
    (res, String::from_utf8(buf).expect("utf8 stdout"))
}

/// Seed an on-disk skill folder under the claude tool home so
/// `library add <path>` has something to ingest.
fn seed_skill_folder(claude_home: &Path, name: &str) -> PathBuf {
    let dir = claude_home.join("skills").join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\nkind: skill\n---\nHand-authored {name}.\n"),
    )
    .unwrap();
    dir
}

#[test]
fn library_new_scaffolds_valid_skill_md() {
    let sb = Sandbox::new();
    let (res, out) = run_library(
        &sb,
        LibraryCmd::New {
            name: "my-new-skill".into(),
            tool: None,
        },
    );
    res.expect("library new ok");
    assert!(out.contains("my-new-skill"), "out names the skill: {out}");

    // The scaffolded SKILL.md exists, has frontmatter, and is parseable.
    let skill_md = sb.claude_home.join("skills").join("my-new-skill").join("SKILL.md");
    assert!(skill_md.is_file(), "SKILL.md scaffolded at {skill_md:?}");
    let text = fs::read_to_string(&skill_md).unwrap();
    assert!(text.starts_with("---"), "has frontmatter open: {text}");
    assert!(
        text.contains("name: my-new-skill"),
        "frontmatter name: {text}"
    );
    assert!(
        text.contains("kind: skill"),
        "frontmatter declares kind: {text}"
    );
    // Frontmatter is closed by a second `---`.
    assert_eq!(
        text.matches("---").count(),
        2,
        "frontmatter opened and closed: {text}"
    );

    // It was registered in library.yaml with a tool-home-relative path.
    let lib = sb.library();
    let owned = lib.get("my-new-skill").expect("registered");
    assert_eq!(owned.path, ".claude/skills/my-new-skill");
    assert!(owned.promoted_uri.is_none());
}

#[test]
fn library_add_existing_folder() {
    let sb = Sandbox::new();
    let dir = seed_skill_folder(&sb.claude_home, "adopt-me");

    let (res, out) = run_library(
        &sb,
        LibraryCmd::Add {
            path: dir.clone(),
            tool: None,
        },
    );
    res.expect("library add ok");
    assert!(out.contains("adopt-me"), "out names the skill: {out}");

    let lib = sb.library();
    let owned = lib.get("adopt-me").expect("registered after add");
    assert_eq!(
        owned.path, ".claude/skills/adopt-me",
        "registers the tool-home-relative path"
    );
}

#[test]
fn library_list_shows_owned() {
    let sb = Sandbox::new();
    // Register two via `new` so we have rows to list.
    run_library(
        &sb,
        LibraryCmd::New {
            name: "alpha".into(),
            tool: None,
        },
    )
    .0
    .expect("new alpha");
    run_library(
        &sb,
        LibraryCmd::New {
            name: "beta".into(),
            tool: None,
        },
    )
    .0
    .expect("new beta");

    // Plain list.
    let (res, out) = run_library(&sb, LibraryCmd::List { json: false });
    res.expect("list ok");
    assert!(out.contains("alpha"), "lists alpha: {out}");
    assert!(out.contains("beta"), "lists beta: {out}");

    // JSON list.
    let (res, json) = run_library(&sb, LibraryCmd::List { json: true });
    res.expect("list --json ok");
    let parsed: serde_json::Value = serde_json::from_str(json.trim()).expect("valid json");
    let arr = parsed.as_array().expect("json array");
    assert_eq!(arr.len(), 2, "two owned units: {json}");
    let names: Vec<&str> =
        arr.iter().filter_map(|v| v.get("name").and_then(|n| n.as_str())).collect();
    assert!(
        names.contains(&"alpha") && names.contains(&"beta"),
        "names: {names:?}"
    );
}

#[test]
fn library_add_rejects_outside_tool_home() {
    let sb = Sandbox::new();
    // A folder OUTSIDE the claude tool home — sibling of it, not under it.
    let outside = sb._tmp.path().join("outside-skills").join("rogue");
    fs::create_dir_all(&outside).unwrap();
    fs::write(
        outside.join("SKILL.md"),
        "---\nname: rogue\nkind: skill\n---\nNot under any tool home.\n",
    )
    .unwrap();

    let (res, _out) = run_library(
        &sb,
        LibraryCmd::Add {
            path: outside.clone(),
            tool: None,
        },
    );
    assert!(
        res.is_err(),
        "add must refuse a path outside the sandbox tool homes"
    );
    let msg = format!("{}", res.unwrap_err());
    assert!(
        msg.to_lowercase().contains("outside") || msg.to_lowercase().contains("tool home"),
        "error names the safety reason: {msg}"
    );

    // Nothing was registered.
    let lib = sb.library();
    assert!(
        lib.get("rogue").is_none(),
        "rejected path is not registered"
    );
}
