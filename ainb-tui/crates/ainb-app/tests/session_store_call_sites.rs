//! Keeps every sessions read and write on the resolver (P6e). A direct
//! `SessionStore::load`, `lock` or `mutate` outside the resolver's `File` and
//! `Degraded` paths reads or writes `sessions.json` whatever this process's
//! session source is, which is the split brain P6e exists to remove.
//!
//! The committed list is empty since P6e-4 (P6e-3 moved the readers, P6e-4
//! the writers). A new direct call fails here until it goes through
//! `cli::util::{load,mutate}_session_store` instead, or is triaged onto the
//! list with a reason.
//!
//! The scan is literal text, like `serialize_guard.rs`: it finds the calls
//! spelled out on one line. `Self::load()` inside `impl SessionStore` is the
//! store itself and is not matched. The runtime proof is the resolver's
//! tests; this list backs it, it does not replace it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const COMMITTED: &str = include_str!("fixtures/session_store_call_sites.txt");

/// The direct calls this guard fences.
const CALL: &[&str] = &[
    "SessionStore::load(",
    "SessionStore::lock(",
    "SessionStore::mutate(",
];

/// Source roots of every surface that can read or write sessions.
const ROOTS: &[&str] = &[
    "ainb-app/src",
    "ainb-core/src",
    "ainb-web/src",
    "ainb-desktop/src",
];

/// The resolver itself: its `File` and `Degraded` paths are the one place a
/// direct call belongs.
const RESOLVER: &str = "ainb-app/src/cli/util.rs";

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The call and its argument text, up to the matching parenthesis on the line.
fn call_text(line: &str, start: usize) -> String {
    let mut depth = 0usize;
    let mut end = line.len();
    for (offset, ch) in line[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = start + offset + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    line[start..end].trim().to_string()
}

/// Each direct call as `file | call | N`, N being how many times that exact
/// call appears in that file, so a second copy of a triaged call is a change
/// and fails like a new one.
fn current_call_sites() -> BTreeSet<String> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for root in ROOTS {
        let mut files = Vec::new();
        rust_files(&crates.join(root), &mut files);
        for file in files {
            let rel =
                file.strip_prefix(&crates).unwrap_or(&file).to_string_lossy().replace('\\', "/");
            if rel == RESOLVER || rel.ends_with("_tests.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            // Brace depth left to close in an in-file `#[cfg(test)] mod`, whose
            // lines are skipped; code after the module is scanned again.
            let mut in_test_module: Option<i64> = None;
            for (index, line) in lines.iter().enumerate() {
                if let Some(depth) = in_test_module.as_mut() {
                    *depth += i64::try_from(line.matches('{').count()).unwrap_or(i64::MAX)
                        - i64::try_from(line.matches('}').count()).unwrap_or(i64::MAX);
                    if *depth <= 0 {
                        in_test_module = None;
                    }
                    continue;
                }
                if line.trim_start().starts_with("#[cfg(test)]")
                    && lines.get(index + 1).is_some_and(|next| {
                        next.trim_start().starts_with("mod ") && next.contains('{')
                    })
                {
                    in_test_module = Some(0);
                    continue;
                }
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                for call in CALL {
                    let mut from = 0;
                    while let Some(found) = line[from..].find(call) {
                        let start = from + found;
                        *counts
                            .entry(format!("{rel} | {}", call_text(line, start)))
                            .or_default() += 1;
                        from = start + call.len();
                    }
                }
            }
        }
    }
    counts.into_iter().map(|(site, n)| format!("{site} | {n}")).collect()
}

#[test]
fn every_direct_session_store_call_is_triaged() {
    let current = current_call_sites();
    if std::env::var_os("UPDATE_SESSION_STORE_CALL_SITES").is_some() {
        let mut out = String::from(
            "# Direct SessionStore::{load,lock,mutate} calls outside the resolver\n\
             # (ainb-app/src/cli/util.rs), locked by tests/session_store_call_sites.rs.\n\
             # Empty since P6e-4: every read goes through cli::util::load_session_store\n\
             # and every write through cli::util::mutate_session_store. A new line is a\n\
             # regression unless it carries its reason here.\n\
             # Regenerate: UPDATE_SESSION_STORE_CALL_SITES=1 cargo test -p ainb-app --test session_store_call_sites\n",
        );
        for site in &current {
            out.push_str(site);
            out.push('\n');
        }
        std::fs::write(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/session_store_call_sites.txt"),
            out,
        )
        .expect("write call-site fixture");
        return;
    }
    let committed: BTreeSet<String> = COMMITTED
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    let added: Vec<_> = current.difference(&committed).collect();
    let removed: Vec<_> = committed.difference(&current).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "direct SessionStore calls changed outside the resolver.\n\
         Read through cli::util::load_session_store and write through \
         cli::util::mutate_session_store, so every surface uses this process's \
         one session source. A site removed from the list is progress: \
         regenerate with UPDATE_SESSION_STORE_CALL_SITES=1.\n\
         added: {added:#?}\nremoved: {removed:#?}"
    );
}
