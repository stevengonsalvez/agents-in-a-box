//! `ainb search` integration tests.

use std::path::{Path, PathBuf};

use ainb_cli::{AddArgs, Command, NameArg, SearchArgs, SourceCommand, dispatch};

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("ainb-search-test-").tempdir().expect("tempdir")
}

fn raw_fixture(skills: &[(&str, &str, &[&str])]) -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    for (name, description, tags) in skills {
        let p: PathBuf = dir.path().join(format!("skills/{name}/SKILL.md"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let tags_yaml = if tags.is_empty() {
            String::new()
        } else {
            format!(
                "tags: [{}]\n",
                tags.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ")
            )
        };
        std::fs::write(
            &p,
            format!("---\nname: {name}\ndescription: {description}\n{tags_yaml}---\nbody\n"),
        )
        .unwrap();
    }
    (format!("local:{}", dir.path().display()), dir)
}

fn run_search(home: &Path, query: &str, kind: Option<&str>) -> (String, anyhow::Result<()>) {
    let mut buf = Vec::new();
    let res = dispatch(
        home,
        Command::Search(SearchArgs {
            query: query.to_string(),
            kind: kind.map(str::to_string),
        }),
        &mut buf,
    );
    (String::from_utf8(buf).expect("utf8"), res)
}

fn add_source(home: &Path, uri: String, name: &str) {
    let mut buf = Vec::new();
    dispatch(
        home,
        Command::Source {
            action: SourceCommand::Add(AddArgs {
                uri,
                name: Some(name.to_string()),
                kind: None,
            }),
        },
        &mut buf,
    )
    .expect("add");
}

#[test]
fn empty_manifest_yields_no_matches() {
    let home = tmp_home();
    let (out, res) = run_search(home.path(), "anything", None);
    res.unwrap();
    assert!(out.contains("no matches"), "got: {out}");
}

#[test]
fn finds_units_across_added_source() {
    let home = tmp_home();
    let (uri, _fix) = raw_fixture(&[
        ("commit", "well-formatted commits", &["git", "workflow"]),
        ("review", "thorough code review", &["code"]),
    ]);
    add_source(home.path(), uri, "src1");

    let (out, res) = run_search(home.path(), "", None);
    res.unwrap();
    assert!(out.contains("URI"), "header missing: {out}");
    assert!(out.contains("commit"), "got: {out}");
    assert!(out.contains("review"), "got: {out}");
}

#[test]
fn filters_by_substring_in_description() {
    let home = tmp_home();
    let (uri, _fix) = raw_fixture(&[
        ("commit", "git commits", &[]),
        ("agent", "ai assistant routing", &[]),
    ]);
    add_source(home.path(), uri, "src");

    let (out, res) = run_search(home.path(), "routing", None);
    res.unwrap();
    assert!(out.contains("agent"));
    assert!(!out.contains("commit"));
}

#[test]
fn filters_by_tag() {
    let home = tmp_home();
    let (uri, _fix) = raw_fixture(&[("a", "x", &["git"]), ("b", "x", &["docker"])]);
    add_source(home.path(), uri, "src");

    let (out, res) = run_search(home.path(), "docker", None);
    res.unwrap();
    assert!(out.contains("b"));
    assert!(!out.contains(" a "), "should not list 'a': {out}");
}

#[test]
fn filters_by_kind() {
    let home = tmp_home();
    let dir = tempfile::tempdir().unwrap();
    // Mix a skill and a plugin in one source.
    std::fs::create_dir_all(dir.path().join("skills/foo")).unwrap();
    std::fs::write(
        dir.path().join("skills/foo/SKILL.md"),
        "---\nname: foo\ndescription: a skill\n---\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("plugins/bar")).unwrap();
    std::fs::write(
        dir.path().join("plugins/bar/plugin.json"),
        r#"{"name":"bar","description":"a plugin"}"#,
    )
    .unwrap();

    let uri = format!("local:{}", dir.path().display());
    add_source(home.path(), uri, "mixed");

    let (out, res) = run_search(home.path(), "", Some("plugin"));
    res.unwrap();
    assert!(out.contains("bar"));
    assert!(!out.contains("foo"));
}

#[test]
fn disabled_sources_are_excluded() {
    let home = tmp_home();
    let (uri, _fix) = raw_fixture(&[("only", "test", &[])]);
    add_source(home.path(), uri, "off");

    // Disable the source.
    let mut buf = Vec::new();
    dispatch(
        home.path(),
        Command::Source {
            action: SourceCommand::Disable(NameArg { name: "off".into() }),
        },
        &mut buf,
    )
    .unwrap();

    let (out, res) = run_search(home.path(), "", None);
    res.unwrap();
    assert!(out.contains("no matches"));
}

#[test]
fn search_results_carry_full_unit_uri() {
    let home = tmp_home();
    let (uri, fixture) = raw_fixture(&[("commit", "test", &[])]);
    add_source(home.path(), uri, "src1");

    let (out, res) = run_search(home.path(), "commit", None);
    res.unwrap();
    // URI in output should be `local:<path>@main/skills/commit`.
    let expected_prefix = format!("local:{}", fixture.path().display());
    assert!(
        out.contains(&expected_prefix),
        "uri prefix missing: got {out}, want prefix {expected_prefix}"
    );
    assert!(out.contains("@main/skills/commit"), "got: {out}");
}
