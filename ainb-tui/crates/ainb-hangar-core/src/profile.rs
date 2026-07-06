//! Agent-profile domain: the canonical Claude-subagent `.md` master format, the
//! logical model tier, and the two down-compilers (P5, D14-D16).
//!
//! # Why this lives in `core`
//!
//! Two surfaces need to turn a profile master into a tool-native artifact:
//!
//! - the **daemon** compiles on dispatch (D16) — it materialises the resolved
//!   Claude `.md` / Codex profile+prompt into the task's execution env before
//!   spawning the provider;
//! - the **profile-editor TUI** previews *both* compile targets live as the human
//!   edits an unsaved profile.
//!
//! The compiler is therefore pure, IO-free vocabulary — exactly what this crate
//! is for (mirroring [`crate::skill`]). The daemon adds the disk read/write, the
//! DB index, and the fs-watch around it; the plugin borrows it for preview. No
//! copy of the format logic exists in either.
//!
//! # The canonical master (D14)
//!
//! A profile master is a **Claude subagent `.md`** — YAML-ish frontmatter fenced
//! by `---`, then a Markdown system-prompt body:
//!
//! ```text
//! ---
//! name: code-reviewer
//! description: Reviews a diff for correctness and style
//! model: premium
//! tools: Read, Grep, Glob
//! color: cyan
//! ---
//! You are a meticulous reviewer. …
//! ```
//!
//! The one deviation from a stock Claude subagent is that `model:` carries a
//! **logical tier** (`premium` / `balanced` / `fast`, D15) rather than a concrete
//! model id. The tier is resolved to a real model **per provider at launch**
//! (compile time): a `premium` profile compiles to Claude `opus` and to Codex
//! `gpt-5`. That keeps one master driving both a Claude and a Codex run (the P5
//! acceptance) instead of pinning a provider-specific model into the master.
//!
//! # The two compile targets
//!
//! - **Claude — lossless (`compile_claude`).** Emits a real subagent `.md` with
//!   the tier resolved to a Claude model and *every* master field preserved
//!   (`name`, `description`, `tools`, `color`, body). "Same `.md` 1:1" in the
//!   golden sense: nothing the master carries is dropped.
//! - **Codex — lossy (`compile_codex`).** Emits a `[profiles.<slug>]` fragment
//!   for `~/.codex/config.toml` (just the resolved model) plus a
//!   `~/.codex/prompts/<slug>.md` holding the body. Codex profiles have no tool
//!   allowlist and no color, so those fields are **dropped with a visible
//!   [`CodexCompile::warnings`] line each** — never silently.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The frontmatter fence delimiter (`---` on its own line).
const FENCE: &str = "---";

/// Logical model tier (D15): a provider-independent capability band the profile
/// requests, resolved to a concrete model **per provider** at compile time.
///
/// Storing a tier rather than a concrete model is what lets one master drive both
/// a Claude and a Codex run: the same `premium` profile resolves to Claude `opus`
/// and to Codex `gpt-5` without the master naming either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelTier {
    /// Highest-capability band (Claude `opus` / Codex `gpt-5`).
    Premium,
    /// The default, balanced band (Claude `sonnet` / Codex `gpt-5-codex`).
    Balanced,
    /// Fastest / cheapest band (Claude `haiku` / Codex `gpt-5-mini`).
    Fast,
}

