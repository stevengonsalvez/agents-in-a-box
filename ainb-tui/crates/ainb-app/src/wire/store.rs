// ABOUTME: The renderer half of the D15 contract: a mirror store that applies a
// channel drain as one transaction, runs effects after the commit, and exposes
// root selectors that can only return scalars.
//
//   channel ──drain──▶ MirrorStore::apply_drain ──commit──▶ effects ──▶ selectors
//                       (one transaction per drain)          (after commit)
//
// Any renderer (the headless test renderer, the desktop host, the web client
// through `AppState.ts`) keeps this shape. The fan-out bench measures the same
// three invariants in a real reactive store.

use crate::app::versioned::SectionId;
use crate::wire::frame::{DaemonRead, Frame, FrameBatch, HostId, Subscription};
use serde::Serialize;
use std::collections::BTreeMap;

/// One section as the renderer holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct MirroredSection {
    pub version: u64,
    pub host_id: HostId,
    pub daemon_read: Option<DaemonRead>,
    pub body: serde_json::Value,
}

/// What one drain changed, handed to every effect after the commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Commit {
    /// Sections whose committed value changed, in [`SectionId::ALL`] order.
    pub changed: Vec<SectionId>,
    /// The store's transaction count after this commit.
    pub transaction: u64,
}

/// A value a root selector may return. There is no list or object variant, so
/// a root selector cannot fan a single section write out to a list of readers
/// (D15 invariant 3).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Scalar {
    Bool(bool),
    Count(u64),
    Number(f64),
    Text(String),
    Absent,
}

/// A named read over the whole store.
#[derive(Clone, Copy)]
pub struct RootSelector {
    pub name: &'static str,
    pub read: fn(&MirrorStore) -> Scalar,
}

impl std::fmt::Debug for RootSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootSelector").field("name", &self.name).finish()
    }
}

type Effect = Box<dyn FnMut(&MirrorStore, &Commit) + Send>;

/// The renderer's copy of the subscribed sections.
pub struct MirrorStore {
    subscription: Subscription,
    sections: BTreeMap<SectionId, MirroredSection>,
    transactions: u64,
    frames_applied: u64,
    frames_ignored: u64,
    effects: Vec<Effect>,
}

impl std::fmt::Debug for MirrorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MirrorStore")
            .field("subscription", &self.subscription)
            .field("sections", &self.sections.keys().collect::<Vec<_>>())
            .field("transactions", &self.transactions)
            .field("frames_applied", &self.frames_applied)
            .field("frames_ignored", &self.frames_ignored)
            .field("effects", &self.effects.len())
            .finish()
    }
}

impl MirrorStore {
    #[must_use]
    pub fn new(subscription: Subscription) -> Self {
        Self {
            subscription,
            sections: BTreeMap::new(),
            transactions: 0,
            frames_applied: 0,
            frames_ignored: 0,
            effects: Vec::new(),
        }
    }

    /// Run `effect` after every commit that changed something.
    pub fn on_commit(&mut self, effect: impl FnMut(&Self, &Commit) + Send + 'static) {
        self.effects.push(Box::new(effect));
    }

    #[must_use]
    pub const fn subscription(&self) -> Subscription {
        self.subscription
    }

    #[must_use]
    pub fn section(&self, id: SectionId) -> Option<&MirroredSection> {
        self.sections.get(&id)
    }

    /// How many drains committed a change.
    #[must_use]
    pub const fn transactions(&self) -> u64 {
        self.transactions
    }

    /// Frames that changed the store.
    #[must_use]
    pub const fn frames_applied(&self) -> u64 {
        self.frames_applied
    }

    /// Frames dropped: an unsubscribed or unknown section, or a version the
    /// store already holds or has passed.
    #[must_use]
    pub const fn frames_ignored(&self) -> u64 {
        self.frames_ignored
    }

