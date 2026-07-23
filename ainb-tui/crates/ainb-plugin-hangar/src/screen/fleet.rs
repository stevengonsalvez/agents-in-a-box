//! Fleet pane pure reducer and dense table plus detail renderer.
//!
//! This module owns no data plane. It consumes local serde wire rows matching
//! the daemon Fleet snapshot, preserves selection by stable session key, and
//! emits typed intents for plugin glue to execute later.

#![allow(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};

use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};
use serde::{Deserialize, Serialize};

const BROADCAST_MAX_PARALLEL: usize = 8;
const FG: Color = Color::rgb(220, 220, 230);
const MUTED: Color = Color::rgb(120, 120, 140);
const GOLD: Color = Color::rgb(255, 215, 0);
const BLUE: Color = Color::rgb(100, 149, 237);
const GREEN: Color = Color::rgb(100, 200, 100);

/// Capability wire shape accepted from current and planned daemon snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FleetCapabilities {
    List(Vec<String>),
    Flags(BTreeMap<String, bool>),
    Json(String),
}

impl Default for FleetCapabilities {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl FleetCapabilities {
    fn contains(&self, capability: &str) -> bool {
        match self {
            Self::List(items) => items.iter().any(|item| item.eq_ignore_ascii_case(capability)),
            Self::Flags(items) => items
                .iter()
                .any(|(name, enabled)| *enabled && name.eq_ignore_ascii_case(capability)),
            Self::Json(raw) => serde_json::from_str::<serde_json::Value>(raw)
                .ok()
                .is_some_and(|value| capability_value_contains(&value, capability)),
        }
    }

    fn labels(&self) -> Vec<String> {
        match self {
            Self::List(items) => items.clone(),
            Self::Flags(items) => items
                .iter()
                .filter(|(_, enabled)| **enabled)
                .map(|(name, _)| name.clone())
                .collect(),
            Self::Json(raw) => serde_json::from_str::<serde_json::Value>(raw)
                .ok()
                .map_or_else(Vec::new, |value| capability_value_labels(&value)),
        }
    }
}

fn capability_value_contains(value: &serde_json::Value, capability: &str) -> bool {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|item| item.eq_ignore_ascii_case(capability)),
        serde_json::Value::Object(items) => items.iter().any(|(name, enabled)| {
            enabled.as_bool().unwrap_or(false) && name.eq_ignore_ascii_case(capability)
        }),
        _ => false,
    }
}

fn capability_value_labels(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(items) => {
            items.iter().filter_map(serde_json::Value::as_str).map(str::to_string).collect()
        }
        serde_json::Value::Object(items) => items
            .iter()
            .filter(|(_, enabled)| enabled.as_bool().unwrap_or(false))
            .map(|(name, _)| name.clone())
            .collect(),
        _ => Vec::new(),
    }
}

/// Local wire row matching the planned Fleet snapshot payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetSessionRow {
    pub session_key: String,
    pub provider: String,
    #[serde(default)]
    pub provider_session_id: Option<String>,
    #[serde(default)]
    pub current_request_fingerprint: Option<String>,
    #[serde(default)]
    pub current_request: Option<serde_json::Value>,
    #[serde(alias = "lifecycle")]
    pub lifecycle_state: String,
    #[serde(alias = "attention")]
    pub attention_state: String,
    #[serde(alias = "management")]
    pub management_state: String,
    #[serde(default)]
    pub provenance: String,
    pub confidence: String,
    pub transport_health: String,
    #[serde(default)]
    pub capabilities: FleetCapabilities,
    pub version: i64,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub tmux_target: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub discovered_at: i64,
    #[serde(default)]
    pub last_observed_at: i64,
    #[serde(default)]
    pub metadata_updated_at: i64,
    #[serde(default)]
    pub lifecycle_updated_at: i64,
    #[serde(default)]
    pub attention_updated_at: i64,
    #[serde(default)]
    pub transport_updated_at: i64,
}

impl FleetSessionRow {
    fn is_running(&self) -> bool {
        self.lifecycle_state.eq_ignore_ascii_case("RUNNING")
    }

    fn is_actionable(&self) -> bool {
        !self.attention_state.eq_ignore_ascii_case("NONE")
    }

    fn is_managed(&self) -> bool {
        self.management_state.eq_ignore_ascii_case("managed")
    }

    fn session_name(&self) -> String {
        self.display_name.clone().unwrap_or_else(|| {
            self.tmux_target
                .as_deref()
                .and_then(|target| target.split(':').next())
                .filter(|name| !name.is_empty())
                .unwrap_or(&self.session_key)
                .to_string()
        })
    }
}

impl From<ainb_hangar_proto::fleet::FleetSession> for FleetSessionRow {
    fn from(session: ainb_hangar_proto::fleet::FleetSession) -> Self {
        use ainb_hangar_proto::fleet::{
            AttentionState, FleetConfidence, FleetProvenance, FleetProvider, LifecycleState,
            ManagementState, TransportHealth,
        };

        let capabilities = session.capabilities;
        let capabilities = FleetCapabilities::Flags(BTreeMap::from([
            ("structured_answer".into(), capabilities.structured_answer),
            ("approvals".into(), capabilities.approvals),
            ("send_prompt".into(), capabilities.send_prompt),
            ("continue_turn".into(), capabilities.continue_turn),
            ("retry".into(), capabilities.retry),
            ("interrupt".into(), capabilities.interrupt),
            ("start".into(), capabilities.start),
            ("stop".into(), capabilities.stop),
            ("restart".into(), capabilities.restart),
            ("kill".into(), capabilities.kill),
            ("archive".into(), capabilities.archive),
            ("tmux_attach".into(), capabilities.tmux_attach),
            ("tmux_text".into(), capabilities.tmux_text),
            ("verified_picker".into(), capabilities.verified_picker),
        ]));
        Self {
            session_key: session.session_key,
            provider: match session.provider {
                FleetProvider::Claude => "claude",
                FleetProvider::Codex => "codex",
                FleetProvider::Unknown => "unknown",
            }
            .into(),
            provider_session_id: session.provider_session_id,
            current_request_fingerprint: session.current_request_fingerprint,
            current_request: session.current_request,
            lifecycle_state: match session.lifecycle {
                LifecycleState::Starting => "STARTING",
                LifecycleState::Running => "RUNNING",
                LifecycleState::TurnComplete => "TURN_COMPLETE",
                LifecycleState::Idle => "IDLE",
                LifecycleState::Exited => "EXITED",
                LifecycleState::Unknown => "UNKNOWN",
            }
            .into(),
            attention_state: match session.attention {
                AttentionState::None => "NONE",
                AttentionState::Ask => "ASK",
                AttentionState::Approval => "APPROVAL",
                AttentionState::Waiting => "WAITING",
                AttentionState::Error => "ERROR",
            }
            .into(),
            management_state: match session.management {
                ManagementState::Managed => "MANAGED",
                ManagementState::Degraded => "DEGRADED",
            }
            .into(),
            provenance: match session.provenance {
                FleetProvenance::Authoritative => "hangar-authoritative",
                FleetProvenance::Inferred => "hangar-inferred",
            }
            .into(),
            confidence: match session.confidence {
                FleetConfidence::High => "HIGH",
                FleetConfidence::Medium => "MEDIUM",
                FleetConfidence::Low => "LOW",
            }
            .into(),
            transport_health: match session.transport_health {
                TransportHealth::Healthy => "HEALTHY",
                TransportHealth::Degraded => "DEGRADED",
                TransportHealth::Unavailable => "UNAVAILABLE",
                TransportHealth::Unknown => "UNKNOWN",
            }
            .into(),
            capabilities,
            version: session.version,
            cwd: session.cwd,
            tmux_target: session.tmux_target,
            display_name: session.display_name,
            discovered_at: session.discovered_at,
            last_observed_at: session.last_observed_at,
            metadata_updated_at: session.last_observed_at,
            lifecycle_updated_at: session.lifecycle_updated_at,
            attention_updated_at: session.attention_updated_at,
            transport_updated_at: session.last_observed_at,
        }
    }
}

/// Fleet table filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FleetFilter {
    #[default]
    Focus,
    Actionable,
    Managed,
    Degraded,
    Claude,
    Codex,
    All,
}

impl FleetFilter {
    fn matches(self, row: &FleetSessionRow) -> bool {
        match self {
            Self::Focus => !row.is_running(),
            Self::Actionable => row.is_actionable(),
            Self::Managed => row.is_managed(),
            Self::Degraded => !row.is_managed(),
            Self::Claude => row.provider.eq_ignore_ascii_case("claude"),
            Self::Codex => row.provider.eq_ignore_ascii_case("codex"),
            Self::All => true,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Focus => "Focus",
            Self::Actionable => "Actionable",
            Self::Managed => "Managed",
            Self::Degraded => "Degraded",
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::All => "All",
        }
    }
}

/// Keyboard event understood by the standalone reducer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FleetKey {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Space,
}

/// Fleet action requested for one selected session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetAction {
    StructuredAnswer {
        request_fingerprint: String,
        request_identity: Option<ainb_hangar_proto::fleet::FleetRequestIdentity>,
        answers: Vec<ainb_hangar_proto::fleet::FleetQuestionAnswer>,
    },
    Approve {
        request_fingerprint: String,
        request_identity: Option<ainb_hangar_proto::fleet::FleetRequestIdentity>,
    },
    Deny {
        request_fingerprint: String,
        request_identity: Option<ainb_hangar_proto::fleet::FleetRequestIdentity>,
    },
    SendText {
        text: String,
    },
    VerifiedPicker {
        request_fingerprint: String,
        key: String,
    },
    Continue,
    Retry,
    Interrupt,
    Stop,
    Restart,
    Kill,
    Archive,
}

impl FleetAction {
    /// Map pane action into authoritative daemon wire action.
    pub fn into_control_action(self) -> Result<ainb_hangar_proto::fleet::ControlAction, String> {
        use ainb_hangar_proto::fleet::ControlAction;
        Ok(match self {
            Self::StructuredAnswer {
                request_fingerprint,
                request_identity,
                answers,
            } => ControlAction::StructuredAnswer {
                request_fingerprint,
                request_identity,
                answers,
            },
            Self::Approve {
                request_fingerprint,
                request_identity,
            } => ControlAction::Approve {
                request_fingerprint,
                request_identity,
            },
            Self::Deny {
                request_fingerprint,
                request_identity,
            } => ControlAction::Deny {
                request_fingerprint,
                request_identity,
            },
            Self::SendText { text } => ControlAction::SendPrompt { text },
            Self::VerifiedPicker {
                request_fingerprint,
                key,
            } => ControlAction::VerifiedPicker {
                request_fingerprint,
                key,
            },
            Self::Continue => ControlAction::Continue,
            Self::Retry => ControlAction::Retry,
            Self::Interrupt => ControlAction::Interrupt,
            Self::Stop => ControlAction::Stop,
            Self::Restart => ControlAction::Restart,
            Self::Kill => ControlAction::Kill,
            Self::Archive => ControlAction::Archive,
        })
    }

