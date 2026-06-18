//! Compile-time guard for the embedded curated `agent_template` registry (P6.3).
//!
//! Every template JSON in `templates/` references a list of skill *names*. This
//! build script reads each template and **fails the build** (`panic!`) if a
//! template is malformed — missing/empty `skills` array, unterminated strings,
//! empty names. That keeps the embedded registry self-consistent before a
//! binary is ever produced.
//!
//! Skill-name *existence* (does `<name>` resolve to a real `SKILL.md`?) is NOT
//! checked here: the curated skills now live in the standalone
//! `stevengonsalvez/ainb-toolkit` repo, not in a local `toolkit/` checkout, so
//! the build must stay self-contained (no filesystem dependency on a sibling
//! repo, and no fetch-in-build). Existence is instead validated by a CI step
//! that clones `ainb-toolkit@<pinned-tag>` and cross-checks every template
//! skill name against `skills/<name>/SKILL.md` (see `.github/workflows`), and
//! by `tests/template_registry_tests.rs` when run against a fetched checkout.
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

    // Re-run if any template changes.
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
        // Validates shape: non-empty `skills` array, no empty / unterminated
        // names. Existence against ainb-toolkit is a CI/test-time concern.
        let _ = extract_skills(&raw, json);
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
