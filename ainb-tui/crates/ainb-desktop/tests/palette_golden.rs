#![allow(missing_docs)]

// ABOUTME: The palette's row set, captured from the host that builds it today,
// so the move of that ownership into the reducer (#1161) is diffed against what
// the window offered rather than against itself.
//
// The golden is the contract. It is written from the live host ONLY when
// `UPDATE_PALETTE_GOLDEN=1` is set, read as a diff and committed; a plain test
// run never rewrites it, which is what keeps it a check rather than an echo.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use ainb_app::app::Effect;
use ainb_app::config::AppConfig;
use ainb_app::wire::frame::{FrameBatch, HostId, Subscription};
use ainb_app::{Intent, Keymap, SectionId};
use ainb_desktop::host::{DesktopHost, Executor};

mod support;
use support::isolated_home;

/// Runs no effect: the palette is read from the state the host starts in, and
/// an effect that reached a real executor would make this depend on the box.
struct NoEffects;

impl Executor for NoEffects {
    fn execute(&mut self, _effect: Effect) -> Vec<Intent> {
        Vec::new()
    }
}

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/palette_rows.txt")
}

/// One line per row: `id|context|chord|active`, sorted, so a diff names the row
/// that moved rather than the order it was built in.
fn rows() -> Vec<String> {
    isolated_home();
    let frames: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
    let sink = {
        let frames = Rc::clone(&frames);
        move |batch: FrameBatch| *frames.borrow_mut() += batch.frames.len()
    };
    let mut host = DesktopHost::new(
        AppConfig::default(),
        Keymap::defaults(),
        HostId::local(),
        Subscription::only(&[SectionId::Shell]),
        sink,
    )
    // The window opens on the session list, so that is the state whose palette
    // a person sees at rest.
    .without_attention_poll();
    host.open_sessions(&mut NoEffects);

    let mut rows: Vec<String> = host
        .palette()
        .into_iter()
        .map(|entry| {
            format!(
                "{}|{}|{}|{}",
                entry.id.as_str(),
                entry.context,
                entry.chord.unwrap_or_else(|| "-".to_string()),
                entry.active
            )
        })
        .collect();
    rows.sort();
    rows
}

#[test]
fn the_palette_row_set_matches_the_golden() {
    let rows = rows();
    assert!(!rows.is_empty(), "the keymap has commands to offer");
    let committed = std::fs::read_to_string(golden_path()).unwrap_or_else(|error| {
        panic!(
            "no golden at {}: {error}; write it with UPDATE_PALETTE_GOLDEN=1",
            golden_path().display()
        )
    });
    let committed: Vec<&str> = committed.lines().filter(|line| !line.is_empty()).collect();

    let added: Vec<&String> =
        rows.iter().filter(|row| !committed.contains(&row.as_str())).collect();
    let gone: Vec<&&str> =
        committed.iter().filter(|row| !rows.iter().any(|have| have == *row)).collect();
    assert!(
        added.is_empty() && gone.is_empty(),
        "the palette's row set moved.\nadded: {added:#?}\ngone: {gone:#?}\nrewrite with UPDATE_PALETTE_GOLDEN=1 once the move is deliberate"
    );
}

#[test]
fn update_golden_when_asked() {
    if std::env::var_os("UPDATE_PALETTE_GOLDEN").is_none() {
        return;
    }
    let body = format!("{}\n", rows().join("\n"));
    std::fs::write(golden_path(), body).expect("write the palette golden");
}