impl ModelTier {
    /// The lowercase wire token (`"premium"` / `"balanced"` / `"fast"`) — the
    /// value stored in the master's `model:` field, the DB `tier` column, and the
    /// CTS wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Premium => "premium",
            Self::Balanced => "balanced",
            Self::Fast => "fast",
        }
    }

    /// Parse a tier token, case-insensitively. Returns `None` for an unknown
    /// token so the caller can decide (the parser defaults a *missing* `model:`
    /// to [`Self::Balanced`], but treats an *unknown* token as a hard error).
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token.trim().to_ascii_lowercase().as_str() {
            "premium" => Some(Self::Premium),
            "balanced" => Some(Self::Balanced),
            "fast" => Some(Self::Fast),
            _ => None,
        }
    }

    /// Resolve this tier to the concrete Claude model id written into the
    /// compiled subagent `.md`.
    ///
    /// The default mapping (premium→`opus`, balanced→`sonnet`, fast→`haiku`) is
    /// the resolution point for D15; changing the band→model policy is an edit
    /// here alone.
    #[must_use]
    pub const fn claude_model(self) -> &'static str {
        match self {
            Self::Premium => "opus",
            Self::Balanced => "sonnet",
            Self::Fast => "haiku",
        }
    }

    /// Resolve this tier to the concrete Codex model id written into the
    /// `[profiles.<slug>]` fragment.
    #[must_use]
    pub const fn codex_model(self) -> &'static str {
        match self {
            Self::Premium => "gpt-5",
            Self::Balanced => "gpt-5-codex",
            Self::Fast => "gpt-5-mini",
        }
    }
}

impl fmt::Display for ModelTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why parsing a profile master failed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProfileParseError {
    /// The text did not open with a `---` frontmatter fence.
    #[error("profile master must open with a `---` frontmatter fence")]
    MissingFrontmatter,
    /// The opening fence was never closed by a second `---`.
    #[error("profile frontmatter fence was never closed")]
    UnterminatedFrontmatter,
    /// The resolved slug was empty or not kebab-case (`[a-z0-9-]`, no leading /
    /// trailing / doubled dash).
    #[error("invalid profile slug {0:?}: must be non-empty kebab-case ([a-z0-9-])")]
    InvalidSlug(String),
    /// A `model:` field carried a token that is not a known tier.
    #[error("unknown model tier {0:?}: expected premium | balanced | fast")]
    UnknownTier(String),
}

/// A parsed canonical profile master — the in-memory form of a
/// `~/.agents-in-a-box/profiles/<slug>.md`.
///
/// Round-trips through [`Self::parse`] / [`Self::to_master_md`]: re-serialising a
/// parsed master yields a canonical (field-ordered) form, and re-parsing that is
/// a fixpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileMaster {
    /// The profile slug — its identity, the DB PK, and the board-assignee slug
    /// (D16). Kebab-case; comes from the master's `name:` field when present,
    /// else the filename stem.
    pub slug: String,
    /// One-line description of when to reach for this profile.
    pub description: String,
    /// The logical model tier (D15).
    pub tier: ModelTier,
    /// Claude-only tool allowlist (comma-separated in the master). Dropped by the
    /// Codex compiler with a warning.
    pub tools: Vec<String>,
    /// Claude-only card color. Dropped by the Codex compiler with a warning.
    pub color: Option<String>,
    /// The system-prompt body (everything after the frontmatter fence, verbatim
    /// apart from a single trailing-newline normalisation).
    pub body: String,
}

impl ProfileMaster {
    /// Parse a master `.md`. `slug_hint` (the filename stem) supplies the slug
    /// when the frontmatter omits `name:`.
    ///
    /// # Errors
    ///
    /// - [`ProfileParseError::MissingFrontmatter`] / `UnterminatedFrontmatter`
    ///   for a malformed fence,
    /// - [`ProfileParseError::InvalidSlug`] when the resolved slug is not
    ///   kebab-case,
    /// - [`ProfileParseError::UnknownTier`] when `model:` is present but not a
    ///   known tier.
    pub fn parse(slug_hint: &str, text: &str) -> Result<Self, ProfileParseError> {
        let (frontmatter, body) = split_frontmatter(text)?;

        let mut name: Option<String> = None;
        let mut description = String::new();
        let mut tier: Option<ModelTier> = None;
        let mut tools: Vec<String> = Vec::new();
        let mut color: Option<String> = None;

        for line in frontmatter.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some((key, value)) = trimmed.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim().to_ascii_lowercase().as_str() {
                "name" => name = Some(value.to_string()),
                "description" => description = value.to_string(),
                "model" | "tier" => {
                    tier = Some(
                        ModelTier::parse(value)
                            .ok_or_else(|| ProfileParseError::UnknownTier(value.to_string()))?,
                    );
                }
                "tools" => tools = split_tools(value),
                "color" => {
                    if !value.is_empty() {
                        color = Some(value.to_string());
                    }
                }
                _ => {}
            }
        }

