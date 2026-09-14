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
use std::collections::{BTreeMap, BTreeSet};

/// One section as the renderer holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MirroredSection {
    pub version: u64,
    pub host_id: HostId,
    pub daemon_read: Option<DaemonRead>,
    pub body: serde_json::Value,
}

/// A section as held from one host. Two hosts' copies of the same section are
/// two entries, so mirroring a second machine never overwrites the first.
pub type SectionKey = (HostId, SectionId);

/// What one drain changed, handed to every effect after the commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Commit {
    /// Sections whose committed value changed, by host then [`SectionId::ALL`] order.
    pub changed: Vec<SectionKey>,
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
        f.debug_struct("RootSelector").field("name", &self.name).finish_non_exhaustive()
    }
}

type Effect = Box<dyn FnMut(&MirrorStore, &Commit) + Send>;

/// The most distinct hosts one store holds. A frame from a host beyond it is
/// ignored until [`MirrorStore::evict_host`] makes room, so a misbehaving
/// peer set cannot grow the store without bound.
pub const MAX_HOSTS: usize = 64;

/// The renderer's copy of the subscribed sections.
pub struct MirrorStore {
    subscription: Subscription,
    sections: BTreeMap<SectionKey, MirroredSection>,
    /// The boot epoch each host's held versions count in.
    epochs: BTreeMap<HostId, u64>,
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
            epochs: BTreeMap::new(),
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

    /// One host's copy of a section.
    #[must_use]
    pub fn section(&self, host: &HostId, id: SectionId) -> Option<&MirroredSection> {
        self.sections.get(&(host.clone(), id))
    }

