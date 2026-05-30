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
//!                              top-of-frame plugin drain
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

    // Stamp a build-identity string into the binary BEFORE the render-path lint
    // (which may early-return). Exposed to clap as `--version` long output so a
    // build is identifiable by commit + date, not just the Cargo version number
    // — the bare number is stale between releases (Cargo.toml only bumps at
    // release time, so a mid-cycle main build still reports the last release).
    emit_version_info(&crate_root);

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
        if !fn_names.iter().any(|n| *n == name) {
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
    for j in sig_line..max {
        depth += brace_delta(lines[j]);
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

// ──────────────────────────────────────────────────────────────────────────
// Build-identity stamping
//
// Emits `AINB_VERSION_LONG` (read via `env!` in `cli::root_clap_command` as
// clap's `--version` long output) shaped like:
//   `1.2.0 (e9b6abd, 2026-05-29, ci)`           — release/CI build (brew ships this)
//   `1.2.0 (ecec5a2-dirty, 2026-05-29, source)` — local build with edits
//   `1.2.0 (2026-05-29, source)`                — no git available (rare)
//
// SHA precedence: `AINB_BUILD_GIT_SHA` env (release CI may inject) → `git
// rev-parse`. Origin is `ci` (GITHUB_ACTIONS) else `source`. The SHA is the
// real disambiguator — a release binary's SHA equals its tag's commit; a stale
// local build shows an older SHA than `origin/main`.
// ──────────────────────────────────────────────────────────────────────────
fn emit_version_info(crate_root: &Path) {
    use std::process::Command;

    let pkg = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    let git = |args: &[&str]| -> Option<String> {
        let out = Command::new("git").args(args).current_dir(crate_root).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if s.is_empty() { None } else { Some(s) }
    };

    let sha = std::env::var("AINB_BUILD_GIT_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| git(&["rev-parse", "--short", "HEAD"]));

    // Dirty = tracked source differs from HEAD. Ignore untracked files
    // (`-uno`) so stray artifacts like a local `uv.lock` don't falsely mark an
    // otherwise-pristine checkout dirty.
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    // Build date (UTC, YYYY-MM-DD). `date` is present on every macOS/Linux build
    // host; if it's somehow missing we just omit the date.
    let date = Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());

    // Origin = build environment, and only what we can know reliably. We do NOT
    // sniff HOMEBREW_* — those are exported into most macOS interactive shells
    // by `brew shellenv`, so they'd falsely tag every local build "brew". The
    // binary brew distributes is the release tarball built in CI, so "ci" is the
    // honest tag for it; a local checkout build is "source".
    let origin = if std::env::var_os("GITHUB_ACTIONS").is_some() {
        "ci"
    } else {
        "source"
    };

    // Compose the parenthetical metadata: (sha[-dirty], date, origin).
    let mut meta: Vec<String> = Vec::new();
    if let Some(sha) = sha {
        meta.push(if dirty { format!("{sha}-dirty") } else { sha });
    } else if dirty {
        meta.push("dirty".to_string());
    }
    if let Some(date) = date {
        meta.push(date);
    }
    meta.push(origin.to_string());

    let long = format!("{pkg} ({})", meta.join(", "));
    println!("cargo:rustc-env=AINB_VERSION_LONG={long}");

    // Rebuild the stamp when HEAD moves or the override changes. Best-effort:
    // the .git pointer lives at the repo root, which `git` resolves for us; we
    // watch the common locations so a `git checkout`/commit re-stamps the build.
    println!("cargo:rerun-if-env-changed=AINB_BUILD_GIT_SHA");
    if let Some(git_dir) = git(&["rev-parse", "--git-dir"]) {
        let git_path = crate_root.join(&git_dir);
        println!("cargo:rerun-if-changed={}", git_path.join("HEAD").display());
    }
}