        let slug = name.unwrap_or_else(|| slug_hint.to_string());
        let slug = slug.trim().to_string();
        if !is_valid_slug(&slug) {
            return Err(ProfileParseError::InvalidSlug(slug));
        }

        Ok(Self {
            slug,
            description,
            // A master with no `model:` defaults to the balanced band rather than
            // failing — an omitted tier is a common, valid authoring shorthand.
            tier: tier.unwrap_or(ModelTier::Balanced),
            tools,
            color,
            body: normalise_body(body),
        })
    }

    /// Serialise back to the canonical master `.md` form (field-ordered
    /// frontmatter + body). Optional fields (`tools`, `color`) are omitted when
    /// empty. This is what [`profile/upsert`] writes to disk.
    #[must_use]
    pub fn to_master_md(&self) -> String {
        let mut out = String::new();
        out.push_str(FENCE);
        out.push('\n');
        out.push_str(&format!("name: {}\n", self.slug));
        out.push_str(&format!("description: {}\n", self.description));
        out.push_str(&format!("model: {}\n", self.tier.as_str()));
        if !self.tools.is_empty() {
            out.push_str(&format!("tools: {}\n", self.tools.join(", ")));
        }
        if let Some(color) = &self.color {
            out.push_str(&format!("color: {color}\n"));
        }
        out.push_str(FENCE);
        out.push('\n');
        if !self.body.is_empty() {
            out.push_str(&self.body);
            out.push('\n');
        }
        out
    }

    /// Compile the **lossless** Claude subagent `.md` (tier resolved to a Claude
    /// model; every field preserved). This is the artifact the daemon writes into
    /// a Claude task's `.claude/agents/<slug>.md` on dispatch.
    #[must_use]
    pub fn compile_claude(&self) -> ClaudeCompile {
        let mut contents = String::new();
        contents.push_str(FENCE);
        contents.push('\n');
        contents.push_str(&format!("name: {}\n", self.slug));
        contents.push_str(&format!("description: {}\n", self.description));
        contents.push_str(&format!("model: {}\n", self.tier.claude_model()));
        if !self.tools.is_empty() {
            contents.push_str(&format!("tools: {}\n", self.tools.join(", ")));
        }
        if let Some(color) = &self.color {
            contents.push_str(&format!("color: {color}\n"));
        }
        contents.push_str(FENCE);
        contents.push('\n');
        if !self.body.is_empty() {
            contents.push_str(&self.body);
            contents.push('\n');
        }
        ClaudeCompile {
            filename: format!("{}.md", self.slug),
            contents,
        }
    }

    /// Compile the **lossy** Codex target: a `[profiles.<slug>]` config fragment
    /// (resolved model only) + a `prompts/<slug>.md` body, and a
    /// [`CodexCompile::warnings`] line for each dropped Claude-only field
    /// (`tools`, `color`). D14: dropped fields warn, they are never silently lost.
    #[must_use]
    pub fn compile_codex(&self) -> CodexCompile {
        let mut fragment = String::new();
        fragment.push_str(&format!("[profiles.{}]\n", self.slug));
        fragment.push_str(&format!("model = \"{}\"\n", self.tier.codex_model()));

        let mut warnings = Vec::new();
        if !self.tools.is_empty() {
            warnings.push(format!(
                "profile {:?}: dropped Claude-only field `tools` ({}) — Codex profiles have no tool allowlist",
                self.slug,
                self.tools.join(", ")
            ));
        }
        if let Some(color) = &self.color {
            warnings.push(format!(
                "profile {:?}: dropped Claude-only field `color` ({color}) — Codex profiles have no color",
                self.slug
            ));
        }

        CodexCompile {
            slug: self.slug.clone(),
            config_fragment: fragment,
            prompt_filename: format!("{}.md", self.slug),
            prompt_contents: if self.body.is_empty() {
                String::new()
            } else {
                let mut b = self.body.clone();
                b.push('\n');
                b
            },
            warnings,
        }
    }
}

