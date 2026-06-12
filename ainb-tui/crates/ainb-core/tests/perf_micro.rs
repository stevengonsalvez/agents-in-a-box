// ABOUTME: Micro-measurements for TUI render-path hot spots (perf review aid).
//
// These are NOT correctness tests. They quantify the per-call cost of work that
// the session-list render path performs on every frame, so the performance
// review can put hard numbers on findings without standing up live sessions.
//
// Gated behind `AINB_PERF_MICRO=1` so they never run in CI or normal `cargo
// test`. Run with:
//   AINB_PERF_MICRO=1 HOME=/tmp/ainb-perf-home \
//     cargo test --release -p ainb-core --test perf_micro -- --nocapture

use std::path::Path;
use std::time::Instant;

fn enabled() -> bool {
    std::env::var_os("AINB_PERF_MICRO").is_some()
}

#[test]
fn micro_favorites_store_load() {
    if !enabled() {
        eprintln!("skipped (set AINB_PERF_MICRO=1 to run)");
        return;
    }
    // Warm the page cache.
    let _ = ainb::config::FavoritesStore::load();

    let iters = 5_000u32;
    let start = Instant::now();
    let mut sink = 0usize;
    for _ in 0..iters {
        let store = ainb::config::FavoritesStore::load();
        sink = sink.wrapping_add(store.favorites.len());
    }
    let elapsed = start.elapsed();
    let per = elapsed.as_secs_f64() * 1e6 / iters as f64;
    eprintln!(
        "[micro] FavoritesStore::load() x{iters}: total {:.2} ms, {:.2} us/call (favorites seen={})",
        elapsed.as_secs_f64() * 1e3,
        per,
        sink / iters as usize,
    );
    eprintln!(
        "[micro] at 30 fps that is {:.3} ms/s of render-thread time spent re-parsing favorites.yaml",
        per * 30.0 / 1000.0
    );
}

#[test]
fn micro_git_open_remote() {
    if !enabled() {
        eprintln!("skipped (set AINB_PERF_MICRO=1 to run)");
        return;
    }
    // Resolve a real repo path to open: the workspace root (cwd at test time is
    // the crate dir, so walk up to the git root).
    let cwd = std::env::current_dir().expect("cwd");
    let mut repo_path = cwd.as_path();
    while !repo_path.join(".git").exists() {
        match repo_path.parent() {
            Some(p) => repo_path = p,
            None => break,
        }
    }
    eprintln!("[micro] using repo path: {}", repo_path.display());

    // Warm.
    if let Ok(r) = ainb::git::RepositoryManager::open(repo_path) {
        let _ = r.get_remote_url();
    }

    let iters = 1_000u32;
    let start = Instant::now();
    let mut ok = 0u32;
    for _ in 0..iters {
        if let Ok(r) = ainb::git::RepositoryManager::open(Path::new(repo_path)) {
            if r.get_remote_url().is_ok() {
                ok += 1;
            }
        }
    }
    let elapsed = start.elapsed();
    let per = elapsed.as_secs_f64() * 1e6 / iters as f64;
    eprintln!(
        "[micro] RepositoryManager::open()+get_remote_url() x{iters}: total {:.2} ms, {:.2} us/call (ok={ok})",
        elapsed.as_secs_f64() * 1e3,
        per,
    );
    eprintln!(
        "[micro] per workspace per frame: 10 workspaces at 30 fps = {:.2} ms/s of render-thread time",
        per * 10.0 * 30.0 / 1000.0
    );
}
