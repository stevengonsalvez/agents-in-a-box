//! P4.8 — Frosted progress-banner state machine: the pure cross-screen reducer.
//!
//! The active-task banner is pinned across *every* screen: it appears on
//! [`HangarEvent::TaskStarted`], ticks its elapsed clock once per second (a 1Hz
//! tick channel, never 60Hz — see the P4.8 flicker-risk note), updates its tool
//! count on [`HangarEvent::TaskProgress`], shows the latest transcript line, and
//! hides on [`HangarEvent::TaskFinished`]. Capital-`X` from any screen emits a
//! cancel intent ([`BannerIntent::CancelTask`]).
//!
//! As with every Hangar reducer this is **pure**: it folds a [`BannerEvent`] into
//! a new [`BannerState`] + optional intent. The host owns the 1Hz timer and feeds
//! [`BannerEvent::Tick`]; the reducer never reads a clock itself, so elapsed time
//! is deterministic in tests.
//!
//! The banner state machine is deliberately isolated so the P7 autopilot screen
//! can reuse it for a "next autopilot run in X" countdown.

use ainb_hangar_proto::events::HangarEvent;

use crate::screen::ActiveTaskBanner;

/// The banner reducer's render-state cache.
///
/// Holds the optional [`ActiveTaskBanner`] and the latest transcript line (the
/// banner's second row). When no task is running both are absent / empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BannerState {
    banner: Option<ActiveTaskBanner>,
    latest_message: String,
    /// The agent label learned from the most recent `TaskQueued`, so the
    /// subsequent `TaskStarted` can show the banner with the right name. Cleared
    /// once the banner is shown or the task finishes.
    pending_agent_label: Option<String>,
}

impl BannerState {
    /// The active-task banner, when a task is running.
    #[must_use]
    pub const fn banner(&self) -> Option<&ActiveTaskBanner> {
        self.banner.as_ref()
    }

    /// The latest transcript line shown on the banner's second row.
    #[must_use]
    pub fn latest_message(&self) -> &str {
        &self.latest_message
    }
}

/// An input the banner reducer folds into [`BannerState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerEvent {
    /// A host stream event (task lifecycle / progress / message).
    Event(HangarEvent),
    /// A 1Hz elapsed-clock tick from the host timer.
    Tick,
    /// A printable key from any screen (capital `X` cancels).
    Key(char),
}

/// A side-effect the plugin glue performs after a banner reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerIntent {
    /// Cancel the running task (capital `X` from any screen).
    CancelTask(ainb_hangar_core::ids::TaskId),
}

/// The result of folding one [`BannerEvent`] into a [`BannerState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerReduction {
    /// The next banner state.
    pub state: BannerState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<BannerIntent>,
}

/// Fold one [`BannerEvent`] into `state`. Pure: no IO, no clock read.
#[must_use]
pub fn reduce_banner(state: &BannerState, ev: BannerEvent) -> BannerReduction {
    match ev {
        BannerEvent::Tick => tick(state),
        BannerEvent::Key(c) => reduce_key(state, c),
        BannerEvent::Event(event) => fold_event(state, event),
    }
}

/// Advance the elapsed clock one second when a banner is showing.
fn tick(state: &BannerState) -> BannerReduction {
    let mut next = state.clone();
    if let Some(banner) = &mut next.banner {
        banner.elapsed_secs = banner.elapsed_secs.saturating_add(1);
    }
    no_intent(next)
}

/// Handle a printable key: capital `X` cancels the running task.
fn reduce_key(state: &BannerState, c: char) -> BannerReduction {
    if let (Some(banner), 'X') = (&state.banner, c) {
        return BannerReduction {
            state: state.clone(),
            intent: Some(BannerIntent::CancelTask(banner.task_id.clone())),
        };
    }
    no_intent(state.clone())
}

/// Fold a host event into the banner: queue records the agent label, start shows
/// the banner, progress updates the tool count, a message updates the second row,
/// and finish hides the banner.
fn fold_event(state: &BannerState, event: HangarEvent) -> BannerReduction {
    let mut next = state.clone();
    match event {
        HangarEvent::TaskQueued { agent_id, .. } => {
            // Remember the agent label so the next `TaskStarted` can name it. The
            // banner stays hidden until the task actually starts.
            next.pending_agent_label = Some(agent_id.as_str().to_string());
        }
        HangarEvent::TaskStarted { task_id, .. } => {
            // Name from the prior queue if known, else a neutral default.
            let agent_label = next
                .pending_agent_label
                .take()
                .unwrap_or_else(|| "agent".to_string());
            next.banner = Some(ActiveTaskBanner {
                task_id,
                agent_label,
                elapsed_secs: 0,
                tool_calls: 0,
            });
        }
        HangarEvent::TaskProgress { task_id, tool_calls, .. } => {
            if let Some(banner) = next.banner.as_mut().filter(|b| b.task_id == task_id) {
                banner.tool_calls = tool_calls;
            }
        }
        HangarEvent::TaskMessage { task_id, body, .. }
            if next.banner.as_ref().is_some_and(|b| b.task_id == task_id) =>
        {
            next.latest_message = body;
        }
        HangarEvent::TaskFinished { task_id, .. }
            if next.banner.as_ref().is_some_and(|b| b.task_id == task_id) =>
        {
            next.banner = None;
            next.latest_message.clear();
            next.pending_agent_label = None;
        }
        _ => {}
    }
    no_intent(next)
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: BannerState) -> BannerReduction {
    BannerReduction {
        state,
        intent: None,
    }
}
