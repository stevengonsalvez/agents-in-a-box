// ABOUTME: In-tree ScreenRegistry holding Box<dyn Screen> for built-in screens; Phase 3 will route plugin-owned screens through the same lookup

use std::collections::HashMap;

use crate::app::screens::{Screen, ScreenId};

/// Registry of in-tree screens, keyed by stable [`ScreenId`].
///
/// Phase 2a only registers built-in screens. Phase 3 will additionally route
/// plugin-owned screens (resolved via `ainb-plugin-host::ScreenRegistry`)
/// through the same `get_mut` lookup so layout dispatch is uniform.
#[derive(Default)]
pub struct ScreenRegistry {
    screens: HashMap<ScreenId, Box<dyn Screen>>,
    /// Insertion order — preserves tab-cycle ordering and matches user muscle
    /// memory.
    order: Vec<ScreenId>,
}

impl ScreenRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, screen: Box<dyn Screen>) {
        let id: ScreenId = screen.id().to_string();
        if !self.screens.contains_key(&id) {
            self.order.push(id.clone());
        }
        self.screens.insert(id, screen);
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Box<dyn Screen>> {
        self.screens.get_mut(id)
    }

    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.screens.contains_key(id)
    }

    #[must_use]
    pub fn order(&self) -> &[ScreenId] {
        &self.order
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.screens.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.screens.is_empty()
    }
}

impl std::fmt::Debug for ScreenRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScreenRegistry")
            .field("order", &self.order)
            .field("len", &self.screens.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;
    use crate::app::screens::{EventOutcome, ids};
    use ratatui::{Frame, layout::Rect};

    struct Stub {
        id: &'static str,
        rendered: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Screen for Stub {
        fn id(&self) -> &str {
            self.id
        }
        fn render(&mut self, _frame: &mut Frame, _area: Rect, _state: &mut AppState) {
            self.rendered.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        fn handle_event(&mut self, _state: &mut AppState) -> EventOutcome {
            EventOutcome::Handled
        }
    }

    #[test]
    fn registry_preserves_insertion_order() {
        let mut r = ScreenRegistry::new();
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        r.register(Box::new(Stub {
            id: ids::HOME,
            rendered: counter.clone(),
        }));
        r.register(Box::new(Stub {
            id: ids::ANALYTICS,
            rendered: counter.clone(),
        }));
        r.register(Box::new(Stub {
            id: ids::CONFIG,
            rendered: counter,
        }));
        assert_eq!(r.order(), &[ids::HOME, ids::ANALYTICS, ids::CONFIG]);
        assert_eq!(r.len(), 3);
        assert!(r.contains(ids::ANALYTICS));
        assert!(!r.contains("nonexistent"));
    }

    #[test]
    fn re_registering_same_id_keeps_one_entry_and_order() {
        let mut r = ScreenRegistry::new();
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        r.register(Box::new(Stub {
            id: ids::HOME,
            rendered: counter.clone(),
        }));
        r.register(Box::new(Stub {
            id: ids::ANALYTICS,
            rendered: counter.clone(),
        }));
        // Re-register HOME — must replace in place, not duplicate the order entry.
        r.register(Box::new(Stub {
            id: ids::HOME,
            rendered: counter,
        }));
        assert_eq!(r.len(), 2);
        assert_eq!(r.order(), &[ids::HOME, ids::ANALYTICS]);
    }

    #[test]
    fn unknown_id_returns_none() {
        let mut r = ScreenRegistry::new();
        assert!(r.get_mut("nope").is_none());
    }
}
