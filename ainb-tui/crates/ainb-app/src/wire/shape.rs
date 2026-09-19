// ABOUTME: A fully populated sample `AppState` and the locked set of leaf key
// paths its section frames produce. Shared by the leak tests and `ainb doctor`.
//
// A frame only shows what is present: a `None` or an empty `Vec` hides every
// field below it. So the shape check runs against a state where every optional
// screen is open, every list has a row, and every text field the four leak
// checks care about holds a value. Text comes from a [`Seed`], so the same
// builder serves the canary test (unique markers in typed input), the tripwire
// (credential-shaped strings in captured content) and the fixture (plain text).
//
// The builder writes every value explicitly after construction, so a developer
// machine's config, presets and favorites never change the traced shape.

use crate::app::AppState;
use crate::app::versioned::SectionId;
use crate::wire::{section_name, serialize_section, trace};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

/// The committed leaf key paths of every section frame of [`sample_state`].
pub const COMMITTED_KEY_PATHS: &str = include_str!("../../tests/fixtures/section_key_paths.txt");

/// The fixture's path in the repository, for messages an installed binary prints.
pub const COMMITTED_KEY_PATHS_REPO_PATH: &str =
    "ainb-tui/crates/ainb-app/tests/fixtures/section_key_paths.txt";

/// Where [`COMMITTED_KEY_PATHS`] lives in the source tree, for regeneration.
pub const COMMITTED_KEY_PATHS_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/section_key_paths.txt"
);

/// What a sample field is filled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKind {
    /// Something the operator types: a key buffer, a filter, a composer.
    Typed,
    /// Something the app captured: scrollback, a diff, config, agent output.
    Captured,
}

/// Supplies the text for every seeded field, by a stable label.
pub trait Seed {
    fn text(&mut self, label: &'static str, kind: TextKind) -> String;
}

/// Plain, deterministic text: the label itself.
#[derive(Debug, Default)]
pub struct PlainSeed;

impl Seed for PlainSeed {
    fn text(&mut self, label: &'static str, _: TextKind) -> String {
        format!("sample {label}")
    }
}

/// Trace every section frame of every state into one [`trace::Trace`].
///
/// # Panics
///
/// When a frame fails to serialise: that frame would fail on the wire too, and
/// a check over a partial trace would pass on what it never saw.
#[must_use]
pub fn trace_states(states: &[AppState]) -> trace::Trace {
    let mut all = trace::Trace::default();
    for state in states {
        for id in SectionId::ALL {
            let one = trace::trace(section_name(id), &SectionFrame { state, id }).unwrap_or_else(
                |error| panic!("{} frame failed to serialise: {error}", section_name(id)),
            );
            all.fields.extend(one.fields);
            all.strings.extend(one.strings);
            all.leaf_paths.extend(one.leaf_paths);
            frame_envelope_paths(state, id, &mut all.leaf_paths);
        }
        let web = web_snapshot_trace(state);
        all.fields.extend(web.fields);
        all.strings.extend(web.strings);
        all.leaf_paths.extend(web.leaf_paths);
    }
    all
}

/// The web dashboard's session rows (`/api/snapshot`'s `sessions[]`), projected
/// from the Sessions frame by [`crate::wire::web::session_rows`]. Traced like a
/// frame, so the key-path fixture and the name and type deny-lists see the row
/// keys and values: a field the frame withholds cannot come back on the web
/// through a new row key (#1056). The canary and the tripwire build from the
/// frames alone and do not see these rows.
///
/// `needs[]` is traced the same way from a sample daemon card that carries
/// every key the daemon writes, so a card key the allow-list starts passing
/// shows up as a new path (#1081).
fn web_snapshot_trace(state: &AppState) -> trace::Trace {
    #[derive(serde::Serialize)]
    struct WebSnapshotSessions {
        sessions: Vec<crate::wire::web::WebSessionRow>,
        needs: Vec<crate::wire::web::WebNeedCard>,
        cost: Option<crate::wire::web::WebCost>,
    }
    let rows = WebSnapshotSessions {
        sessions: crate::wire::web::session_rows(state),
        needs: sample_web_needs(&mut PlainSeed),
        cost: sample_web_cost(&mut PlainSeed),
    };
    trace::trace("web_snapshot", &rows)
        .unwrap_or_else(|error| panic!("web snapshot rows failed to serialise: {error}"))
}

/// The web cost panel projected from a full sample `ainb fleet cost` report:
/// every section the report writes, including the per-session rows with their
/// absolute `cwd` that the panel must drop (#1113).
pub fn sample_web_cost(seed: &mut dyn Seed) -> Option<crate::wire::web::WebCost> {
    use TextKind::Captured;
    let bucket = serde_json::json!({
        "input_tokens": 10,
        "cache_creation_tokens": 1,
        "cache_read_tokens": 2,
        "output_tokens": 5,
        "reasoning_tokens": 3,
        "call_count": 1,
        "cost_usd": 0.5,
    });
    let report = serde_json::json!({
        "totals": {"cost_usd": 0.5, "session_count": 1, "model_count": 1, "bucket": bucket},
        "sessions": [{
            "session_id": "sample-session",
            "provider": "claude",
            "project": seed.text("cost.project", Captured),
            "cwd": format!("/work/{}", seed.text("cost.cwd", Captured)),
            "group": "sample-group",
            "cost_usd": 0.5,
            "bucket": bucket,
        }],
        "models": [{"model": seed.text("cost.model", Captured), "cost_usd": 0.5, "bucket": bucket}],
        "daily": [{"date": "2026-09-15", "cost_usd": 0.5, "bucket": bucket}],
        "groups": [{
            "group": seed.text("cost.group", Captured),
            "cost_usd": 0.5,
            "session_count": 1,
            "bucket": bucket,
        }],
        "budget_breaches": [],
    });
    crate::wire::web::cost_panel(&report)
}

/// The web needs cards projected from sample daemon cards: every key
/// `ainb-web`'s inbox mapping writes, a fully populated ASK payload with its
/// text drawn from `seed`, and one unparsed payload.
///
/// Public so the canary and the tripwire in `tests/state_serde.rs` run their
/// seeds through the needs projection as they do through the frames (#1081).
pub fn sample_web_needs(seed: &mut dyn Seed) -> Vec<crate::wire::web::WebNeedCard> {
    use TextKind::Captured;
    let cards = serde_json::json!([
        {
            "attentionId": "01J0SAMPLEATTENTION",
            "kind": "ASK",
            "wireKind": "ask_user_question",
            "sessionId": "sample-session",
            "cwd": format!("/work/{}", seed.text("needs.cwd", Captured)),
            "workspaceId": "sample-workspace",
            "degraded": false,
            "createdAt": 1_700_000_000_000_i64,
            "channels": ["web"],
            "sessionKey": "claude:sample-session",
            "state": "waiting",
            "provenance": "hook",
            "tier": 0,
            "evidenceObservedAt": 1_700_000_000_000_i64,
            "hostId": "local",
            "paneUnbound": false,
            "payload": {
                "question": seed.text("needs.question", Captured),
                "options": [seed.text("needs.option", Captured)],
                "text": seed.text("needs.text", Captured),
                "marker": seed.text("needs.marker", Captured),
                "snippet": seed.text("needs.snippet", Captured),
                "pattern": seed.text("needs.pattern", Captured),
                "message": seed.text("needs.message", Captured),
                "tool_input": {"questions": [seed.text("needs.tool_input", Captured)]},
            },
        },
        {"kind": "ERR", "payload": seed.text("needs.unparsed_payload", Captured)},
    ]);
    crate::wire::web::need_cards(&cards)
}

/// The frame around a section body, as it is serialised: `frame.section`,
/// `frame.version`, `frame.epoch`, `frame.host_id` and `frame.daemon_read.*`.
/// The body's own paths are the section's; a field added to the envelope is a
/// new field on the mirror wire just the same.
fn frame_envelope_paths(state: &AppState, id: SectionId, into: &mut BTreeSet<String>) {
    fn leaves(value: &serde_json::Value, path: &str, into: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    leaves(child, &format!("{path}.{key}"), into);
                }
            }
            _ => {
                into.insert(path.to_string());
            }
        }
    }
    // A fixed host: the key paths are the contract, and no process-wide state
    // may decide them (#1066).
    let host = crate::wire::frame::HostId::local();
    let mut envelope = serde_json::to_value(crate::wire::frame::Frame::new(state, id, host))
        .expect("a frame serialises");
    if let Some(map) = envelope.as_object_mut() {
        map.remove("body");
    }
    leaves(&envelope, "frame", into);
}

/// The leaf key paths the frames of `states` produce.
#[must_use]
pub fn key_paths(states: &[AppState]) -> BTreeSet<String> {
    trace_states(states).leaf_paths
}

/// The committed fixture as a set.
#[must_use]
pub fn committed_key_paths() -> BTreeSet<String> {
    parse_key_paths(COMMITTED_KEY_PATHS)
}

/// Parse a key-path fixture: one path per line, `#` comments and blanks ignored.
#[must_use]
pub fn parse_key_paths(text: &str) -> BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Render a key-path set in fixture form.
#[must_use]
pub fn render_key_paths(paths: &BTreeSet<String>) -> String {
    let mut out = String::from(
        "# Leaf key paths of every section frame of `wire::shape::sample_state`.\n\
         # Locked by tests/state_serde.rs (issue #983). A new line here is a new\n\
         # field on the mirror wire: triage it against the four leak checks first.\n\
         # Regenerate: UPDATE_SECTION_KEY_PATHS=1 cargo test -p ainb-app --test state_serde\n",
    );
    for path in paths {
        out.push_str(path);
        out.push('\n');
    }
    out
}

