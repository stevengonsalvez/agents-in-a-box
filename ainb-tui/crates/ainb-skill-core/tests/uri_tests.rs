//! URI parser tests — positive + negative cases from spec §6.1.

use ainb_skill_core::{SourceType, Uri};

fn assert_roundtrip(input: &str) -> Uri {
    let parsed = Uri::parse(input).unwrap_or_else(|e| panic!("`{input}` failed to parse: {e}"));
    assert_eq!(parsed.display(), input, "round-trip mismatch for `{input}`");
    parsed
}

#[test]
fn parses_gh_unit_uri() {
    let u = assert_roundtrip("gh:stevengonsalvez/my-skills@main/skills/review-pr");
    assert_eq!(u.source_type, SourceType::Gh);
    assert_eq!(u.locator, "stevengonsalvez/my-skills");
    assert_eq!(u.ref_.as_deref(), Some("main"));
    assert_eq!(u.path.as_deref(), Some("skills/review-pr"));
    assert!(u.is_unit());
}

#[test]
fn parses_gh_source_only_uri() {
    let u = assert_roundtrip("gh:stevengonsalvez/ai-coder-rules");
    assert_eq!(u.source_type, SourceType::Gh);
    assert_eq!(u.locator, "stevengonsalvez/ai-coder-rules");
    assert!(u.ref_.is_none());
    assert!(u.path.is_none());
    assert!(u.is_source());
    assert!(!u.is_unit());
}

#[test]
fn parses_gh_source_with_ref_only() {
    let u = assert_roundtrip("gh:stevengonsalvez/my-skills@v1.2");
    assert_eq!(u.locator, "stevengonsalvez/my-skills");
    assert_eq!(u.ref_.as_deref(), Some("v1.2"));
    assert!(u.path.is_none());
    assert!(u.is_source());
    assert!(!u.is_unit());
}

#[test]
fn parses_git_https_unit_uri() {
    let u = assert_roundtrip("git:https://gitlab.example.com/x/y@v2.1.0/skills/abc");
    assert_eq!(u.source_type, SourceType::Git);
    assert_eq!(u.locator, "https://gitlab.example.com/x/y");
    assert_eq!(u.ref_.as_deref(), Some("v2.1.0"));
    assert_eq!(u.path.as_deref(), Some("skills/abc"));
}

#[test]
fn parses_local_absolute_path() {
    let u = assert_roundtrip("local:/Users/me/dev/my-repo@HEAD/skills/foo");
    assert_eq!(u.source_type, SourceType::Local);
    assert_eq!(u.locator, "/Users/me/dev/my-repo");
    assert_eq!(u.ref_.as_deref(), Some("HEAD"));
    assert_eq!(u.path.as_deref(), Some("skills/foo"));
}

#[test]
fn parses_marketplace() {
    let u =
        assert_roundtrip("marketplace:anthropic/claude-marketplace@latest/plugin/code-reviewer");
    assert_eq!(u.source_type, SourceType::Marketplace);
    assert_eq!(u.locator, "anthropic/claude-marketplace");
    assert_eq!(u.ref_.as_deref(), Some("latest"));
    assert_eq!(u.path.as_deref(), Some("plugin/code-reviewer"));
}

#[test]
fn parses_gist_single_file() {
    let u = assert_roundtrip("gist:abc123def@HEAD/skill.md");
    assert_eq!(u.source_type, SourceType::Gist);
    assert_eq!(u.locator, "abc123def");
    assert_eq!(u.ref_.as_deref(), Some("HEAD"));
    assert_eq!(u.path.as_deref(), Some("skill.md"));
}

#[test]
fn parses_sha_pinned_uri() {
    let u = assert_roundtrip("gh:anthropics/claude-plugins@abc1234/plugins/reflect");
    assert_eq!(u.ref_.as_deref(), Some("abc1234"));
}

#[test]
fn parses_npm_package_source() {
    let u = assert_roundtrip("npm:@scope/my-pkg@1.0.0/skills/foo");
    assert_eq!(u.source_type, SourceType::Npm);
    // Last `@` wins — supports scoped npm names cleanly.
    assert_eq!(u.locator, "@scope/my-pkg");
    assert_eq!(u.ref_.as_deref(), Some("1.0.0"));
    assert_eq!(u.path.as_deref(), Some("skills/foo"));
}

#[test]
fn rejects_empty_input() {
    let err = Uri::parse("").unwrap_err().to_string();
    assert!(err.contains("invalid unit URI"), "got: {err}");
}

#[test]
fn rejects_missing_colon() {
    assert!(Uri::parse("ghfoo").is_err());
}

#[test]
fn rejects_unknown_source_type() {
    let err = Uri::parse("weird:foo/bar@main/path").unwrap_err().to_string();
    assert!(err.contains("unknown source type"), "got: {err}");
}

#[test]
fn rejects_empty_locator() {
    assert!(Uri::parse("gh:@main/path").is_err());
}

#[test]
fn rejects_empty_ref_after_at() {
    assert!(Uri::parse("gh:foo/bar@").is_err());
}

#[test]
fn rejects_empty_path_after_ref_slash() {
    assert!(Uri::parse("gh:foo/bar@main/").is_err());
}

#[test]
fn rejects_empty_ref_before_path_slash() {
    assert!(Uri::parse("gh:foo/bar@/path").is_err());
}

#[test]
fn serde_yaml_round_trip() {
    let original = Uri::parse("gh:stevengonsalvez/my-skills@main/skills/review-pr").unwrap();
    let yaml = serde_yaml_ng::to_string(&original).unwrap();
    // Should serialize as a plain string, not a tagged map.
    assert!(yaml.contains("gh:stevengonsalvez/my-skills@main/skills/review-pr"));
    let decoded: Uri = serde_yaml_ng::from_str(&yaml).unwrap();
    assert_eq!(decoded, original);
}
