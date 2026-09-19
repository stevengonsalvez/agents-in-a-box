//! The terminal's section 20 (agent status) host (T0-section, #1015): the
//! shared reader plus the plugin publisher.
//!
//! The reader is `ainb_app::fleet::agent_status_reader`, the one both hosts run
//! (#1188): one joined daemon read per Fleet revision, folded into section 20.
//! What is the terminal's alone is the publish: the TUI loop publishes section
//! 20 to the plugins as an envelope (#1031), so the Fleet panel costs no read of
//! its own, and ticks the card clock while the section holds cards (#1054).

use std::time::Duration;

use ainb_app::app::sections::AgentStatusSection;
use ainb_app::app::state::AppState;
use ainb_app::fleet::agent_status_reader::AgentStatusReader;
pub use ainb_app::fleet::agent_status_reader::{AgentStatusUpdate, Dialer, apply};
use ainb_hangar_proto::status_topic::{
    AGENT_STATUS_CLOCK_TOPIC, AGENT_STATUS_ENVELOPE_MAX_BYTES, AGENT_STATUS_TOPIC,
    AgentStatusClock, AgentStatusEnvelope, AgentStatusHealth,
};

/// The shared reader and what the terminal publishes from it.
pub struct AgentStatusHost {
    reader: AgentStatusReader,
    /// The last envelope sequence handed to the plugin runtime.
    sequence: u64,
    /// Section 20 moved since the last publish.
    unpublished: bool,
    /// When the card clock was last published (#1054).
    last_tick: Option<std::time::Instant>,
}

impl AgentStatusHost {
    /// Start the shared reader. Must be called inside a tokio runtime.
    ///
    /// `legacy_panel` is `[fleet.status] legacy_panel`: take the two reads even
    /// from a daemon that serves the joined one.
    #[must_use]
    pub fn spawn(dialer: Dialer, legacy_panel: bool) -> Self {
        Self {
            reader: AgentStatusReader::spawn(dialer, legacy_panel),
            sequence: 0,
            unpublished: false,
            last_tick: None,
        }
    }

    /// Fold every update that has arrived into section 20, and remember to
    /// publish it. Returns whether section 20's version moved; see
    /// [`AgentStatusReader::drain_into`].
    pub fn drain_into(&mut self, state: &mut AppState) -> bool {
        let changed = self.reader.drain_into(state);
        self.unpublished |= changed;
        changed
    }

    /// Publish section 20 to the plugins on [`AGENT_STATUS_TOPIC`] when it
    /// moved since the last publish (#1031). Every update drained in one loop
    /// iteration lands as one envelope, and the task pays at most one read per
    /// revision, so a burst of events yields one publish per revision.
    ///
    /// With no plugin runtime yet the change is held and published once one
    /// exists, so a runtime that starts after the first read still gets it.
    ///
    /// This task is also the tick source for the cards' ages (#1054): while
    /// section 20 holds cards it publishes the card clock on
    /// [`AGENT_STATUS_CLOCK_TOPIC`] once a second, and right after every
    /// envelope. Each publish marks the subscribing panel for a repaint, so an
    /// idle card's age advances without a key press or a daemon event.
    ///
    /// Returns whether an envelope was published.
    pub fn publish(
        &mut self,
        state: &AppState,
        runtime: Option<&ainb_plugin_runtime::RuntimeHandle>,
    ) -> bool {
        let Some(runtime) = runtime else {
            return false;
        };
        self.publish_with(
            state,
            std::time::Instant::now(),
            now_ms(),
            |topic, payload| {
                runtime.publish_snapshot(topic, payload.into());
            },
        )
    }