/// The difference between the frames this build produces and the fixture.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ShapeDiff {
    /// On the wire now, not in the fixture.
    pub added: Vec<String>,
    /// In the fixture, no longer on the wire.
    pub removed: Vec<String>,
}

impl ShapeDiff {
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

impl std::fmt::Display for ShapeDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return writeln!(f, "section frame shape matches the committed fixture");
        }
        for path in &self.added {
            writeln!(f, "+ {path}")?;
        }
        for path in &self.removed {
            writeln!(f, "- {path}")?;
        }
        Ok(())
    }
}

/// Compare the current frame shape of the sample state with the fixture.
#[must_use]
pub fn diff_against_committed() -> ShapeDiff {
    diff(
        &key_paths(&sample_states(&mut PlainSeed)),
        &committed_key_paths(),
    )
}

/// Compare two key-path sets.
#[must_use]
pub fn diff(current: &BTreeSet<String>, committed: &BTreeSet<String>) -> ShapeDiff {
    ShapeDiff {
        added: current.difference(committed).cloned().collect(),
        removed: committed.difference(current).cloned().collect(),
    }
}

struct SectionFrame<'a> {
    state: &'a AppState,
    id: SectionId,
}

impl serde::Serialize for SectionFrame<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // The traced key paths are the contract; a fixed host keeps them from
        // depending on who sends the section (#1066).
        serialize_section(
            self.state,
            self.id,
            &crate::wire::frame::HostId::local(),
            serializer,
        )
    }
}

/// Text labels [`sample_state`] seeds as [`TextKind::Typed`], in seeding order.
pub const TYPED_LABELS: &[&str] = &[
    "config.edit_buffer",
    "config.search",
    "config.keychain_popup",
    "onboarding.auth_setup.api_key_input",
    "onboarding.auth_provider_popup.api_key_input",
    "onboarding.auth_pane.key_entry",
    "onboarding.otel_api_token",
    "onboarding.otel_instance_id",
    "onboarding.otel_otlp_endpoint",
    "onboarding.git_directories_input",
    "claude_chat.input_buffer",
    "fleet.ask.free_text",
    "fleet.ask.in_flight_draft",
    "fleet.ask.delivered_via",
    "fleet.broadcast.text",
    "fleet.conversation.composer",
    "git_view.commit_message_input",
    "git_view.quick_commit_message",
    "new_session.pick_repo.filter",
    "new_session.configure.prompt",
    "new_session.configure.branch_edit",
    "skills.input.buffer",
    "skills.search",
    "skills.source_filter",
    "recovery.search_query",
    "logs.history.search_query",
    "logs.history.selected_text",
    "ssh.rename_buffer",
    "tmux.rename_buffer",
    "session_labels.rename_buffer",
    // The plain-text popup variant from `sample_states`.
    "config.text_popup",
    "config.number_popup",
    // Name editors on the Configure form.
    "new_session.configure.branch_prefix_edit",
    "new_session.configure.session_prefix_edit",
    "new_session.configure.save_preset_modal",
];

/// Every sample the checks trace.
///
/// [`sample_state`], whose config popup is the Ctrl+K `SecretInput`, and the
/// same state with a plain setting open in a `TextInput`. The popup holds one
/// variant at a time, so both are needed for either to be seen.
///
/// # Panics
///
/// When the registry has no plain text row, or the reducer stops opening one
/// in a `TextInput`: the variant would silently drop out of every check.
#[must_use]
pub fn sample_states(seed: &mut dyn Seed) -> Vec<AppState> {
    use crate::app::state::ConfigValue;
    use crate::components::config_popup::ConfigPopupType;

    let secret_popup = sample_state(seed);
    let mut text_popup = sample_state(seed);
    let screen = &mut text_popup.config.get_mut().config_screen_state;
    let row = screen
        .settings
        .iter()
        .find_map(|(category, rows)| {
            rows.iter()
                .position(|row| row.key == "web.listen")
                .map(|index| (*category, index))
        })
        .expect("the registry declares web.listen as a text row");
    let value = seed.text("config.text_popup", TextKind::Typed);
    if let Some(ConfigValue::Text(text)) = screen
        .settings
        .get_mut(&row.0)
        .and_then(|rows| rows.get_mut(row.1))
        .map(|setting| &mut setting.value)
    {
        *text = value;
    }
    screen.visible_rows = vec![row];
    screen.selected_setting = 0;
    screen.keychain_target = None;
    crate::app::EventHandler::process_event(
        crate::app::AppEvent::ConfigEditSetting,
        &mut text_popup,
    );
    assert!(
        matches!(
            text_popup.config.config_popup_state.popup_type,
            ConfigPopupType::TextInput { .. }
        ),
        "Enter on a plain text row opens a TextInput"
    );
    let mut states = vec![secret_popup, text_popup];
    for round in 0..ALTERNATE_ROUNDS {
        states.push(alternate_state(seed, round));
    }
    states
}

/// How many [`alternate_state`]s [`sample_states`] builds: the most unseeded
/// variants any one single-valued field has (`ConfirmAction`'s seven).
const ALTERNATE_ROUNDS: usize = 7;

/// The `round`-th of `variants`, the last one once the rounds outrun them.
///
/// Every alternate block picks through here, so which variant repeats is the
/// same rule everywhere, and a block that grows past [`ALTERNATE_ROUNDS`] fails
/// in a debug build instead of silently never seeding its tail.
fn pick<T>(mut variants: Vec<T>, round: usize) -> T {
    debug_assert!(
        variants.len() <= ALTERNATE_ROUNDS,
        "{} variants but only {ALTERNATE_ROUNDS} alternate rounds",
        variants.len()
    );
    let index = round.min(variants.len() - 1);
    variants.swap_remove(index)
}

