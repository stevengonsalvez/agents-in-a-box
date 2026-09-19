//! The daemons panel the settings page draws (D3d), from the `hangar`
//! section's `daemons_state`.
//!
//! The terminal's Daemons screen arms a background collector on entry and
//! touches it on every paint; the collector parks after thirty seconds
//! without a touch. A window paints nothing on the host side, so this does the
//! screen's part while the reducer is on the Config screen, which is where the
//! desktop's settings page keeps it: arm the collector, keep it alive, and
//! bump the section only when a new collection landed, so the panel frames
//! rows as they move and not on every tick.

use ainb_app::AppState;
use ainb_app::app::screens::ids as screen_ids;
use ainb_app::components::daemons::DaemonsState;
use ainb_app::fleet::daemons::heartbeat::now_ms;

/// Where the panel's reading stands: the last collection it framed.
#[derive(Debug, Default)]
pub struct DaemonsPanel {
    framed_collected_at_ms: Option<i64>,
}

impl DaemonsPanel {
    /// Keep the collector running while the settings page is open, and frame
    /// the section when it collected something new.
    pub fn tick(&mut self, state: &mut AppState) {
        if state.shell.current_screen != screen_ids::CONFIG {
            return;
        }
        // Read through `Deref`: a `&mut` path through the section bumps its
        // version, which is a frame the window applies for nothing.
        let shared = state.hangar.daemons_state.shared.clone();
        let Some(shared) = shared else {
            // First open: arm the collector, as entering the Daemons screen does.
            state.hangar.daemons_state.arm();
            return;
        };
        let (collected_at_ms, parked) = {
            let guard = shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            (guard.collected_at_ms, guard.collector_parked)
        };
        if parked {
            // `arm` touches and revives it; the bump it costs is the rare case.
            state.hangar.daemons_state.arm();
        } else {
            // A touch keeps it alive without bumping the section.
            let _ = DaemonsState::touch(&shared, now_ms());
        }
        if self.framed_collected_at_ms != Some(collected_at_ms) {
            self.framed_collected_at_ms = Some(collected_at_ms);
            // Fold finished actions and clamp the selection, as the terminal's
            // tick does, and let the section frame its new rows.
            state.hangar.daemons_state.tick();
        }
    }
}