    /// Apply everything one channel drain produced, as a single transaction.
    ///
    /// Frames are staged first; later frames for a section replace earlier
    /// ones in the same drain, so a section that moved five times since the
    /// last drain is written once. The commit swaps every staged section in at
    /// once, and only then do effects run, each seeing the fully committed
    /// store. A drain that changes nothing commits nothing and runs no effect.
    pub fn apply_drain(&mut self, batches: impl IntoIterator<Item = FrameBatch>) -> Commit {
        let mut staged: BTreeMap<SectionId, MirroredSection> = BTreeMap::new();
        for frame in batches.into_iter().flat_map(|batch| batch.frames) {
            match self.accept(&frame, &staged) {
                Some(id) => {
                    staged.insert(id, into_section(frame));
                }
                None => self.frames_ignored += 1,
            }
        }
        if staged.is_empty() {
            return Commit {
                changed: Vec::new(),
                transaction: self.transactions,
            };
        }
        self.frames_applied += staged.len() as u64;
        let changed: Vec<SectionId> = staged.keys().copied().collect();
        self.sections.extend(staged);
        self.transactions += 1;
        let commit = Commit {
            changed,
            transaction: self.transactions,
        };
        let mut effects = std::mem::take(&mut self.effects);
        for effect in &mut effects {
            effect(self, &commit);
        }
        effects.append(&mut self.effects);
        self.effects = effects;
        commit
    }

    fn accept(
        &self,
        frame: &Frame,
        staged: &BTreeMap<SectionId, MirroredSection>,
    ) -> Option<SectionId> {
        let id = frame.section_id()?;
        if !self.subscription.contains(id) {
            return None;
        }
        let held = staged
            .get(&id)
            .or_else(|| self.sections.get(&id))
            .map(|section| section.version);
        match held {
            Some(version) if frame.version <= version => None,
            _ => Some(id),
        }
    }

    /// Read every root selector.
    #[must_use]
    pub fn read_selectors(&self) -> Vec<(&'static str, Scalar)> {
        ROOT_SELECTORS
            .iter()
            .map(|selector| (selector.name, (selector.read)(self)))
            .collect()
    }

    fn body(&self, id: SectionId) -> Option<&serde_json::Value> {
        self.sections.get(&id).map(|section| &section.body)
    }
}

fn into_section(frame: Frame) -> MirroredSection {
    MirroredSection {
        version: frame.version,
        host_id: frame.host_id,
        daemon_read: frame.daemon_read,
        body: frame.body,
    }
}

fn count<'a>(values: impl Iterator<Item = &'a serde_json::Value>) -> u64 {
    values.count() as u64
}

/// The root selectors every renderer shares: the counts and flags a status
/// bar, a tab badge or a window title draws. Each returns a [`Scalar`], so a
/// write to one row re-runs a selector once, never a list of row readers.
pub const ROOT_SELECTORS: &[RootSelector] = &[
    RootSelector {
        name: "session_count",
        read: |store| {
            store.body(SectionId::Sessions).map_or(Scalar::Absent, |body| {
                Scalar::Count(count(
                    body["workspaces"].as_array().into_iter().flatten().flat_map(|workspace| {
                        workspace["sessions"].as_array().into_iter().flatten()
                    }),
                ))
            })
        },
    },
    RootSelector {
        name: "needs_input_count",
        read: |store| {
            store.body(SectionId::Fleet).map_or(Scalar::Absent, |body| {
                Scalar::Count(count(
                    body["daemon_attention"]["all"]
                        .as_object()
                        .into_iter()
                        .flat_map(|all| all.values())
                        .filter(|chip| {
                            matches!(chip["kind"].as_str(), Some("Ask" | "Wait" | "Approve"))
                        }),
                ))
            })
        },
    },
    RootSelector {
        name: "notification_count",
        read: |store| {
            store.body(SectionId::Shell).map_or(Scalar::Absent, |body| {
                Scalar::Count(count(
                    body["notifications"].as_array().into_iter().flatten(),
                ))
            })
        },
    },
    RootSelector {
        name: "current_screen",
        read: |store| {
            store
                .body(SectionId::Shell)
                .and_then(|body| body["current_screen"].as_str())
                .map_or(Scalar::Absent, |screen| Scalar::Text(screen.to_string()))
        },
    },
    RootSelector {
        name: "config_popup_open",
        read: |store| {
            store
                .body(SectionId::Config)
                .and_then(|body| body["config_popup_state"]["show_popup"].as_bool())
                .map_or(Scalar::Absent, Scalar::Bool)
        },
    },
    RootSelector {
        name: "workspaces_loading",
        read: |store| {
            store
                .body(SectionId::WorkspaceLoad)
                .and_then(|body| body["is_loading_workspaces"].as_bool())
                .map_or(Scalar::Absent, Scalar::Bool)
        },
    },
];