    /// [`Self::publish`] through `send`, at `now` and the local wall clock
    /// `local_now_ms`. The change stays unpublished until an envelope actually
    /// goes out: a section mid-reset, with nothing to encode yet, is published
    /// by a later iteration instead of being forgotten.
    fn publish_with(
        &mut self,
        state: &AppState,
        now: std::time::Instant,
        local_now_ms: i64,
        mut send: impl FnMut(&str, Vec<u8>),
    ) -> bool {
        let mut published = false;
        if self.unpublished {
            if let Some(payload) = encode(&state.agent_status, self.sequence + 1) {
                self.sequence += 1;
                send(AGENT_STATUS_TOPIC, payload);
                self.unpublished = false;
                published = true;
            }
        }
        let tick_due = self.last_tick.is_none_or(|at| now.duration_since(at) >= CLOCK_TICK);
        if published || tick_due {
            if let Some(payload) = encode_clock(&state.agent_status, local_now_ms) {
                send(AGENT_STATUS_CLOCK_TOPIC, payload);
                self.last_tick = Some(now);
            }
        }
        published
    }
}

/// How often the card clock is published while section 20 holds cards: ages
/// render in whole seconds.
const CLOCK_TICK: Duration = Duration::from_secs(1);

/// The local clock the tick carries.
///
/// The pane maps it onto the daemon's clock itself: since W0-mirror,
/// `FleetPaneState::evidence_clock_ms` is `view.daemon_now_ms(now)`, the
/// daemon's clock at its last read (`read_at_ms`) plus the local time held
/// since. So the tick must stay LOCAL: sending the daemon estimate here would
/// apply the skew twice. What the tick adds is the "since": without it the
/// pane's `now` never moves and every age freezes at the read (#1054).
fn card_clock_ms(_section: &AgentStatusSection, local_now_ms: i64) -> i64 {
    local_now_ms
}

/// The card-clock tick the plugins fold, encoded, or `None` while section 20
/// holds no cards (nothing on screen has an age to advance).
#[must_use]
pub fn encode_clock(section: &AgentStatusSection, local_now_ms: i64) -> Option<Vec<u8>> {
    if section.view.as_ref().is_none_or(|view| view.cards.is_empty()) {
        return None;
    }
    serde_json::to_vec(&AgentStatusClock {
        clock_ms: card_clock_ms(section, local_now_ms),
    })
    .ok()
}