/// A sample whose single-valued enum fields hold the variants
/// [`sample_state`] does not, one per `round` (#1146).
///
/// The tracer only sees the variant a value holds, so every payload-carrying
/// variant needs some sample that holds it, or no leak check ever reads its
/// payload. A field with fewer variants than rounds repeats its last one.
fn alternate_state(seed: &mut dyn Seed, round: usize) -> AppState {
    use crate::components::config_popup::ConfigPopupType;
    use crate::components::git_view::{MarkdownLine, MarkdownStyle};
    use crate::components::onboarding::state::{AuthAgent, AuthPane, OnboardingFocus};
    use crate::components::skill_manager_screen::{DiscoveryBannerCounts, DiscoveryBannerState};
    use TextKind::{Captured, Typed};

    let mut state = sample_state(seed);

    // ---- config: the popup types the two popup samples do not open ----------
    state.config.get_mut().config_popup_state.popup_type = pick(
        vec![
            ConfigPopupType::Boolean { value: true },
            ConfigPopupType::Choice {
                options: vec!["tmux".to_string(), "docker".to_string()],
                selected_index: 1,
            },
            ConfigPopupType::NumberInput {
                value: 30,
                input_buffer: seed.text("config.number_popup", Typed),
            },
            // Captured as well, so the credential tripwire reads the scrub on
            // the buffer: typed text is only ever checked by the canary.
            ConfigPopupType::NumberInput {
                value: 30,
                input_buffer: seed.text("config.number_popup_pasted", Captured),
            },
        ],
        round,
    );

    // ---- git view: a fenced code block's language line -----------------------
    if let Some(view) = state.git_view.get_mut().git_view_state.as_mut() {
        view.markdown_content.push(MarkdownLine {
            content: "fn main() {}".to_string(),
            style: MarkdownStyle::CodeBlockHeader(
                seed.text("git_view.code_block_language", Captured),
            ),
        });
    }

    // ---- onboarding: the method picker, and focus on a list item -------------
    if let Some(wizard) = state.onboarding.get_mut().onboarding_state.as_mut() {
        wizard.auth_pane = AuthPane::MethodPicker {
            agent: AuthAgent::Claude,
            cursor: 1,
        };
        wizard.focus = OnboardingFocus::Item(2);
    }

    // ---- fleet: every answer route on the chips, and a finished broadcast ----
    {
        use crate::fleet::attention::{Answerable, Unanswerable};
        let answerable = pick(
            vec![
                Answerable::Daemon {
                    attention_id: "a-1".to_string(),
                },
                Answerable::Broker {
                    session_id: "s-1".to_string(),
                },
                Answerable::No(Unanswerable::DaemonGone),
            ],
            round,
        );
        let fleet = state.fleet.get_mut();
        {
            let mut attention =
                fleet.daemon_attention.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let crate::fleet::attention::DaemonAttention {
                by_session_id, all, ..
            } = &mut *attention;
            for chip in by_session_id.values_mut().flatten().chain(all.values_mut()) {
                chip.answerable = answerable.clone();
            }
        }
        let sent = Ok(vec![ainb_hangar_proto::fleet::FleetActionReceipt {
            request_id: "r-1".to_string(),
            session_key: "claude:s-1".to_string(),
            action_kind: "send_prompt".to_string(),
            action_fingerprint: "fp-1".to_string(),
            expected_version: 1,
            idempotency_key: Some("tui-broadcast:sample".to_string()),
            status: ainb_hangar_proto::fleet::ActionReceiptStatus::Delivered,
            detail: Some(seed.text("fleet.broadcast.receipt_detail", Captured)),
            session_version: Some(2),
            created_at: 1,
            updated_at: 2,
        }]);
        let failed = Err(seed.text("fleet.broadcast.failure", Captured));
        fleet.broadcast.publish_outcome(pick(vec![sent, failed], round));
        fleet.broadcast.tick();
        // The conversation's other text-free status, so the leak walk sees the
        // `Opening` leaf as well as the `Unavailable` one the base sample holds.
        fleet.conversation.status = crate::fleet::conversation::ConversationStatus::Opening {
            call: "fleet/channel_create".to_string(),
        };
    }

    // ---- session labels: every attachable ref, as menu target and rename target
    {
        use crate::app::state::{AttachableRef, SessionContextMenu};
        let target = pick(
            vec![
                AttachableRef::WorkspaceSession {
                    workspace_idx: 0,
                    session_idx: 0,
                },
                AttachableRef::WorkspaceShell { workspace_idx: 0 },
                AttachableRef::SshSession { ssh_idx: 0 },
                AttachableRef::OtherTmux { other_idx: 0 },
            ],
            round,
        );
        let labels = state.session_labels.get_mut();
        labels.session_context_menu = Some(SessionContextMenu {
            target: target.clone(),
            selected: 0,
        });
        labels.session_label_rename_target = Some(target);
    }

    // ---- shell: every confirmation the delete sample does not ask ------------
    {
        use crate::app::state::{ConfirmAction, DialogOption};
        let actions = vec![
            ConfirmAction::StopSession(uuid::Uuid::nil()),
            ConfirmAction::BulkDeleteSessions(vec![uuid::Uuid::nil()]),
            ConfirmAction::BulkStopSessions(vec![uuid::Uuid::nil()]),
            ConfirmAction::KillOtherTmux("scratch".to_string()),
            ConfirmAction::KillOtherTmuxSessions(vec!["scratch".to_string()]),
            ConfirmAction::KillWorkspaceShell(0),
            ConfirmAction::McpStopServer("github".to_string()),
        ];
        if let Some(dialog) = state.shell.get_mut().confirmation_dialog.as_mut() {
            dialog.confirm_action = pick(actions.clone(), round);
            dialog.options = Some(
                actions
                    .iter()
                    .map(|action| DialogOption {
                        label: "Confirm".to_string(),
                        action: action.clone(),
                    })
                    .collect(),
            );
        }
    }

    // ---- new session: every repo source, as row, pending clone and target ---
    {
        use crate::components::new_session::pick_repo::{PickRepoRow, RepoRowKind};
        use crate::git::repo_source::RepoSource;
        let mut sources = vec![
            RepoSource::SshUrl(seed.text("new_session.repo_source.ssh_url", Captured)),
            RepoSource::SshSession(seed.text("new_session.repo_source.ssh_session", Captured)),
            RepoSource::GithubShorthand {
                owner: seed.text("new_session.repo_source.owner", Captured),
                repo: seed.text("new_session.repo_source.repo", Captured),
            },
            RepoSource::LocalPath(PathBuf::from("/work/other-repo")),
        ];
        let target = pick(sources.clone(), round);
        sources.push(RepoSource::HttpsUrl(
            seed.text("new_session.repo_source.https_url", Captured),
        ));
        if let Some(new_session) = state.new_session.get_mut().new_session_state.as_mut() {
            if let Some(pick) = new_session.pick_repo_state.as_mut() {
                pick.rows = sources
                    .iter()
                    .enumerate()
                    .map(|(index, source)| PickRepoRow {
                        id: format!("row:{index}"),
                        label: format!("repo {index}"),
                        source: source.clone(),
                        kind: RepoRowKind::Local,
                    })
                    .collect();
                pick.filtered_indices = (0..pick.rows.len()).collect();
                pick.pending_clone_source = Some(target.clone());
            }
            if let Some(configure) = new_session.configure_state.as_mut() {
                configure.repo_source = target;
            }
        }
    }

    // ---- internally tagged payloads (#1146): each variant's own fields ------
    {
        let config = state.config.get_mut();
        let claude_docker: crate::config::ContainerTemplate =
            serde_json::from_value(serde_json::json!({
                "name": "claude-docker",
                "description": "built from the claude-docker Dockerfile",
                "config": {
                    "image_source": {
                        "type": "ClaudeDocker",
                        "base_image": seed.text("config.container.base_image", Captured),
                        "build_args": {
                            "PIP_TOKEN": seed.text("config.container.claude_docker_args", Captured),
                        },
                    },
                },
            }))
            .expect("sample claude-docker template parses");
        config
            .app_config
            .container_templates
            .insert("claude-docker".to_string(), claude_docker);
        for (name, installation) in [
            (
                "from-npm",
                serde_json::json!({
                    "type": "Npm",
                    "package": seed.text("config.mcp.npm_package", Captured),
                    "version": seed.text("config.mcp.npm_version", Captured),
                }),
            ),
            (
                "from-script",
                serde_json::json!({
                    "type": "Custom",
                    "script": seed.text("config.mcp.custom_script", Captured),
                }),
            ),
        ] {
            let server: crate::config::McpServerConfig =
                serde_json::from_value(serde_json::json!({
                    "name": name,
                    "description": "installed by its own installer",
                    "installation": installation,
                    "definition": { "type": "Command", "command": "mcp", "args": [] },
                }))
                .expect("sample mcp server parses");
            config.app_config.mcp_servers.insert(name.to_string(), server);
        }
    }
    if let Some(view) = state.agent_status.get_mut().view.as_mut() {
        view.health = ainb_hangar_proto::status_view::ViewHealth::Stale {
            read_revision: view.read_revision,
            head_revision: view.read_revision + 1,
        };
    }
    if let Some(wizard) = state.onboarding.get_mut().onboarding_state.as_mut() {
        use crate::setup::catalog::{Consumer, DepTier};
        use crate::setup::detect::{DepReport, DepState, SetupStatus, TopicReport};
        let report = |id: &'static str, state: DepState| DepReport {
            id,
            name: id,
            why: "sample dependency",
            tier: DepTier::Recommended,
            consumers: vec![Consumer::Core],
            install_hint: format!("brew install {id}"),
            auto_installable: false,
            satisfied: state.satisfied(),
            state,
        };
        wizard.dependency_status = Some(SetupStatus {
            topics: vec![TopicReport {
                id: "sample",
                label: "Sample",
                description: "every detection outcome",
                deps: vec![
                    report(
                        "ok",
                        DepState::Ok(Some(seed.text("onboarding.dep.version", Captured))),
                    ),
                    report(
                        "alt",
                        DepState::Alt(seed.text("onboarding.dep.alt", Captured)),
                    ),
                    report(
                        "old",
                        DepState::TooOld(seed.text("onboarding.dep.too_old", Captured)),
                    ),
                    report("missing", DepState::Missing),
                    report("unknown", DepState::Unknown),
                ],
            }],
        });
    }

    // ---- skills: the discovery banner, collapsed then expanded ---------------
    let counts = DiscoveryBannerCounts {
        marketplace_plugins: 2,
        orphan_units_total: 3,
        orphan_units_per_tool: vec![("claude".to_string(), 3)],
        conflicts: 1,
    };
    state.skills.get_mut().skill_manager_state.banner = pick(
        vec![
            DiscoveryBannerState::Visible(counts.clone()),
            DiscoveryBannerState::Details(counts),
        ],
        round,
    );

    state
}