/// The compiled lossless Claude subagent artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeCompile {
    /// The `.md` filename (`<slug>.md`).
    pub filename: String,
    /// The full subagent `.md` contents (resolved-model frontmatter + body).
    pub contents: String,
}

/// The compiled lossy Codex artifacts + the dropped-field warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexCompile {
    /// The profile slug (echoed for the caller's logging / file placement).
    pub slug: String,
    /// The `[profiles.<slug>]` fragment appended to `~/.codex/config.toml`.
    pub config_fragment: String,
    /// The prompt filename (`<slug>.md`) written under `~/.codex/prompts/`.
    pub prompt_filename: String,
    /// The prompt body written to that file.
    pub prompt_contents: String,
    /// One human-readable warning per dropped Claude-only field. Empty when the
    /// master carries nothing Codex cannot represent.
    pub warnings: Vec<String>,
}

/// Split `text` into `(frontmatter, body)` around the leading `---` … `---`
/// fence. `frontmatter` excludes both fence lines; `body` is everything after the
/// closing fence.
fn split_frontmatter(text: &str) -> Result<(&str, &str), ProfileParseError> {
    // Tolerate a leading BOM / blank lines before the fence.
    let text = text.trim_start_matches('\u{feff}');
    let rest = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
        .ok_or(ProfileParseError::MissingFrontmatter)?;

    // Find the closing fence: a line that is exactly `---`.
    let mut idx = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == FENCE {
            let frontmatter = &rest[..idx];
            let body = &rest[idx + line.len()..];
            return Ok((frontmatter, body));
        }
        idx += line.len();
    }
    Err(ProfileParseError::UnterminatedFrontmatter)
}

/// Normalise a body: strip a single leading newline (the gap after the closing
/// fence) and any trailing whitespace-only tail, so round-trips are stable.
fn normalise_body(body: &str) -> String {
    body.trim_matches(|c| c == '\n' || c == '\r').trim_end().to_string()
}

