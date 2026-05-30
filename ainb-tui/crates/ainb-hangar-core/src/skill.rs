//! Skill domain types (IO-free).
//!
//! A *skill* is a reusable instruction bundle (Anthropic-style): a top-level
//! body (a `SKILL.md`) plus zero or more supporting files. Skills are owned by a
//! workspace and attached to agents many-to-many. This module carries the pure
//! vocabulary the persistence layer ([`ainb_hangar_store`]) and the daemon both
//! build on — no `sqlx`, no `tokio`.
//!
//! # The `(workspace_id, name)` identity
//!
//! Within a workspace a skill is identified by its [`SkillName`]. The name is
//! normalised to lowercase kebab-case at construction so two spellings of the
//! same skill (`Code Reviewer`, `code_reviewer`) can never split into two rows —
//! the importer hot path ([`ainb_hangar_store`]'s `upsert_by_name`) relies on a
//! single canonical key per logical skill.
//!
//! [`ainb_hangar_store`]: ../../ainb_hangar_store/index.html

use std::fmt;

use crate::ids::SkillId;

/// Failure mode when constructing a [`SkillName`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillNameError {
    /// The supplied name was empty, or normalised to the empty string (e.g. it
    /// contained only separator characters like `"--"` or `"  "`).
    #[error("skill name must not be empty after normalisation")]
    Empty,
}

/// A workspace-scoped skill name, normalised to lowercase kebab-case.
///
/// Construction routes through [`SkillName::new`] (or the [`TryFrom<&str>`]
/// impl), which lowercases the input and collapses every run of non-alphanumeric
/// characters into a single `-`, trimming leading/trailing separators. This
/// encodes the invariant the schema only documents: a single canonical key per
/// logical skill, so `Code Reviewer`, `code_reviewer`, and `code-reviewer` all
/// land on the same `(workspace_id, name)` row.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct SkillName(String);

impl SkillName {
    /// Construct a normalised skill name from arbitrary input.
    ///
    /// Lowercases the input, replaces every maximal run of characters that are
    /// not ASCII alphanumeric with a single `-`, and trims leading/trailing
    /// `-`. The result is guaranteed non-empty and to match
    /// `^[a-z0-9]+(-[a-z0-9]+)*$`.
    ///
    /// # Errors
    ///
    /// Returns [`SkillNameError::Empty`] when the normalised form is empty
    /// (input was empty or contained only separators).
    pub fn new(raw: impl AsRef<str>) -> Result<Self, SkillNameError> {
        let mut out = String::with_capacity(raw.as_ref().len());
        let mut prev_dash = true; // leading state suppresses a leading dash
        for ch in raw.as_ref().chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch.to_ascii_lowercase());
                prev_dash = false;
            } else if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        }
        if out.ends_with('-') {
            out.pop();
        }
        if out.is_empty() {
            return Err(SkillNameError::Empty);
        }
        Ok(Self(out))
    }

    /// Borrow the (guaranteed non-empty, kebab-case) inner string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for SkillName {
    type Error = SkillNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for SkillName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<SkillName> for String {
    fn from(name: SkillName) -> Self {
        name.0
    }
}

impl<'de> serde::Deserialize<'de> for SkillName {
    /// Decode from a bare JSON string, re-applying normalisation so a peer can
    /// never inject an un-normalised or empty name over the wire.
    fn deserialize<D>(de: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

/// A single file belonging to a skill, keyed by its relative `path`.
///
/// Ordered file lists are carried as `Vec<SkillFileInput>` (never a `HashMap`)
/// so import order is stable and byte-deterministic across processes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillFileInput {
    /// Relative path of the file within the skill bundle (e.g.
    /// `references/x.md`).
    pub path: String,
    /// File contents.
    pub content: String,
}

impl SkillFileInput {
    /// Construct a file input from a path and contents.
    pub fn new(path: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
        }
    }
}

/// A skill plus its ordered file list — the aggregate the daemon and plugin both
/// consume.
///
/// The `files` vector is ordered by `path` so two reads of the same skill
/// produce byte-identical output (materialisation and snapshot tests rely on
/// this).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillWithFiles {
    /// Skill primary key.
    pub id: SkillId,
    /// Owning workspace id (raw string; the store re-types as needed).
    pub workspace_id: String,
    /// Normalised, workspace-unique skill name.
    pub name: SkillName,
    /// Short description; `None` when unset.
    pub description: Option<String>,
    /// Top-level skill body (`SKILL.md`); `None` when file-only.
    pub content: Option<String>,
    /// Supporting files, ordered by `path`.
    pub files: Vec<SkillFileInput>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_normalises_to_kebab_case() {
        assert_eq!(SkillName::new("Code Reviewer").unwrap().as_str(), "code-reviewer");
        assert_eq!(SkillName::new("code_reviewer").unwrap().as_str(), "code-reviewer");
        assert_eq!(SkillName::new("CODE-REVIEWER").unwrap().as_str(), "code-reviewer");
        assert_eq!(SkillName::new("find  missing   tests").unwrap().as_str(), "find-missing-tests");
        assert_eq!(SkillName::new("  --commit--  ").unwrap().as_str(), "commit");
    }

    #[test]
    fn name_rejects_empty_and_separator_only() {
        assert_eq!(SkillName::new(""), Err(SkillNameError::Empty));
        assert_eq!(SkillName::new("   "), Err(SkillNameError::Empty));
        assert_eq!(SkillName::new("---"), Err(SkillNameError::Empty));
    }

    #[test]
    fn name_try_from_matches_new() {
        let via_try: SkillName = "Hello World".try_into().unwrap();
        assert_eq!(via_try.as_str(), "hello-world");
    }

    #[test]
    fn name_roundtrips_through_json() {
        let n = SkillName::new("commit").unwrap();
        let json = serde_json::to_string(&n).unwrap();
        assert_eq!(json, "\"commit\"");
        // Deserialisation re-normalises, so an un-normalised wire value is fixed.
        let from_wire: SkillName = serde_json::from_str("\"Commit Helper\"").unwrap();
        assert_eq!(from_wire.as_str(), "commit-helper");
    }
}