    /// Every host's copy of a section, by host id.
    pub fn section_by_host(
        &self,
        id: SectionId,
    ) -> impl Iterator<Item = (&HostId, &MirroredSection)> {
        self.sections
            .iter()
            .filter(move |((_, section), _)| *section == id)
            .map(|((host, _), held)| (host, held))
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

    /// The boot epoch this store holds a host's sections in.
    #[must_use]
    pub fn epoch(&self, host: &HostId) -> Option<u64> {
        self.epochs.get(host).copied()
    }

    /// Apply everything one channel drain produced, as a single transaction.
    ///
    /// Frames are staged first; later frames for a section replace earlier
    /// ones in the same drain, so a section that moved five times since the
    /// last drain is written once. A frame from a larger boot epoch than the
    /// one held for its host drops everything held from that host and starts
    /// its versions over; a frame from a smaller epoch is a dead process and is
    /// ignored. The commit swaps every staged section in at once, and only then
    /// do effects run, each seeing the fully committed store. A drain that
    /// changes nothing commits nothing and runs no effect.
    ///
    /// `peer` is the host at the other end of the channel the batches came
    /// over, as the transport identified it. A frame naming any other host is
    /// ignored: a frame's own `host_id` is never trusted to pick the key.
    pub fn apply_drain(
        &mut self,
        peer: &HostId,
        batches: impl IntoIterator<Item = FrameBatch>,
    ) -> Commit {
        let mut drain = Drain::default();
        for frame in batches.into_iter().flat_map(|batch| batch.frames) {
            match self.accept(peer, &frame, &mut drain) {
                Some(key) => {
                    drain.staged.insert(key, into_section(frame));
                }
                None => self.frames_ignored += 1,
            }
        }
        let dropped: Vec<SectionKey> = self
            .sections
            .keys()
            .filter(|(host, _)| drain.restarted.contains(host))
            .cloned()
            .collect();
        self.epochs.extend(drain.epochs);
        if drain.staged.is_empty() && dropped.is_empty() {
            return self.unchanged();
        }
        self.frames_applied += drain.staged.len() as u64;
        let changed: BTreeSet<SectionKey> =
            dropped.iter().chain(drain.staged.keys()).cloned().collect();
        for key in &dropped {
            self.sections.remove(key);
        }
        self.sections.extend(drain.staged);
        self.commit(changed)
    }

    /// Drop everything held from a host that went away: its channel closed or
    /// its daemon stopped answering. Counts over hosts fall at once. A later
    /// frame from the host starts it over.
    pub fn evict_host(&mut self, host: &HostId) -> Commit {
        self.epochs.remove(host);
        let dropped: BTreeSet<SectionKey> =
            self.sections.keys().filter(|(held, _)| held == host).cloned().collect();
        if dropped.is_empty() {
            return self.unchanged();
        }
        self.sections.retain(|(held, _), _| held != host);
        self.commit(dropped)
    }

    /// Change what this store holds. Sections no longer subscribed are dropped
    /// in the same transaction, from every host, so no selector keeps reading a
    /// section the renderer stopped receiving.
    pub fn resubscribe(&mut self, subscription: Subscription) -> Commit {
        self.subscription = subscription;
        let dropped: BTreeSet<SectionKey> = self
            .sections
            .keys()
            .filter(|(_, id)| !subscription.contains(*id))
            .cloned()
            .collect();
        if dropped.is_empty() {
            return self.unchanged();
        }
        self.sections.retain(|(_, id), _| subscription.contains(*id));
        self.commit(dropped)
    }

    const fn unchanged(&self) -> Commit {
        Commit {
            changed: Vec::new(),
            transaction: self.transactions,
        }
    }

    /// Count one transaction and run every effect on the committed store.
    ///
    /// Effects are moved out while they run, since each reads the store. If one
    /// unwinds, they are put back before the panic continues, so a renderer that
    /// catches it keeps every effect registered instead of silently losing all
    /// of them.
    fn commit(&mut self, changed: BTreeSet<SectionKey>) -> Commit {
        self.transactions += 1;
        let commit = Commit {
            changed: changed.into_iter().collect(),
            transaction: self.transactions,
        };
        let mut effects = std::mem::take(&mut self.effects);
        let store = &*self;
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for effect in &mut effects {
                effect(store, &commit);
            }
        }));
        effects.append(&mut self.effects);
        self.effects = effects;
        if let Err(panic) = ran {
            std::panic::resume_unwind(panic);
        }
        commit
    }

    fn accept(&self, peer: &HostId, frame: &Frame, drain: &mut Drain) -> Option<SectionKey> {
        let id = frame.section_id()?;
        if !self.subscription.contains(id) || frame.host_id != *peer {
            return None;
        }
        let host = peer;
        let known = self.epochs.contains_key(host) || drain.epochs.contains_key(host);
        if !known {
            let hosts: BTreeSet<&HostId> = self.epochs.keys().chain(drain.epochs.keys()).collect();
            if hosts.len() >= MAX_HOSTS {
                return None;
            }
        }
        match drain.epochs.get(host).or_else(|| self.epochs.get(host)) {
            Some(held) if frame.epoch < *held => return None,
            Some(held) if frame.epoch > *held => {
                // The host restarted: its versions count from zero again.
                drain.staged.retain(|(staged_host, _), _| staged_host != host);
                drain.restarted.insert(host.clone());
                drain.epochs.insert(host.clone(), frame.epoch);
            }
            Some(_) => {}
            None => {
                drain.epochs.insert(host.clone(), frame.epoch);
            }
        }
        // Versions are per host: host B's section 3 is not older than host A's 9.
        let key = (host.clone(), id);
        let committed =
            (!drain.restarted.contains(host)).then(|| self.sections.get(&key)).flatten();
        let held = drain.staged.get(&key).or(committed).map(|section| section.version);
        match held {
            Some(version) if frame.version <= version => None,
            _ => Some(key),
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

    /// Every host's body for a section.
    fn bodies(&self, id: SectionId) -> impl Iterator<Item = &serde_json::Value> {
        self.section_by_host(id).map(|(_, held)| &held.body)
    }

    /// The body of a single-host section, when exactly one host has sent it.
    fn only_body(&self, id: SectionId) -> Option<&serde_json::Value> {
        let mut bodies = self.bodies(id);
        let first = bodies.next()?;
        bodies.next().is_none().then_some(first)
    }
}

/// What one drain has staged so far.
#[derive(Default)]
struct Drain {
    staged: BTreeMap<SectionKey, MirroredSection>,
    /// Hosts whose frames came from a larger epoch than the store holds.
    restarted: BTreeSet<HostId>,
    /// The epoch each host's frames in this drain count in.
    epochs: BTreeMap<HostId, u64>,
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

/// The root selectors every renderer shares.
///
/// The counts and flags a status bar, a tab badge or a window title draws.
/// Each returns a [`Scalar`], so a write to one row re-runs a selector once,
/// never a list of row readers.
pub const ROOT_SELECTORS: &[RootSelector] =
    &[
        RootSelector {
            name: "session_count",
            read: |store| {
                if store.bodies(SectionId::Sessions).next().is_none() {
                    return Scalar::Absent;
                }
                Scalar::Count(count(store.bodies(SectionId::Sessions).flat_map(|body| {
                    body["workspaces"].as_array().into_iter().flatten().flat_map(|workspace| {
                        workspace["sessions"].as_array().into_iter().flatten()
                    })
                })))
            },
        },
        RootSelector {
            name: "needs_input_count",
            read: |store| {
                if store.bodies(SectionId::Fleet).next().is_none() {
                    return Scalar::Absent;
                }
                Scalar::Count(count(
                    store
                        .bodies(SectionId::Fleet)
                        .flat_map(|body| {
                            body["daemon_attention"]["all"]
                                .as_object()
                                .into_iter()
                                .flat_map(|all| all.values())
                        })
                        .filter(|chip| {
                            matches!(chip["kind"].as_str(), Some("Ask" | "Wait" | "Approve"))
                        }),
                ))
            },
        },
        RootSelector {
            name: "agent_count",
            read: |store| {
                if store.bodies(SectionId::AgentStatus).next().is_none() {
                    return Scalar::Absent;
                }
                Scalar::Count(count(store.bodies(SectionId::AgentStatus).flat_map(
                    |body| body["view"]["cards"].as_array().into_iter().flatten(),
                )))
            },
        },
        RootSelector {
            name: "host_count",
            read: |store| {
                let hosts: std::collections::BTreeSet<&HostId> =
                    store.sections.keys().map(|(host, _)| host).collect();
                Scalar::Count(hosts.len() as u64)
            },
        },
        RootSelector {
            name: "notification_count",
            read: |store| {
                store.only_body(SectionId::Shell).map_or(Scalar::Absent, |body| {
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
                    .only_body(SectionId::Shell)
                    .and_then(|body| body["current_screen"].as_str())
                    .map_or(Scalar::Absent, |screen| Scalar::Text(screen.to_string()))
            },
        },
        RootSelector {
            name: "config_popup_open",
            read: |store| {
                store
                    .only_body(SectionId::Config)
                    .and_then(|body| body["config_popup_state"]["show_popup"].as_bool())
                    .map_or(Scalar::Absent, Scalar::Bool)
            },
        },
        RootSelector {
            name: "workspaces_loading",
            read: |store| {
                store
                    .only_body(SectionId::WorkspaceLoad)
                    .and_then(|body| body["is_loading_workspaces"].as_bool())
                    .map_or(Scalar::Absent, Scalar::Bool)
            },
        },
    ];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame(host: &str, section: &str, epoch: u64, version: u64) -> Frame {
        serde_json::from_value(json!({
            "section": section,
            "version": version,
            "epoch": epoch,
            "host_id": host,
            "body": {"version": version},
        }))
        .expect("a frame")
    }

    fn drain(store: &mut MirrorStore, peer: &str, frames: Vec<Frame>) -> Commit {
        store.apply_drain(&HostId::new(peer), [FrameBatch { frames }])
    }

    fn held(store: &MirrorStore, host: &str, id: SectionId) -> Option<u64> {
        store.section(&HostId::new(host), id).map(|section| section.version)
    }

    fn host_count(store: &MirrorStore) -> Scalar {
        store
            .read_selectors()
            .into_iter()
            .find_map(|(name, value)| (name == "host_count").then_some(value))
            .expect("host_count selector")
    }

    #[test]
    fn an_older_frame_replayed_in_the_same_epoch_is_ignored() {
        let mut store = MirrorStore::new(Subscription::all());
        drain(&mut store, "h", vec![frame("h", "shell", 7, 5)]);
        let commit = drain(&mut store, "h", vec![frame("h", "shell", 7, 3)]);
        assert!(commit.changed.is_empty());
        assert_eq!(held(&store, "h", SectionId::Shell), Some(5));
        assert_eq!(store.frames_ignored(), 1);
    }

    #[test]
    fn a_lower_version_after_an_epoch_bump_is_applied_and_drops_the_old_process() {
        let mut store = MirrorStore::new(Subscription::all());
        drain(
            &mut store,
            "h",
            vec![frame("h", "shell", 7, 5), frame("h", "config", 7, 9)],
        );
        drain(&mut store, "other", vec![frame("other", "shell", 1, 4)]);

        let commit = drain(&mut store, "h", vec![frame("h", "shell", 8, 1)]);

        assert_eq!(held(&store, "h", SectionId::Shell), Some(1));
        assert_eq!(
            held(&store, "h", SectionId::Config),
            None,
            "the dead process's section is dropped"
        );
        assert_eq!(
            held(&store, "other", SectionId::Shell),
            Some(4),
            "another host is untouched"
        );
        assert_eq!(store.epoch(&HostId::new("h")), Some(8));
        assert!(commit.changed.contains(&(HostId::new("h"), SectionId::Config)));

        let stale = drain(&mut store, "h", vec![frame("h", "shell", 7, 6)]);
        assert!(
            stale.changed.is_empty(),
            "a frame from the dead process is ignored"
        );
    }

    #[test]
    fn a_frame_naming_a_host_other_than_the_channel_peer_is_ignored() {
        let mut store = MirrorStore::new(Subscription::all());
        let commit = drain(&mut store, "h", vec![frame("impostor", "shell", 1, 1)]);
        assert!(commit.changed.is_empty());
        assert!(store.section(&HostId::new("impostor"), SectionId::Shell).is_none());
        assert_eq!(store.frames_ignored(), 1);
    }

    #[test]
    fn evicting_a_vanished_host_drops_its_sections_and_its_count() {
        let mut store = MirrorStore::new(Subscription::all());
        drain(&mut store, "a", vec![frame("a", "shell", 1, 1)]);
        drain(
            &mut store,
            "b",
            vec![frame("b", "shell", 1, 1), frame("b", "config", 1, 1)],
        );
        assert_eq!(host_count(&store), Scalar::Count(2));

        let commit = store.evict_host(&HostId::new("b"));

        assert_eq!(commit.changed.len(), 2);
        assert_eq!(host_count(&store), Scalar::Count(1));
        assert!(store.section(&HostId::new("b"), SectionId::Shell).is_none());
        assert_eq!(store.epoch(&HostId::new("b")), None);
        assert!(
            store.evict_host(&HostId::new("b")).changed.is_empty(),
            "nothing left to drop"
        );
    }

    #[test]
    fn resubscribing_drops_the_sections_no_longer_subscribed() {
        let mut store = MirrorStore::new(Subscription::all());
        drain(
            &mut store,
            "h",
            vec![frame("h", "shell", 1, 1), frame("h", "config", 1, 1)],
        );

        let commit = store.resubscribe(Subscription::only(&[SectionId::Shell]));

        assert_eq!(commit.changed, vec![(HostId::new("h"), SectionId::Config)]);
        assert!(store.section(&HostId::new("h"), SectionId::Config).is_none());
        assert_eq!(held(&store, "h", SectionId::Shell), Some(1));
        let later = drain(&mut store, "h", vec![frame("h", "config", 1, 2)]);
        assert!(
            later.changed.is_empty(),
            "an unsubscribed section stays out"
        );
    }

    #[test]
    fn a_panicking_effect_leaves_every_effect_registered() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        let mut store = MirrorStore::new(Subscription::all());
        let first = Arc::new(AtomicU32::new(0));
        let later = Arc::new(AtomicU32::new(0));
        let first_runs = Arc::clone(&first);
        store.on_commit(move |_, _| {
            assert!(
                first_runs.fetch_add(1, Ordering::SeqCst) > 0,
                "the first commit's effect fails"
            );
        });
        let later_runs = Arc::clone(&later);
        store.on_commit(move |_, _| {
            later_runs.fetch_add(1, Ordering::SeqCst);
        });

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drain(&mut store, "h", vec![frame("h", "shell", 1, 1)]);
        }));
        assert!(unwound.is_err());

        drain(&mut store, "h", vec![frame("h", "shell", 1, 2)]);
        assert_eq!(first.load(Ordering::SeqCst), 2);
        assert_eq!(
            later.load(Ordering::SeqCst),
            1,
            "the later effect is still registered"
        );
    }

    #[test]
    fn a_host_beyond_the_cap_is_ignored_until_one_is_evicted() {
        let mut store = MirrorStore::new(Subscription::all());
        for index in 0..MAX_HOSTS {
            let host = format!("h{index}");
            drain(&mut store, &host, vec![frame(&host, "shell", 1, 1)]);
        }
        let refused = drain(&mut store, "late", vec![frame("late", "shell", 1, 1)]);
        assert!(refused.changed.is_empty());
        assert_eq!(host_count(&store), Scalar::Count(MAX_HOSTS as u64));

        store.evict_host(&HostId::new("h0"));
        let admitted = drain(&mut store, "late", vec![frame("late", "shell", 1, 1)]);
        assert_eq!(
            admitted.changed,
            vec![(HostId::new("late"), SectionId::Shell)]
        );
    }
}