    fn is_supported_by(&self, capabilities: &FleetCapabilities) -> bool {
        match self {
            Self::StructuredAnswer { .. } => capabilities.contains("structured_answer"),
            Self::Approve { .. } | Self::Deny { .. } => capabilities.contains("approvals"),
            Self::SendText { .. } => {
                capabilities.contains("send_prompt") || capabilities.contains("tmux_text")
            }
            Self::VerifiedPicker { .. } => capabilities.contains("verified_picker"),
            Self::Continue => capabilities.contains("continue_turn"),
            Self::Retry => capabilities.contains("retry"),
            Self::Interrupt => capabilities.contains("interrupt"),
            Self::Stop => capabilities.contains("stop"),
            Self::Restart => capabilities.contains("restart"),
            Self::Kill => capabilities.contains("kill"),
            Self::Archive => capabilities.contains("archive"),
        }
    }

    fn capability_label(&self) -> &'static str {
        match self {
            Self::StructuredAnswer { .. } => "structured_answer",
            Self::Approve { .. } | Self::Deny { .. } => "approvals",
            Self::SendText { .. } => "send_prompt or tmux_text",
            Self::VerifiedPicker { .. } => "verified_picker",
            Self::Continue => "continue_turn",
            Self::Retry => "retry",
            Self::Interrupt => "interrupt",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Kill => "kill",
            Self::Archive => "archive",
        }
    }

    fn is_structured(&self) -> bool {
        matches!(
            self,
            Self::StructuredAnswer { .. } | Self::Approve { .. } | Self::Deny { .. }
        )
    }

    fn is_destructive(&self) -> bool {
        matches!(
            self,
            Self::Stop | Self::Restart | Self::Kill | Self::Archive
        )
    }

    pub fn is_high_risk(&self) -> bool {
        self.is_structured() || self.is_destructive()
    }
}

/// Per-recipient broadcast result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReceiptStatus {
    Delivered,
    Failed,
    Unknown,
}

