// ABOUTME: Version-stamped sections of `AppState`. Every `&mut` access to a
// section bumps its counter, so a surface can ask "what changed since I last
// looked" with one integer compare per section instead of diffing state.
//
// This only means anything because the draw path no longer mutates core state
// (Phase 3 moved renderer-local state into `UiState` and put `Screen::render`
// behind `&AppState`). Before that, every frame bumped everything and the
// counters would have said "all 19 changed" forever.

use std::ops::{Deref, DerefMut};

/// A section of [`crate::app::state::AppState`] with a change counter.
///
/// `DerefMut` is the whole design: any code that takes `&mut` through it bumps
/// the version, whether or not it goes on to write. Over-bumping costs a
/// surface one redundant send; under-bumping loses an update, which is why the
/// bump lives on the borrow rather than on an explicit `set`.
#[derive(Debug, Default, Clone)]
pub struct Versioned<T> {
    version: u64,
    data: T,
}

impl<T> Versioned<T> {
    pub const fn new(data: T) -> Self {
        Self { version: 0, data }
    }

    pub const fn version(&self) -> u64 {
        self.version
    }

    pub const fn get(&self) -> &T {
        &self.data
    }

    /// Borrow mutably and bump. Prefer this at a site that takes several
    /// fields of one section at once: it bumps once, and the caller can then
    /// split-borrow the inner struct's fields freely.
    pub const fn get_mut(&mut self) -> &mut T {
        self.version += 1;
        &mut self.data
    }

    /// Run `f` against the section and bump only if it reports a change.
    ///
    /// For the draw path, which runs every frame whether or not anything
    /// moved. A tick that needs `&mut` to run at all would otherwise bump on
    /// every frame through `DerefMut`, and a section that changes every frame
    /// tells a subscriber nothing.
    ///
    /// This is the ONE place the coarse-bump rule is relaxed, so the contract
    /// is narrow: `f` must return true whenever it wrote anything an observer
    /// could see. Use it for calls that already answer that question, the
    /// `tick()` family whose bool means exactly "I changed something", and
    /// nothing else. Everywhere else `DerefMut` is the right tool precisely
    /// because it cannot be forgotten.
    pub fn update(&mut self, f: impl FnOnce(&mut T) -> bool) -> bool {
        let changed = f(&mut self.data);
        if changed {
            self.version += 1;
        }
        changed
    }

    /// Assign only when the value differs, bumping only when it does.
    ///
    /// The read goes through the section without bumping; the write happens
    /// only on a real change. For the rects and cursors the renderer
    /// recomputes every frame and usually recomputes identically.
    pub fn set_if_changed<U: PartialEq>(
        &mut self,
        field: impl Fn(&mut T) -> &mut U,
        value: U,
    ) -> bool {
        if *field(&mut self.data) == value {
            return false;
        }
        *field(self.get_mut()) = value;
        true
    }
}

impl<T> Deref for Versioned<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.data
    }
}

impl<T> DerefMut for Versioned<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.get_mut()
    }
}

/// The 19 sections, in the order [`SectionVersions`] indexes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SectionId {
    Sessions,
    SessionLabels,
    Tmux,
    Ssh,
    GitView,
    WorkspaceLoad,
    NewSession,
    Logs,
    ClaudeChat,
    Fleet,
    Hangar,
    McpPool,
    Inbox,
    PluginsHost,
    Config,
    Skills,
    Recovery,
    Onboarding,
    Shell,
}

impl SectionId {
    pub const COUNT: usize = 19;

    pub const ALL: [Self; Self::COUNT] = [
        Self::Sessions,
        Self::SessionLabels,
        Self::Tmux,
        Self::Ssh,
        Self::GitView,
        Self::WorkspaceLoad,
        Self::NewSession,
        Self::Logs,
        Self::ClaudeChat,
        Self::Fleet,
        Self::Hangar,
        Self::McpPool,
        Self::Inbox,
        Self::PluginsHost,
        Self::Config,
        Self::Skills,
        Self::Recovery,
        Self::Onboarding,
        Self::Shell,
    ];

    pub const fn index(self) -> usize {
        self as usize
    }
}

/// One version per section, indexed by [`SectionId::index`].
pub type SectionVersions = [u64; SectionId::COUNT];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_does_not_bump_and_a_write_does() {
        let mut section = Versioned::new(String::from("a"));
        assert_eq!(section.version(), 0);
        assert_eq!(section.get().as_str(), "a");
        assert_eq!(section.len(), 1); // through Deref
        assert_eq!(section.version(), 0, "reads must not bump");

        section.push('b');
        assert_eq!(section.version(), 1);
        assert_eq!(section.as_str(), "ab");
    }

    #[test]
    fn one_get_mut_bumps_once_however_many_fields_it_touches() {
        let mut section = Versioned::new((0_u8, 0_u8));
        let inner = section.get_mut();
        inner.0 = 1;
        inner.1 = 2;
        assert_eq!(
            section.version(),
            1,
            "the split borrow is one bump, not two"
        );
    }

    #[test]
    fn update_bumps_only_when_the_closure_reports_a_change() {
        let mut section = Versioned::new(0_u8);
        assert!(!section.update(|_| false), "a no-op tick reported a change");
        assert_eq!(section.version(), 0, "a no-op tick bumped");

        assert!(section.update(|n| {
            *n += 1;
            true
        }));
        assert_eq!(section.version(), 1);
    }

    #[test]
    fn set_if_changed_is_silent_on_an_identical_write() {
        let mut section = Versioned::new((1_u8, 2_u8));
        assert!(!section.set_if_changed(|s| &mut s.0, 1));
        assert_eq!(section.version(), 0, "writing the same value bumped");

        assert!(section.set_if_changed(|s| &mut s.0, 9));
        assert_eq!(section.version(), 1);
        assert_eq!(section.0, 9);
    }

    #[test]
    fn the_index_matches_the_declaration_order() {
        for (i, id) in SectionId::ALL.iter().enumerate() {
            assert_eq!(id.index(), i, "{id:?} indexes {} not {i}", id.index());
        }
        assert_eq!(SectionId::ALL.len(), SectionId::COUNT);
    }
}