/// Build the sample state. Every optional screen is open and every field the
/// leak checks name holds text from `seed`.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn sample_state(seed: &mut dyn Seed) -> AppState {
    use crate::app::state::{
        AuthProviderPopupState, AuthSetupState, ClaudeChatState, ConfigValue, NewSessionState,
        Notification, NotificationType, SecretValue,
    };
    use crate::components::code_review::model::{DiffRow, Hunk, ReviewFile, RowKind};
    use crate::components::git_view::{GitFileStatus, GitViewState, MarkdownLine, MarkdownStyle};
    use crate::components::live_logs_stream::{LogEntry, LogEntryLevel};
    use crate::components::session_recovery::{OrphanType, OrphanedSession, OrphanedWorktree};
    use TextKind::{Captured, Typed};

    let mut state = AppState::new();

    // ---- config ------------------------------------------------------------
    {
        let config = state.config.get_mut();
        config.app_config = crate::config::AppConfig::default();
        config.app_config.fleet.bridge = Some(toml::Value::Table(
            toml::from_str(&format!(
                "[telegram]\ntoken = {:?}\n[slack]\nbot_token = {:?}\n",
                seed.text("config.fleet.bridge.telegram.token", Captured),
                seed.text("config.fleet.bridge.slack.bot_token", Captured),
            ))
            .expect("sample bridge table parses"),
        ));
        let mcp: crate::config::McpServerConfig = serde_json::from_value(serde_json::json!({
            "name": "github",
            "description": "GitHub MCP",
            "installation": { "type": "PreInstalled" },
            "definition": {
                "type": "Command",
                "command": "github-mcp",
                "args": [seed.text("config.mcp.args", Captured)],
                "env": { "GITHUB_TOKEN": seed.text("config.mcp.env", Captured) },
            },
            "required_env": ["GITHUB_TOKEN"],
        }))
        .expect("sample mcp server parses");
        let mcp_json: crate::config::McpServerConfig = serde_json::from_value(serde_json::json!({
            "name": "imported",
            "description": "imported blob",
            "installation": { "type": "PreInstalled" },
            "definition": {
                "type": "Json",
                "config": { "headers": { "Authorization": seed.text("config.mcp.json", Captured) } },
            },
        }))
        .expect("sample imported mcp server parses");
        config.app_config.mcp_servers = HashMap::from([
            ("github".to_string(), mcp),
            ("imported".to_string(), mcp_json),
        ]);
        let template: crate::config::ContainerTemplate = serde_json::from_value(serde_json::json!({
            "name": "claude",
            "description": "sample template",
            "config": {
                "image_source": { "type": "Image", "name": "claude:latest" },
                "command": ["claude"],
                "environment": { "ANTHROPIC_API_KEY": seed.text("config.container.env", Captured) },
            },
            "required_env": ["ANTHROPIC_API_KEY"],
        }))
        .expect("sample container template parses");
        config.app_config.container_templates = HashMap::from([("claude".to_string(), template)]);

        config.statusline_status = Some(crate::cli::statusline_install::StatuslineStatus::Other(
            seed.text("config.statusline_command", Captured),
        ));
        let screen = &mut config.config_screen_state;
        *screen = crate::app::state::ConfigScreenState::from_app_config(&config.app_config);
        screen.edit_buffer = seed.text("config.edit_buffer", Typed);
        screen.api_key_input_mode = true;
        screen.editing = true;
        screen.search = Some(seed.text("config.search", Typed));
        screen.keychain_target = Some("fleet.bridge.telegram.token".to_string());
        // One secret row holds a literal, the case the Ctrl+K flow pre-fills.
        let literal = seed.text("config.keychain_popup", Typed);
        let mut secret_row = None;
        for (category, rows) in &mut screen.settings {
            for (index, row) in rows.iter_mut().enumerate() {
                if let ConfigValue::Secret(secret) = &mut row.value {
                    if secret_row.is_none() {
                        *secret = SecretValue {
                            reference: literal.clone(),
                            resolved: true,
                        };
                        secret_row = Some((*category, index));
                    }
                }
            }
        }
        let secret_row = secret_row.expect("the registry declares secret rows");
        screen.visible_rows = vec![secret_row];
        screen.selected_setting = 0;
    }
    // Open the keychain popup through the real Ctrl+K event, so the sample
    // carries exactly what that flow puts in state.
    crate::app::EventHandler::process_event(
        crate::app::AppEvent::ConfigSecretToKeychain,
        &mut state,
    );

    // ---- onboarding ------------------------------------------------------------
    {
        let onboarding = state.onboarding.get_mut();
        onboarding.auth_setup_state = Some(AuthSetupState {
            selected_method: crate::app::state::AuthMethod::ApiKey,
            api_key_input: seed.text("onboarding.auth_setup.api_key_input", Typed),
            is_processing: false,
            error_message: Some(seed.text("onboarding.auth_setup.error", Captured)),
            show_cursor: true,
        });
        let mut popup =
            AuthProviderPopupState::from_app_config(&crate::config::AppConfig::default());
        popup.is_entering_key = true;
        popup.show_popup = true;
        popup.api_key_input = seed.text("onboarding.auth_provider_popup.api_key_input", Typed);
        onboarding.auth_provider_popup_state = popup;
        let mut wizard = crate::components::onboarding::OnboardingState::new();
        // Detected from the machine: pinned so the traced shape is the same
        // everywhere `ainb doctor --wire-shape` runs.
        wizard.available_editors = vec![crate::components::onboarding::state::EditorOption {
            name: "VS Code".to_string(),
            command: "code".to_string(),
            available: true,
        }];
        wizard.dependency_status = None;
        wizard.install_states.clear();
        wizard.validated_directories.clear();
        wizard.otel_api_token = seed.text("onboarding.otel_api_token", Typed);
        wizard.otel_instance_id = seed.text("onboarding.otel_instance_id", Typed);
        wizard.otel_otlp_endpoint = seed.text("onboarding.otel_otlp_endpoint", Typed);
        wizard.git_directories_input = seed.text("onboarding.git_directories_input", Typed);
        wizard.auth_pane = crate::components::onboarding::state::AuthPane::KeyEntry {
            agent: crate::components::onboarding::state::AuthAgent::Claude,
            buf: seed.text("onboarding.auth_pane.key_entry", Typed),
        };
        wizard.auth_statuses = vec![crate::components::onboarding::state::AgentAuthStatus {
            agent: crate::components::onboarding::state::AuthAgent::Claude,
            method: crate::components::onboarding::state::AuthMethodKind::ApiKey,
            key_masked: Some("sk-ant-••••".to_string()),
        }];
        onboarding.onboarding_state = Some(wizard);
    }

    // ---- sessions and ssh --------------------------------------------------------
    let session = |seed: &mut dyn Seed, name: &str| {
        let mut session =
            crate::models::Session::new(name.to_string(), "/work/sample-repo".to_string());
        session.preview_content = Some(seed.text("session.preview_content", Captured));
        session.recent_logs = Some(seed.text("session.recent_logs", Captured));
        session.boss_prompt = Some(seed.text("session.boss_prompt", Captured));
        session.tmux_session_name = Some(format!("ainb-{name}"));
        session.display_name = Some(format!("{name} label"));
        session.model = Some("claude-sonnet-4-5".to_string());
        session.status =
            crate::models::SessionStatus::Error(seed.text("session.status.error", Captured));
        session.live_attention = vec![
            crate::fleet::attention::SessionAttention::local(
                crate::fleet::attention::AttentionKind::Ask,
                1_000,
            )
            .with_detail(seed.text("session.attention.detail", Captured))
            .with_options(vec![crate::fleet::attention::AttentionOption {
                label: seed.text("session.attention.option_label", Captured),
                description: seed.text("session.attention.option_description", Captured),
            }]),
        ];
        let mut target = crate::models::SshTarget::new("build.example.com".to_string());
        target.user = Some("deploy".to_string());
        target.identity_file = Some(PathBuf::from("/home/sample/.ssh/id_ed25519"));
        session.ssh_target = Some(target);
        session
    };
    {
        let mut workspace = crate::models::Workspace::new(
            "sample-repo".to_string(),
            PathBuf::from("/work/sample-repo"),
        );
        workspace.sessions.push(session(seed, "managed"));
        let sessions = state.sessions.get_mut();
        sessions.workspaces = vec![workspace];
        sessions.favorite_workspace_paths.insert(PathBuf::from("/work/sample-repo"));
        let ssh = state.ssh.get_mut();
        ssh.ssh_sessions = vec![session(seed, "remote")];
        ssh.ssh_session_rename_mode = true;
        ssh.ssh_session_rename_buffer = seed.text("ssh.rename_buffer", Typed);
    }
    {
        let tmux = state.tmux.get_mut();
        tmux.other_tmux_rename_mode = true;
        tmux.other_tmux_rename_buffer = seed.text("tmux.rename_buffer", Typed);
        let labels = state.session_labels.get_mut();
        labels.session_label_store = crate::config::SessionLabelStore::default();
        labels.session_label_store.set(
            "ainb-managed".to_string(),
            Some("managed label".to_string()),
        );
        labels.session_label_rename_mode = true;
        labels.session_label_rename_buffer = seed.text("session_labels.rename_buffer", Typed);
    }

    // ---- git view ----------------------------------------------------------------
    {
        let git = state.git_view.get_mut();
        let mut view = GitViewState::new(PathBuf::from("/work/sample-repo"));
        view.diff_content = vec![seed.text("git_view.diff_content", Captured)];
        view.commit_message_input = Some(seed.text("git_view.commit_message_input", Typed));
        view.markdown_content = vec![MarkdownLine {
            content: seed.text("git_view.markdown_content", Captured),
            style: MarkdownStyle::Paragraph,
        }];
        view.review.files = vec![ReviewFile {
            path: ".env.local".to_string(),
            status: GitFileStatus::Modified,
            insertions: 1,
            deletions: 0,
            language: None,
            collapsed: false,
            binary: false,
            hunks: vec![Hunk {
                old_start: 1,
                new_start: 1,
                gap_before: 0,
                gap_after: 0,
                expanded_before: 0,
                expanded_after: 0,
                rows: vec![DiffRow {
                    kind: RowKind::Added,
                    old_lineno: None,
                    new_lineno: Some(1),
                    raw: seed.text("git_view.review.hunk_row", Captured),
                    emphasis: vec![(0, 1)],
                }],
            }],
            new_lines: vec![seed.text("git_view.review.new_lines", Captured)],
        }];
        git.git_view_state = Some(view);
        git.quick_commit_message = Some(seed.text("git_view.quick_commit_message", Typed));
    }

    // ---- new session ---------------------------------------------------------------
    {
        let mut pick =
            crate::components::new_session::pick_repo::PickRepoState::from_disk_no_locals();
        pick.rows = vec![crate::components::new_session::pick_repo::PickRepoRow {
            id: "local:/work/sample-repo".to_string(),
            label: "sample-repo".to_string(),
            source: crate::git::repo_source::RepoSource::LocalPath(PathBuf::from(
                "/work/sample-repo",
            )),
            kind: crate::components::new_session::pick_repo::RepoRowKind::Local,
        }];
        pick.filtered_indices = vec![0];
        pick.selected = 0;
        pick.filter = seed.text("new_session.pick_repo.filter", Typed);
        pick.git_auth_error = Some(seed.text("new_session.pick_repo.git_auth_error", Captured));
        pick.pending_clone_source = Some(crate::git::repo_source::RepoSource::HttpsUrl(
            seed.text("new_session.pick_repo.clone_url", Captured),
        ));
        pick.defaults = crate::config::SessionDefaults::default();
        pick.defaults.last_repo = Some("/work/sample-repo".to_string());
        pick.favorites = crate::config::favorites_store::FavoritesStore::default();
        let mut configure =
            crate::components::new_session::configure::ConfigureState::from_pick_repo(
                crate::git::repo_source::RepoSource::HttpsUrl(
                    seed.text("new_session.configure.repo_url", Captured),
                ),
                "sample-repo".to_string(),
                &crate::config::SessionDefaults::default(),
                Some("main".to_string()),
                "agents/",
                Vec::new(),
                vec!["main".to_string()],
            );
        let preset = crate::config::RepositoryPreset {
            name: "sample".to_string(),
            custom_rules: Some(seed.text("new_session.preset.custom_rules", Captured)),
            environment: HashMap::from([(
                "API_TOKEN".to_string(),
                seed.text("new_session.preset.env", Captured),
            )]),
            ..crate::config::RepositoryPreset::default()
        };
        configure.current_preset = preset.clone();
        configure.presets_cache = HashMap::from([("sample".to_string(), preset)]);
        configure.available_presets = vec!["sample".to_string()];
        configure.prompt = crate::text_editor::TextEditor::from_string(
            &seed.text("new_session.configure.prompt", Typed),
        );
        configure.branch_edit = Some(seed.text("new_session.configure.branch_edit", Typed));
        state.new_session.get_mut().new_session_state = Some(NewSessionState {
            pick_repo_state: Some(pick),
            configure_state: Some(configure),
            ..NewSessionState::default()
        });
    }

    // ---- logs ------------------------------------------------------------------------
    {
        let session_id = uuid::Uuid::nil();
        let mut entry = LogEntry::new(
            LogEntryLevel::Info,
            "container".to_string(),
            seed.text("logs.live.message", Captured),
        );
        entry.metadata = HashMap::from([(
            "tool_input".to_string(),
            seed.text("logs.live.metadata", Captured),
        )]);
        let logs = state.log_streams.get_mut();
        logs.logs = HashMap::from([(session_id, vec![seed.text("logs.fetched", Captured)])]);
        logs.live_logs = HashMap::from([(session_id, vec![entry.clone()])]);
        logs.log_history_state.current_logs = vec![entry];
        logs.log_history_state.search_query = Some(seed.text("logs.history.search_query", Typed));
        logs.log_history_state.selection.selected_text =
            Some(seed.text("logs.history.selected_text", Typed));
        logs.log_history_state.log_dir = Some(PathBuf::from("/home/sample/.agents-in-a-box/logs"));
    }

    // ---- claude chat -------------------------------------------------------------------
    {
        let mut chat = ClaudeChatState::new();
        chat.messages = vec![crate::claude::ClaudeMessage::user(
            seed.text("claude_chat.messages", Captured),
        )];
        chat.input_buffer = seed.text("claude_chat.input_buffer", Typed);
        chat.current_streaming_response = Some(seed.text("claude_chat.streaming", Captured));
        state.claude_chat.get_mut().claude_chat_state = Some(chat);
    }

    // ---- fleet -------------------------------------------------------------------------
    {
        let chip = crate::fleet::attention::SessionAttention {
            kind: crate::fleet::attention::AttentionKind::Ask,
            since_ms: 1,
            source: crate::fleet::attention::AttentionSource::Daemon,
            detail: Some(seed.text("fleet.attention.detail", Captured)),
            options: Vec::new(),
            answerable: crate::fleet::attention::Answerable::Tmux,
        };
        let fleet = state.fleet.get_mut();
        fleet.ask_state.retarget(&chip);
        for c in seed.text("fleet.ask.free_text", Typed).chars() {
            fleet.ask_state.push_char(c);
        }
        for c in seed.text("fleet.broadcast.text", Typed).chars() {
            fleet.broadcast.push(c);
        }

        // The open conversation, as the reducer projects it: one row per actor
        // and one card per decode outcome, so every scrubbed leaf below it is
        // walked by the leak checks rather than sitting behind an empty list.
        {
            use crate::fleet::conversation::{
                Conversation, ConversationActor, ConversationCard, ConversationCardState,
                ConversationKind, ConversationRow, ConversationStatus, ConversationTopic,
            };
            fn row(
                actor: ConversationActor,
                kind: ConversationKind,
                body: String,
            ) -> ConversationRow {
                ConversationRow {
                    id: "m-1".to_string(),
                    actor,
                    kind,
                    reply: false,
                    body,
                    truncated: true,
                }
            }
            let operator_body = seed.text("fleet.conversation.operator_body", Captured);
            let pal_body = seed.text("fleet.conversation.pal_body", Captured);
            let session_body = seed.text("fleet.conversation.session_body", Captured);
            let unattributed_body = seed.text("fleet.conversation.unattributed_body", Captured);
            fleet.conversation = Conversation {
                topic: ConversationTopic::Pal,
                scope_key: Some("channel:01J8Z".to_string()),
                target_session_key: Some("claude:s-1".to_string()),
                // The variant that carries text: `Live` and `Opening` have
                // none, so this is the one a leak check has to walk.
                status: ConversationStatus::Unavailable {
                    detail: seed.text("fleet.conversation.unavailable", Captured),
                },
                rows: vec![
                    row(
                        ConversationActor::Operator,
                        ConversationKind::User,
                        operator_body,
                    ),
                    row(ConversationActor::Pal, ConversationKind::Agent, pal_body),
                    row(
                        ConversationActor::Session("claude:s-1".to_string()),
                        ConversationKind::Marker,
                        session_body,
                    ),
                    row(
                        ConversationActor::Unattributed,
                        ConversationKind::Agent,
                        unattributed_body,
                    ),
                ],
                rows_held: 4,
                cards: vec![
                    ConversationCard {
                        confirm_id: "c-1".to_string(),
                        tool: "shell".to_string(),
                        arguments: serde_json::json!({
                            "command": seed.text("fleet.conversation.card_argument", Captured),
                        }),
                        arguments_bytes: 64,
                        state: ConversationCardState::Open,
                        detail: String::new(),
                    },
                    ConversationCard {
                        confirm_id: "c-2".to_string(),
                        tool: "unknown".to_string(),
                        arguments: serde_json::Value::Null,
                        arguments_bytes: 0,
                        state: ConversationCardState::Unrecognised,
                        detail: seed.text("fleet.conversation.card_detail", Captured),
                    },
                ],
                cards_held: 2,
                send_block: seed.text("fleet.conversation.send_block", Captured),
                composer: seed.text("fleet.conversation.composer", Typed),
            };
            // The open ACP transcript: one chunk per kind, and the status
            // that carries text, so every leaf reaches the leak walk.
            {
                use crate::fleet::transcript::{
                    ChunkKind, Transcript, TranscriptChunk, TranscriptStatus,
                };
                let kinds = [
                    ChunkKind::Message,
                    ChunkKind::UserMessage,
                    ChunkKind::Thought,
                    ChunkKind::ToolCall,
                    ChunkKind::Plan,
                    ChunkKind::Permission,
                    ChunkKind::Usage,
                    ChunkKind::Lifecycle,
                ];
                let mut chunks = Vec::new();
                for (order, kind) in (1_i64..).zip(kinds) {
                    chunks.push(TranscriptChunk {
                        order,
                        kind,
                        body: seed.text("fleet.transcript.body", Captured),
                        truncated: true,
                    });
                }
                fleet.transcript = Transcript {
                    session_key: Some("acp:s-1".to_string()),
                    status: TranscriptStatus::Unavailable {
                        detail: seed.text("fleet.transcript.unavailable", Captured),
                    },
                    chunks,
                    chunks_held: 8,
                    starts_part_way: true,
                };
            }
        }
        {
            let mut attention =
                fleet.daemon_attention.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            attention.by_session_id = HashMap::from([("s-1".to_string(), vec![chip.clone()])]);
            attention.by_cwd =
                HashMap::from([("/work/sample-repo".to_string(), vec![chip.clone()])]);
            attention.by_cwd_without_session_id =
                HashMap::from([("/work/other-repo".to_string(), vec![chip.clone()])]);
            attention.all = HashMap::from([("a-1".to_string(), chip)]);
            attention.error = Some(seed.text("fleet.attention.error", Captured));
        }
        let fleet_session = {
            use ainb_hangar_proto::fleet as proto;
            proto::FleetSession {
                session_key: "claude:s-1".to_string(),
                provider: proto::FleetProvider::Claude,
                provider_session_id: Some("s-1".to_string()),
                tmux_target: Some("ainb-managed".to_string()),
                pane_binding: proto::PaneBinding::default(),
                process_start_fingerprint: None,
                cwd: "/work/sample-repo".to_string(),
                display_name: Some("managed".to_string()),
                lifecycle: proto::LifecycleState::Running,
                active_work_count: 1,
                attention: proto::AttentionState::default(),
                current_request_fingerprint: Some("fp".to_string()),
                current_request: Some(serde_json::json!({
                    "tool_input": seed.text("fleet.snapshot.current_request", Captured),
                })),
                management: proto::ManagementState::Managed,
                transport_health: proto::TransportHealth::Healthy,
                capabilities: proto::FleetCapabilities::default(),
                provenance: proto::FleetProvenance::Authoritative,
                confidence: proto::FleetConfidence::High,
                discovered_at: 1,
                last_observed_at: 2,
                lifecycle_updated_at: 2,
                attention_updated_at: 1,
                model: Some("claude-sonnet-4-5".to_string()),
                reasoning_effort: Some("high".to_string()),
                model_updated_at: 2,
                version: 1,
                updated_revision: 3,
            }
        };
        let agent_status_session = fleet_session.clone();
        *fleet.fleet_snapshot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            vec![fleet_session];

        // ---- agent status (section 20) ---------------------------------------------------------
        use ainb_hangar_proto::agent_status as status;
        let row = status::AgentStatusRow {
            session_key: agent_status_session.session_key.clone(),
            provider: agent_status_session.provider,
            cwd: seed.text("agent_status.card.cwd", Captured),
            display_name: Some(seed.text("agent_status.card.display_name", Captured)),
            state: status::AgentState::Waiting,
            provenance: status::Provenance::Hook,
            tier: status::Tier::Hook,
            evidence_observed_at: 2,
            has_open_request: true,
            pane_unbound: true,
            pane_unbound_detail: Some(seed.text("agent_status.card.pane_unbound_detail", Captured)),
            host_id: status::LOCAL_HOST_ID.to_string(),
            turn_complete: false,
            wait_kind: Some(status::WaitKind::Ask),
            attachment: status::Attachment::Unbound,
        };
        let mut card_session = agent_status_session;
        card_session.cwd = seed.text("agent_status.session.cwd", Captured);
        card_session.display_name = Some(seed.text("agent_status.session.display_name", Captured));
        card_session.current_request = Some(serde_json::json!({
            "tool_input": seed.text("agent_status.session.current_request", Captured),
        }));
        let section = state.agent_status.get_mut();
        section.observe_head(4);
        section.apply_read(
            status::RosterStatusResult {
                rows: vec![status::RosterStatusRow {
                    session: card_session,
                    status: row,
                    read_revision: 3,
                }],
                read_revision: 3,
                unknown_events: Vec::new(),
                read_at_ms: 0,
            },
            5,
        );
        section.mark_read_failed(seed.text("agent_status.view.reason", Captured), 6);
        section.absent = Some(seed.text("agent_status.absent", Captured));
    }

    // ---- hangar, mcp pool, plugins ---------------------------------------------------------
    {
        let status: crate::fleet::daemons::DaemonStatus =
            serde_json::from_value(serde_json::json!({
                "kind": "bridge",
                "state": "running",
                "reason": "serving",
                "last_error": seed.text("hangar.daemon.last_error", Captured),
            }))
            .expect("sample daemon status parses");
        let hangar = state.hangar.get_mut();
        hangar.daemons_state.shared = Some(std::sync::Arc::new(std::sync::Mutex::new(
            crate::components::daemons::Snapshot {
                rows: vec![status],
                ..crate::components::daemons::Snapshot::default()
            },
        )));
        // Not in the frame; seeded with captured text so a view that starts
        // carrying the queue again trips the credential-shape check.
        hangar.pending_daemon_config_edits = vec![(
            "attention.poll_ms".to_string(),
            seed.text("hangar.pending_daemon_config_edit", Captured),
        )];
        state.mcp_pool.get_mut().mcp_overlay = Some(crate::app::state::McpOverlayState {
            pool_enabled: true,
            daemon_running: true,
            servers: vec![crate::mcp_pool::proxy::ServerStatus {
                name: "github".to_string(),
                socket: "/home/sample/.agents-in-a-box/mcp/sockets/github.sock".to_string(),
                sessions: vec!["ainb-managed".to_string()],
                state: "running".to_string(),
                ..crate::mcp_pool::proxy::ServerStatus::default()
            }],
            selected: 0,
            loading: false,
            last_refreshed: None,
            refresh_secs: 0,
            fetch_rx: None,
            last_action: Some(seed.text("mcp_pool.last_action", Captured)),
        });
        let plugins = state.plugins_host.get_mut();
        plugins.plugin_render_errors.insert(
            "plugin:sample".to_string(),
            seed.text("plugins.render_error", Captured),
        );
        plugins.plugin_presence.insert(
            crate::app::screens::ids::HANGAR.to_string(),
            crate::app::sections::PluginPresence {
                registered: true,
                wedged: false,
                abi: ainb_plugin_protocol::manifest::ABI_VERSION,
            },
        );
    }

    // ---- skills ------------------------------------------------------------------------------
    {
        let skills = &mut state.skills.get_mut().skill_manager_state;
        skills.sources = vec![crate::components::skill_manager_screen::SourceRow {
            name: "private".to_string(),
            uri: seed.text("skills.source.uri", Captured),
            r#ref: "main".to_string(),
            enabled: true,
            is_library: false,
        }];
        skills.input = Some(crate::components::skill_manager_screen::InputState {
            kind: crate::components::skill_manager_screen::InputKind::AddSource,
            buffer: seed.text("skills.input.buffer", Typed),
        });
        skills.search = Some(seed.text("skills.search", Typed));
        skills.source_filter = Some(seed.text("skills.source_filter", Typed));
    }

    // ---- recovery ------------------------------------------------------------------------------
    {
        let recovery = &mut state.recovery.get_mut().session_recovery_state;
        recovery.orphaned_sessions = vec![OrphanedSession {
            session: "ainb-orphan".to_string(),
            task: seed.text("recovery.orphan.task", Captured),
            directory: "/work/sample-repo".to_string(),
            created: "2026-09-14".to_string(),
            status: "dead".to_string(),
            transcript_path: Some("/home/sample/.claude/projects/x/s.jsonl".to_string()),
            worktree_branch: Some("agents/x".to_string()),
            can_resume: true,
            time_ago: "1h".to_string(),
            label: None,
        }];
        recovery.orphaned_worktrees = vec![OrphanedWorktree {
            id: None,
            path: PathBuf::from("/work/.worktrees/x"),
            name: "x".to_string(),
            branch: Some("agents/x".to_string()),
            last_commit: None,
            source_repo: Some(seed.text("recovery.worktree.source_repo", Captured)),
            orphan_type: OrphanType::NoTmux,
            time_ago: "1h".to_string(),
            agent_type: None,
            label: None,
        }];
        recovery.last_error = Some(seed.text("recovery.last_error", Captured));
        recovery.action_result = Some(seed.text("recovery.action_result", Captured));
        recovery.search_query = seed.text("recovery.search_query", Typed);
        recovery.search_active = true;
    }

    // ---- shell and workspace load ------------------------------------------------------------------
    {
        let shell = state.shell.get_mut();
        shell.notifications = vec![Notification::new(
            seed.text("shell.notification", Captured),
            NotificationType::Error,
        )];
        state.workspace_load.get_mut().workspace_load_error =
            Some(seed.text("workspace_load.error", Captured));
    }

    fill_secondary_screens(&mut state, seed);
    state
}

