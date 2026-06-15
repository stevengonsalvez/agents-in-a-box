//! Compile-time guard for the embedded curated `agent_template` registry (P6.3).
//!
//! Every template JSON in `templates/` references a list of skill *names*. This
//! build script reads each template, extracts its `skills`, and **fails the
//! build** (`panic!`) if any referenced skill name lacks a real
//! `toolkit/packages/skills/<name>/SKILL.md`. That keeps the embedded templates
//! from drifting away from the actual curated skill set — a missing or renamed
//! skill is caught before a binary is ever produced, not at run time.
//!
//! The script is intentionally dependency-free (no serde): it does a minimal
//! scan of the JSON `"skills"` array so the build has no extra build-deps. The
//! template JSONs are authored in this repo and are trivially shaped, so a hand
//! scan is sufficient and robust.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let templates_dir = manifest_dir.join("templates");
    // Repo layout: <repo>/ainb-tui/crates/ainb-hangar-core -> <repo>/toolkit/...
    let skills_dir = manifest_dir.join("../../../toolkit/packages/skills");

    // Re-run if any template changes or the skills tree changes shape.
    println!("cargo:rerun-if-changed={}", templates_dir.display());
    println!("cargo:rerun-if-changed=build.rs");

    let entries = fs::read_dir(&templates_dir)
        .unwrap_or_else(|e| panic!("cannot read templates dir {}: {e}", templates_dir.display()));

    let mut json_files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    json_files.sort();

    assert!(
        !json_files.is_empty(),
        "no template JSONs found in {} — the embedded registry would be empty",
        templates_dir.display()
    );

    for json in &json_files {
        println!("cargo:rerun-if-changed={}", json.display());
        let raw = fs::read_to_string(json)
            .unwrap_or_else(|e| panic!("cannot read template {}: {e}", json.display()));
        for skill in extract_skills(&raw, json) {
            let skill_md = skills_dir.join(&skill).join("SKILL.md");
            assert!(
                skill_md.is_file(),
                "template `{}` references skill `{}`, but `{}` does not exist. \
                 Add the skill to toolkit/packages/skills/ or fix the template's `skills` array.",
                json.display(),
                skill,
                skill_md.display(),
            );
        }
    }
}

/// Extract the string entries of the top-level `"skills"` JSON array.
///
/// Minimal hand scanner (no serde build-dep): locate the `"skills"` key, find
/// the following `[ ... ]`, and collect every double-quoted token inside. The
/// template JSONs are flat and authored in-repo, so this is sufficient and keeps
/// the build dependency-free.
fn extract_skills(raw: &str, path: &Path) -> Vec<String> {
    let key = raw
        .find("\"skills\"")
        .unwrap_or_else(|| panic!("template {} has no `skills` field", path.display()));
    let after = &raw[key..];
    let open = after
        .find('[')
        .unwrap_or_else(|| panic!("template {} `skills` is not an array", path.display()));
    let close = after[open..]
        .find(']')
        .unwrap_or_else(|| panic!("template {} `skills` array is not closed", path.display()));
    let body = &after[open + 1..open + close];

    let mut out = Vec::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == '"' {
            // Collect until the closing quote (skills names never contain quotes).
            let rest = &body[i + 1..];
            let end = rest.find('"').unwrap_or_else(|| {
                panic!(
                    "template {} has an unterminated skill string",
                    path.display()
                )
            });
            let name = &rest[..end];
            assert!(
                !name.is_empty(),
                "template {} has an empty skill name",
                path.display()
            );
            out.push(name.to_string());
            // Advance past the closing quote.
            for _ in 0..=end {
                chars.next();
            }
        }
    }
    assert!(
        !out.is_empty(),
        "template {} bundles no skills (the `skills` array is empty)",
        path.display()
    );
    out
}
