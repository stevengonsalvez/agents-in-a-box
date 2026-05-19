//! TUI render-thread `.await` lint.
//!
//! Phase 7 architectural invariant: the ratatui draw thread is *strictly*
//! synchronous. Plugin work happens on the tokio runtime that
//! `ainb_plugin_runtime::Runtime` owns; the TUI thread only ever picks
//! up cached frames via `try_recv_render` and dispatches fresh requests
//! whose oneshot results land back in the runtime. A single
//! `runtime.block_on(plugin.render())` on the render path would
//! deadlock under load — the whole point of the runtime crate is that
//! the TUI never sees a future.
//!
//! This build script greps the render-path source set for the literal
//! token `.await`. If it finds one outside the allow-list it fails the
//! build with a pointer to the runtime façade. The check is structural:
//! it works without rust-analyzer / ast-grep / proc-macro infrastructure
//! and runs on every change to a watched file.
//!
//! ## Watched paths
//! - `src/components/**/*.rs` — every screen/component renderer
//! - `src/widgets/**/*.rs`    — reusable render widgets
//! - `src/app/state.rs`       — `App::tick_plugin_renders`, the
//!   top-of-frame plugin drain
//!
//! ## Allow-list
//! Currently empty. If you genuinely need to await something in the
//! render path (you almost certainly don't — push the work onto a
//! background task or a `RuntimeHandle::*` channel), add the file's
//! repository-relative path to `ALLOW_LIST` below with a one-line
//! justification comment. Reviewers should push back hard on every
//! addition.

use std::fs;
use std::path::{Path, PathBuf};

const FORBIDDEN: &str = ".await";

/// Repo-relative file paths exempt from the lint. Keep empty by default.
/// Any addition needs a justification comment on the same line.
const ALLOW_LIST: &[&str] = &[];

/// Directories to scan recursively from the crate root (the dir that
/// contains this `build.rs`). Every `.rs` file under these is render
/// path by convention.
const SCAN_DIRS: &[&str] = &["src/components", "src/widgets"];

/// Files where the lint runs **only inside the named functions**.
/// `state.rs` is a 10k-line god-object that mixes render-path code
/// (`tick_plugin_renders`) with async lifecycle methods (`init`,
/// `refresh_oauth_tokens`, …). Whole-file scan would drown the lint
/// in false positives, so we walk the file at function granularity.
/// Phase 7c todo: split the render-path methods into their own module
/// and move them under `SCAN_DIRS`.
const SCAN_FN_SCOPED: &[(&str, &[&str])] = &[("src/app/state.rs", &["tick_plugin_renders"])];

fn main() {
    let crate_root =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR set"));

    let mut offenders: Vec<(PathBuf, usize, String)> = Vec::new();

    for dir in SCAN_DIRS {
        let abs = crate_root.join(dir);
        // `cargo:rerun-if-changed` on the directory itself is enough —
        // cargo recurses on file changes inside it.
        println!("cargo:rerun-if-changed={}", abs.display());
        scan_dir(&abs, &crate_root, &mut offenders);
    }

    for (file, fns) in SCAN_FN_SCOPED {
        let abs = crate_root.join(file);
        println!("cargo:rerun-if-changed={}", abs.display());
        scan_file_fn_scoped(&abs, &crate_root, fns, &mut offenders);
    }

    if offenders.is_empty() {
        return;
    }

    eprintln!("\n[ainb render-thread lint] forbidden `.await` on the TUI render path:");
    for (path, line_no, line) in &offenders {
        eprintln!("  {}:{}: {}", path.display(), line_no, line.trim());
    }
    eprintln!(
        "\nThe TUI render thread MUST stay synchronous. Use \
         `ainb_plugin_runtime::RuntimeHandle::try_recv_render` (or one of \
         the other `try_*` helpers) instead of `.await`-ing a plugin \
         future inline. If you genuinely need an exception, add the file \
         to ALLOW_LIST in build.rs with a one-line justification."
    );
    std::process::exit(1);
}