/// Everything below the leak fields: the lists, dialogs and overlays a screen
/// opens. Left empty they hide their text from all four checks, which
/// `the_sample_fills_every_structured_subtree` refuses.
#[allow(clippy::too_many_lines)]
fn fill_secondary_screens(state: &mut AppState, seed: &mut dyn Seed) {
    use crate::components::skill_manager_screen as skills;
    use TextKind::Captured;

    {
        let shell_path = PathBuf::from("/work/sample-repo");
        let mut shell = crate::models::session::ShellSession::new(
            shell_path.clone(),
            shell_path,
            Some("main".to_string()),
        );
        shell.preview_content = Some(seed.text("session.shell.preview_content", Captured));
        state.sessions.get_mut().workspaces[0].shell_session = Some(shell);
        // One hidden row, so the verdict's leaf reaches the leak walk: ids
        // only, and the walk proves it.
        let hidden = state.sessions.get_mut().workspaces[0]
            .sessions
            .first()
            .map(|session| session.id);
        state.sessions.get_mut().hidden_sessions.extend(hidden);
    }
    {
        let tmux = state.tmux.get_mut();
        tmux.other_tmux_sessions = vec![crate::models::OtherTmuxSession {
            name: "scratch".to_string(),
            attached: false,
            windows: 1,
            created: Some("2026-09-14".to_string()),
        }];
    }
    {
        let fleet = state.fleet.get_mut();
        fleet.fleet_metadata.insert(
            uuid::Uuid::nil(),
            crate::app::state::SessionFleetMetadata {
                model: Some("claude-sonnet-4-5".to_string()),
                reasoning_effort: Some("high".to_string()),
                direct_child_count: 1,
                provider_session_id: Some("s-1".to_string()),
                ..crate::app::state::SessionFleetMetadata::default()
            },
        );
        fleet.ask_state.set_phase(
            "request-1",
            crate::fleet::answer::AnswerPhase::Failed {
                reason: seed.text("fleet.ask.failure_reason", Captured),
                draft: Some("typed answer".to_string()),
            },
        );
        // Every phase, not just the failed one (#1145): a payload no sample
        // reaches is a payload no leak check has ever seen.
        fleet.ask_state.set_phase(
            "request-2",
            crate::fleet::answer::AnswerPhase::InFlight {
                since: std::time::Instant::now(),
                draft: Some(seed.text("fleet.ask.in_flight_draft", TextKind::Typed)),
            },
        );
        fleet.ask_state.set_phase(
            "request-3",
            crate::fleet::answer::AnswerPhase::Delivered {
                via: seed.text("fleet.ask.delivered_via", TextKind::Typed),
            },
        );
        let mut attention =
            fleet.daemon_attention.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let crate::fleet::attention::DaemonAttention {
            by_session_id, all, ..
        } = &mut *attention;
        for chip in by_session_id.values_mut().flatten().chain(all.values_mut()) {
            chip.options = vec![crate::fleet::attention::AttentionOption {
                label: "yes".to_string(),
                description: seed.text("fleet.attention.option", Captured),
            }];
        }
        drop(attention);
    }
    {
        let git = state.git_view.get_mut();
        if let Some(view) = git.git_view_state.as_mut() {
            view.changed_files = vec![crate::components::git_view::ChangedFile {
                path: ".env.local".to_string(),
                status: crate::components::git_view::GitFileStatus::Modified,
                insertions: 1,
                deletions: 0,
            }];
            view.file_tree_items = vec![crate::components::git_view::FileTreeItem {
                display_name: ".env.local".to_string(),
                full_path: ".env.local".to_string(),
                depth: 0,
                is_folder: false,
                status: Some(crate::components::git_view::GitFileStatus::Modified),
                is_last_in_group: true,
                is_expanded: false,
                file_count: 0,
            }];
            view.commits = vec![crate::git::operations::CommitInfo {
                hash_short: "abc1234".to_string(),
                author: "sample".to_string(),
                date: "2026-09-14".to_string(),
                message: seed.text("git_view.commit.message", Captured),
            }];
        }
    }
    {
        let new_session = state.new_session.get_mut();
        if let Some(flow) = new_session.new_session_state.as_mut() {
            if let Some(pick) = flow.pick_repo_state.as_mut() {
                pick.clone_progress =
                    Some(crate::components::new_session::pick_repo::CloneProgress {
                        url: seed.text("new_session.clone_progress.url", Captured),
                        bytes_done: 1,
                        bytes_total: 2,
                        error: Some(seed.text("new_session.clone_progress.error", Captured)),
                    });
                pick.git_auth_status =
                    Some(crate::components::new_session::pick_repo::GitAuthStatus::Authenticated);
            }
            if let Some(configure) = flow.configure_state.as_mut() {
                use crate::components::new_session::configure as cfg;
                configure.base_selection = Some(cfg::BaseSelection {
                    display: "origin/main".to_string(),
                    short_name: "main".to_string(),
                    is_remote: true,
                    mode: cfg::BaseMode::BaseOff,
                });
                let mut picker = cfg::BranchPickerState::new(
                    vec![cfg::PickerBranchEntry {
                        entry: crate::git::branch_list::BranchEntry {
                            display: "origin/main".to_string(),
                            short_name: "main".to_string(),
                            is_remote: true,
                            is_default: true,
                        },
                        in_use: false,
                    }],
                    false,
                );
                picker.error = Some(seed.text("new_session.branch_picker.error", Captured));
                configure.branch_picker = Some(picker);
                configure.custom_overrides =
                    Some(cfg::CustomOverrides::seed_from(&configure.current_preset));
                configure.repo_check =
                    cfg::RepoCheck::Failed(seed.text("new_session.repo_check", Captured));
            }
        }
    }
    {
        let logs = state.log_streams.get_mut();
        logs.log_history_state.sessions =
            vec![crate::components::log_history_viewer::SessionLogSummary {
                filename: "session.jsonl".to_string(),
                display_name: "session".to_string(),
                log_path: PathBuf::from("/home/sample/.agents-in-a-box/logs/session.jsonl"),
                log_count: 1,
                error_count: 0,
                warn_count: 0,
                is_jsonl: true,
            }];
        logs.log_history_state.log_entries_area = Some(crate::geometry::Area {
            x: 0,
            y: 0,
            width: 80,
            height: 24,
        });
    }
    {
        let onboarding = state.onboarding.get_mut();
        if let Some(wizard) = onboarding.onboarding_state.as_mut() {
            wizard.install_states.insert(
                "tmux".to_string(),
                crate::components::onboarding::state::DepInstall::Error(
                    seed.text("onboarding.install_error", Captured),
                ),
            );
            wizard.validated_directories =
                vec![crate::components::onboarding::state::ValidatedPath {
                    path: PathBuf::from("~/work"),
                    is_valid: false,
                    expanded_path: PathBuf::from("/home/sample/work"),
                    error: Some(seed.text("onboarding.validated_path.error", Captured)),
                }];
        }
    }
    {
        let recovery = &mut state.recovery.get_mut().session_recovery_state;
        recovery.recovery_overlay = Some(crate::components::session_recovery::RecoveryOverlay {
            title: "Cleanup".to_string(),
            results: vec![crate::components::session_recovery::RecoveryResultLine {
                name: "x".to_string(),
                success: false,
                detail: seed.text("recovery.result.detail", Captured),
            }],
            scroll_offset: 0,
        });
    }
    {
        let shell = state.shell.get_mut();
        shell.confirmation_dialog = Some(crate::app::state::ConfirmationDialog {
            title: "Delete session".to_string(),
            message: seed.text("shell.confirmation.message", Captured),
            confirm_action: crate::app::state::ConfirmAction::DeleteSession(uuid::Uuid::nil()),
            selected_option: false,
            warning: Some(seed.text("shell.confirmation.warning", Captured)),
            options: Some(vec![crate::app::state::DialogOption {
                label: "Delete".to_string(),
                action: crate::app::state::ConfirmAction::DeleteSession(uuid::Uuid::nil()),
            }]),
            selected_index: 0,
        });
    }
    {
        let hangar = state.hangar.get_mut();
        hangar.daemons_state.outcomes.insert(
            "bridge",
            crate::components::daemons::ActionOutcome {
                action: crate::cli::daemon::Action::Start,
                ok: false,
                summary: seed.text("hangar.outcome.summary", Captured),
                detail: seed.text("hangar.outcome.detail", Captured),
                local_only: false,
            },
        );
        if let Some(shared) = hangar.daemons_state.shared.as_ref() {
            shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner).atc =
                Some(crate::components::daemons::AtcModeView {
                    name: "atc".to_string(),
                    provider: "claude".to_string(),
                    help: vec![seed.text("hangar.atc.help", Captured)],
                });
        }
    }
    {
        let config = state.config.get_mut();
        let dockerfile: crate::config::ContainerTemplate = serde_json::from_value(serde_json::json!({
            "name": "custom",
            "description": "built from a Dockerfile",
            "default_mcp_servers": ["github"],
            "config": {
                "image_source": {
                    "type": "Dockerfile",
                    "path": "/work/sample-repo/Dockerfile",
                    "build_args": { "NPM_TOKEN": seed.text("config.container.build_args", Captured) },
                },
                "volumes": [{ "host_path": "/home/sample/.cache", "container_path": "/cache" }],
                "entrypoint": ["/bin/sh", "-c"],
                "user": "agent",
                "system_packages": ["git"],
                "npm_packages": ["typescript"],
                "python_packages": ["ruff"],
            },
        }))
        .expect("sample dockerfile template parses");
        config.app_config.container_templates.insert("custom".to_string(), dockerfile);
        let git_mcp: crate::config::McpServerConfig = serde_json::from_value(serde_json::json!({
            "name": "from-git",
            "description": "installed from git",
            "installation": {
                "type": "Git",
                "url": seed.text("config.mcp.git_url", Captured),
                "branch": "main",
                "install_command": seed.text("config.mcp.install_command", Captured),
            },
            "definition": { "type": "Command", "command": "mcp", "args": [] },
        }))
        .expect("sample git mcp server parses");
        config.app_config.mcp_servers.insert("from-git".to_string(), git_mcp);
        let mut plugin_table = toml::map::Map::new();
        plugin_table.insert(
            "api_token".to_string(),
            toml::Value::String(seed.text("config.plugins.values", Captured)),
        );
        config
            .app_config
            .plugins
            .values
            .insert("sample".to_string(), toml::Value::Table(plugin_table));
        // The plugin's `[[config]]` field becomes a `plugin:sample:api_token`
        // settings row carrying the saved value, as discovery builds it.
        let manifest = ainb_plugin_protocol::manifest::Manifest {
            plugin: ainb_plugin_protocol::manifest::PluginMeta {
                name: "sample".into(),
                version: "0.1.0".into(),
                abi_version: 2,
                description: "sample plugin".into(),
            },
            capabilities: ainb_plugin_protocol::manifest::Capabilities::default(),
            provides: ainb_plugin_protocol::manifest::Provides::default(),
            subscribes: ainb_plugin_protocol::manifest::Subscribes::default(),
            lifecycle: ainb_plugin_protocol::manifest::Lifecycle {
                spawn: ainb_plugin_protocol::manifest::SpawnMode::Lazy,
                idle_reap_secs: 600,
            },
            config: vec![ainb_plugin_protocol::manifest::ConfigField {
                key: "api_token".to_string(),
                kind: ainb_plugin_protocol::manifest::ConfigKind::String,
                label: "API token".to_string(),
                default: String::new(),
                choices: Vec::new(),
            }],
        };
        let plugins = config.app_config.plugins.clone();
        let screen = &mut config.config_screen_state;
        let (visible_rows, selected_setting) =
            (screen.visible_rows.clone(), screen.selected_setting);
        screen.apply_plugin_manifests(&[manifest], &plugins);
        // Rebuilding the tree refreshes the right pane; keep the secret row the
        // sample selected so the popup and the pane still agree.
        screen.visible_rows = visible_rows;
        screen.selected_setting = selected_setting;
    }
    {
        let skills = &mut state.skills.get_mut().skill_manager_state;
        let uri = seed.text("skills.unit.uri", Captured);
        skills.units = vec![skills::UnitRow {
            idx: 0,
            name: "sample".to_string(),
            kind: "skill".to_string(),
            source: uri.clone(),
            git_ref: "main".to_string(),
            targets: vec!["claude".to_string()],
            declared_uri: uri.clone(),
        }];
        skills.detail = Some(skills::UnitDetail {
            uri: uri.clone(),
            deployed: vec!["~/.claude/skills/sample".to_string()],
            last_used: None,
            invocations: Some(1),
            requires: Vec::new(),
            upstream_status: "current".to_string(),
        });
        skills.source_remove_confirm = Some(skills::SourceRemoveConfirm {
            source_name: "private".to_string(),
            source_uri: uri,
            unit_count: 1,
            cursor: 0,
        });
        skills.sync_confirm = Some(skills::SyncConfirmState {
            target: "claude".to_string(),
            label: "sync".to_string(),
            plan: vec![seed.text("skills.sync.plan", Captured)],
            scroll: 0,
        });
        skills.library = Some(skills::LibraryViewState {
            rows: vec![skills::LibraryRow {
                name: "sample".to_string(),
                kind: "skill".to_string(),
                path: "~/.claude/skills/sample".to_string(),
                created: "2026-09-14".to_string(),
                deploy: "claude".to_string(),
            }],
            selected: 0,
            show_detail: false,
        });
        skills.browse = Some(skills::BrowseViewState {
            results: vec![skills::BrowseRow {
                name: "sample".to_string(),
                repo: "o/r".to_string(),
                stars: 1,
                install_uri: seed.text("skills.browse.install_uri", Captured),
                description: "sample".to_string(),
                kind: ainb_skill_core::catalog::CatalogEntryKind::default(),
            }],
            status: Some(seed.text("skills.browse.status", Captured)),
            ..skills::BrowseViewState::default()
        });
    }
    fill_text_fields(state, seed);
}

