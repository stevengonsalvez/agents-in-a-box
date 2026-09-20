//! P6e: the shipped desktop app really does flush its session-store writes.
//!
//! The worker's queue is only as good as the flush that drains it, and the
//! desktop's exit paths never unwind: tao's run loop ends in `process::exit`,
//! and a restart replaces the process. So no destructor runs, and the flush
//! has to be called by hand on the paths that end the process. Nothing in this
//! crate can drive tao, so this reads the shell's own source and holds it to
//! that: every `restart()` is preceded by a flush, and the run loop's `Exit`
//! event flushes too.

use std::path::PathBuf;

/// How far ahead of a `restart()` (or after the `Exit` arm) the flush may sit
/// and still be the flush for it.
const NEARBY_LINES: usize = 6;

/// The flush every exit path has to call.
const FLUSH: &str = "flush_session_store_writes";

fn main_rs() -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "src", "main.rs"].iter().collect();
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn every_restart_flushes_the_session_store_first() {
    let source = main_rs();
    let lines: Vec<&str> = source.lines().collect();
    let restarts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.contains(".restart()"))
        .map(|(at, _)| at)
        .collect();
    assert!(
        !restarts.is_empty(),
        "no restart call was found; this guard has drifted from the shell"
    );
    for at in restarts {
        let from = at.saturating_sub(NEARBY_LINES);
        let flushed = lines[from..at].iter().any(|line| line.contains(FLUSH));
        assert!(
            flushed,
            "main.rs:{} restarts without flushing the session store first: {}",
            at + 1,
            lines[at].trim()
        );
    }
}

#[test]
fn the_run_loop_flushes_the_session_store_on_exit() {
    let source = main_rs();
    let lines: Vec<&str> = source.lines().collect();
    let exit = lines
        .iter()
        .position(|line| line.contains("RunEvent::Exit"))
        .expect("the run loop does not handle RunEvent::Exit, so a quit drops queued writes");
    let to = (exit + 1 + NEARBY_LINES).min(lines.len());
    let flushed = lines[exit..to].iter().any(|line| line.contains(FLUSH));
    assert!(
        flushed,
        "main.rs:{} handles RunEvent::Exit without flushing the session store",
        exit + 1
    );
}
