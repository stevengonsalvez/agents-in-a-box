//! P5.6 — first-run danger-full-access flow (modal show / dismiss / ack).
//!
//! The first time the Hangar TUI launches on a machine, it shows the
//! [`danger_access_modal`](crate::widgets::danger_access_modal) once, over the
//! landing screen, so the user acknowledges that agents run with
//! `danger-full-access` (full FS + network, no sandbox) before any work starts.
//! Pressing `y` dismisses it and records the `first_run` ack into
//! `~/.agents-in-a-box/hangar/state.toml::warnings_ack`; on every later launch the recorded
//! ack suppresses it.
//!
//! Split into two layers:
//!
//! - a **pure reducer** ([`FirstRunModal`] + [`reduce_first_run`]) that folds the
//!   accept (`y`) / quit (`q`) keys into show/dismissed state and an
//!   [`FirstRunIntent`] for the glue — no IO, fully unit-testable;
//! - a thin **IO** half ([`state_path`], [`read_acks`], [`ack_first_run`]) that
//!   reads/writes the `warnings_ack` array of `state.toml`, preserving every
//!   foreign key (the workspace switch state shares the file).
//!
//! The decision (`should_warn_first_run`) + the `first_run` key live in
//! [`ainb_hangar_core::warnings`] so the host, the daemon, and this plugin share
//! one source of truth.

use std::path::{Path, PathBuf};

use ainb_hangar_core::warnings::{FIRST_RUN_KEY, should_warn_first_run};

/// The `state.toml` array key holding dismissed-warning keys.
const WARNINGS_KEY: &str = "warnings_ack";

/// Resolve `~/.agents-in-a-box/hangar/state.toml` from the environment.
///
/// Delegates to the shared [`ainb_hangar_core::hangar_home`] resolver
/// (`$AINB_HANGAR_HOME` when set + non-empty, else `~/.agents-in-a-box` via
/// `dirs::home_dir`). The plugin runs under the same home as the daemon + CLI,
/// so the file it reads is the one they write — going through the shared helper
/// (not a private `$HOME` read) keeps that resolution identical on every side.
#[must_use]
pub fn state_path() -> Option<PathBuf> {
    Some(ainb_hangar_core::hangar_home()?.join("hangar").join("state.toml"))
}

/// Read the `warnings_ack` array from `path` (missing file / key → empty).
#[must_use]
pub fn read_acks(path: &Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&raw) else {
        return Vec::new();
    };
    doc.get(WARNINGS_KEY)
        .and_then(toml::Value::as_array)
        .map(|arr| arr.iter().filter_map(toml::Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Append `first_run` to `path`'s `warnings_ack` array if absent, preserving
/// every foreign key/section, writing atomically (temp + rename).
///
/// # Errors
///
/// Returns an error if the parent dir, the temp write, or the rename fails.
pub fn ack_first_run(path: &Path) -> std::io::Result<()> {
    let mut acks = read_acks(path);
    if acks.iter().any(|a| a == FIRST_RUN_KEY) {
        return Ok(());
    }
    acks.push(FIRST_RUN_KEY.to_string());

    let mut doc: toml::Value = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        toml::from_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = doc.as_table_mut().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "state.toml root is not a table",
        )
    })?;
    table.insert(
        WARNINGS_KEY.to_string(),
        toml::Value::Array(acks.iter().map(|a| toml::Value::String(a.clone())).collect()),
    );

    let serialised = toml::to_string_pretty(&doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp.firstrun");
    std::fs::write(&tmp, serialised.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The first-run modal's render state.
///
/// `Showing` while the modal is up (folded from the recorded acks at launch),
/// `Dismissed` once the user accepts. A pure value so transitions are testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FirstRunModal {
    /// Not shown — already acked, or already dismissed this session.
    #[default]
    Dismissed,
    /// Shown over the active screen, awaiting `y` (accept) or `q` (quit).
    Showing,
}

impl FirstRunModal {
    /// Decide the initial modal state from the recorded `acks`: `Showing` when
    /// the first-run warning has not been acknowledged, else `Dismissed`.
    #[must_use]
    pub fn from_acks(acks: &[String]) -> Self {
        if should_warn_first_run(acks) {
            Self::Showing
        } else {
            Self::Dismissed
        }
    }

    /// `true` while the modal should be rendered over the active screen.
    #[must_use]
    pub const fn is_showing(self) -> bool {
        matches!(self, Self::Showing)
    }
}

/// A side-effect the plugin glue performs after folding a key into the modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstRunIntent {
    /// The user accepted (`y`): persist the `first_run` ack.
    AckFirstRun,
    /// The user chose to quit (`q`) at the warning.
    Quit,
}