/// The plain text fields no screen above fills. An empty string is inert to
/// every leak check, so each gets a value; `the_sample_fills_every_string_field`
/// names any the builder misses.
#[allow(clippy::too_many_lines)]
fn fill_text_fields(state: &mut AppState, seed: &mut dyn Seed) {
    use TextKind::{Captured, Typed};

    {
        let config = state.config.get_mut();
        let app = &mut config.app_config;
        app.authentication.github_method = Some("gh".to_string());
        app.docker.host = Some(seed.text("config.docker.host", Captured));
        app.fleet.terminal = Some("ghostty".to_string());
        app.fleet.interview.surface = Some("tmux".to_string());
        app.ui_preferences.config_tree_expanded = vec!["fleet".to_string()];
        app.ui_preferences.preferred_editor = Some("code".to_string());
        app.usage.model_aliases =
            HashMap::from([("sonnet".to_string(), "claude-sonnet-4-5".to_string())]);
        app.workspace_defaults.exclude_paths = vec!["node_modules".to_string()];
        app.workspace_defaults.workspace_scan_paths = vec![PathBuf::from("/work")];
        let screen = &mut config.config_screen_state;
        screen.dirty.insert("web.listen".to_string());
        screen.expanded.insert("fleet".to_string());
    }
    {
        let onboarding = state.onboarding.get_mut();
        if let Some(provider) = onboarding.auth_provider_popup_state.providers.first_mut() {
            provider.icon = "key".to_string();
        }
        if let Some(wizard) = onboarding.onboarding_state.as_mut() {
            wizard.auth_method = Some("api_key".to_string());
            wizard.skipped_dependencies = vec!["docker".to_string()];
        }
    }
    {
        let sessions = state.sessions.get_mut();
        for session in &mut sessions.workspaces[0].sessions {
            session.container_id = Some("c0ffee".to_string());
        }
        let tmux = state.tmux.get_mut();
        tmux.embed_session = crate::app::effect::TmuxSessionName::new("ainb-managed");
        tmux.selected_other_tmux_sessions.insert("scratch".to_string());
        state.shell.get_mut().previous_screen = Some(crate::app::screens::ids::HOME.to_string());
        let fleet = state.fleet.get_mut();
        fleet.live_window.model = Some("claude-sonnet-4-5".to_string());
        fleet.live_window.codex_plan_type = Some("plus".to_string());
        for row in fleet
            .fleet_snapshot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter_mut()
        {
            row.process_start_fingerprint = Some("pid:4242@1".to_string());
        }
    }
    {
        let git = state.git_view.get_mut();
        if let Some(view) = git.git_view_state.as_mut() {
            view.expanded_folders.insert("src".to_string());
            view.review_ui.collapsed_dirs.insert("docs".to_string());
        }
    }
    {
        let new_session = state.new_session.get_mut();
        if let Some(configure) = new_session
            .new_session_state
            .as_mut()
            .and_then(|flow| flow.configure_state.as_mut())
        {
            configure.branch_override = Some("agents/sample".to_string());
            configure.branch_prefix_edit =
                Some(seed.text("new_session.configure.branch_prefix_edit", Typed));
            configure.session_prefix = "sample".to_string();
            configure.session_prefix_edit =
                Some(seed.text("new_session.configure.session_prefix_edit", Typed));
            configure.save_preset_modal =
                Some(seed.text("new_session.configure.save_preset_modal", Typed));
            configure.existing_branches = vec!["agents/older".to_string()];
            configure.current_preset.skills = vec!["review".to_string()];
            configure.current_preset.plugins = vec!["notifyd".to_string()];
        }
    }
    {
        let logs = state.log_streams.get_mut();
        logs.log_history_state.selected_log_file = Some("session.jsonl".to_string());
        let hangar = state.hangar.get_mut();
        hangar.daemons_state.attach_request = Some("ainb-atc".to_string());
        if let Some(shared) = hangar.daemons_state.shared.as_ref() {
            use ainb_plugin_notifyd::install as hooks;
            shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner).hook_health =
                Some(hooks::HookHealth {
                    bundled_version: "1.1.0".to_string(),
                    installed_version: Some("1.0.0".to_string()),
                    version_current: false,
                    script_path: PathBuf::from("/home/sample/.claude/hooks/ainb-notify.sh"),
                    script_ready: true,
                    hook_binary: Some(PathBuf::from("/opt/homebrew/bin/ainb")),
                    hook_binary_mode: Some(hooks::HookBinaryMode::Release),
                    hook_binary_ready: true,
                    running_binary: Some(PathBuf::from("/opt/homebrew/bin/ainb")),
                    hook_binary_is_running_binary: true,
                    agents: vec![hooks::HookAgentHealth {
                        agent: "claude".to_string(),
                        installed: true,
                        wiring_ready: false,
                        detail: seed.text("hangar.hook_health.agent_detail", Captured),
                    }],
                    notify_socket_live: true,
                    approve_socket_live: true,
                    last_event: Some(seed.text("hangar.hook_health.last_event", Captured)),
                    issues: vec![hooks::HookHealthIssue {
                        component: "hooks".to_string(),
                        message: seed.text("hangar.hook_health.issue", Captured),
                        repair: "ainb doctor --fix-hooks".to_string(),
                    }],
                });
        }
        state.skills.get_mut().skills_state.data = Some(crate::models::SkillsData {
            skills: vec![crate::models::skills::Skill {
                name: "review".to_string(),
                description: seed.text("skills.data.skill_description", Captured),
                user_invocable: Some(true),
                source_path: PathBuf::from("/home/sample/.claude/skills/review/SKILL.md"),
            }],
            agents: vec![crate::models::skills::AgentDef {
                name: "reviewer".to_string(),
                description: seed.text("skills.data.agent_description", Captured),
                tools: vec!["Read".to_string()],
                source_path: PathBuf::from("/home/sample/.claude/agents/reviewer.md"),
            }],
            associations: HashMap::from([("reviewer".to_string(), vec!["review".to_string()])]),
        });
        let recovery = &mut state.recovery.get_mut().session_recovery_state;
        for orphan in &mut recovery.orphaned_sessions {
            orphan.label = Some("orphan label".to_string());
        }
        for worktree in &mut recovery.orphaned_worktrees {
            worktree.id = Some("wt-1".to_string());
            worktree.label = Some("worktree label".to_string());
            worktree.last_commit = Some(seed.text("recovery.worktree.last_commit", Captured));
        }
        let skills = &mut state.skills.get_mut().skill_manager_state;
        skills.pending_remove_confirm = Some("sample".to_string());
        if let Some(detail) = skills.detail.as_mut() {
            detail.last_used = Some("2026-09-14".to_string());
            detail.requires = vec!["base".to_string()];
        }
    }
}