fn scan_dir(dir: &Path, crate_root: &Path, offenders: &mut Vec<(PathBuf, usize, String)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, crate_root, offenders);
            continue;
        }
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        scan_file(&path, crate_root, offenders);
    }
}

fn scan_file(path: &Path, crate_root: &Path, offenders: &mut Vec<(PathBuf, usize, String)>) {
    let rel = path.strip_prefix(crate_root).unwrap_or(path).to_path_buf();
    let rel_str = rel.to_string_lossy();
    if ALLOW_LIST.iter().any(|p| *p == rel_str) {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for (i, line) in text.lines().enumerate() {
        if is_offending(line) {
            offenders.push((rel.clone(), i + 1, line.to_string()));
        }
    }
}

/// Walk `path` and report `.await` only inside the bodies of named
/// functions. Brace-depth tracking is naive (counts `{`/`}` outside of
/// strings/comments via a tiny state machine) but Good Enough for the
/// hand-written renderer entry points the lint actually targets.
fn scan_file_fn_scoped(
    path: &Path,
    crate_root: &Path,
    fn_names: &[&str],
    offenders: &mut Vec<(PathBuf, usize, String)>,
) {
    let rel = path.strip_prefix(crate_root).unwrap_or(path).to_path_buf();
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        // Match `fn NAME` or `pub fn NAME` (with optional async, etc.).
        let Some(fn_idx) = trimmed.find("fn ") else {
            continue;
        };
        let after_fn = &trimmed[fn_idx + 3..];
        let name_end = after_fn
            .find(|c: char| !c.is_alphanumeric() && c != '_')
            .unwrap_or(after_fn.len());
        let name = &after_fn[..name_end];
        if !fn_names.contains(&name) {
            continue;
        }
        // Found the function — walk forward until brace depth returns
        // to zero. Start from the line that opens the body.
        let (start_line, mut depth) = find_body_start(&lines, i);
        if depth == 0 {
            continue; // signature with no body — declaration only
        }
        let mut j = start_line;
        while j < lines.len() && depth > 0 {
            let body_line = lines[j];
            depth += brace_delta(body_line);
            if is_offending(body_line) {
                offenders.push((rel.clone(), j + 1, body_line.to_string()));
            }
            j += 1;
        }
    }
}

fn is_offending(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return false;
    }
    line.contains(FORBIDDEN)
}

/// Find the line index where the function body opens (first `{` at or
/// after the signature line), and return the brace depth after that
/// line. Returns `(start_line, 0)` if no opening brace is found within
/// a reasonable window — caller treats that as "no body to scan".
fn find_body_start(lines: &[&str], sig_line: usize) -> (usize, i32) {
    let mut depth = 0_i32;
    let max = lines.len().min(sig_line + 16);
    for (j, line) in lines.iter().enumerate().take(max).skip(sig_line) {
        depth += brace_delta(line);
        if depth > 0 {
            return (j + 1, depth);
        }
    }
    (sig_line, 0)
}

/// `+1` for every unescaped `{`, `-1` for every unescaped `}`, ignoring
/// braces inside line comments and string literals. Block comments and
/// raw strings are intentionally NOT handled — the targeted render-path
/// functions don't use them. If that ever changes, the lint may
/// over-count and either flag false positives or miss real ones; both
/// are loud enough to catch in code review.
fn brace_delta(line: &str) -> i32 {
    let mut d = 0_i32;
    let mut in_str = false;
    let mut prev = '\0';
    for c in line.chars() {
        if !in_str && prev == '/' && c == '/' {
            // line comment — stop counting
            break;
        }
        match c {
            '"' if prev != '\\' => in_str = !in_str,
            '{' if !in_str => d += 1,
            '}' if !in_str => d -= 1,
            _ => {}
        }
        prev = c;
    }
    d
}