/// The result of folding one key into the modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstRunReduction {
    /// The next modal state.
    pub state: FirstRunModal,
    /// A side-effect for the glue, if any.
    pub intent: Option<FirstRunIntent>,
}

/// Fold a key into the first-run modal. Pure: no IO.
///
/// While `Showing`: `y` (or `Y`) dismisses + raises [`FirstRunIntent::AckFirstRun`];
/// `q` raises [`FirstRunIntent::Quit`] (the modal stays up — the glue quits);
/// every other key is swallowed (the modal is *modal* — it captures input until
/// dismissed). When already `Dismissed` the modal ignores all keys.
#[must_use]
pub const fn reduce_first_run(state: FirstRunModal, key: char) -> FirstRunReduction {
    if !state.is_showing() {
        return FirstRunReduction {
            state,
            intent: None,
        };
    }
    match key {
        'y' | 'Y' => FirstRunReduction {
            state: FirstRunModal::Dismissed,
            intent: Some(FirstRunIntent::AckFirstRun),
        },
        'q' => FirstRunReduction {
            state,
            intent: Some(FirstRunIntent::Quit),
        },
        // Modal: swallow every other key (no leaking to the screen beneath).
        _ => FirstRunReduction {
            state,
            intent: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_acks_shows_when_unacked() {
        assert_eq!(FirstRunModal::from_acks(&[]), FirstRunModal::Showing);
        assert_eq!(
            FirstRunModal::from_acks(&[FIRST_RUN_KEY.to_string()]),
            FirstRunModal::Dismissed
        );
    }

    #[test]
    fn accept_dismisses_and_acks() {
        let out = reduce_first_run(FirstRunModal::Showing, 'y');
        assert_eq!(out.state, FirstRunModal::Dismissed);
        assert_eq!(out.intent, Some(FirstRunIntent::AckFirstRun));
    }

    #[test]
    fn quit_raises_quit_intent_modal_stays() {
        let out = reduce_first_run(FirstRunModal::Showing, 'q');
        assert_eq!(out.state, FirstRunModal::Showing);
        assert_eq!(out.intent, Some(FirstRunIntent::Quit));
    }

    #[test]
    fn other_keys_swallowed_while_showing() {
        for k in ['1', 'j', 'a', ',', '\n'] {
            let out = reduce_first_run(FirstRunModal::Showing, k);
            assert_eq!(
                out.state,
                FirstRunModal::Showing,
                "key {k:?} must not dismiss"
            );
            assert_eq!(out.intent, None);
        }
    }

    #[test]
    fn dismissed_ignores_keys() {
        let out = reduce_first_run(FirstRunModal::Dismissed, 'y');
        assert_eq!(out.state, FirstRunModal::Dismissed);
        assert_eq!(out.intent, None);
    }

    #[test]
    fn ack_first_run_persists_and_preserves_foreign() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hangar").join("state.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "active_workspace = \"01ID\"\n\n[other]\nx = 1\n").unwrap();

        ack_first_run(&path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("warnings_ack"));
        assert!(raw.contains("first_run"));
        assert!(
            raw.contains("active_workspace"),
            "lost workspace key:\n{raw}"
        );
        assert!(raw.contains("[other]"), "lost foreign section:\n{raw}");

        // Idempotent + the recorded ack now suppresses the modal.
        ack_first_run(&path).unwrap();
        assert_eq!(read_acks(&path), vec![FIRST_RUN_KEY.to_string()]);
        assert_eq!(
            FirstRunModal::from_acks(&read_acks(&path)),
            FirstRunModal::Dismissed
        );
    }
}