/// Section 20 as the envelope the plugins fold, encoded.
///
/// `None` while the section holds neither a view nor an absent reason (a reset
/// with its read still in flight): the plugins keep the last envelope until
/// the read lands. A roster too large for the plugin framer is published as an
/// absent view with the reason, never cut short.
#[must_use]
pub fn encode(section: &AgentStatusSection, sequence: u64) -> Option<Vec<u8>> {
    let mut envelope = match (&section.view, &section.absent) {
        (Some(view), _) => AgentStatusEnvelope::from_view(sequence, view),
        (None, Some(reason)) => {
            AgentStatusEnvelope::absent(sequence, reason.clone(), section.head_revision)
        }
        (None, None) => return None,
    };
    // A failure reason can carry daemon error text verbatim (an RPC, IO or
    // decode message), so it is scrubbed exactly as the section 20 frame
    // scrubs it (`wire/mod.rs`). Here rather than in the proto, which has no
    // redactor, so every publish passes through it.
    match &mut envelope.health {
        AgentStatusHealth::Unreachable { reason, .. } | AgentStatusHealth::Absent { reason } => {
            *reason = ainb_app::fleet::bridge::redact::scrub(reason);
        }
        AgentStatusHealth::Live | AgentStatusHealth::Stale { .. } => {}
    }
    let bytes = match serde_json::to_vec(&envelope) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "agent status: envelope failed to encode");
            return None;
        }
    };
    if bytes.len() <= AGENT_STATUS_ENVELOPE_MAX_BYTES {
        return Some(bytes);
    }
    tracing::warn!(
        bytes = bytes.len(),
        rows = envelope.rows.len(),
        "agent status: roster too large to publish to plugins"
    );
    serde_json::to_vec(&AgentStatusEnvelope::absent(
        sequence,
        "roster too large to publish",
        section.head_revision,
    ))
    .ok()
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_app::fleet::bridge::daemon::{DaemonClient, DaemonError};
    use ainb_hangar_proto::agent_status::RosterStatusResult;

    fn dialer(socket: std::path::PathBuf) -> Dialer {
        Box::new(move || Ok(DaemonClient::with_parts(socket.clone(), "t".to_string())))
    }

    /// #1031: the published envelope is section 20 exactly, the absent reason
    /// when there is no view, and nothing mid-reset.
    #[test]
    fn the_envelope_is_section_20_or_its_absent_reason() {
        let mut state = AppState::default();
        assert_eq!(
            encode(&state.agent_status, 1),
            None,
            "nothing to publish yet"
        );

        apply(
            &mut state,
            AgentStatusUpdate::Absent("daemon serves no agent status read".into()),
        );
        let absent: AgentStatusEnvelope =
            serde_json::from_slice(&encode(&state.agent_status, 1).unwrap()).unwrap();
        assert_eq!(
            absent.into_view(),
            Err("daemon serves no agent status read".to_string())
        );

        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: Vec::new(),
                    read_revision: 4,
                    read_at_ms: 0,
                    unknown_events: Vec::new(),
                },
                10,
            ),
        );
        apply(&mut state, AgentStatusUpdate::Head(6));
        let envelope: AgentStatusEnvelope =
            serde_json::from_slice(&encode(&state.agent_status, 2).unwrap()).unwrap();
        assert_eq!(envelope.sequence, 2);
        assert_eq!(
            &envelope.into_view().expect("a view"),
            state.agent_status.view.as_ref().unwrap()
        );

        apply(&mut state, AgentStatusUpdate::Reset);
        assert_eq!(
            encode(&state.agent_status, 3),
            None,
            "a reset publishes nothing until its read lands"
        );
    }

    /// #1038 review item 6: a change stays unpublished while there is no
    /// runtime or nothing to encode, and is cleared only once an envelope goes
    /// out.
    #[tokio::test]
    async fn a_change_stays_unpublished_until_an_envelope_goes_out() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = AgentStatusHost::spawn(dialer(dir.path().join("missing.sock")), false);
        let mut state = AppState::default();
        host.unpublished = true;

        assert!(!host.publish(&state, None), "no runtime: held");
        assert!(host.unpublished);
        let mut sent = Vec::new();
        assert!(
            !host.publish_with(&state, std::time::Instant::now(), 1, |topic, payload| sent
                .push((topic.to_string(), payload))),
            "nothing to encode mid-reset: held"
        );
        assert!(
            host.unpublished,
            "a failed encode does not clear the change"
        );

        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: Vec::new(),
                    read_revision: 2,
                    read_at_ms: 0,
                    unknown_events: Vec::new(),
                },
                5,
            ),
        );
        assert!(
            host.publish_with(&state, std::time::Instant::now(), 1, |topic, payload| {
                sent.push((topic.to_string(), payload))
            })
        );
        assert!(!host.unpublished);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, AGENT_STATUS_TOPIC);
        assert!(
            !host.publish_with(&state, std::time::Instant::now(), 1, |topic, payload| sent
                .push((topic.to_string(), payload))),
            "published once"
        );
    }

    /// #1038 review item 4: a daemon RPC error carrying a token-shaped string
    /// reaches the plugins redacted.
    #[test]
    fn a_token_in_a_daemon_error_is_published_redacted() {
        let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        // The reader reports a daemon RPC error as its own text.
        let error = DaemonError::Rpc {
            code: -32000,
            message: format!("store refused credential {token}"),
        };
        let mut state = AppState::default();
        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: Vec::new(),
                    read_revision: 1,
                    read_at_ms: 0,
                    unknown_events: Vec::new(),
                },
                1,
            ),
        );
        apply(&mut state, AgentStatusUpdate::Failed(error.to_string(), 2));
        let envelope: AgentStatusEnvelope =
            serde_json::from_slice(&encode(&state.agent_status, 1).unwrap()).unwrap();
        let AgentStatusHealth::Unreachable { reason, .. } = envelope.health else {
            panic!("unreachable: {:?}", envelope.health);
        };
        assert!(!reason.contains(&token), "{reason}");
        assert!(
            reason.contains(ainb_app::fleet::bridge::redact::REDACTED),
            "{reason}"
        );
    }

    /// #1054: the host task is the tick source. Holding cards, it publishes the
    /// card clock right after an envelope and then once a second, never faster,
    /// and never while section 20 is empty.
    #[tokio::test]
    async fn the_host_publishes_the_card_clock_once_a_second_while_it_holds_cards() {
        use ainb_hangar_proto::agent_status::{RosterStatusRow, status_row};
        use ainb_hangar_proto::fleet::{
            AttentionState, FleetCapabilities, FleetConfidence, FleetProvenance, FleetProvider,
            FleetSession, LifecycleState, ManagementState, PaneBinding, TransportHealth,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut host = AgentStatusHost::spawn(dialer(dir.path().join("missing.sock")), false);
        let mut state = AppState::default();
        let start = std::time::Instant::now();
        let mut sent: Vec<(String, Vec<u8>)> = Vec::new();

        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: Vec::new(),
                    read_revision: 1,
                    unknown_events: Vec::new(),
                    read_at_ms: 0,
                },
                1,
            ),
        );
        host.unpublished = true;
        host.publish_with(&state, start, 10_000, |topic, payload| {
            sent.push((topic.into(), payload))
        });
        assert_eq!(
            sent.iter().map(|(topic, _)| topic.as_str()).collect::<Vec<_>>(),
            [AGENT_STATUS_TOPIC],
            "no cards: no clock"
        );

        let session = FleetSession {
            session_key: "claude:a".into(),
            provider: FleetProvider::Claude,
            provider_session_id: Some("a".into()),
            tmux_target: None,
            pane_binding: PaneBinding::Bound,
            process_start_fingerprint: None,
            cwd: "/w".into(),
            display_name: None,
            lifecycle: LifecycleState::Idle,
            active_work_count: 0,
            attention: AttentionState::Ask,
            current_request_fingerprint: None,
            current_request: None,
            management: ManagementState::Managed,
            transport_health: TransportHealth::Healthy,
            capabilities: FleetCapabilities::default(),
            provenance: FleetProvenance::Authoritative,
            confidence: FleetConfidence::High,
            discovered_at: 1,
            last_observed_at: 1,
            lifecycle_updated_at: 1,
            attention_updated_at: 1,
            model: None,
            reasoning_effort: None,
            model_updated_at: 0,
            version: 1,
            updated_revision: 2,
        };
        apply(
            &mut state,
            AgentStatusUpdate::Read(
                RosterStatusResult {
                    rows: vec![RosterStatusRow {
                        status: status_row(&session, true),
                        session,
                        read_revision: 2,
                    }],
                    read_revision: 2,
                    unknown_events: Vec::new(),
                    read_at_ms: 0,
                },
                2,
            ),
        );
        host.unpublished = true;
        sent.clear();
        host.publish_with(&state, start, 10_000, |topic, payload| {
            sent.push((topic.into(), payload))
        });
        assert_eq!(
            sent.iter().map(|(topic, _)| topic.as_str()).collect::<Vec<_>>(),
            [AGENT_STATUS_TOPIC, AGENT_STATUS_CLOCK_TOPIC],
            "an envelope with cards is followed by the clock"
        );
        let tick: AgentStatusClock = serde_json::from_slice(&sent[1].1).unwrap();
        assert_eq!(tick.clock_ms, 10_000);

        sent.clear();
        host.publish_with(
            &state,
            start + Duration::from_millis(400),
            10_400,
            |topic, payload| {
                sent.push((topic.into(), payload));
            },
        );
        assert!(sent.is_empty(), "not faster than once a second: {sent:?}");
        host.publish_with(
            &state,
            start + Duration::from_millis(1_000),
            11_000,
            |topic, payload| {
                sent.push((topic.into(), payload));
            },
        );
        assert_eq!(
            sent.iter().map(|(topic, _)| topic.as_str()).collect::<Vec<_>>(),
            [AGENT_STATUS_CLOCK_TOPIC],
            "an idle second later, the clock alone"
        );
        let tick: AgentStatusClock = serde_json::from_slice(&sent[0].1).unwrap();
        assert_eq!(tick.clock_ms, 11_000);
    }
}