/// Durable-looking receipt supplied back to the pure reducer by plugin glue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BroadcastReceipt {
    pub session_key: String,
    pub status: ReceiptStatus,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BroadcastStage {
    Compose,
    Recipients,
    Confirm,
    InFlight,
    Receipts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BroadcastState {
    stage: BroadcastStage,
    text: String,
    expanded_roster: bool,
    cursor: usize,
    selected: BTreeSet<String>,
    receipts: BTreeMap<String, BroadcastReceipt>,
    in_flight_idempotency_key: Option<String>,
    failure_return_stage: Option<BroadcastStage>,
}

impl Default for BroadcastState {
    fn default() -> Self {
        Self {
            stage: BroadcastStage::Compose,
            text: String::new(),
            expanded_roster: false,
            cursor: 0,
            selected: BTreeSet::new(),
            receipts: BTreeMap::new(),
            in_flight_idempotency_key: None,
            failure_return_stage: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FleetMode {
    Browse,
    Answer(AnswerState),
    Start(StartState),
    Prompt {
        text: String,
    },
    Broadcast(BroadcastState),
    Confirm {
        session_key: String,
        action: FleetAction,
    },
    TypedConfirm {
        session_key: String,
        expected_name: String,
        typed: String,
        action: FleetAction,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnswerQuestion {
    id: String,
    text: String,
    options: Vec<String>,
    multi_select: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AnswerState {
    session_key: String,
    expected_version: i64,
    request_fingerprint: String,
    request_identity: Option<ainb_hangar_proto::fleet::FleetRequestIdentity>,
    questions: Vec<AnswerQuestion>,
    question_index: usize,
    option_cursor: usize,
    selections: Vec<BTreeSet<usize>>,
    texts: Vec<String>,
    editing_text: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartStage {
    Cwd,
    Prompt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartState {
    provider: ainb_hangar_proto::fleet::FleetProvider,
    stage: StartStage,
    cwd: String,
    prompt: String,
}

/// Pure Fleet pane state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetPaneState {
    roster: Vec<FleetSessionRow>,
    filter: FleetFilter,
    selected_key: Option<String>,
    mode: FleetMode,
    feedback: Option<String>,
    now_ms: i64,
    head_revision: i64,
}

impl Default for FleetPaneState {
    fn default() -> Self {
        Self {
            roster: Vec::new(),
            filter: FleetFilter::Focus,
            selected_key: None,
            mode: FleetMode::Browse,
            feedback: None,
            now_ms: 0,
            head_revision: 0,
        }
    }
}

impl FleetPaneState {
    pub fn apply_snapshot(&mut self, head_revision: i64, roster: Vec<FleetSessionRow>) {
        if head_revision < self.head_revision {
            return;
        }
        self.head_revision = head_revision;
        self.set_sessions(roster);
    }

    pub const fn head_revision(&self) -> i64 {
        self.head_revision
    }

    pub fn observe_revision(&mut self, revision: i64) {
        self.head_revision = self.head_revision.max(revision);
    }

    pub fn is_modal_open(&self) -> bool {
        !matches!(self.mode, FleetMode::Browse)
    }

    pub fn is_capturing_text(&self) -> bool {
        matches!(
            self.mode,
            FleetMode::TypedConfirm { .. }
                | FleetMode::Start(_)
                | FleetMode::Prompt { .. }
                | FleetMode::Broadcast(BroadcastState {
                    stage: BroadcastStage::Compose,
                    ..
                })
        )
    }

    pub fn set_sessions(&mut self, roster: Vec<FleetSessionRow>) {
        self.roster = roster;
        self.preserve_or_reset_selection();
    }

    pub const fn filter(&self) -> FleetFilter {
        self.filter
    }

    pub fn selected_key(&self) -> Option<&str> {
        self.selected_key.as_deref()
    }

    pub fn selected_session(&self) -> Option<&FleetSessionRow> {
        let key = self.selected_key.as_ref()?;
        self.roster.iter().find(|row| &row.session_key == key)
    }

    pub fn visible_sessions(&self) -> Vec<&FleetSessionRow> {
        self.roster.iter().filter(|row| self.filter.matches(row)).collect()
    }

    /// Total sessions in unfiltered authoritative roster.
    pub fn session_count(&self) -> usize {
        self.roster.len()
    }

    pub fn feedback(&self) -> Option<&str> {
        self.feedback.as_deref()
    }

    fn preserve_or_reset_selection(&mut self) {
        let keep = self.selected_key.as_ref().is_some_and(|key| {
            self.roster
                .iter()
                .any(|row| &row.session_key == key && self.filter.matches(row))
        });
        if !keep {
            self.selected_key = self
                .roster
                .iter()
                .find(|row| self.filter.matches(row))
                .map(|row| row.session_key.clone());
        }
    }
}

/// Input folded into the Fleet pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetEvent {
    Snapshot(Vec<FleetSessionRow>),
    SetFilter(FleetFilter),
    Key(FleetKey),
    RequestAction(FleetAction),
    ActionSucceeded { session_key: String },
    ActionFailed { session_key: String, detail: String },
    BroadcastReceipts(Vec<BroadcastReceipt>),
    BroadcastFailed { detail: String },
    Feedback(String),
    Tick(i64),
}

/// Side effect requested by the pure Fleet reducer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetIntent {
    Execute {
        session_key: String,
        expected_version: i64,
        action: FleetAction,
    },
    Start {
        provider: ainb_hangar_proto::fleet::FleetProvider,
        cwd: String,
        prompt: Option<String>,
    },
    AttachEmbedded {
        session_key: String,
        tmux_target: String,
    },
    AttachFullscreen {
        session_key: String,
        tmux_target: String,
    },
    Broadcast {
        text: String,
        recipient_keys: Vec<String>,
        idempotency_key: String,
        max_parallel: usize,
        retry_failures_only: bool,
    },
}

/// Result of one pure Fleet reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetReduction {
    pub state: FleetPaneState,
    pub intent: Option<FleetIntent>,
}

/// Fold one event into Fleet pane state without IO.
#[must_use]
pub fn reduce_fleet(state: &FleetPaneState, event: FleetEvent) -> FleetReduction {
    let mut next = state.clone();
    let intent = match event {
        FleetEvent::Snapshot(roster) => {
            next.set_sessions(roster);
            None
        }
        FleetEvent::SetFilter(filter) => {
            next.filter = filter;
            next.preserve_or_reset_selection();
            None
        }
        FleetEvent::Key(key) => reduce_key(&mut next, key),
        FleetEvent::RequestAction(action) => request_action(&mut next, action),
        FleetEvent::ActionSucceeded { session_key } => {
            next.feedback = Some(format!("action succeeded: {session_key}"));
            select_next_focus(&mut next, &session_key);
            None
        }
        FleetEvent::ActionFailed {
            session_key,
            detail,
        } => {
            next.feedback = Some(format!("action failed: {session_key}: {detail}"));
            None
        }
        FleetEvent::BroadcastReceipts(receipts) => {
            apply_broadcast_receipts(&mut next, receipts);
            None
        }
        FleetEvent::BroadcastFailed { detail } => {
            apply_broadcast_failure(&mut next, detail);
            None
        }
        FleetEvent::Feedback(message) => {
            next.feedback = Some(message);
            None
        }
        FleetEvent::Tick(now_ms) => {
            next.now_ms = now_ms;
            None
        }
    };
    FleetReduction {
        state: next,
        intent,
    }
}

fn reduce_key(state: &mut FleetPaneState, key: FleetKey) -> Option<FleetIntent> {
    match state.mode.clone() {
        FleetMode::Browse => reduce_browse_key(state, key),
        FleetMode::Answer(answer) => reduce_answer_key(state, answer, key),
        FleetMode::Start(start) => reduce_start_key(state, start, key),
        FleetMode::Prompt { text } => reduce_prompt_key(state, text, key),
        FleetMode::Broadcast(broadcast) => reduce_broadcast_key(state, broadcast, key),
        FleetMode::Confirm {
            session_key,
            action,
        } => reduce_confirm_key(state, &session_key, action, key),
        FleetMode::TypedConfirm {
            session_key,
            expected_name,
            typed,
            action,
        } => reduce_typed_confirm_key(state, &session_key, &expected_name, typed, action, key),
    }
}

fn reduce_browse_key(state: &mut FleetPaneState, key: FleetKey) -> Option<FleetIntent> {
    match key {
        FleetKey::Down | FleetKey::Char('j') => move_selection(state, 1),
        FleetKey::Up | FleetKey::Char('k') => move_selection(state, -1),
        FleetKey::Right => return attach_intent(state, false),
        FleetKey::Char('A') => return attach_intent(state, true),
        FleetKey::Enter => begin_structured_answer(state),
        FleetKey::Char('t') => {
            state.mode = FleetMode::Start(StartState {
                provider: ainb_hangar_proto::fleet::FleetProvider::Codex,
                stage: StartStage::Cwd,
                cwd: String::new(),
                prompt: String::new(),
            });
        }
        FleetKey::Char('p') => {
            state.mode = FleetMode::Prompt {
                text: String::new(),
            }
        }
        FleetKey::Char('b' | 'B') => state.mode = FleetMode::Broadcast(BroadcastState::default()),
        _ => {}
    }
    None
}

fn begin_structured_answer(state: &mut FleetPaneState) {
    let Some(row) = state.selected_session().cloned() else {
        state.feedback = Some("no Fleet session selected".into());
        return;
    };
    if !row.attention_state.eq_ignore_ascii_case("ASK") {
        state.feedback = Some("selected session has no structured question".into());
        return;
    }
    if !row.is_managed() || !row.capabilities.contains("structured_answer") {
        state.feedback = Some("structured answer unavailable for selected session".into());
        return;
    }
    let Some(request_fingerprint) = row.current_request_fingerprint.clone() else {
        state.feedback = Some("structured request fingerprint unavailable".into());
        return;
    };
    let Some(request) = row.current_request.as_ref() else {
        state.feedback = Some("structured request payload unavailable".into());
        return;
    };
    let questions = answer_questions(request);
    if questions.is_empty() {
        state.feedback = Some("structured request has no questions".into());
        return;
    }
    let selections = vec![BTreeSet::new(); questions.len()];
    let texts = vec![String::new(); questions.len()];
    let editing_text = questions[0].options.is_empty();
    state.mode = FleetMode::Answer(AnswerState {
        session_key: row.session_key,
        expected_version: row.version,
        request_fingerprint,
        request_identity: request_identity(request),
        questions,
        question_index: 0,
        option_cursor: 0,
        selections,
        texts,
        editing_text,
    });
}

fn answer_questions(request: &serde_json::Value) -> Vec<AnswerQuestion> {
    let payload = request.get("payload").unwrap_or(request);
    let input = payload.get("tool_input").or_else(|| payload.get("input")).unwrap_or(payload);
    input
        .get("questions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, question)| AnswerQuestion {
            id: question
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map_or_else(|| index.to_string(), str::to_string),
            text: question
                .get("question")
                .or_else(|| question.get("text"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            options: question
                .get("options")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|option| {
                    option
                        .as_str()
                        .or_else(|| option.get("label").and_then(serde_json::Value::as_str))
                        .map(str::to_string)
                })
                .collect(),
            multi_select: question
                .get("multiSelect")
                .or_else(|| question.get("multi_select"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        })
        .collect()
}

fn request_identity(
    request: &serde_json::Value,
) -> Option<ainb_hangar_proto::fleet::FleetRequestIdentity> {
    let payload = request.get("payload").unwrap_or(request);
    let identity = payload.get("identity").unwrap_or(payload);
    let request_id = identity
        .get("requestId")
        .or_else(|| identity.get("request_id"))
        .or_else(|| identity.get("tool_use_id"))
        .or_else(|| identity.get("id"))?
        .clone();
    let text = |camel: &str, snake: &str| {
        identity
            .get(camel)
            .or_else(|| identity.get(snake))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    Some(ainb_hangar_proto::fleet::FleetRequestIdentity {
        request_id,
        thread_id: text("threadId", "thread_id"),
        turn_id: text("turnId", "turn_id"),
        item_id: text("itemId", "item_id"),
    })
}

fn reduce_answer_key(
    state: &mut FleetPaneState,
    mut answer: AnswerState,
    key: FleetKey,
) -> Option<FleetIntent> {
    if key == FleetKey::Esc {
        state.mode = FleetMode::Browse;
        return None;
    }
    let Some(question) = answer.questions.get(answer.question_index) else {
        state.mode = FleetMode::Browse;
        state.feedback = Some("structured question changed before answer".into());
        return None;
    };
    if answer.editing_text {
        match key {
            FleetKey::Backspace => {
                answer.texts[answer.question_index].pop();
            }
            FleetKey::Enter if answer.texts[answer.question_index].trim().is_empty() => {
                state.feedback = Some("answer text required".into());
            }
            FleetKey::Enter => return advance_or_submit_answer(state, answer),
            FleetKey::Char(character) => {
                answer.texts[answer.question_index].push(character);
            }
            FleetKey::Space => answer.texts[answer.question_index].push(' '),
            _ => {}
        }
        state.mode = FleetMode::Answer(answer);
        return None;
    }
    match key {
        FleetKey::Up | FleetKey::Char('k') => {
            answer.option_cursor = answer.option_cursor.saturating_sub(1);
        }
        FleetKey::Down | FleetKey::Char('j') => {
            answer.option_cursor =
                (answer.option_cursor + 1).min(question.options.len().saturating_sub(1));
        }
        FleetKey::Space if question.multi_select => {
            let selected = &mut answer.selections[answer.question_index];
            if !selected.remove(&answer.option_cursor) {
                selected.insert(answer.option_cursor);
            }
        }
        FleetKey::Char('o') => {
            answer.editing_text = true;
        }
        FleetKey::Enter => {
            if answer.selections[answer.question_index].is_empty() {
                answer.selections[answer.question_index].insert(answer.option_cursor);
            }
            let selected_other = answer.selections[answer.question_index]
                .iter()
                .filter_map(|index| question.options.get(*index))
                .any(|option| option.eq_ignore_ascii_case("other"));
            if selected_other && answer.texts[answer.question_index].trim().is_empty() {
                answer.editing_text = true;
            } else {
                return advance_or_submit_answer(state, answer);
            }
        }
        _ => {}
    }
    state.mode = FleetMode::Answer(answer);
    None
}

fn advance_or_submit_answer(
    state: &mut FleetPaneState,
    mut answer: AnswerState,
) -> Option<FleetIntent> {
    if answer.question_index + 1 < answer.questions.len() {
        answer.question_index += 1;
        answer.option_cursor = 0;
        answer.editing_text = answer.questions[answer.question_index].options.is_empty();
        state.mode = FleetMode::Answer(answer);
        return None;
    }
    let answers = answer
        .questions
        .iter()
        .zip(&answer.selections)
        .zip(&answer.texts)
        .map(|((question, selected), text)| {
            let text = (!text.trim().is_empty()).then(|| text.trim().to_string());
            let selected_other = selected
                .iter()
                .filter_map(|index| question.options.get(*index))
                .any(|option| option.eq_ignore_ascii_case("other"));
            let selected_options = if !question.multi_select && selected_other && text.is_some() {
                Vec::new()
            } else {
                selected
                    .iter()
                    .filter_map(|index| question.options.get(*index).cloned())
                    .collect()
            };
            ainb_hangar_proto::fleet::FleetQuestionAnswer {
                question_id: question.id.clone(),
                selected_options,
                text,
            }
        })
        .collect();
    state.mode = FleetMode::Browse;
    Some(FleetIntent::Execute {
        session_key: answer.session_key,
        expected_version: answer.expected_version,
        action: FleetAction::StructuredAnswer {
            request_fingerprint: answer.request_fingerprint,
            request_identity: answer.request_identity,
            answers,
        },
    })
}

fn reduce_start_key(
    state: &mut FleetPaneState,
    mut start: StartState,
    key: FleetKey,
) -> Option<FleetIntent> {
    if key == FleetKey::Esc {
        state.mode = FleetMode::Browse;
        return None;
    }
    let field = match start.stage {
        StartStage::Cwd => &mut start.cwd,
        StartStage::Prompt => &mut start.prompt,
    };
    match key {
        FleetKey::Backspace => {
            field.pop();
        }
        FleetKey::Char(character) => field.push(character),
        FleetKey::Space => field.push(' '),
        FleetKey::Enter if start.stage == StartStage::Cwd && start.cwd.trim().is_empty() => {
            state.feedback = Some("working directory required".into());
        }
        FleetKey::Enter if start.stage == StartStage::Cwd => {
            start.cwd = start.cwd.trim().to_string();
            start.stage = StartStage::Prompt;
        }
        FleetKey::Enter => {
            state.mode = FleetMode::Browse;
            return Some(FleetIntent::Start {
                provider: start.provider,
                cwd: start.cwd,
                prompt: (!start.prompt.trim().is_empty()).then(|| start.prompt.trim().to_string()),
            });
        }
        _ => {}
    }
    state.mode = FleetMode::Start(start);
    None
}

fn reduce_prompt_key(
    state: &mut FleetPaneState,
    mut text: String,
    key: FleetKey,
) -> Option<FleetIntent> {
    match key {
        FleetKey::Esc => state.mode = FleetMode::Browse,
        FleetKey::Backspace => {
            text.pop();
            state.mode = FleetMode::Prompt { text };
        }
        FleetKey::Enter if !text.trim().is_empty() => {
            state.mode = FleetMode::Browse;
            return request_action(state, FleetAction::SendText { text });
        }
        FleetKey::Enter => {
            state.feedback = Some("prompt text required".into());
            state.mode = FleetMode::Prompt { text };
        }
        FleetKey::Char(character) => {
            text.push(character);
            state.mode = FleetMode::Prompt { text };
        }
        FleetKey::Space => {
            text.push(' ');
            state.mode = FleetMode::Prompt { text };
        }
        _ => state.mode = FleetMode::Prompt { text },
    }
    None
}

fn move_selection(state: &mut FleetPaneState, delta: isize) {
    let keys: Vec<String> =
        state.visible_sessions().iter().map(|row| row.session_key.clone()).collect();
    if keys.is_empty() {
        state.selected_key = None;
        return;
    }
    let current = state
        .selected_key
        .as_ref()
        .and_then(|key| keys.iter().position(|candidate| candidate == key))
        .unwrap_or(0);
    let next = if delta < 0 {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        (current + delta as usize).min(keys.len() - 1)
    };
    state.selected_key = keys.get(next).cloned();
}

fn attach_intent(state: &mut FleetPaneState, fullscreen: bool) -> Option<FleetIntent> {
    let Some(row) = state.selected_session() else {
        state.feedback = Some("no Fleet session selected".into());
        return None;
    };
    if !row.capabilities.contains("tmux_attach") {
        state.feedback = Some("session lacks tmux_attach capability".into());
        return None;
    }
    let Some(target) = row.tmux_target.clone() else {
        state.feedback = Some("selected session has no tmux target".into());
        return None;
    };
    let session_key = row.session_key.clone();
    Some(if fullscreen {
        FleetIntent::AttachFullscreen {
            session_key,
            tmux_target: target,
        }
    } else {
        FleetIntent::AttachEmbedded {
            session_key,
            tmux_target: target,
        }
    })
}

fn request_action(state: &mut FleetPaneState, action: FleetAction) -> Option<FleetIntent> {
    let Some(row) = state.selected_session().cloned() else {
        state.feedback = Some("no Fleet session selected".into());
        return None;
    };
    if !row.is_managed() && (action.is_structured() || action.is_destructive()) {
        state.feedback = Some("degraded session blocks structured and destructive actions".into());
        return None;
    }
    if !action.is_supported_by(&row.capabilities) {
        state.feedback = Some(format!(
            "session lacks {} capability",
            action.capability_label()
        ));
        return None;
    }
    match action {
        FleetAction::Stop | FleetAction::Restart => {
            state.mode = FleetMode::Confirm {
                session_key: row.session_key,
                action,
            };
            None
        }
        FleetAction::Kill | FleetAction::Archive => {
            let expected_name = row.session_name();
            state.mode = FleetMode::TypedConfirm {
                session_key: row.session_key,
                expected_name,
                typed: String::new(),
                action,
            };
            None
        }
        action => Some(FleetIntent::Execute {
            session_key: row.session_key,
            expected_version: row.version,
            action,
        }),
    }
}

/// Build approve or deny from exact selected request identity.
pub fn selected_approval_action(
    state: &FleetPaneState,
    approve: bool,
) -> Result<FleetAction, String> {
    let row = state
        .selected_session()
        .ok_or_else(|| "no Fleet session selected".to_string())?;
    if !row.attention_state.eq_ignore_ascii_case("APPROVAL") {
        return Err("selected session has no approval request".into());
    }
    let fingerprint = row
        .current_request_fingerprint
        .clone()
        .ok_or_else(|| "approval request fingerprint unavailable".to_string())?;
    let identity = row.current_request.as_ref().and_then(request_identity);
    Ok(if approve {
        FleetAction::Approve {
            request_fingerprint: fingerprint,
            request_identity: identity,
        }
    } else {
        FleetAction::Deny {
            request_fingerprint: fingerprint,
            request_identity: identity,
        }
    })
}

fn reduce_confirm_key(
    state: &mut FleetPaneState,
    session_key: &str,
    action: FleetAction,
    key: FleetKey,
) -> Option<FleetIntent> {
    match key {
        FleetKey::Esc => {
            state.mode = FleetMode::Browse;
            None
        }
        FleetKey::Enter => execute_confirmed(state, session_key, action),
        _ => None,
    }
}

fn reduce_typed_confirm_key(
    state: &mut FleetPaneState,
    session_key: &str,
    expected_name: &str,
    mut typed: String,
    action: FleetAction,
    key: FleetKey,
) -> Option<FleetIntent> {
    match key {
        FleetKey::Esc => {
            state.mode = FleetMode::Browse;
            None
        }
        FleetKey::Backspace => {
            typed.pop();
            state.mode = FleetMode::TypedConfirm {
                session_key: session_key.to_string(),
                expected_name: expected_name.to_string(),
                typed,
                action,
            };
            None
        }
        FleetKey::Char(character) => {
            typed.push(character);
            state.mode = FleetMode::TypedConfirm {
                session_key: session_key.to_string(),
                expected_name: expected_name.to_string(),
                typed,
                action,
            };
            None
        }
        FleetKey::Enter if typed == expected_name => execute_confirmed(state, session_key, action),
        FleetKey::Enter => {
            state.feedback = Some(format!("type exact session name: {expected_name}"));
            None
        }
        _ => None,
    }
}

fn execute_confirmed(
    state: &mut FleetPaneState,
    session_key: &str,
    action: FleetAction,
) -> Option<FleetIntent> {
    let Some(row) = state.roster.iter().find(|row| row.session_key == session_key) else {
        state.mode = FleetMode::Browse;
        state.feedback = Some("session disappeared before confirmation".into());
        return None;
    };
    let intent = FleetIntent::Execute {
        session_key: row.session_key.clone(),
        expected_version: row.version,
        action,
    };
    state.mode = FleetMode::Browse;
    Some(intent)
}

fn select_next_focus(state: &mut FleetPaneState, completed_key: &str) {
    state.filter = FleetFilter::Focus;
    let keys: Vec<String> = state
        .roster
        .iter()
        .filter(|row| FleetFilter::Focus.matches(row))
        .map(|row| row.session_key.clone())
        .collect();
    state.selected_key = match keys.iter().position(|key| key == completed_key) {
        Some(index) if keys.len() > 1 => keys.get((index + 1) % keys.len()).cloned(),
        Some(index) => keys.get(index).cloned(),
        None => keys.first().cloned(),
    };
}

fn reduce_broadcast_key(
    state: &mut FleetPaneState,
    broadcast: BroadcastState,
    key: FleetKey,
) -> Option<FleetIntent> {
    if key == FleetKey::Esc && broadcast.stage != BroadcastStage::InFlight {
        state.mode = FleetMode::Browse;
        return None;
    }
    match broadcast.stage {
        BroadcastStage::Compose => reduce_broadcast_compose(state, broadcast, key),
        BroadcastStage::Recipients => reduce_broadcast_recipients(state, broadcast, key),
        BroadcastStage::Confirm => reduce_broadcast_confirm(state, broadcast, key),
        BroadcastStage::InFlight => {
            state.feedback = Some("broadcast already in flight".into());
            state.mode = FleetMode::Broadcast(broadcast);
            None
        }
        BroadcastStage::Receipts => reduce_broadcast_receipts(state, broadcast, key),
    }
}

fn reduce_broadcast_compose(
    state: &mut FleetPaneState,
    mut broadcast: BroadcastState,
    key: FleetKey,
) -> Option<FleetIntent> {
    match key {
        FleetKey::Char(character) => broadcast.text.push(character),
        FleetKey::Backspace => {
            broadcast.text.pop();
        }
        FleetKey::Enter if !broadcast.text.trim().is_empty() => {
            broadcast.stage = BroadcastStage::Recipients;
        }
        FleetKey::Enter => state.feedback = Some("broadcast text required".into()),
        _ => {}
    }
    state.mode = FleetMode::Broadcast(broadcast);
    None
}

fn broadcast_candidate_keys(state: &FleetPaneState, expanded: bool) -> Vec<String> {
    state
        .roster
        .iter()
        .filter(|row| expanded || state.filter.matches(row))
        .map(|row| row.session_key.clone())
        .collect()
}

fn reduce_broadcast_recipients(
    state: &mut FleetPaneState,
    mut broadcast: BroadcastState,
    key: FleetKey,
) -> Option<FleetIntent> {
    let candidates = broadcast_candidate_keys(state, broadcast.expanded_roster);
    match key {
        FleetKey::Down | FleetKey::Char('j') => {
            broadcast.cursor = (broadcast.cursor + 1).min(candidates.len().saturating_sub(1));
        }
        FleetKey::Up | FleetKey::Char('k') => {
            broadcast.cursor = broadcast.cursor.saturating_sub(1);
        }
        FleetKey::Space => {
            if let Some(key) = candidates.get(broadcast.cursor) {
                if !broadcast.selected.remove(key) {
                    broadcast.selected.insert(key.clone());
                }
            }
        }
        FleetKey::Char('a') => broadcast.selected.extend(candidates),
        FleetKey::Char('e') => {
            broadcast.expanded_roster = true;
            broadcast.cursor = 0;
        }
        FleetKey::Enter if !broadcast.selected.is_empty() => {
            broadcast.stage = BroadcastStage::Confirm;
        }
        FleetKey::Enter => state.feedback = Some("select at least one recipient".into()),
        _ => {}
    }
    state.mode = FleetMode::Broadcast(broadcast);
    None
}

fn reduce_broadcast_confirm(
    state: &mut FleetPaneState,
    mut broadcast: BroadcastState,
    key: FleetKey,
) -> Option<FleetIntent> {
    if key != FleetKey::Enter {
        state.mode = FleetMode::Broadcast(broadcast);
        return None;
    }
    let recipient_keys = broadcast.selected.iter().cloned().collect();
    let idempotency_key = broadcast
        .in_flight_idempotency_key
        .get_or_insert_with(|| format!("fleet-broadcast-{}", uuid::Uuid::new_v4()))
        .clone();
    let intent = FleetIntent::Broadcast {
        text: broadcast.text.clone(),
        recipient_keys,
        idempotency_key,
        max_parallel: BROADCAST_MAX_PARALLEL,
        retry_failures_only: false,
    };
    broadcast.failure_return_stage = Some(BroadcastStage::Confirm);
    broadcast.stage = BroadcastStage::InFlight;
    state.mode = FleetMode::Broadcast(broadcast);
    Some(intent)
}

fn reduce_broadcast_receipts(
    state: &mut FleetPaneState,
    mut broadcast: BroadcastState,
    key: FleetKey,
) -> Option<FleetIntent> {
    let failed: Vec<String> = broadcast
        .receipts
        .values()
        .filter(|receipt| receipt.status == ReceiptStatus::Failed)
        .map(|receipt| receipt.session_key.clone())
        .collect();
    match key {
        FleetKey::Down | FleetKey::Char('j') => {
            broadcast.cursor = (broadcast.cursor + 1).min(failed.len().saturating_sub(1));
        }
        FleetKey::Up | FleetKey::Char('k') => {
            broadcast.cursor = broadcast.cursor.saturating_sub(1);
        }
        FleetKey::Space => {
            if let Some(key) = failed.get(broadcast.cursor) {
                if !broadcast.selected.remove(key) {
                    broadcast.selected.insert(key.clone());
                }
            }
        }
        FleetKey::Char('r') => {
            let recipient_keys: Vec<String> =
                failed.into_iter().filter(|key| broadcast.selected.contains(key)).collect();
            if recipient_keys.is_empty() {
                state.feedback = Some("select failed recipients to retry".into());
            } else {
                let idempotency_key = broadcast
                    .in_flight_idempotency_key
                    .get_or_insert_with(|| format!("fleet-broadcast-{}", uuid::Uuid::new_v4()))
                    .clone();
                let intent = FleetIntent::Broadcast {
                    text: broadcast.text.clone(),
                    recipient_keys,
                    idempotency_key,
                    max_parallel: BROADCAST_MAX_PARALLEL,
                    retry_failures_only: true,
                };
                broadcast.failure_return_stage = Some(BroadcastStage::Receipts);
                broadcast.stage = BroadcastStage::InFlight;
                state.mode = FleetMode::Broadcast(broadcast);
                return Some(intent);
            }
        }
        _ => {}
    }
    state.mode = FleetMode::Broadcast(broadcast);
    None
}

fn apply_broadcast_receipts(state: &mut FleetPaneState, receipts: Vec<BroadcastReceipt>) {
    let FleetMode::Broadcast(mut broadcast) = state.mode.clone() else {
        state.feedback = Some("ignored broadcast receipts without active broadcast".into());
        return;
    };
    for receipt in receipts {
        broadcast.receipts.insert(receipt.session_key.clone(), receipt);
    }
    broadcast.selected.retain(|session_key| {
        broadcast
            .receipts
            .get(session_key)
            .is_some_and(|receipt| receipt.status == ReceiptStatus::Failed)
    });
    broadcast.cursor = 0;
    broadcast.in_flight_idempotency_key = None;
    broadcast.failure_return_stage = None;
    broadcast.stage = BroadcastStage::Receipts;
    state.mode = FleetMode::Broadcast(broadcast);
}

fn apply_broadcast_failure(state: &mut FleetPaneState, detail: String) {
    let FleetMode::Broadcast(mut broadcast) = state.mode.clone() else {
        state.feedback = Some(format!("broadcast failed: {detail}"));
        return;
    };
    if broadcast.stage != BroadcastStage::InFlight {
        state.feedback = Some(format!("ignored stale broadcast failure: {detail}"));
        return;
    }
    broadcast.stage = broadcast.failure_return_stage.unwrap_or_else(|| {
        if broadcast.receipts.is_empty() {
            BroadcastStage::Confirm
        } else {
            BroadcastStage::Receipts
        }
    });
    broadcast.in_flight_idempotency_key = None;
    broadcast.failure_return_stage = None;
    state.feedback = Some(format!("broadcast failed: {detail}"));
    state.mode = FleetMode::Broadcast(broadcast);
}

/// Render dense Fleet table, selected-session detail, and active modal.
pub fn render_fleet(
    buffer: &mut WireBuffer,
    area_width: u16,
    top: u16,
    bottom: u16,
    state: &FleetPaneState,
) {
    if area_width == 0 || bottom <= top {
        return;
    }
    let list_width = (area_width * 2 / 3).max(48).min(area_width);
    put_str(
        buffer,
        1,
        top,
        &format!(
            "Fleet  [{}]  {}/{} sessions",
            state.filter.label(),
            state.visible_sessions().len(),
            state.roster.len()
        ),
        GOLD,
        list_width,
    );
    if top + 1 < bottom {
        put_str(
            buffer,
            1,
            top + 1,
            "SESSION         PRV LIFE   ATTN AGE   MODE CONF NET",
            MUTED,
            list_width,
        );
    }

    let visible = state.visible_sessions();
    let mut row_y = top.saturating_add(2);
    for session in visible {
        if row_y >= bottom {
            break;
        }
        let selected = state.selected_key.as_deref() == Some(session.session_key.as_str());
        render_table_row(buffer, row_y, list_width, session, selected, state.now_ms);
        row_y = row_y.saturating_add(1);
    }
    if row_y == top.saturating_add(2) && row_y < bottom {
        put_str(
            buffer,
            2,
            row_y,
            "No sessions match filter",
            MUTED,
            list_width,
        );
    }

    if list_width < area_width {
        render_divider(buffer, list_width, top, bottom);
        render_detail(
            buffer,
            list_width.saturating_add(2),
            area_width,
            top,
            bottom,
            state,
        );
    }
    render_mode(buffer, area_width, top, bottom, state);
}

/// Overlay a compact degraded banner without hiding the cached Fleet roster.
pub fn render_degraded_banner(buffer: &mut WireBuffer, area_width: u16, top: u16) {
    put_str(
        buffer,
        0,
        top,
        "Fleet daemon offline, cached snapshot, high-risk actions disabled",
        GOLD,
        area_width,
    );
}

fn render_table_row(
    buffer: &mut WireBuffer,
    row_y: u16,
    right: u16,
    session: &FleetSessionRow,
    selected: bool,
    now_ms: i64,
) {
    let marker = if selected { '>' } else { ' ' };
    let name = truncate(&session.session_name(), 15);
    let provider = truncate(&session.provider, 3).to_uppercase();
    let lifecycle = truncate(&session.lifecycle_state, 6).to_uppercase();
    let attention = truncate(&session.attention_state, 4).to_uppercase();
    let mode = if session.is_managed() { "MNG" } else { "DEG" };
    let confidence = truncate(&session.confidence, 4).to_uppercase();
    let health = truncate(&session.transport_health, 3).to_uppercase();
    let age = format_age(now_ms, session.last_observed_at);
    let text = format!(
        "{marker}{name:<15} {provider:<3} {lifecycle:<6} {attention:<4} {age:<5} {mode:<3} {confidence:<4} {health:<3}"
    );
    put_str(
        buffer,
        0,
        row_y,
        &text,
        if selected { GREEN } else { FG },
        right,
    );
}

fn render_divider(buffer: &mut WireBuffer, x: u16, top: u16, bottom: u16) {
    for y in top..bottom {
        put_char(buffer, x, y, '│', BLUE);
    }
}

fn render_detail(
    buffer: &mut WireBuffer,
    left: u16,
    right: u16,
    top: u16,
    bottom: u16,
    state: &FleetPaneState,
) {
    let Some(session) = state.selected_session() else {
        put_str(buffer, left, top, "No selection", MUTED, right);
        return;
    };
    let mut lines = vec![
        ("Session", session.session_name()),
        ("Key", session.session_key.clone()),
        ("Provider", session.provider.clone()),
        (
            "State",
            format!("{} / {}", session.lifecycle_state, session.attention_state),
        ),
    ];
    if let Some(request) = &session.current_request {
        lines.extend(request_detail_lines(request));
    }
    if session.attention_state.eq_ignore_ascii_case("ASK") {
        lines.push(("Action", "Enter answer".into()));
    } else if session.attention_state.eq_ignore_ascii_case("APPROVAL") {
        lines.push(("Action", "y approve, n deny".into()));
    }
    lines.push((
        "Keys",
        "p prompt, t start, i interrupt, s stop, r restart, A attach".into(),
    ));
    lines.extend([
        ("Mode", session.management_state.clone()),
        ("Source", session.provenance.clone()),
        ("Transport", session.transport_health.clone()),
        ("Confidence", session.confidence.clone()),
        ("Version", session.version.to_string()),
        ("Cwd", session.cwd.clone()),
        (
            "Tmux",
            session.tmux_target.clone().unwrap_or_else(|| "none".into()),
        ),
        ("Capabilities", session.capabilities.labels().join(", ")),
    ]);
    let mut y = top;
    let detail_width = usize::from(right.saturating_sub(left).saturating_sub(1)).max(1);
    for (label, value) in lines {
        let prefix = format!("{label}: ");
        let continuation = " ".repeat(prefix.chars().count());
        for (index, part) in wrap_text(&value, detail_width.saturating_sub(prefix.len()).max(1))
            .into_iter()
            .enumerate()
        {
            if y >= bottom {
                break;
            }
            let line_prefix = if index == 0 { &prefix } else { &continuation };
            put_str(buffer, left, y, &format!("{line_prefix}{part}"), FG, right);
            y = y.saturating_add(1);
        }
    }
    if let Some(feedback) = &state.feedback {
        if bottom > top {
            put_str(buffer, left, bottom - 1, feedback, GOLD, right);
        }
    }
}

fn request_detail_lines(request: &serde_json::Value) -> Vec<(&'static str, String)> {
    let questions = request
        .pointer("/payload/questions")
        .or_else(|| request.pointer("/payload/tool_input/questions"))
        .or_else(|| request.get("questions"))
        .or_else(|| request.pointer("/params/questions"))
        .or_else(|| request.pointer("/tool_input/questions"))
        .or_else(|| request.pointer("/request/questions"))
        .and_then(serde_json::Value::as_array);
    let Some(questions) = questions else {
        return vec![("Request", request.to_string())];
    };
    let mut lines = Vec::new();
    for (question_index, question) in questions.iter().enumerate() {
        let header =
            question.get("header").and_then(serde_json::Value::as_str).unwrap_or("Question");
        let text = question
            .get("question")
            .or_else(|| question.get("text"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        lines.push((
            "Question",
            format!("{} {header}: {text}", question_index + 1),
        ));
        if let Some(options) = question.get("options").and_then(serde_json::Value::as_array) {
            for (option_index, option) in options.iter().enumerate() {
                let (label, description) = option.as_str().map_or_else(
                    || {
                        (
                            option
                                .get("label")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default(),
                            option
                                .get("description")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default(),
                        )
                    },
                    |label| (label, ""),
                );
                let separator = if description.is_empty() { "" } else { ": " };
                lines.push((
                    "Option",
                    format!("{}. {label}{separator}{description}", option_index + 1),
                ));
            }
        }
    }
    lines
}

fn wrap_text(value: &str, width: usize) -> Vec<String> {
    if value.is_empty() {
        return vec![String::new()];
    }
    let characters: Vec<char> = value.chars().collect();
    characters.chunks(width).map(|chunk| chunk.iter().collect()).collect()
}

fn render_mode(
    buffer: &mut WireBuffer,
    area_width: u16,
    top: u16,
    bottom: u16,
    state: &FleetPaneState,
) {
    match &state.mode {
        FleetMode::Browse => {}
        FleetMode::Answer(answer) => {
            let question = &answer.questions[answer.question_index];
            let mut lines = vec![
                format!(
                    "Question {}/{}: {}",
                    answer.question_index + 1,
                    answer.questions.len(),
                    question.text
                ),
                if answer.editing_text {
                    "Type answer, Enter advances".into()
                } else if question.multi_select {
                    "Space toggles, Enter advances".into()
                } else {
                    "Up/Down selects, Enter advances, o enters Other text".into()
                },
            ];
            if answer.editing_text {
                lines.push(format!("> {}", answer.texts[answer.question_index]));
            } else {
                for (index, option) in question.options.iter().enumerate() {
                    let cursor = if index == answer.option_cursor {
                        '>'
                    } else {
                        ' '
                    };
                    let mark = if answer.selections[answer.question_index].contains(&index) {
                        'x'
                    } else {
                        ' '
                    };
                    lines.push(format!("{cursor}[{mark}] {option}"));
                }
            }
            render_modal(buffer, area_width, top, bottom, "Structured answer", &lines);
        }
        FleetMode::Start(start) => render_modal(
            buffer,
            area_width,
            top,
            bottom,
            "Start managed session",
            &[
                "Provider: Codex".into(),
                match start.stage {
                    StartStage::Cwd => format!("Working directory: > {}", start.cwd),
                    StartStage::Prompt => format!("Working directory: {}", start.cwd),
                },
                match start.stage {
                    StartStage::Cwd => "Enter continues to optional prompt".into(),
                    StartStage::Prompt => format!("Optional prompt: > {}", start.prompt),
                },
                match start.stage {
                    StartStage::Cwd => "Esc cancels".into(),
                    StartStage::Prompt => "Enter starts, Esc cancels".into(),
                },
            ],
        ),
        FleetMode::Prompt { text } => render_modal(
            buffer,
            area_width,
            top,
            bottom,
            "Send prompt",
            &[
                format!("> {text}"),
                "Enter sends through fleet/action, Esc cancels".into(),
            ],
        ),
        FleetMode::Confirm {
            session_key,
            action,
        } => render_modal(
            buffer,
            area_width,
            top,
            bottom,
            "Confirm action",
            &[
                format!("{action:?}"),
                session_key.clone(),
                "Enter confirms, Esc cancels".into(),
            ],
        ),
        FleetMode::TypedConfirm {
            expected_name,
            typed,
            action,
            ..
        } => render_modal(
            buffer,
            area_width,
            top,
            bottom,
            "Typed confirmation",
            &[
                format!("{action:?}"),
                format!("Type: {expected_name}"),
                format!("> {typed}"),
            ],
        ),
        FleetMode::Broadcast(broadcast) => {
            render_broadcast_modal(buffer, area_width, top, bottom, state, broadcast)
        }
    }
}

fn render_broadcast_modal(
    buffer: &mut WireBuffer,
    area_width: u16,
    top: u16,
    bottom: u16,
    state: &FleetPaneState,
    broadcast: &BroadcastState,
) {
    let mut lines = Vec::new();
    match broadcast.stage {
        BroadcastStage::Compose => {
            lines.push("Compose message".into());
            lines.push(format!("> {}", broadcast.text));
            lines.push("Enter chooses recipients".into());
        }
        BroadcastStage::Recipients => {
            lines.push(format!("Message: {}", broadcast.text));
            let candidates = broadcast_candidate_keys(state, broadcast.expanded_roster);
            for (index, key) in candidates.iter().enumerate().take(8) {
                let cursor = if index == broadcast.cursor { '>' } else { ' ' };
                let mark = if broadcast.selected.contains(key) {
                    'x'
                } else {
                    ' '
                };
                lines.push(format!("{cursor}[{mark}] {key}"));
            }
            lines.push("Space toggle, a all visible, e full roster".into());
        }
        BroadcastStage::Confirm => {
            lines.push(format!("Message: {}", broadcast.text));
            lines.push(format!("Recipients: {}", broadcast.selected.len()));
            lines.extend(broadcast.selected.iter().take(6).cloned());
            lines.push("Enter sends, Esc cancels".into());
        }
        BroadcastStage::InFlight => {
            lines.push(format!("Message: {}", broadcast.text));
            lines.push(format!("Recipients: {}", broadcast.selected.len()));
            lines.push("Sending, waiting for receipts".into());
        }
        BroadcastStage::Receipts => {
            for receipt in broadcast.receipts.values().take(8) {
                lines.push(format!("{:?}  {}", receipt.status, receipt.session_key));
            }
            lines.push("Space selects failed, r retries selected failures".into());
        }
    }
    render_modal(buffer, area_width, top, bottom, "Broadcast", &lines);
}

fn render_modal(
    buffer: &mut WireBuffer,
    area_width: u16,
    top: u16,
    bottom: u16,
    title: &str,
    lines: &[String],
) {
    let available_height = bottom.saturating_sub(top);
    if available_height < 4 || area_width < 20 {
        return;
    }
    let width = area_width.saturating_sub(8).clamp(20, 72);
    let height = (lines.len() as u16 + 2).clamp(4, available_height);
    let left = (area_width.saturating_sub(width)) / 2;
    let modal_top = top + available_height.saturating_sub(height) / 2;
    let right = left + width;
    let modal_bottom = modal_top + height;
    for x in left..right {
        put_char(buffer, x, modal_top, '─', BLUE);
        put_char(buffer, x, modal_bottom - 1, '─', BLUE);
    }
    for y in modal_top..modal_bottom {
        put_char(buffer, left, y, '│', BLUE);
        put_char(buffer, right - 1, y, '│', BLUE);
    }
    put_char(buffer, left, modal_top, '┌', BLUE);
    put_char(buffer, right - 1, modal_top, '┐', BLUE);
    put_char(buffer, left, modal_bottom - 1, '└', BLUE);
    put_char(buffer, right - 1, modal_bottom - 1, '┘', BLUE);
    put_str(
        buffer,
        left + 2,
        modal_top,
        &format!(" {title} "),
        GOLD,
        right - 1,
    );
    for (index, line) in lines.iter().enumerate() {
        let y = modal_top + 1 + index as u16;
        if y >= modal_bottom - 1 {
            break;
        }
        put_str(buffer, left + 2, y, line, FG, right - 2);
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn format_age(now_ms: i64, observed_ms: i64) -> String {
    if now_ms <= 0 || observed_ms <= 0 || observed_ms > now_ms {
        return "?".into();
    }
    let seconds = (now_ms - observed_ms) / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3600)
    }
}

fn put_str(buffer: &mut WireBuffer, x: u16, row: u16, value: &str, color: Color, right: u16) {
    let mut column = x;
    for raw_character in value.chars() {
        if column >= right {
            break;
        }
        let character = if raw_character.is_control() {
            '�'
        } else {
            raw_character
        };
        let mut cell = Cell::new(character.to_string());
        cell.fg = Some(color);
        buffer.push(Coord::new(column, row), cell);
        column = column.saturating_add(1);
    }
}

fn put_char(buffer: &mut WireBuffer, x: u16, row: u16, character: char, color: Color) {
    let mut cell = Cell::new(character.to_string());
    cell.fg = Some(color);
    buffer.push(Coord::new(x, row), cell);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(
        key: &str,
        provider: &str,
        lifecycle: &str,
        attention: &str,
        management: &str,
    ) -> FleetSessionRow {
        FleetSessionRow {
            session_key: key.into(),
            provider: provider.into(),
            provider_session_id: Some(format!("provider-{key}")),
            current_request_fingerprint: None,
            current_request: None,
            lifecycle_state: lifecycle.into(),
            attention_state: attention.into(),
            management_state: management.into(),
            provenance: "hangar-authoritative".into(),
            confidence: "authoritative".into(),
            transport_health: "healthy".into(),
            capabilities: FleetCapabilities::List(
                [
                    "structured_answer",
                    "approvals",
                    "send_prompt",
                    "verified_picker",
                    "tmux_attach",
                    "continue_turn",
                    "retry",
                    "interrupt",
                    "start",
                    "stop",
                    "restart",
                    "kill",
                    "archive",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            version: 7,
            cwd: format!("/work/{key}"),
            tmux_target: Some(format!("{key}:0.0")),
            display_name: Some(key.into()),
            discovered_at: 1_000,
            last_observed_at: 9_000,
            metadata_updated_at: 9_000,
            lifecycle_updated_at: 9_000,
            attention_updated_at: 9_000,
            transport_updated_at: 9_000,
        }
    }

    fn roster() -> Vec<FleetSessionRow> {
        vec![
            session("claude:ask", "claude", "IDLE", "ASK", "managed"),
            session("codex:run", "codex", "RUNNING", "NONE", "managed"),
            session("legacy:wait", "claude", "UNKNOWN", "WAITING", "degraded"),
        ]
    }

    fn state_with_roster() -> FleetPaneState {
        let mut state = FleetPaneState::default();
        state.set_sessions(roster());
        state
    }

    fn apply(state: &FleetPaneState, event: FleetEvent) -> FleetReduction {
        reduce_fleet(state, event)
    }

    fn type_text(mut state: FleetPaneState, text: &str) -> FleetPaneState {
        for character in text.chars() {
            state = apply(&state, FleetEvent::Key(FleetKey::Char(character))).state;
        }
        state
    }

    fn row_text(buffer: &WireBuffer, row: u16, width: u16) -> String {
        let mut output = String::new();
        for x in 0..width {
            let character = buffer
                .cells
                .iter()
                .find(|(coord, _)| coord.x == x && coord.y == row)
                .map_or(' ', |(_, cell)| cell.symbol.chars().next().unwrap_or(' '));
            output.push(character);
        }
        output.trim_end().to_string()
    }

    #[test]
    fn focus_is_default_and_excludes_running() {
        let state = state_with_roster();
        let keys: Vec<_> =
            state.visible_sessions().iter().map(|row| row.session_key.as_str()).collect();
        assert_eq!(state.filter(), FleetFilter::Focus);
        assert_eq!(keys, ["claude:ask", "legacy:wait"]);
    }

    #[test]
    fn selection_survives_snapshot_reorder_by_session_key() {
        let state = state_with_roster();
        let state = apply(&state, FleetEvent::Key(FleetKey::Down)).state;
        assert_eq!(state.selected_key(), Some("legacy:wait"));

        let mut reordered = roster();
        reordered.reverse();
        let state = apply(&state, FleetEvent::Snapshot(reordered)).state;
        assert_eq!(state.selected_key(), Some("legacy:wait"));
    }

    #[test]
    fn every_filter_selects_expected_rows() {
        let state = state_with_roster();
        let cases = [
            (FleetFilter::Actionable, vec!["claude:ask", "legacy:wait"]),
            (FleetFilter::Managed, vec!["claude:ask", "codex:run"]),
            (FleetFilter::Degraded, vec!["legacy:wait"]),
            (FleetFilter::Claude, vec!["claude:ask", "legacy:wait"]),
            (FleetFilter::Codex, vec!["codex:run"]),
            (
                FleetFilter::All,
                vec!["claude:ask", "codex:run", "legacy:wait"],
            ),
        ];
        for (filter, expected) in cases {
            let filtered = apply(&state, FleetEvent::SetFilter(filter)).state;
            let actual: Vec<_> =
                filtered.visible_sessions().iter().map(|row| row.session_key.as_str()).collect();
            assert_eq!(actual, expected, "filter {filter:?}");
        }
    }

    #[test]
    fn successful_action_advances_to_next_focus_row() {
        let state = state_with_roster();
        assert_eq!(state.selected_key(), Some("claude:ask"));
        let state = apply(
            &state,
            FleetEvent::ActionSucceeded {
                session_key: "claude:ask".into(),
            },
        )
        .state;
        assert_eq!(state.filter(), FleetFilter::Focus);
        assert_eq!(state.selected_key(), Some("legacy:wait"));
    }

    #[test]
    fn right_and_uppercase_a_emit_exact_attach_intents() {
        let state = state_with_roster();
        assert_eq!(
            apply(&state, FleetEvent::Key(FleetKey::Right)).intent,
            Some(FleetIntent::AttachEmbedded {
                session_key: "claude:ask".into(),
                tmux_target: "claude:ask:0.0".into(),
            })
        );
        assert_eq!(
            apply(&state, FleetEvent::Key(FleetKey::Char('A'))).intent,
            Some(FleetIntent::AttachFullscreen {
                session_key: "claude:ask".into(),
                tmux_target: "claude:ask:0.0".into(),
            })
        );
    }

    #[test]
    fn degraded_gates_structured_and_destructive_but_allows_safe_fallbacks() {
        let mut state = state_with_roster();
        state.selected_key = Some("legacy:wait".into());
        let structured = apply(
            &state,
            FleetEvent::RequestAction(FleetAction::StructuredAnswer {
                request_fingerprint: "req".into(),
                request_identity: None,
                answers: vec![ainb_hangar_proto::fleet::FleetQuestionAnswer {
                    question_id: "q1".into(),
                    selected_options: vec!["yes".into()],
                    text: None,
                }],
            }),
        );
        assert!(structured.intent.is_none());
        assert!(structured.state.feedback().is_some_and(|message| message.contains("degraded")));
        assert!(apply(&state, FleetEvent::RequestAction(FleetAction::Kill)).intent.is_none());
        assert!(matches!(
            apply(
                &state,
                FleetEvent::RequestAction(FleetAction::SendText {
                    text: "ping".into()
                })
            )
            .intent,
            Some(FleetIntent::Execute {
                action: FleetAction::SendText { .. },
                ..
            })
        ));
        assert!(matches!(
            apply(
                &state,
                FleetEvent::RequestAction(FleetAction::VerifiedPicker {
                    request_fingerprint: "req".into(),
                    key: "1".into(),
                })
            )
            .intent,
            Some(FleetIntent::Execute {
                action: FleetAction::VerifiedPicker { .. },
                ..
            })
        ));
    }

    #[test]
    fn enter_collects_complete_multi_question_structured_answer() {
        let mut row = session("claude:ask", "claude", "IDLE", "ASK", "managed");
        row.current_request_fingerprint = Some("fingerprint-1".into());
        row.current_request = Some(serde_json::json!({
            "tool_use_id": "tool-1",
            "questions": [
                {
                    "id": "tools",
                    "question": "Pick tools",
                    "multiSelect": true,
                    "options": [
                        {"label": "rg"},
                        {"label": "ast-grep"}
                    ]
                },
                {
                    "id": "ship",
                    "question": "Ship?",
                    "options": [
                        {"label": "No"},
                        {"label": "Yes"}
                    ]
                }
            ]
        }));
        let mut state = FleetPaneState::default();
        state.set_sessions(vec![row]);

        state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        state = apply(&state, FleetEvent::Key(FleetKey::Space)).state;
        state = apply(&state, FleetEvent::Key(FleetKey::Down)).state;
        state = apply(&state, FleetEvent::Key(FleetKey::Space)).state;
        state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        state = apply(&state, FleetEvent::Key(FleetKey::Down)).state;
        let submitted = apply(&state, FleetEvent::Key(FleetKey::Enter));

        let Some(FleetIntent::Execute {
            session_key,
            expected_version,
            action:
                FleetAction::StructuredAnswer {
                    request_fingerprint,
                    request_identity,
                    answers,
                },
        }) = submitted.intent
        else {
            panic!("final question must emit structured Fleet action");
        };
        assert_eq!(session_key, "claude:ask");
        assert_eq!(expected_version, 7);
        assert_eq!(request_fingerprint, "fingerprint-1");
        assert_eq!(
            request_identity.unwrap().request_id,
            serde_json::json!("tool-1")
        );
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].question_id, "tools");
        assert_eq!(answers[0].selected_options, ["rg", "ast-grep"]);
        assert_eq!(answers[1].question_id, "ship");
        assert_eq!(answers[1].selected_options, ["Yes"]);
    }

    #[test]
    fn prompt_composer_emits_exact_text_through_versioned_action() {
        let state = state_with_roster();
        let mut state = apply(&state, FleetEvent::Key(FleetKey::Char('p'))).state;
        state = type_text(state, "status now");
        let submitted = apply(&state, FleetEvent::Key(FleetKey::Enter));
        assert_eq!(
            submitted.intent,
            Some(FleetIntent::Execute {
                session_key: "claude:ask".into(),
                expected_version: 7,
                action: FleetAction::SendText {
                    text: "status now".into(),
                },
            })
        );
    }

    #[test]
    fn empty_roster_starts_managed_codex_with_exact_cwd_and_optional_prompt() {
        let state = FleetPaneState::default();
        let mut state = apply(&state, FleetEvent::Key(FleetKey::Char('t'))).state;
        state = type_text(state, " /work/new ");
        state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        state = type_text(state, " inspect failures ");
        let submitted = apply(&state, FleetEvent::Key(FleetKey::Enter));
        assert_eq!(
            submitted.intent,
            Some(FleetIntent::Start {
                provider: ainb_hangar_proto::fleet::FleetProvider::Codex,
                cwd: "/work/new".into(),
                prompt: Some("inspect failures".into()),
            })
        );

        let mut state = apply(
            &FleetPaneState::default(),
            FleetEvent::Key(FleetKey::Char('t')),
        )
        .state;
        state = type_text(state, "/work/no-prompt");
        state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        let submitted = apply(&state, FleetEvent::Key(FleetKey::Enter));
        assert_eq!(
            submitted.intent,
            Some(FleetIntent::Start {
                provider: ainb_hangar_proto::fleet::FleetProvider::Codex,
                cwd: "/work/no-prompt".into(),
                prompt: None,
            })
        );
    }

    #[test]
    fn structured_answer_preserves_nested_codex_identity_free_text_and_other() {
        let mut row = session("codex:ask", "codex", "IDLE", "ASK", "managed");
        row.current_request_fingerprint = Some("codex-fingerprint".into());
        row.current_request = Some(serde_json::json!({
            "payload": {
                "identity": {
                    "requestId": 73,
                    "threadId": "thread-1",
                    "turnId": "turn-2",
                    "itemId": "item-3"
                },
                "questions": [
                    {
                        "id": "free-form-question",
                        "question": "What failed?",
                        "options": []
                    },
                    {
                        "id": "deployment-question",
                        "question": "Where next?",
                        "options": [
                            {"label": "Other"},
                            {"label": "Production"}
                        ]
                    }
                ]
            }
        }));
        let mut state = FleetPaneState::default();
        state.set_sessions(vec![row]);

        state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        state = type_text(state, "timeout");
        state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        state = type_text(state, "staging-east");
        let submitted = apply(&state, FleetEvent::Key(FleetKey::Enter));

        let Some(FleetIntent::Execute {
            action:
                FleetAction::StructuredAnswer {
                    request_identity: Some(identity),
                    answers,
                    ..
                },
            ..
        }) = submitted.intent
        else {
            panic!("structured answer intent expected");
        };
        assert_eq!(identity.request_id, serde_json::json!(73));
        assert_eq!(identity.thread_id, "thread-1");
        assert_eq!(identity.turn_id, "turn-2");
        assert_eq!(identity.item_id, "item-3");
        assert_eq!(answers[0].question_id, "free-form-question");
        assert!(answers[0].selected_options.is_empty());
        assert_eq!(answers[0].text.as_deref(), Some("timeout"));
        assert_eq!(answers[1].question_id, "deployment-question");
        assert!(
            answers[1].selected_options.is_empty(),
            "single-select Other text is the sole answer"
        );
        assert_eq!(answers[1].text.as_deref(), Some("staging-east"));
    }

    #[test]
    fn approval_binding_preserves_exact_request_identity() {
        let mut row = session("claude:approve", "claude", "IDLE", "APPROVAL", "managed");
        row.current_request_fingerprint = Some("approve-fp".into());
        row.current_request = Some(serde_json::json!({
            "tool_use_id": "permission-1",
            "thread_id": "thread-1",
            "turn_id": "turn-1",
            "item_id": "item-1"
        }));
        let mut state = FleetPaneState::default();
        state.set_sessions(vec![row]);
        assert_eq!(
            selected_approval_action(&state, true),
            Ok(FleetAction::Approve {
                request_fingerprint: "approve-fp".into(),
                request_identity: Some(ainb_hangar_proto::fleet::FleetRequestIdentity {
                    request_id: serde_json::json!("permission-1"),
                    thread_id: "thread-1".into(),
                    turn_id: "turn-1".into(),
                    item_id: "item-1".into(),
                }),
            })
        );
    }

    #[test]
    fn stop_and_restart_require_explicit_confirmation() {
        let state = state_with_roster();
        for action in [FleetAction::Stop, FleetAction::Restart] {
            let pending = apply(&state, FleetEvent::RequestAction(action.clone()));
            assert!(pending.intent.is_none());
            assert!(matches!(pending.state.mode, FleetMode::Confirm { .. }));
            let confirmed = apply(&pending.state, FleetEvent::Key(FleetKey::Enter));
            assert_eq!(
                confirmed.intent,
                Some(FleetIntent::Execute {
                    session_key: "claude:ask".into(),
                    expected_version: 7,
                    action,
                })
            );
        }
    }

    #[test]
    fn kill_and_archive_require_exact_typed_session_name() {
        let state = state_with_roster();
        for action in [FleetAction::Kill, FleetAction::Archive] {
            let pending = apply(&state, FleetEvent::RequestAction(action.clone()));
            let wrong = type_text(pending.state, "wrong");
            let wrong = apply(&wrong, FleetEvent::Key(FleetKey::Enter));
            assert!(wrong.intent.is_none());

            let pending = apply(&state, FleetEvent::RequestAction(action.clone()));
            let exact = type_text(pending.state, "claude:ask");
            let exact = apply(&exact, FleetEvent::Key(FleetKey::Enter));
            assert_eq!(
                exact.intent,
                Some(FleetIntent::Execute {
                    session_key: "claude:ask".into(),
                    expected_version: 7,
                    action,
                })
            );
        }
    }

    #[test]
    fn broadcast_supports_composer_toggle_visible_all_expand_preview_and_bound() {
        let state = state_with_roster();
        let state = apply(&state, FleetEvent::Key(FleetKey::Char('b'))).state;
        let state = type_text(state, "ship it");
        let state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        let state = apply(&state, FleetEvent::Key(FleetKey::Space)).state;
        let FleetMode::Broadcast(broadcast) = &state.mode else {
            panic!("broadcast modal expected");
        };
        assert_eq!(broadcast.selected.len(), 1);

        let state = apply(&state, FleetEvent::Key(FleetKey::Char('a'))).state;
        let FleetMode::Broadcast(broadcast) = &state.mode else {
            panic!("broadcast modal expected");
        };
        assert_eq!(broadcast.selected.len(), 2, "all visible Focus rows");

        let state = apply(&state, FleetEvent::Key(FleetKey::Char('e'))).state;
        let state = apply(&state, FleetEvent::Key(FleetKey::Char('a'))).state;
        let state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        let sent = apply(&state, FleetEvent::Key(FleetKey::Enter));
        let Some(FleetIntent::Broadcast {
            text,
            recipient_keys,
            idempotency_key,
            max_parallel,
            retry_failures_only,
        }) = &sent.intent
        else {
            panic!("broadcast intent expected");
        };
        assert_eq!(text, "ship it");
        assert_eq!(
            recipient_keys,
            &vec![
                "claude:ask".to_string(),
                "codex:run".to_string(),
                "legacy:wait".to_string(),
            ]
        );
        assert!(idempotency_key.starts_with("fleet-broadcast-"));
        assert_eq!(*max_parallel, 8);
        assert!(!*retry_failures_only);

        let repeated = apply(&sent.state, FleetEvent::Key(FleetKey::Enter));
        assert!(repeated.intent.is_none(), "repeat Enter must not dispatch");
        let FleetMode::Broadcast(broadcast) = &repeated.state.mode else {
            panic!("in-flight broadcast expected");
        };
        assert_eq!(broadcast.stage, BroadcastStage::InFlight);
        assert_eq!(
            broadcast.in_flight_idempotency_key.as_deref(),
            Some(idempotency_key.as_str()),
            "one idempotency key must survive until receipts"
        );

        let cannot_close = apply(&repeated.state, FleetEvent::Key(FleetKey::Esc));
        assert!(cannot_close.intent.is_none());
        assert!(matches!(
            &cannot_close.state.mode,
            FleetMode::Broadcast(BroadcastState {
                stage: BroadcastStage::InFlight,
                ..
            })
        ));

        let failed = apply(
            &cannot_close.state,
            FleetEvent::BroadcastFailed {
                detail: "socket closed".into(),
            },
        );
        let FleetMode::Broadcast(broadcast) = &failed.state.mode else {
            panic!("confirmation must be restored after initial failure");
        };
        assert_eq!(broadcast.stage, BroadcastStage::Confirm);
        assert!(broadcast.in_flight_idempotency_key.is_none());
        assert!(broadcast.failure_return_stage.is_none());
        assert_eq!(broadcast.selected.len(), 3);
        assert!(broadcast.receipts.is_empty());
        assert_eq!(
            failed.state.feedback(),
            Some("broadcast failed: socket closed")
        );
        let cancelled = apply(&failed.state, FleetEvent::Key(FleetKey::Esc));
        assert!(matches!(cancelled.state.mode, FleetMode::Browse));

        let resent = apply(&failed.state, FleetEvent::Key(FleetKey::Enter));
        let Some(FleetIntent::Broadcast {
            idempotency_key: resent_key,
            ..
        }) = &resent.intent
        else {
            panic!("restored confirmation must resend");
        };
        assert_ne!(resent_key, idempotency_key, "failed attempt key must clear");
        let settled = apply(
            &resent.state,
            FleetEvent::BroadcastReceipts(vec![BroadcastReceipt {
                session_key: "claude:ask".into(),
                status: ReceiptStatus::Delivered,
                detail: None,
            }]),
        );
        let FleetMode::Broadcast(broadcast) = &settled.state.mode else {
            panic!("receipt view expected");
        };
        assert_eq!(broadcast.stage, BroadcastStage::Receipts);
        assert!(broadcast.in_flight_idempotency_key.is_none());
    }

    #[test]
    fn receipts_preserve_three_outcomes_and_retry_selected_failures_only() {
        let state = state_with_roster();
        let state = apply(&state, FleetEvent::Key(FleetKey::Char('b'))).state;
        let state = type_text(state, "retry me");
        let state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        let state = apply(&state, FleetEvent::Key(FleetKey::Char('e'))).state;
        let state = apply(&state, FleetEvent::Key(FleetKey::Char('a'))).state;
        let state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        let state = apply(&state, FleetEvent::Key(FleetKey::Enter)).state;
        let state = apply(
            &state,
            FleetEvent::BroadcastReceipts(vec![
                BroadcastReceipt {
                    session_key: "claude:ask".into(),
                    status: ReceiptStatus::Failed,
                    detail: None,
                },
                BroadcastReceipt {
                    session_key: "codex:run".into(),
                    status: ReceiptStatus::Unknown,
                    detail: None,
                },
                BroadcastReceipt {
                    session_key: "legacy:wait".into(),
                    status: ReceiptStatus::Failed,
                    detail: None,
                },
                BroadcastReceipt {
                    session_key: "finished".into(),
                    status: ReceiptStatus::Delivered,
                    detail: None,
                },
            ]),
        )
        .state;
        let FleetMode::Broadcast(broadcast) = &state.mode else {
            panic!("receipt view expected");
        };
        assert_eq!(broadcast.receipts.len(), 4);
        assert!(
            broadcast
                .receipts
                .values()
                .any(|receipt| receipt.status == ReceiptStatus::Delivered)
        );
        assert!(
            broadcast
                .receipts
                .values()
                .any(|receipt| receipt.status == ReceiptStatus::Unknown)
        );
        let mut state = state;
        let FleetMode::Broadcast(broadcast) = &mut state.mode else {
            panic!("receipt view expected");
        };
        broadcast.selected.clear();
        broadcast.selected.insert("legacy:wait".into());
        let retry = apply(&state, FleetEvent::Key(FleetKey::Char('r')));
        let Some(FleetIntent::Broadcast {
            text,
            recipient_keys,
            idempotency_key,
            max_parallel,
            retry_failures_only,
        }) = &retry.intent
        else {
            panic!("retry broadcast intent expected");
        };
        assert_eq!(text, "retry me");
        assert_eq!(recipient_keys, &vec!["legacy:wait".to_string()]);
        assert!(idempotency_key.starts_with("fleet-broadcast-"));
        assert_eq!(*max_parallel, 8);
        assert!(*retry_failures_only);

        let repeated = apply(&retry.state, FleetEvent::Key(FleetKey::Char('r')));
        assert!(repeated.intent.is_none(), "repeat retry must not dispatch");
        let FleetMode::Broadcast(broadcast) = &repeated.state.mode else {
            panic!("in-flight retry expected");
        };
        assert_eq!(
            broadcast.in_flight_idempotency_key.as_deref(),
            Some(idempotency_key.as_str())
        );

        let failed = apply(
            &repeated.state,
            FleetEvent::BroadcastFailed {
                detail: "daemon unavailable".into(),
            },
        );
        let FleetMode::Broadcast(broadcast) = &failed.state.mode else {
            panic!("receipts must be restored after retry failure");
        };
        assert_eq!(broadcast.stage, BroadcastStage::Receipts);
        assert!(broadcast.in_flight_idempotency_key.is_none());
        assert!(broadcast.failure_return_stage.is_none());
        assert_eq!(broadcast.receipts.len(), 4);
        assert_eq!(
            broadcast.selected,
            BTreeSet::from(["legacy:wait".to_string()])
        );

        let redispatched = apply(&failed.state, FleetEvent::Key(FleetKey::Char('r')));
        let Some(FleetIntent::Broadcast {
            idempotency_key: redispatched_key,
            ..
        }) = &redispatched.intent
        else {
            panic!("restored retry view must retry");
        };
        assert_ne!(
            redispatched_key, idempotency_key,
            "failed retry key must clear"
        );
        let merged = apply(
            &redispatched.state,
            FleetEvent::BroadcastReceipts(vec![BroadcastReceipt {
                session_key: "legacy:wait".into(),
                status: ReceiptStatus::Delivered,
                detail: Some("retry delivered".into()),
            }]),
        )
        .state;
        let FleetMode::Broadcast(broadcast) = &merged.mode else {
            panic!("merged receipt view expected");
        };
        assert_eq!(broadcast.receipts.len(), 4, "retry must merge subset");
        assert_eq!(
            broadcast.receipts["claude:ask"].status,
            ReceiptStatus::Failed,
            "unselected failed receipt must survive retry"
        );
        assert_eq!(
            broadcast.receipts["codex:run"].status,
            ReceiptStatus::Unknown
        );
        assert_eq!(
            broadcast.receipts["finished"].status,
            ReceiptStatus::Delivered
        );
        assert_eq!(
            broadcast.receipts["legacy:wait"].status,
            ReceiptStatus::Delivered
        );
        assert!(
            broadcast.selected.is_empty(),
            "unselected failures must remain unselected after subset retry"
        );
    }

    #[test]
    fn dense_render_contains_table_and_selected_detail() {
        let state = apply(&state_with_roster(), FleetEvent::Tick(10_000)).state;
        let mut buffer = WireBuffer::new(120, 24);
        render_fleet(&mut buffer, 120, 0, 20, &state);
        assert!(row_text(&buffer, 0, 120).contains("Fleet  [Focus]"));
        assert!(row_text(&buffer, 1, 80).contains("SESSION"));
        assert!(row_text(&buffer, 2, 80).contains("CLA"));
        let rendered: String =
            (0..20).map(|row| row_text(&buffer, row, 120)).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("Key: claude:ask"));
        assert!(rendered.contains("State: IDLE / ASK"));
        assert!(rendered.contains("Capabilities:"));
    }

    #[test]
    fn capability_wire_accepts_list_flags_and_json() {
        let list: FleetCapabilities =
            serde_json::from_str(r#"["text_send","tmux_attach"]"#).unwrap();
        let flags: FleetCapabilities =
            serde_json::from_str(r#"{"text_send":true,"kill":false}"#).unwrap();
        let json = FleetCapabilities::Json(r#"{"verified_picker":true}"#.into());
        assert!(list.contains("text_send"));
        assert!(flags.contains("text_send"));
        assert!(!flags.contains("kill"));
        assert!(json.contains("verified_picker"));
    }

    #[test]
    fn proto_snapshot_rows_convert_without_wire_name_drift() {
        use ainb_hangar_proto::fleet::{
            AttentionState, FleetCapabilities as ProtoCapabilities, FleetConfidence,
            FleetProvenance, FleetProvider, FleetSession, LifecycleState, ManagementState,
            TransportHealth,
        };
        let row = FleetSessionRow::from(FleetSession {
            session_key: "codex:thread-1".into(),
            provider: FleetProvider::Codex,
            provider_session_id: Some("thread-1".into()),
            tmux_target: Some("codex-1:0.0".into()),
            process_start_fingerprint: None,
            cwd: "/work/shared".into(),
            display_name: Some("codex-1".into()),
            lifecycle: LifecycleState::Running,
            attention: AttentionState::None,
            current_request_fingerprint: None,
            current_request: Some(serde_json::json!({
                "questions": [{"id": "q1", "text": "Ship?", "options": ["yes", "no"]}]
            })),
            management: ManagementState::Managed,
            transport_health: TransportHealth::Healthy,
            capabilities: ProtoCapabilities {
                interrupt: true,
                tmux_attach: true,
                ..ProtoCapabilities::default()
            },
            provenance: FleetProvenance::Authoritative,
            confidence: FleetConfidence::High,
            discovered_at: 10,
            last_observed_at: 20,
            lifecycle_updated_at: 20,
            attention_updated_at: 10,
            version: 3,
            updated_revision: 4,
        });
        assert_eq!(row.provider, "codex");
        assert_eq!(row.lifecycle_state, "RUNNING");
        assert_eq!(row.management_state, "MANAGED");
        assert!(row.capabilities.contains("interrupt"));
        assert!(row.capabilities.contains("tmux_attach"));
        let request_lines = request_detail_lines(row.current_request.as_ref().unwrap());
        assert!(request_lines.iter().any(|(_, line)| line.contains("Ship?")));
        assert!(request_lines.iter().any(|(_, line)| line.contains("1. yes")));
        assert!(request_lines.iter().any(|(_, line)| line.contains("2. no")));
    }

    #[test]
    fn renderer_replaces_control_characters() {
        let mut state = state_with_roster();
        state.roster[0].cwd = "/work/\u{1b}]52;c;AAAA\u{7}".into();
        let mut buffer = WireBuffer::new(120, 24);
        render_fleet(&mut buffer, 120, 0, 20, &state);
        assert!(!buffer.cells.iter().any(|(_, cell)| cell.symbol.chars().any(char::is_control)));
    }
}