/// Split a comma-separated `tools:` value into trimmed, non-empty entries.
fn split_tools(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// Whether `s` is a valid profile slug: non-empty kebab-case `[a-z0-9-]` with no
/// leading / trailing / doubled dash.
#[must_use]
pub fn is_valid_slug(s: &str) -> bool {
    if s.is_empty() || s.starts_with('-') || s.ends_with('-') || s.contains("--") {
        return false;
    }
    s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\n\
name: code-reviewer\n\
description: Reviews a diff for correctness and style\n\
model: premium\n\
tools: Read, Grep, Glob\n\
color: cyan\n\
---\n\
You are a meticulous reviewer.\n\
Check the diff line by line.\n";

    #[test]
    fn parses_all_frontmatter_fields() {
        let m = ProfileMaster::parse("ignored-hint", SAMPLE).unwrap();
        assert_eq!(m.slug, "code-reviewer");
        assert_eq!(m.description, "Reviews a diff for correctness and style");
        assert_eq!(m.tier, ModelTier::Premium);
        assert_eq!(m.tools, vec!["Read", "Grep", "Glob"]);
        assert_eq!(m.color.as_deref(), Some("cyan"));
        assert_eq!(
            m.body,
            "You are a meticulous reviewer.\nCheck the diff line by line."
        );
    }

    #[test]
    fn slug_falls_back_to_hint_when_name_absent() {
        let text = "---\ndescription: no name field\nmodel: fast\n---\nbody\n";
        let m = ProfileMaster::parse("filename-stem", text).unwrap();
        assert_eq!(m.slug, "filename-stem");
        assert_eq!(m.tier, ModelTier::Fast);
    }

    #[test]
    fn missing_model_defaults_balanced() {
        let text = "---\nname: plain\ndescription: d\n---\nbody\n";
        let m = ProfileMaster::parse("plain", text).unwrap();
        assert_eq!(m.tier, ModelTier::Balanced);
    }

    #[test]
    fn unknown_tier_is_a_hard_error() {
        let text = "---\nname: x\nmodel: turbo\n---\nb\n";
        assert_eq!(
            ProfileMaster::parse("x", text),
            Err(ProfileParseError::UnknownTier("turbo".to_string()))
        );
    }

    #[test]
    fn missing_frontmatter_errors() {
        assert_eq!(
            ProfileMaster::parse("x", "no fence here"),
            Err(ProfileParseError::MissingFrontmatter)
        );
    }

    #[test]
    fn unterminated_frontmatter_errors() {
        assert_eq!(
            ProfileMaster::parse("x", "---\nname: x\nnever closed\n"),
            Err(ProfileParseError::UnterminatedFrontmatter)
        );
    }

    #[test]
    fn invalid_slug_rejected() {
        let text = "---\nname: Bad_Slug\n---\nb\n";
        assert_eq!(
            ProfileMaster::parse("x", text),
            Err(ProfileParseError::InvalidSlug("Bad_Slug".to_string()))
        );
    }

    #[test]
    fn slug_validator() {
        assert!(is_valid_slug("code-reviewer"));
        assert!(is_valid_slug("agent1"));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("-lead"));
        assert!(!is_valid_slug("lead-"));
        assert!(!is_valid_slug("a--b"));
        assert!(!is_valid_slug("Cap"));
        assert!(!is_valid_slug("under_score"));
    }

    #[test]
    fn round_trips_through_master_md() {
        let m = ProfileMaster::parse("code-reviewer", SAMPLE).unwrap();
        let serialised = m.to_master_md();
        let reparsed = ProfileMaster::parse("code-reviewer", &serialised).unwrap();
        assert_eq!(m, reparsed);
    }

    /// GOLDEN: same profile → Claude `.md` 1:1 (lossless, tier resolved).
    #[test]
    fn compile_claude_is_lossless_golden() {
        let m = ProfileMaster::parse("code-reviewer", SAMPLE).unwrap();
        let out = m.compile_claude();
        assert_eq!(out.filename, "code-reviewer.md");
        let expected = "---\n\
name: code-reviewer\n\
description: Reviews a diff for correctness and style\n\
model: opus\n\
tools: Read, Grep, Glob\n\
color: cyan\n\
---\n\
You are a meticulous reviewer.\n\
Check the diff line by line.\n";
        assert_eq!(out.contents, expected);
    }

    /// GOLDEN: same profile → Codex lossy, WARN on dropped tools + color.
    #[test]
    fn compile_codex_is_lossy_with_warnings_golden() {
        let m = ProfileMaster::parse("code-reviewer", SAMPLE).unwrap();
        let out = m.compile_codex();
        assert_eq!(
            out.config_fragment,
            "[profiles.code-reviewer]\nmodel = \"gpt-5\"\n"
        );
        assert_eq!(out.prompt_filename, "code-reviewer.md");
        assert_eq!(
            out.prompt_contents,
            "You are a meticulous reviewer.\nCheck the diff line by line.\n"
        );
        assert_eq!(out.warnings.len(), 2, "both tools and color must warn");
        assert!(out.warnings[0].contains("`tools`"));
        assert!(out.warnings[0].contains("Read, Grep, Glob"));
        assert!(out.warnings[1].contains("`color`"));
        assert!(out.warnings[1].contains("cyan"));
    }

    /// A profile with nothing Codex-incompatible produces zero warnings.
    #[test]
    fn compile_codex_no_warnings_when_no_dropped_fields() {
        let text = "---\nname: plain\ndescription: d\nmodel: fast\n---\nhello\n";
        let m = ProfileMaster::parse("plain", text).unwrap();
        let out = m.compile_codex();
        assert!(out.warnings.is_empty());
        assert_eq!(
            out.config_fragment,
            "[profiles.plain]\nmodel = \"gpt-5-mini\"\n"
        );
    }

    #[test]
    fn tier_resolution_per_provider() {
        assert_eq!(ModelTier::Premium.claude_model(), "opus");
        assert_eq!(ModelTier::Balanced.claude_model(), "sonnet");
        assert_eq!(ModelTier::Fast.claude_model(), "haiku");
        assert_eq!(ModelTier::Premium.codex_model(), "gpt-5");
        assert_eq!(ModelTier::Balanced.codex_model(), "gpt-5-codex");
        assert_eq!(ModelTier::Fast.codex_model(), "gpt-5-mini");
    }
}
