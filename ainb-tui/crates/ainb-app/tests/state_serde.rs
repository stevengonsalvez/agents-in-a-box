// ABOUTME: Issue #983's leak checks over the per-section mirror frames. Nothing
// sensitive may reach `wire::section_json`, and the frame shape is locked.
//
// Four independent checks, each over what `section_json` actually emits:
//   1. name deny-list   JSON keys at any depth, case-insensitive, word-aware
//   2. type deny-list   declared Rust type of every field on the wire
//   3. canary           a unique marker in every typed text field of the state
//   4. tripwire         credential-shaped values anywhere in the frames
// plus a fixture of every leaf key path, so a new field fails until triaged.
//
// Each allow-list entry is a field that stays on the wire on purpose, with the
// one-line reason a security reviewer re-checks. An entry that no longer
// matches anything fails too, so the lists cannot go stale.

use ainb_app::app::SectionId;
use ainb_app::fleet::bridge::redact::{REDACTED, find_secret};
use ainb_app::wire::frame::HostId;
use ainb_app::wire::shape::{self, Seed, TextKind};
use ainb_app::wire::{section_json, section_name};
use std::collections::{BTreeMap, BTreeSet};

/// One scratch `HOME` for the whole binary, set once before any `AppState` is
/// built, so neither the developer's config nor a parallel test changes a frame.
fn isolated_home() {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("scratch home");
        std::env::set_var("HOME", home.path());
        home
    });
}

fn all_frames(state: &ainb_app::AppState) -> Vec<(SectionId, serde_json::Value)> {
    SectionId::ALL
        .into_iter()
        .map(|id| (id, section_json(state, id, &HostId::local())))
        .collect()
}

// ---------------------------------------------------------------------------
// Shape
// ---------------------------------------------------------------------------

#[test]
fn every_section_has_one_object_frame() {
    isolated_home();
    let states = shape::sample_states(&mut shape::PlainSeed);
    let frames = all_frames(&states[0]);
    assert_eq!(frames.len(), SectionId::COUNT);
    let names: BTreeSet<_> = SectionId::ALL.into_iter().map(section_name).collect();
    assert_eq!(names.len(), SectionId::COUNT, "section wire names collide");
    for (id, frame) in frames {
        assert!(frame.is_object(), "{} is not an object", section_name(id));
    }
}

/// A mirror frame's body is `section_json`, byte for byte, for every section of
/// every sample. Every check in this file therefore covers what a renderer
/// receives, not only what the seam returns.
#[test]
fn mirror_frames_carry_exactly_the_checked_section_json() {
    use ainb_app::wire::frame::{Mirror, Subscription};
    isolated_home();
    for state in shape::sample_states(&mut shape::PlainSeed) {
        let batch = Mirror::new(HostId::local(), Subscription::all()).batch(&state);
        assert_eq!(batch.frames.len(), SectionId::COUNT);
        for frame in batch.frames {
            let id = frame.section_id().expect("a known section");
            assert_eq!(
                *frame.body(),
                section_json(&state, id, &HostId::local()),
                "{}",
                frame.section
            );
        }
    }
}

/// A session's working directory, its operator label and a pending request's
/// tool input never ride a frame (W0-mirror, #983 M19). `display_name` is also
/// the name of a file-tree row's and a log file's own label, which are not a
/// session's, so those two owners are the only ones allowed it.
#[test]
fn no_frame_carries_a_sessions_cwd_label_or_pending_request() {
    const WITHHELD: [&str; 3] = ["cwd", "current_request", "display_name"];
    const NOT_A_SESSION: [&str; 2] = [
        "FileTreeItem.display_name",
        "SessionLogSummary.display_name",
    ];
    isolated_home();
    let trace = shape::trace_states(&shape::sample_states(&mut shape::PlainSeed));
    let carried: BTreeSet<String> = trace
        .fields
        .iter()
        .filter(|field| {
            let key = field.owner_field.rsplit('.').next().unwrap_or_default();
            WITHHELD.contains(&key) && !NOT_A_SESSION.contains(&field.owner_field.as_str())
        })
        .map(|field| format!("{}  ({})", field.owner_field, field.path))
        .collect();
    assert!(
        carried.is_empty(),
        "session identity text on a frame: {carried:#?}"
    );
}

#[test]
fn leaf_key_paths_match_the_committed_fixture() {
    isolated_home();
    let current = shape::key_paths(&shape::sample_states(&mut shape::PlainSeed));
    if std::env::var_os("UPDATE_SECTION_KEY_PATHS").is_some() {
        std::fs::write(
            shape::COMMITTED_KEY_PATHS_FILE,
            shape::render_key_paths(&current),
        )
        .expect("write key-path fixture");
        return;
    }
    let diff = shape::diff(&current, &shape::committed_key_paths());
    assert!(
        diff.is_empty(),
        "section frame shape changed. Triage every added path against the leak \
         checks, then regenerate with UPDATE_SECTION_KEY_PATHS=1 \
         (or inspect with `ainb doctor --wire-shape`):\n{diff}"
    );
}

#[test]
fn the_sample_shape_does_not_depend_on_what_it_is_seeded_with() {
    isolated_home();
    let plain = shape::key_paths(&shape::sample_states(&mut shape::PlainSeed));
    let canary = shape::key_paths(&shape::sample_states(&mut CanarySeed::default()));
    assert_eq!(plain, canary);
}

// ---------------------------------------------------------------------------
// 1. Name deny-list
// ---------------------------------------------------------------------------

/// #983 section 1, plus the classic `key secret token password`.
const DENY_WORDS: &[&str] = &[
    "key",
    "secret",
    "token",
    "password",
    "credential",
    "cred",
    "passwd",
    "pwd",
    "passphrase",
    "api_key",
    "apikey",
    "bearer",
    "jwt",
    "oauth",
    "refresh_token",
    "access_token",
    "id_token",
    "client_secret",
    "client_id",
    "cookie",
    "session_key",
    "sid",
    "private_key",
    "privkey",
    "pem",
    "p12",
    "pfx",
    "keystore",
    "identity_file",
    "ssh_key",
    "signature",
    "hmac",
    "salt",
    "nonce",
    "otp",
    "totp",
    "mfa",
    "env",
    "environment",
    "environ",
    "dsn",
    "connection_string",
    "buf",
    "buffer",
    "input",
    "edit_buffer",
    "free_text",
    "filter",
    "query",
    "prompt",
    "literal",
    "reference",
    "raw",
    "value",
    "message",
    "messages",
    "transcript",
    "log",
    "logs",
    "output",
    "stdout",
    "stderr",
    "detail",
    "preview",
    "preview_content",
    "recent_logs",
    "scrollback",
    "capture",
    "new_lines",
    "diff",
    "diff_content",
    "markdown_content",
    "selected_text",
    "url",
    "uri",
    "endpoint",
    "webhook",
    "remote",
    "host",
    "path",
    "cwd",
    "current_request",
    "dir",
    "home",
    "file",
    "socket",
    "log_dir",
    "transcript_path",
    "command",
    "cmd",
    "args",
    "argv",
    "clipboard",
    "paste",
];

/// Fields whose key matches a deny word and still carry text, each with why
/// the text is safe on the wire. Keyed by the traced `Owner.field`.
const NAME_ALLOW: &[(&str, &str)] = &[
    (
        "Conversation.scope_key",
        "the daemon's scope for the open thread (`session:<key>`, `channel:<id>`), an identity a second surface reads the same conversation by",
    ),
    (
        "Conversation.target_session_key",
        "stable `provider:session-id` identity of the session the thread reaches, not a credential",
    ),
    (
        "ConversationCard.arguments{}",
        "a held tool call's arguments, every string scrubbed through scrub_json and the whole call withheld past MAX_ARGUMENT_BYTES; the shape is what an operator decides on",
    ),
    (
        "ConversationCard.detail",
        "why a confirm card could not be decoded, scrubbed through redact::scrub",
    ),
    (
        "ConversationStatus::Unavailable.detail",
        "the daemon's own reason the conversation could not open, scrubbed through redact::scrub",
    ),
    (
        "AgentCardFrame.host_id",
        "the host a status row was derived on (`local` today), an identity, not an address",
    ),
    (
        "AgentCardFrame.pane_unbound_detail",
        "why a pane lost its binding, daemon prose scrubbed through redact::scrub",
    ),
    (
        "AgentCardFrame.session_key",
        "stable `provider:session-id` identity, not a credential",
    ),
    (
        "StatusViewFrame.host_id",
        "the host the joined read came from (`local` today), an identity, not an address",
    ),
    (
        "AgentDef.source_path",
        "agent definition file under ~/.claude/agents",
    ),
    (
        "DockerConfig.host",
        "Docker endpoint URL, scrubbed in frame so userinfo becomes `<redacted>@`",
    ),
    (
        "HookAgentHealth.detail",
        "per-agent hook wiring detail, scrubbed",
    ),
    (
        "HookHealth.script_path",
        "path of the installed hook script",
    ),
    (
        "HookHealthIssue.message",
        "hook health issue text, scrubbed",
    ),
    (
        "LogHistoryViewerState.selected_log_file",
        "file name of the log open in the history viewer",
    ),
    ("Skill.source_path", "skill file under ~/.claude/skills"),
    (
        "ConfigPopupType::TextInput",
        "the plain-text popup variant (`Input` in its name); its value is scrubbed",
    ),
    (
        "ConfigPopupType::NumberInput",
        "the number popup variant (`Input` in its name); its buffer is scrubbed",
    ),
    (
        "ConfigPopupType::NumberInput.input_buffer",
        "digits being typed into a number setting, scrubbed; the popup draws them",
    ),
    (
        "ConfigPopupType::TextInput.value",
        "a plain setting being edited, scrubbed; secret and credential-bearing rows open SecretInput",
    ),
    ("ActionOutcome.detail", "daemon action output, scrubbed"),
    (
        "DepState.detail",
        "a probed command's first output line, scrubbed on a frame",
    ),
    (
        "FleetActionReceipt.detail",
        "a broadcast leg's daemon detail, scrubbed by scrub_receipts",
    ),
    (
        "FleetActionReceipt.idempotency_key",
        "the tui-minted broadcast key (a uuid), not a credential",
    ),
    (
        "FleetActionReceipt.session_key",
        "the daemon's stable session identity a receipt names",
    ),
    ("BrowseRow.install_uri", "catalog install URI, scrubbed"),
    ("ChangedFile.path", "repo-relative path of a changed file"),
    ("CloneProgress.url", "clone URL in progress, scrubbed"),
    ("CommitInfo.message", "commit subject in the log, scrubbed"),
    (
        "ConfirmationDialog.message",
        "confirmation prompt text, scrubbed",
    ),
    ("FileTreeItem.full_path", "repo-relative path of a tree row"),
    (
        "GitViewState.file_tree_items",
        "changed-file tree rows (`file` in the field name)",
    ),
    (
        "ImageSource.path",
        "Dockerfile path of a container template",
    ),
    ("LibraryRow.path", "on-disk path of a library skill"),
    (
        "LogsView.log_history_state",
        "the log viewer (`log` in the field name); entries and search text are withheld",
    ),
    (
        "McpInstallation.install_command",
        "MCP install command, scrubbed in frame",
    ),
    (
        "McpInstallation.url",
        "MCP git source URL, scrubbed in frame",
    ),
    (
        "RecoveryResultLine.detail",
        "per-item recovery result, scrubbed",
    ),
    (
        "SessionLogSummary.log_path",
        "log file the history viewer lists",
    ),
    (
        "ShellSession.preview_content",
        "workspace shell scrollback, scrubbed in frame",
    ),
    (
        "ShellSession.working_dir",
        "directory the workspace shell runs in",
    ),
    (
        "ShellSession.workspace_path",
        "repository root the workspace shell belongs to",
    ),
    (
        "SkillsScreenData.detail",
        "the selected unit's detail pane (`detail` in the field name); its URI is scrubbed",
    ),
    (
        "SourceRemoveConfirm.source_uri",
        "skills source URI in the remove prompt, scrubbed",
    ),
    ("UnitDetail.uri", "skill unit URI, scrubbed"),
    (
        "UnitRow.declared_uri",
        "skill unit URI as declared, scrubbed",
    ),
    (
        "ValidatedPath.expanded_path",
        "an onboarding repo directory after `~` expansion",
    ),
    (
        "ValidatedPath.path",
        "an onboarding repo directory as typed",
    ),
    (
        "VolumeMount.container_path",
        "mount point inside a container",
    ),
    (
        "VolumeMount.host_path",
        "host directory a container template mounts",
    ),
    (
        "AuthSetupState.error_message",
        "auth error prose, scrubbed through redact::scrub",
    ),
    (
        "ConfigPopupState.setting_key",
        "the registry key being edited, e.g. `fleet.bridge.telegram.token`, a name not a value",
    ),
    (
        "ConfigSetting.key",
        "registry key of a settings row; names only",
    ),
    (
        "ConfigSetting.value",
        "row value: Text scrubbed; env, build-args, imported-MCP and plugin:<name>:<field> rows redacted; Secret rows emit their source",
    ),
    (
        "EditorOption.command",
        "editor CLI name from the detected list, e.g. `code`",
    ),
    (
        "RepoSource::LocalPath.0",
        "local repository path of a picker row",
    ),
    (
        "ConfigTreeNode.path",
        "settings tree node id, a dotted config path, not a filesystem path",
    ),
    (
        "ConfigValue::Secret.0",
        "carries only the source (`$VAR`, `keychain:svc`, `<literal>`) via SecretValue",
    ),
    (
        "SecretValue.reference",
        "serialised by fields::secret_source: the source kind, never a literal",
    ),
    (
        "ConfigureState.prompt",
        "Boss prompt lines, scrubbed through redact::scrub (the surface edits the prompt)",
    ),
    (
        "ContainerTemplate.required_env",
        "names of env vars a template needs, never their values",
    ),
    (
        "McpServerConfig.required_env",
        "names of env vars a server needs, never their values",
    ),
    (
        "ContainerTemplateConfig.command",
        "container command from config, e.g. `claude`; values live in the redacted env map",
    ),
    (
        "ContainerTemplateConfig.working_dir",
        "in-container working directory, e.g. `/workspace`",
    ),
    (
        "McpServerDefinition.command",
        "MCP server binary name; credentials live in the redacted env map",
    ),
    (
        "McpServerDefinition.args",
        "MCP server arguments, each scrubbed of credential shapes in frame",
    ),
    (
        "DiffRow.raw",
        "diff hunk line the review pane paints, scrubbed through redact::scrub",
    ),
    (
        "GitViewState.diff_content",
        "diff lines the git pane paints, scrubbed through redact::scrub",
    ),
    (
        "GitViewState.markdown_content",
        "rendered markdown lines, each scrubbed through redact::scrub",
    ),
    (
        "GitViewState.worktree_path",
        "the worktree the git view is open on, drawn in its title",
    ),
    (
        "ReviewFile.path",
        "repo-relative path of a changed file, the review list row",
    ),
    (
        "FleetRowFrame.current_request_fingerprint",
        "a hash of the pending request; the request itself never reaches a frame",
    ),
    (
        "FleetRowFrame.host_id",
        "the host a fleet row came from, an identity, so rows from two hosts fold apart",
    ),
    (
        "FleetRowFrame.session_key",
        "stable `provider:session-id` identity, not a credential",
    ),
    (
        "LogEntry.message",
        "live log line, scrubbed in frame; metadata is omitted",
    ),
    (
        "LogsView.live_logs",
        "map of per-session live log entries whose text is scrubbed",
    ),
    (
        "Notification.message",
        "toast text, scrubbed through redact::scrub",
    ),
    (
        "OnboardingState.git_directories_input",
        "repo directories the operator lists, shown as typed; paths, not secrets",
    ),
    (
        "OnboardingState.otel_otlp_endpoint",
        "OTLP endpoint URL, scrubbed; the instance id and token are lengths only",
    ),
    (
        "OrphanedWorktree.path",
        "worktree directory the recovery screen offers to clean up",
    ),
    (
        "PresetsConfig.file",
        "path of presets.toml; its contents are not in config",
    ),
    (
        "RepoSource::HttpsUrl.0",
        "clone URL, scrubbed so userinfo becomes `<redacted>@`",
    ),
    (
        "RepoSource::SshUrl.0",
        "ssh clone URL, scrubbed like the https one",
    ),
    (
        "ServerStatus.socket",
        "MCP pool socket path under ~/.agents-in-a-box/mcp/sockets, drawn in the overlay",
    ),
    ("Session.boss_prompt", "launched prompt, scrubbed in frame"),
    (
        "Session.preview_content",
        "tmux scrollback, scrubbed in frame; the preview pane is the feature",
    ),
    ("Session.recent_logs", "container output, scrubbed in frame"),
    (
        "Session.workspace_path",
        "session working directory, drawn in the session list",
    ),
    (
        "WebNeedCard.hostId",
        "the host a web needs card came from, e.g. `local`",
    ),
    (
        "WebNeedCard.sessionKey",
        "the card's `provider:session-id` identity, not a credential",
    ),
    (
        "WebNeedPayload.message",
        "attention message text on a web needs card, scrubbed through redact::scrub",
    ),
    (
        "SessionAttention.detail",
        "agent question text, scrubbed through redact::scrub",
    ),
    (
        "AttentionMark.detail",
        "a session row's attention detail, the same agent question text scrubbed through redact::scrub",
    ),
    (
        "SessionLabelsView.session_label_rename_buffer",
        "session label being typed; a display name, shown so the surface can edit it",
    ),
    (
        "SshView.ssh_session_rename_buffer",
        "SSH session display name being typed, shown so the surface can edit it",
    ),
    (
        "TmuxView.other_tmux_rename_buffer",
        "tmux session name being typed, shown so the surface can edit it",
    ),
    (
        "ShellView.home_screen_v2_state",
        "home screen copy (`home` in the section name), static text",
    ),
    (
        "SourceRow.uri",
        "skills source URI, scrubbed so userinfo becomes `<redacted>@`",
    ),
    (
        "SshTarget.host",
        "SSH host the session row names; identity_file is omitted",
    ),
    (
        "Workspace.path",
        "repository root the session list groups under",
    ),
];

fn key_tokens(key: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut prev_lower = false;
    for ch in key.chars() {
        if ch == '_' || ch == '-' || ch == '.' || ch == ' ' || ch == ':' || ch == '/' {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        current.extend(ch.to_lowercase());
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn deny_word_in(key: &str) -> Option<&'static str> {
    let tokens = key_tokens(key);
    let squashed: String = tokens.concat();
    DENY_WORDS.iter().copied().find(|word| {
        let word_tokens: Vec<&str> = word.split('_').collect();
        squashed == word.replace('_', "")
            || tokens
                .windows(word_tokens.len())
                .any(|w| w.iter().map(String::as_str).eq(word_tokens.iter().copied()))
    })
}

/// A string that cannot carry a secret: empty, or the redaction marker.
fn inert(value: &str) -> bool {
    value.is_empty() || value == REDACTED
}

#[test]
fn no_deny_listed_key_carries_text_unless_allow_listed() {
    isolated_home();
    let trace = shape::trace_states(&shape::sample_states(&mut shape::PlainSeed));
    let allow: BTreeMap<_, _> = NAME_ALLOW.iter().copied().collect();
    let mut hits: BTreeMap<String, (String, &'static str)> = BTreeMap::new();
    for leaf in trace.strings.iter().filter(|l| !inert(&l.value)) {
        for segment in &leaf.keys {
            if let Some(word) = deny_word_in(&segment.key) {
                hits.entry(segment.owner_field.clone())
                    .or_insert_with(|| (leaf.path.clone(), word));
            }
        }
    }
    let unlisted: Vec<String> = hits
        .iter()
        .filter(|(owner, _)| !allow.contains_key(owner.as_str()))
        .map(|(owner, (path, word))| format!("{owner}  ({path}, matches `{word}`)"))
        .collect();
    let stale: Vec<&str> =
        allow.keys().copied().filter(|owner| !hits.contains_key(*owner)).collect();
    assert!(
        unlisted.is_empty() && stale.is_empty(),
        "name deny-list: {} field(s) carry text under a sensitive key and are not \
         allow-listed:\n  {}\nstale allow-list entries: {stale:?}",
        unlisted.len(),
        unlisted.join("\n  ")
    );
}

// ---------------------------------------------------------------------------
// 2. Type deny-list
// ---------------------------------------------------------------------------

/// Opaque or unbounded types (#983 section 2). Matched against the declared
/// field type, so `Option<PathBuf>` and `HashMap<Uuid, Vec<String>>` count;
/// the string collections are also matched by emitted JSON shape below.
const DENY_TYPES: &[(&str, &str)] = &[
    ("toml::Value", "toml::value::Value"),
    ("serde_json::Value", "serde_json::value::Value"),
    (
        "HashMap<String, String>",
        "HashMap<alloc::string::String, alloc::string::String>",
    ),
    ("Vec<String>", "Vec<alloc::string::String>"),
    ("Vec<LogEntry>", "live_logs_stream::LogEntry>"),
    ("PathBuf", "std::path::PathBuf"),
];

/// Fields of a denied type that stay on the wire, each with its reason.
const TYPE_ALLOW: &[(&str, &str)] = &[
    (
        "SessionsView.hidden_sessions",
        "uuids of the session rows the filter hides; ids only, bounded by the session list itself",
    ),
    (
        "ConversationCard.arguments",
        "a held tool call's arguments as the provider sent them; bounded by MAX_ARGUMENT_BYTES at projection time and every string scrubbed by scrub_json",
    ),
    (
        "ConfirmAction::BulkDeleteSessions.0",
        "ids of the sessions a bulk delete names; uuids, not text",
    ),
    (
        "ConfirmAction::BulkStopSessions.0",
        "ids of the sessions a bulk stop names; uuids, not text",
    ),
    (
        "ConfirmAction::KillOtherTmuxSessions.0",
        "tmux session names the kill dialog lists; the same names the tmux section carries",
    ),
    (
        "ConfigPopupType::Choice.options",
        "the choices a registry setting declares; labels, not user text",
    ),
    (
        "DiscoveryBannerCounts.orphan_units_per_tool",
        "(tool name, count) pairs; tool names come from the agent registry",
    ),
    (
        "AgentDef.source_path",
        "agent definition file under ~/.claude/agents",
    ),
    ("AgentDef.tools", "tool names an agent definition allows"),
    (
        "CodeReviewUi.collapsed_dirs",
        "repo-relative folders collapsed in the review tree",
    ),
    (
        "ConfigScreenState.dirty",
        "registry keys edited this session; names, not values",
    ),
    (
        "ConfigScreenState.expanded",
        "ids of expanded settings tree nodes",
    ),
    (
        "HookHealth.hook_binary",
        "path of the binary the notification hooks launch",
    ),
    (
        "HookHealth.running_binary",
        "path of the running ainb binary",
    ),
    (
        "HookHealth.script_path",
        "path of the installed hook script",
    ),
    ("Skill.source_path", "skill file under ~/.claude/skills"),
    ("SkillsData.associations", "agent name to skill names"),
    (
        "TmuxView.selected_other_tmux_sessions",
        "tmux session names checked in the list",
    ),
    (
        "AskState.phases",
        "(request id, phase) pairs: the id is a daemon attention id or `kind:since_ms`; the draft is a length and the reason scrubbed",
    ),
    ("AtcModeView.help", "ATC help lines, scrubbed"),
    (
        "ConfigureState.prompt",
        "Boss prompt lines, scrubbed as one text",
    ),
    (
        "ContainerTemplateConfig.environment",
        "env var names with every value `<redacted>` in frame",
    ),
    (
        "GitViewState.diff_content",
        "diff lines, scrubbed as one text",
    ),
    (
        "GitViewState.expanded_folders",
        "repo-relative folder paths expanded in the tree",
    ),
    (
        "ImageSource.build_args",
        "build-arg names with every value `<redacted>` in frame",
    ),
    (
        "McpServerDefinition.args",
        "MCP server arguments, scrubbed in frame",
    ),
    (
        "McpServerDefinition.env",
        "env var names with every value `<redacted>` in frame",
    ),
    (
        "PluginsHostView.plugin_render_errors",
        "plugin render failures keyed by screen id, scrubbed",
    ),
    (
        "RepositoryPreset.environment",
        "env var names with every value `<redacted>` in frame",
    ),
    (
        "SessionLabelsView.session_label_store",
        "tmux session name to display label, the labels the session list draws",
    ),
    ("SyncConfirmState.plan", "skills sync plan lines, scrubbed"),
    (
        "ImageSource.path",
        "Dockerfile path of a container template",
    ),
    (
        "SessionLogSummary.log_path",
        "log file the history viewer lists; its entries are not in the frame",
    ),
    (
        "ShellSession.working_dir",
        "directory the workspace shell runs in",
    ),
    (
        "ShellSession.workspace_path",
        "repository root the workspace shell belongs to",
    ),
    ("UnitDetail.deployed", "paths a skill unit is deployed to"),
    ("UnitDetail.requires", "names of units a skill requires"),
    (
        "UnitRow.targets",
        "tool names a skill unit targets, e.g. `claude`",
    ),
    (
        "WebNeedCard.channels",
        "push channel tokens (`web`, `os`) resolved at raise time",
    ),
    (
        "WebNeedPayload.options",
        "ASK option labels on a web needs card, each scrubbed through redact::scrub",
    ),
    (
        "ValidatedPath.expanded_path",
        "an onboarding repo directory after `~` expansion",
    ),
    (
        "ValidatedPath.path",
        "an onboarding repo directory as typed",
    ),
    (
        "ConfigValue::Choice.0",
        "a Choice row's fixed option labels from the registry",
    ),
    (
        "ConfigureState.available_presets",
        "preset NAMES for the picker; preset bodies stay host-side",
    ),
    (
        "ConfigureState.existing_branches",
        "local branch names for the branch picker",
    ),
    (
        "ConfigureState.repo_branch_names",
        "remote branch names for the base-branch picker",
    ),
    (
        "ContainerTemplate.default_mcp_servers",
        "MCP server names a template enables",
    ),
    (
        "ContainerTemplate.required_env",
        "env var NAMES a template needs; values are in the redacted map",
    ),
    (
        "McpServerConfig.required_env",
        "env var NAMES a server needs; values are in the redacted map",
    ),
    (
        "ContainerTemplateConfig.command",
        "container command from the operator's config",
    ),
    (
        "ContainerTemplateConfig.entrypoint",
        "container entrypoint from the operator's config",
    ),
    (
        "ContainerTemplateConfig.npm_packages",
        "package names installed in the container",
    ),
    (
        "ContainerTemplateConfig.python_packages",
        "package names installed in the container",
    ),
    (
        "ContainerTemplateConfig.system_packages",
        "package names installed in the container",
    ),
    (
        "GitViewState.worktree_path",
        "the worktree the git view is open on, drawn in its title",
    ),
    (
        "LogsView.live_logs",
        "live log entries; message scrubbed and metadata omitted in frame",
    ),
    (
        "OnboardingState.skipped_dependencies",
        "dependency names the operator skipped",
    ),
    (
        "OrphanedWorktree.path",
        "worktree directory the recovery screen offers to clean up",
    ),
    (
        "RepoSource::LocalPath.0",
        "local repository path of a picker row",
    ),
    ("RepositoryPreset.plugins", "plugin names a preset enables"),
    ("RepositoryPreset.skills", "skill names a preset enables"),
    (
        "ServerStatus.sessions",
        "tmux session names attached to a pooled MCP server",
    ),
    (
        "SessionsView.favorite_workspace_paths",
        "starred repository roots, drawn as stars in the session list",
    ),
    (
        "UiPreferences.config_tree_expanded",
        "ids of expanded settings tree nodes",
    ),
    (
        "UsageConfig.model_aliases",
        "model-name aliases for cost rollups, e.g. `sonnet -> claude-sonnet-4-5`",
    ),
    (
        "Workspace.path",
        "repository root the session list groups under",
    ),
    (
        "WorkspaceDefaults.exclude_paths",
        "glob patterns excluded from repo scanning",
    ),
    (
        "WorkspaceDefaults.workspace_scan_paths",
        "directories scanned for repositories",
    ),
];

#[test]
fn no_opaque_or_unbounded_type_reaches_the_wire_unless_allow_listed() {
    isolated_home();
    let trace = shape::trace_states(&shape::sample_states(&mut shape::PlainSeed));
    let allow: BTreeMap<_, _> = TYPE_ALLOW.iter().copied().collect();
    let mut hits: BTreeMap<String, (String, &'static str)> = BTreeMap::new();
    for field in &trace.fields {
        // A field under `serialize_with` is traced as serde's private wrapper,
        // whose name embeds the OWNER type. It is not the declared type, and
        // the field is already redacted by construction; the canary and the
        // tripwire cover what the wrapper emits.
        if field.rust_type.contains("__SerializeWith") {
            continue;
        }
        if let Some((label, _)) =
            DENY_TYPES.iter().find(|(_, needle)| field.rust_type.contains(needle))
        {
            hits.entry(field.owner_field.clone())
                .or_insert_with(|| (field.path.clone(), label));
        }
    }
    // The same classes by the JSON shape a field actually emits, whatever its
    // Rust spelling: `Vec<(String, String)>`, `HashMap<Uuid, String>`, a
    // `BTreeMap`, a newtype around any of them, or a custom serializer's output.
    for leaf in &trace.strings {
        let label = if leaf.path.ends_with("[][]") {
            "array of arrays of strings"
        } else if leaf.path.ends_with("[]") {
            "array of strings"
        } else if leaf.path.ends_with("{}") {
            "map with string values"
        } else {
            continue;
        };
        hits.entry(leaf.owner_field.clone())
            .or_insert_with(|| (leaf.path.clone(), label));
    }
    let unlisted: Vec<String> = hits
        .iter()
        .filter(|(owner, _)| !allow.contains_key(owner.as_str()))
        .map(|(owner, (path, label))| format!("{owner}: {label}  ({path})"))
        .collect();
    let stale: Vec<&str> =
        allow.keys().copied().filter(|owner| !hits.contains_key(*owner)).collect();
    assert!(
        unlisted.is_empty() && stale.is_empty(),
        "type deny-list: {} field(s) of an opaque or unbounded type are on the wire \
         and not allow-listed:\n  {}\nstale allow-list entries: {stale:?}",
        unlisted.len(),
        unlisted.join("\n  ")
    );
}

/// Fields that reach the frame only through `serialize_with`. serde traces them
/// as its private wrapper, which hides the declared type from the check above,
/// so each one is named here: a pass-through wrapper cannot slip a field past
/// the type deny-list without showing up in review.
const SERIALIZER_REDACTED: &[&str] = &[
    "AgentDef.description",
    "DockerConfig.host",
    "OrphanedWorktree.last_commit",
    "Skill.description",
    "Snapshot.hook_health",
    "DepState.detail",
    "ImageSource.base_image",
    "ImageSource.name",
    "McpInstallation.branch",
    "McpInstallation.package",
    "McpInstallation.script",
    "McpInstallation.version",
    "BroadcastPhase::Failed.0",
    "BroadcastPhase::Sent.0",
    "ConfigPopupType::Choice.options",
    "ConfigPopupType::NumberInput.input_buffer",
    "ConfigPopupType::TextInput.value",
    "MarkdownStyle::CodeBlockHeader.0",
    "AgentAuthStatus.has_key",
    "AnswerPhase::Failed.draft_len",
    "AnswerPhase::InFlight.draft_len",
    "AnswerPhase::Failed.reason",
    "AttentionMark.detail",
    "AskState.free_text_len",
    "AtcModeView.help",
    "AttentionOption.description",
    "AttentionOption.label",
    "AuthPane::KeyEntry.buf_len",
    "AuthProviderPopupState.api_key_len",
    "AuthSetupState.api_key_len",
    "AuthSetupState.error_message",
    "BranchPickerState.error",
    "BranchPickerState.filter_len",
    "Broadcast.text_len",
    "Conversation.composer_len",
    "Conversation.send_block",
    "ConversationCard.arguments",
    "ConversationCard.detail",
    "ConversationRow.body",
    "ConversationStatus::Unavailable.detail",
    "BrowseRow.install_uri",
    "BrowseViewState.query_len",
    "BrowseViewState.status",
    "ClaudeChatState.current_streaming_response",
    "ClaudeChatState.input_len",
    "ClaudeChatState.message_count",
    "CloneProgress.error",
    "CloneProgress.url",
    "CommitInfo.message",
    "ConfigPopupType::SecretInput.value_len",
    "ConfigScreenState.edit_len",
    "ConfigScreenState.search_len",
    "ConfigureState.prompt",
    "ConfirmationDialog.message",
    "ConfirmationDialog.warning",
    "ContainerTemplateConfig.environment",
    "DaemonAttention.error",
    "DaemonStatus.last_error",
    "DaemonStatus.reason",
    "DaemonsState.hooks_status",
    "DaemonsState.shared",
    "DepInstall::Error.0",
    "DiffRow.raw",
    "FleetView.daemon_attention",
    "FleetView.fleet_snapshot",
    "GitViewState.commit_message_len",
    "GitViewState.diff_content",
    "GitViewView.quick_commit_message_len",
    "Hunk.rows",
    "ImageSource.build_args",
    "InputState.buffer_len",
    "LogEntry.message",
    "LogHistoryViewerState.error_message",
    "LogHistoryViewerState.search_query_len",
    "MarkdownLine.content",
    "McpInstallation.install_command",
    "McpInstallation.url",
    "McpOverlayState.last_action",
    "McpServerDefinition.args",
    "McpServerDefinition.env",
    "Notification.message",
    "OnboardingState.error_message",
    "OnboardingState.otel_api_token_len",
    "OnboardingState.otel_instance_id_len",
    "OnboardingState.otel_otlp_endpoint",
    "OnboardingState.status_message",
    "OrphanedSession.task",
    "OrphanedWorktree.source_repo",
    "PickRepoState.filter_len",
    "PickRepoState.git_auth_error",
    "PluginsHostView.plugin_render_errors",
    "RecoveryResultLine.detail",
    "RepoCheck::Failed.0",
    "RepoSource::GithubShorthand.owner",
    "RepoSource::GithubShorthand.repo",
    "RepoSource::HttpsUrl.0",
    "RepoSource::SshSession.0",
    "RepoSource::SshUrl.0",
    "RepositoryPreset.custom_rules",
    "RepositoryPreset.environment",
    "SecretValue.reference",
    "Session.boss_prompt",
    "Session.attention",
    "Session.preview_content",
    "Session.recent_logs",
    "SessionAttention.detail",
    "SessionStatus::Error.0",
    "SessionRecoveryState.action_result",
    "SessionRecoveryState.last_error",
    "SessionRecoveryState.search_query_len",
    "ShellSession.preview_content",
    "SkillsScreenData.preview_loading",
    "SkillsScreenData.search_len",
    "SkillsScreenData.source_filter_len",
    "SkillsViewState.search_query_len",
    "SourceRemoveConfirm.source_uri",
    "SourceRow.uri",
    "StatuslineStatus::Other.0",
    "SyncConfirmState.plan",
    "UnitDetail.uri",
    "UnitRow.declared_uri",
    "UnitRow.source",
    "ValidatedPath.error",
    "WorkspaceLoadView.workspace_load_error",
];

#[test]
fn every_field_behind_a_custom_serializer_is_named() {
    isolated_home();
    let trace = shape::trace_states(&shape::sample_states(&mut shape::PlainSeed));
    let wrapped: BTreeSet<String> = trace
        .fields
        .iter()
        .filter(|f| f.rust_type.contains("__SerializeWith"))
        .map(|f| f.owner_field.clone())
        .collect();
    let listed: BTreeSet<String> = SERIALIZER_REDACTED.iter().map(|s| (*s).to_string()).collect();
    let unlisted: Vec<_> = wrapped.difference(&listed).collect();
    let stale: Vec<_> = listed.difference(&wrapped).collect();
    assert!(
        unlisted.is_empty() && stale.is_empty(),
        "fields behind serialize_with not in SERIALIZER_REDACTED: {unlisted:#?}\nstale: {stale:#?}"
    );
}

// ---------------------------------------------------------------------------
// Completeness of the sample
// ---------------------------------------------------------------------------

/// Crate prefixes whose types carry no nested fields worth tracing.
const LEAF_TYPE_PREFIXES: &[&str] = &[
    "alloc::",
    "core::",
    "std::",
    "uuid::",
    "chrono::",
    "serde_json::",
    "toml::",
];

/// Containers and options left empty in the sample on purpose, with the reason.
const UNFILLED_WAIVED: &[(&str, &str)] = &[
    (
        "TmuxView.embed_session",
        "a TmuxSessionName written as a bare string: filled, and a leaf by shape",
    ),
    (
        "AgentCardFrame.wait_kind",
        "a WaitKind unit enum: filled, and a leaf by shape",
    ),
    (
        "HookHealth.hook_binary_mode",
        "a HookBinaryMode unit enum: filled, and a leaf by shape",
    ),
    (
        "DaemonsState.action_requests",
        "DaemonKind and Action enums plus a generation number; no text",
    ),
    ("DaemonsState.error_open", "a DaemonKind unit enum; no text"),
    (
        "DaemonsState.inflight",
        "an Action enum and a generation number (the Instant is skipped); no text",
    ),
    (
        "DaemonsState.menu",
        "a DaemonKind, a cursor and the row's ATC instance name; private constructor, opened only by a key press",
    ),
    (
        "FileTreeItem.status",
        "a GitFileStatus unit enum: filled, and a leaf by shape",
    ),
    (
        "OrphanedWorktree.agent_type",
        "a SessionAgentType unit enum; no text",
    ),
    (
        "PickRepoState.git_auth_status",
        "a GitAuthStatus unit enum: filled, and a leaf by shape",
    ),
    ("Session.codex_model", "a CodexModel unit enum; no text"),
    (
        "SessionFleetMetadata.lifecycle",
        "a LifecycleState unit enum; no text",
    ),
    (
        "SetupMenuState.pending_action",
        "a SetupMenuItem unit enum; no text",
    ),
    (
        "SkillsScreenData.preview",
        "the fetched preview itself is skipped; what remains is checkboxes and a cursor",
    ),
    (
        "Snapshot.evidence_census",
        "probe counts and an EvidenceHealth enum; no text",
    ),
    (
        "UsageConfig.plan",
        "a plan id and provider enum, monthly USD, reset day and the date it was set",
    ),
];

/// A traced field emitted as `null` or an empty container whose element type
/// has fields of its own: everything below it is invisible to the four checks.
fn structured_but_empty(
    field: &ainb_app::wire::trace::FieldNode,
    leaves: &BTreeSet<String>,
) -> bool {
    if !leaves.contains(&field.path) {
        return false;
    }
    // Empty in one instance and filled in another (a second template, a second
    // session) still shows its subtree.
    let filled_elsewhere = leaves.iter().any(|leaf| {
        leaf.len() > field.path.len()
            && leaf.starts_with(&field.path)
            && matches!(leaf.as_bytes()[field.path.len()], b'.' | b'[' | b'{')
    });
    if filled_elsewhere {
        return false;
    }
    let ty = &field.rust_type;
    let container = [
        "Option<",
        "Vec<",
        "HashMap<",
        "BTreeMap<",
        "HashSet<",
        "BTreeSet<",
    ]
    .iter()
    .any(|c| ty.contains(c));
    if !container {
        return false;
    }
    let mut token = String::new();
    let mut structured = false;
    for ch in ty.chars().chain(std::iter::once(' ')) {
        if ch.is_alphanumeric() || ch == '_' || ch == ':' {
            token.push(ch);
            continue;
        }
        if token.contains("::") && !LEAF_TYPE_PREFIXES.iter().any(|p| token.starts_with(p)) {
            structured = true;
        }
        token.clear();
    }
    structured
}

#[test]
fn the_sample_fills_every_structured_subtree() {
    isolated_home();
    let trace = shape::trace_states(&shape::sample_states(&mut shape::PlainSeed));
    let waived: BTreeMap<_, _> = UNFILLED_WAIVED.iter().copied().collect();
    let empty: BTreeSet<String> = trace
        .fields
        .iter()
        .filter(|f| structured_but_empty(f, &trace.leaf_paths))
        .map(|f| format!("{}  ({})", f.owner_field, f.rust_type))
        .collect();
    let unlisted: Vec<_> = empty
        .iter()
        .filter(|e| !waived.contains_key(e.split("  (").next().unwrap_or_default()))
        .collect();
    let owners: BTreeSet<_> =
        empty.iter().map(|e| e.split("  (").next().unwrap_or_default()).collect();
    let stale: Vec<_> = waived.keys().filter(|k| !owners.contains(*k)).collect();
    assert!(
        unlisted.is_empty() && stale.is_empty(),
        "sample leaves structured subtrees empty, so no check sees below them. Fill them in \
         wire::shape::sample_state or waive with a reason:\n{unlisted:#?}\nstale waivers: {stale:?}"
    );
}

/// Text fields left empty or unset in the sample on purpose, with the reason.
const EMPTY_STRING_WAIVED: &[(&str, &str)] = &[(
    "PluginsHostView.plugin_captures_text",
    "screen id to bool: the text is the map key, a plugin screen id, and there are no text values",
)];

/// Whether a declared type is text or a plain collection of text, with no
/// struct of its own inside (those are covered field by field).
fn text_typed(field: &ainb_app::wire::trace::FieldNode) -> bool {
    let ty = &field.rust_type;
    if !(ty.contains("alloc::string::String") || ty.contains("std::path::PathBuf")) {
        return false;
    }
    let mut token = String::new();
    for ch in ty.chars().chain(std::iter::once(' ')) {
        if ch.is_alphanumeric() || ch == '_' || ch == ':' {
            token.push(ch);
            continue;
        }
        if token.contains("::") && !LEAF_TYPE_PREFIXES.iter().any(|p| token.starts_with(p)) {
            return false;
        }
        token.clear();
    }
    true
}

/// An empty string is inert to the name deny-list, the canary and the
/// tripwire, so a text field the sample leaves empty (or `None`, or an empty
/// list) is a field none of them has looked at.
#[test]
fn the_sample_fills_every_string_field() {
    isolated_home();
    let trace = shape::trace_states(&shape::sample_states(&mut shape::PlainSeed));
    let under = |field: &str, path: &str| {
        path == field
            || (path.len() > field.len()
                && path.starts_with(field)
                && matches!(path.as_bytes()[field.len()], b'.' | b'[' | b'{'))
    };
    let mut textual: BTreeSet<(String, String)> = BTreeSet::new();
    for field in &trace.fields {
        let wrapped_text = field.rust_type.contains("__SerializeWith")
            && trace.strings.iter().any(|leaf| under(&field.path, &leaf.path));
        if text_typed(field) || wrapped_text {
            textual.insert((field.owner_field.clone(), field.path.clone()));
        }
    }
    let filled: BTreeSet<&str> = textual
        .iter()
        .filter(|(_, path)| {
            trace
                .strings
                .iter()
                .any(|leaf| !leaf.value.is_empty() && under(path, &leaf.path))
        })
        .map(|(owner, _)| owner.as_str())
        .collect();
    let empty: BTreeSet<&str> = textual
        .iter()
        .map(|(owner, _)| owner.as_str())
        .filter(|owner| !filled.contains(owner))
        .collect();
    let waived: BTreeMap<_, _> = EMPTY_STRING_WAIVED.iter().copied().collect();
    let unlisted: Vec<_> = empty.iter().filter(|o| !waived.contains_key(*o)).collect();
    let stale: Vec<_> = waived.keys().filter(|k| !empty.contains(*k)).collect();
    assert!(
        unlisted.is_empty() && stale.is_empty(),
        "text fields the sample never fills, so no leak check reads them. Seed them in \
         wire::shape or waive with a reason:\n{unlisted:#?}\nstale waivers: {stale:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. Canary
// ---------------------------------------------------------------------------

/// Typed fields the frame shows on purpose, so their marker MUST appear. Every
/// other typed label's marker must not.
const CANARY_SHOWN: &[(&str, &str)] = &[
    (
        "config.number_popup",
        "digits being typed into a number setting, scrubbed; the popup draws them",
    ),
    (
        "fleet.ask.delivered_via",
        "how an answer was delivered, built from the tmux session name; the chip draws it",
    ),
    (
        "new_session.configure.branch_prefix_edit",
        "a branch prefix being typed; a name the Configure form draws",
    ),
    (
        "new_session.configure.session_prefix_edit",
        "a session prefix being typed; a name the Configure form draws",
    ),
    (
        "new_session.configure.save_preset_modal",
        "a preset name being typed; a name the save dialog draws",
    ),
    (
        "config.text_popup",
        "a plain setting's value in the edit popup, scrubbed; secret rows open SecretInput",
    ),
    (
        "new_session.configure.branch_edit",
        "a branch name being typed; the surface draws the field it edits",
    ),
    (
        "new_session.configure.prompt",
        "the Boss prompt editor, scrubbed of credential shapes, not withheld",
    ),
    (
        "onboarding.git_directories_input",
        "repository directories, shown as typed",
    ),
    (
        "onboarding.otel_otlp_endpoint",
        "the OTLP endpoint URL, scrubbed; instance id and token are lengths",
    ),
    (
        "session_labels.rename_buffer",
        "a session display name being typed",
    ),
    (
        "ssh.rename_buffer",
        "an SSH session display name being typed",
    ),
    ("tmux.rename_buffer", "a tmux session name being typed"),
];

#[derive(Default)]
struct CanarySeed {
    typed: Vec<&'static str>,
}

impl CanarySeed {
    fn marker(label: &str) -> String {
        let id: String = label
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    'X'
                }
            })
            .collect();
        format!("CNRY{id}Q")
    }
}

impl Seed for CanarySeed {
    fn text(&mut self, label: &'static str, kind: TextKind) -> String {
        match kind {
            TextKind::Typed => {
                self.typed.push(label);
                Self::marker(label)
            }
            TextKind::Captured => format!("captured {label}"),
        }
    }
}

#[test]
fn no_typed_text_reaches_the_wire() {
    isolated_home();
    let mut seed = CanarySeed::default();
    let states = shape::sample_states(&mut seed);
    let seeded: BTreeSet<_> = seed.typed.iter().copied().collect();
    let declared: BTreeSet<_> = shape::TYPED_LABELS.iter().copied().collect();
    assert_eq!(
        seeded, declared,
        "TYPED_LABELS and the sample builder disagree"
    );

    let blob: String =
        states.iter().flat_map(all_frames).map(|(_, frame)| frame.to_string()).collect();
    let shown: BTreeMap<_, _> = CANARY_SHOWN.iter().copied().collect();
    let mut leaked = Vec::new();
    let mut missing = Vec::new();
    for label in &declared {
        let present = blob.contains(&CanarySeed::marker(label));
        match (shown.contains_key(label), present) {
            (false, true) => leaked.push(*label),
            (true, false) => missing.push(*label),
            _ => {}
        }
    }
    let unknown: Vec<_> = shown.keys().filter(|l| !declared.contains(*l)).collect();
    assert!(
        leaked.is_empty() && missing.is_empty() && unknown.is_empty(),
        "canary: typed text leaked into a frame: {leaked:?}\n\
         shown-on-purpose fields missing from the frame (seed never reached it): {missing:?}\n\
         CANARY_SHOWN names unknown labels: {unknown:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. Value-shaped tripwire
// ---------------------------------------------------------------------------

/// Credential-shaped samples, assembled at runtime so no literal in this file
/// trips a secret scanner. Each captured field gets the next one.
fn credential_samples() -> Vec<String> {
    let run = |c: char, n: usize| c.to_string().repeat(n);
    vec![
        format!("sk-ant-api03-{}", run('A', 40)),
        format!("sk-{}", run('b', 48)),
        format!("ghp_{}", run('C', 36)),
        format!("github_pat_{}", run('d', 82)),
        format!("glpat-{}", run('e', 20)),
        format!("AKIA{}", run('F', 16)),
        format!("AIza{}", run('g', 35)),
        format!(
            "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----",
            run('h', 64)
        ),
        format!("eyJ{}.eyJ{}.{}", run('i', 20), run('j', 20), run('k', 20)),
        format!("https://x-access-token:{}@github.com/o/r.git", run('l', 16)),
        format!("bot123456789:{}", run('m', 35)),
        format!("xoxb-1111-2222-{}", run('n', 24)),
        format!("sk_live_{}", run('S', 24)),
        format!("rk_live_{}", run('R', 24)),
        format!("npm_{}", run('N', 36)),
        format!("pypi-AgEIcHlwaS5vcmc{}", run('P', 60)),
        format!("hf_{}", run('H', 34)),
        format!("dop_v1_{}", run('a', 64)),
        format!("SG.{}.{}", run('G', 22), run('g', 43)),
        format!("xoxc-{}", run('1', 40)),
        format!("xoxd-{}", run('2', 40)),
        format!("AWS_SECRET_ACCESS_KEY={}", run('w', 40)),
    ]
}

struct TripwireSeed {
    samples: Vec<String>,
    next: usize,
    captured: Vec<&'static str>,
}

impl Seed for TripwireSeed {
    fn text(&mut self, label: &'static str, kind: TextKind) -> String {
        match kind {
            TextKind::Typed => format!("typed {label}"),
            TextKind::Captured => {
                self.captured.push(label);
                let sample = &self.samples[self.next % self.samples.len()];
                self.next += 1;
                format!("{label}: export VALUE={sample} done")
            }
        }
    }
}

fn find_in_frame(path: &str, value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => {
            if let Some((shape, _)) = find_secret(s) {
                out.push(format!("{path}: {shape}"));
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                find_in_frame(&format!("{path}[]"), item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, item) in map {
                if let Some((shape, _)) = find_secret(key) {
                    out.push(format!("{path} key: {shape}"));
                }
                find_in_frame(&format!("{path}.{key}"), item, out);
            }
        }
        _ => {}
    }
}

#[test]
fn no_credential_shaped_value_reaches_the_wire() {
    isolated_home();
    let mut seed = TripwireSeed {
        samples: credential_samples(),
        next: 0,
        captured: Vec::new(),
    };
    // Every sample must be one `find_secret` recognises, or the tripwire is blind.
    for sample in &seed.samples {
        assert!(
            find_secret(sample).is_some(),
            "tripwire cannot see {sample}"
        );
    }
    let states = shape::sample_states(&mut seed);
    assert!(
        seed.captured.len() >= seed.samples.len(),
        "every credential shape is seeded at least once"
    );
    let mut found = Vec::new();
    for state in &states {
        for (id, frame) in all_frames(state) {
            find_in_frame(section_name(id), &frame, &mut found);
        }
    }
    // The web needs projection is a wire too (#1081): every credential shape
    // seeded into a daemon card must be scrubbed out of the web card.
    let needs = serde_json::to_value(shape::sample_web_needs(&mut seed)).expect("needs");
    find_in_frame("web_snapshot.needs", &needs, &mut found);
    // So is the cost panel (#1113).
    let cost = serde_json::to_value(shape::sample_web_cost(&mut seed)).expect("cost");
    find_in_frame("web_snapshot.cost", &cost, &mut found);
    assert!(
        found.is_empty(),
        "tripwire: {} credential-shaped value(s) in the frames:\n  {}",
        found.len(),
        found.join("\n  ")
    );
}

#[test]
fn deny_word_matching_is_word_aware() {
    assert_eq!(deny_word_in("edit_buffer"), Some("buffer"));
    assert_eq!(deny_word_in("apiKey"), Some("key"));
    assert_eq!(deny_word_in("GITHUB_TOKEN"), Some("token"));
    assert_eq!(deny_word_in("otel_otlp_endpoint"), Some("endpoint"));
    assert_eq!(
        deny_word_in("temperature"),
        None,
        "`pem` is a word, not a substring"
    );
    assert_eq!(
        deny_word_in("considered"),
        None,
        "`sid` is a word, not a substring"
    );
    assert_eq!(deny_word_in("selected_index"), None);
}

// ---------------------------------------------------------------------------
// Disk output is unchanged
// ---------------------------------------------------------------------------

/// The frame-only redaction must not reach config.toml, presets.toml or the
/// session store: those writes go through the same `Serialize` impls, outside
/// a frame, and have to keep the real values or a save wipes the bot tokens.
/// A choice row is not always a closed registry list: a free-form row such as
/// the preferred editor command is promoted from Text to Choice. Its options
/// are scrubbed on the Config frame, in the settings row and in the popup it
/// opens, the same as a Text row's value (#1153 review).
#[test]
fn a_choice_carrying_a_credential_is_scrubbed_in_the_row_and_the_popup() {
    use ainb_app::app::state::ConfigValue;
    use ainb_app::components::config_popup::ConfigPopupType;
    isolated_home();
    let token = credential_samples()[2].clone();
    let option = format!("code --token {token}");
    let mut state = shape::sample_state(&mut shape::PlainSeed);
    {
        let config = &mut *state.config;
        let row = config
            .config_screen_state
            .settings
            .values_mut()
            .flatten()
            .next()
            .expect("the sample has a settings row");
        row.value = ConfigValue::Choice(vec!["vim".to_string(), option.clone()], 1);
        config.config_popup_state.popup_type = ConfigPopupType::Choice {
            options: vec![option.clone()],
            selected_index: 0,
        };
    }
    let frame = section_json(&state, SectionId::Config, &HostId::local()).to_string();
    assert!(
        !frame.contains(&token),
        "a choice option carried a credential onto the Config frame"
    );
    assert!(
        frame.contains("code --token"),
        "the option's harmless text is kept"
    );
}

#[test]
fn saves_outside_a_frame_keep_what_the_frame_withholds() {
    isolated_home();
    let state = shape::sample_state(&mut shape::PlainSeed);

    let config = toml::to_string(&state.config.app_config).expect("config serialises to TOML");
    assert!(
        config.contains("sample config.fleet.bridge.telegram.token"),
        "bridge table kept"
    );
    assert!(
        config.contains("sample config.mcp.env"),
        "MCP env value kept"
    );
    assert!(
        config.contains("sample config.container.env"),
        "container env value kept"
    );
    assert!(
        config.contains("sample config.mcp.json"),
        "imported MCP blob kept"
    );

    let session = serde_json::to_string(&state.sessions.workspaces[0].sessions[0])
        .expect("session serialises");
    assert!(session.contains("sample session.preview_content"));
    assert!(session.contains("id_ed25519"), "identity file kept on disk");

    let frame = section_json(&state, SectionId::Config, &HostId::local()).to_string();
    assert!(!frame.contains("sample config.fleet.bridge.telegram.token"));
    assert!(!frame.contains("sample config.mcp.env"));
    assert!(!frame.contains("sample config.mcp.json"));
    assert!(!frame.contains("sample config.container.env"));
    assert!(
        frame.contains("plugin:sample:api_token")
            && !frame.contains("sample config.plugins.values"),
        "a plugin config field row keeps its key and loses its value"
    );
    assert!(
        config.contains("sample config.plugins.values"),
        "plugin table kept on disk"
    );
    let sessions = section_json(&state, SectionId::Sessions, &HostId::local()).to_string();
    assert!(
        !sessions.contains("id_ed25519"),
        "identity file left out of the frame"
    );
}

/// The frame-only scrubs #1146 added (npm and python package and version, a
/// claude-docker base image, a detected dependency's detail) leave disk and CLI
/// output alone. Those fields are seeded only by the alternate samples, so the
/// last of them is the one read here.
#[test]
fn frame_only_scrubs_leave_the_saved_config_and_the_dependency_report_alone() {
    struct TokenSeed(String);
    impl Seed for TokenSeed {
        fn text(&mut self, label: &'static str, kind: TextKind) -> String {
            match kind {
                TextKind::Typed => format!("typed {label}"),
                TextKind::Captured => format!("{label} {}", self.0),
            }
        }
    }
    isolated_home();
    let plain = shape::sample_states(&mut shape::PlainSeed)
        .pop()
        .expect("the alternate samples");
    let config = toml::to_string(&plain.config.app_config).expect("config serialises to TOML");
    assert!(
        config.contains("sample config.mcp.npm_package"),
        "npm package kept on disk"
    );
    assert!(
        config.contains("sample config.container.base_image"),
        "base image kept on disk"
    );
    let dependencies = serde_json::to_string(
        &plain
            .onboarding
            .onboarding_state
            .as_ref()
            .and_then(|wizard| wizard.dependency_status.as_ref())
            .expect("the alternate sample reports dependencies"),
    )
    .expect("dependency report serialises");
    assert!(
        dependencies.contains("sample onboarding.dep.version"),
        "a detected version kept outside a frame"
    );

    let token = credential_samples()[2].clone();
    let seeded = shape::sample_states(&mut TokenSeed(token.clone()))
        .pop()
        .expect("the alternate samples");
    let config = toml::to_string(&seeded.config.app_config).expect("config serialises to TOML");
    for label in ["config.mcp.npm_package", "config.container.base_image"] {
        assert!(
            config.contains(&format!("{label} {token}")),
            "{label} kept verbatim on disk"
        );
    }
    let frame = section_json(&seeded, SectionId::Config, &HostId::local()).to_string();
    let onboarding = section_json(&seeded, SectionId::Onboarding, &HostId::local()).to_string();
    for (label, frame) in [
        ("config.mcp.npm_package", &frame),
        ("config.mcp.npm_version", &frame),
        ("config.container.base_image", &frame),
        ("onboarding.dep.version", &onboarding),
    ] {
        assert!(frame.contains(label), "{label} reaches its frame");
        assert!(
            !frame.contains(&format!("{label} {token}")),
            "{label} reached its frame unscrubbed"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Payload-carrying enum variants (#1145)
// ---------------------------------------------------------------------------

/// The generated bindings: the declared shape of everything on the wire.
const BINDINGS: &str = include_str!("../bindings/AppState.ts");

/// Payload variants no section sample reaches yet, each one a value the four
/// leak checks and the key-path fixture have never seen (#1146).
///
/// `SessionStatus::Error` was exactly this until #1144 seeded it: it shipped a
/// raw string past every gate. These are the same class, found when this test
/// was written; the fix for each is to seed it in `wire::shape::sample_state`
/// and regenerate the fixture, not to extend this list. The one case seeding
/// cannot fix is a field the bindings still declare but the frame never emits
/// (behind `omit_in_frame`): name it here with that reason. A line that is no
/// longer missing fails too, so the list cannot go stale.
const UNSEEDED_VARIANTS: &[(&str, &str)] = &[];

/// The variants that carry each scrubbed field of an internally tagged enum.
///
/// The tracer names those fields by the enum alone (`McpInstallation.package`),
/// because serde writes the variant as the tag's value. `SERIALIZER_REDACTED`
/// therefore cannot say WHICH variants its entry was triaged for, and a new
/// `Pip { package }` would inherit the pass unread. Pinning the carriers makes
/// that new variant change the set and fail here instead (#1153 review).
const TAGGED_SCRUB_CARRIERS: &[(&str, &str)] = &[
    ("DepState.detail", "DepState::ok|alt|too_old"),
    ("ImageSource.base_image", "ImageSource::ClaudeDocker"),
    ("ImageSource.name", "ImageSource::Image"),
    ("McpInstallation.branch", "McpInstallation::Git"),
    ("McpInstallation.package", "McpInstallation::Npm|Python"),
    ("McpInstallation.script", "McpInstallation::Custom"),
    ("McpInstallation.version", "McpInstallation::Npm|Python"),
];

/// Every externally tagged enum variant with a payload, reachable from a
/// section's view type, is seeded by the sample (#1145).
///
/// The tracer walks the values `section_json` emits, so an UNSEEDED variant is
/// invisible to every gate in this file: add `SessionStatus::Failed(String)`
/// and leave the sample on `Error`, and the new variant ships verbatim. The
/// generated bindings declare what the wire can carry, so walking them from
/// each section's view type gives the variants that must appear; the committed
/// key-path fixture is what the sample actually reached.
///
/// Internally tagged enums (`{ type: "Npm", ... }`) are gated by their FIELDS
/// (#1146): every field beside the tag is a leaf the fixture must hold, named
/// in a failure by the variants that carry it (`McpInstallation::Npm|Python`).
/// What that still cannot see:
/// - a variant with no field besides its tag (`PreInstalled`, `Missing`), since
///   the variant name is the tag's value and no key path carries it;
/// - a variant whose fields are all shared with a seeded one (`Python` beside
///   `Npm`), since the leaf is already present;
/// - a payload field the bindings omit (`specta(skip)`, as on
///   `McpServerDefinition::Json.config`), since the walk never sees it.
#[test]
fn every_payload_carrying_variant_reachable_from_a_section_is_seeded() {
    let bindings = Bindings::parse(BINDINGS);
    let expected = bindings.payload_variant_paths();
    let name_of = |path: &String| {
        format!(
            "{}::{}",
            expected[path],
            path.rsplit('.').next().unwrap_or(path)
        )
    };
    assert!(
        expected.len() > 30,
        "the bindings walk found only {} payload variants, so it has drifted \
         from the generated shape and proves nothing",
        expected.len()
    );
    let committed = shape::committed_key_paths();
    let reached = |variant: &String| {
        committed.contains(variant)
            || committed.iter().any(|path| {
                path.starts_with(&format!("{variant}."))
                    || path.starts_with(&format!("{variant}["))
                    || path.starts_with(&format!("{variant}{{"))
            })
    };

    // Tagged field -> the variants carrying it, as the walk found them.
    let mut carriers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (path, owner) in &expected {
        if let Some((enum_name, _)) = owner.split_once("::") {
            let field = path.rsplit('.').next().unwrap_or(path);
            carriers
                .entry(format!("{enum_name}.{field}"))
                .or_default()
                .insert(owner.clone());
        }
    }
    let drifted: Vec<String> = TAGGED_SCRUB_CARRIERS
        .iter()
        .filter(|(field, pinned)| {
            carriers
                .get(*field)
                .is_none_or(|found| found.iter().any(|owner| owner != pinned))
        })
        .map(|(field, pinned)| {
            format!("{field}: pinned {pinned}, found {:?}", carriers.get(*field))
        })
        .collect();
    assert!(
        drifted.is_empty(),
        "a scrubbed tagged field is carried by other variants than it was triaged \
         for; re-triage it and update TAGGED_SCRUB_CARRIERS:\n{drifted:#?}"
    );

    let allowed: BTreeSet<&str> = UNSEEDED_VARIANTS.iter().map(|(path, _)| *path).collect();
    let missing: Vec<String> = expected
        .keys()
        .filter(|variant| !reached(variant) && !allowed.contains(variant.as_str()))
        .map(|variant| format!("{} at {variant}", name_of(variant)))
        .collect();
    assert!(
        missing.is_empty(),
        "a payload-carrying wire variant that no section sample seeds: its \
         payload has never been through a leak check. Seed it in \
         `wire::shape::sample_state` and regenerate the key-path fixture:\n{missing:#?}"
    );

    let stale: Vec<&str> = UNSEEDED_VARIANTS
        .iter()
        .map(|(path, _)| *path)
        .filter(|variant| {
            let variant = (*variant).to_string();
            reached(&variant) || !expected.contains_key(&variant)
        })
        .collect();
    assert!(
        stale.is_empty(),
        "UNSEEDED_VARIANTS names a variant that is now seeded or no longer \
         exists; drop these lines:\n{stale:#?}"
    );
}

/// The generated bindings, parsed into `type name -> body`.
struct Bindings {
    aliases: BTreeMap<String, String>,
}

impl Bindings {
    fn parse(source: &str) -> Self {
        let mut aliases = BTreeMap::new();
        let mut rest = source;
        while let Some(start) = rest.find("export type ") {
            let after = &rest[start + "export type ".len()..];
            let Some(equals) = after.find('=') else { break };
            let name = after[..equals].trim().to_string();
            let Some(end) = end_of_declaration(after, equals + 1) else {
                break;
            };
            aliases.insert(name, strip_comments(&after[equals + 1..end]));
            rest = &after[end..];
        }
        Self { aliases }
    }

    /// `"<section>.<path to the enum>.<Variant>"` for every payload-carrying
    /// variant reachable from a section's view type.
    fn payload_variant_paths(&self) -> BTreeMap<String, String> {
        let mut found = BTreeMap::new();
        for id in SectionId::ALL {
            let root = format!("{id:?}View");
            let mut chain = Vec::new();
            self.walk(&root, section_name(id), &mut chain, &mut found);
        }
        found
    }

    /// Walk `ty` as it would be serialised at `path`, recording variant paths.
    ///
    /// `chain` holds the named types currently being walked, so a type that
    /// contains itself terminates instead of recursing forever.
    fn walk(
        &self,
        ty: &str,
        path: &str,
        chain: &mut Vec<String>,
        found: &mut BTreeMap<String, String>,
    ) {
        let ty = strip_comments(ty);
        let ty = ty.trim();
        // `T | null` is an optional `T`, not an enum.
        let members = split_top_level(ty, '|');
        let real: Vec<&String> = members
            .iter()
            .filter(|member| !matches!(member.trim(), "null" | "undefined"))
            .collect();
        if real.len() > 1 {
            for member in real {
                let member = member.trim().trim_start_matches('(').trim();
                let Some(inner) = member.strip_prefix('{') else {
                    // A bare string literal is a unit variant: no payload.
                    continue;
                };
                let Some((key, value)) = first_entry(inner) else {
                    continue;
                };
                if key.ends_with('?') {
                    // `Name?: never` names a variant this member is NOT.
                    continue;
                }
                if !key.starts_with(|c: char| c.is_ascii_uppercase()) {
                    // An internally tagged enum: every member leads with the
                    // same tag field (`kind`), and the variant name is that
                    // field's VALUE, which no key path can carry. The tag
                    // itself is already a leaf the fixture locks. Its PAYLOAD
                    // still reaches the wire as ordinary fields beside the
                    // tag, so those are walked here (#1145), and each one is
                    // recorded as a leaf the fixture must hold, named by the
                    // variant that carries it (#1146).
                    let fields = struct_fields(member);
                    let tag = fields
                        .iter()
                        .find(|(field, _)| *field == key)
                        .map_or("?", |(_, value)| value.trim().trim_matches('"'))
                        .to_string();
                    let owner = chain.last().map_or_else(
                        || "(inline enum)".to_string(),
                        |name| name.trim_end_matches("_Serialize").to_string(),
                    );
                    for (field, field_ty) in &fields {
                        if *field == key {
                            continue;
                        }
                        let field_path = format!("{path}.{field}");
                        found
                            .entry(field_path.clone())
                            .and_modify(|carriers| carriers.push_str(&format!("|{tag}")))
                            .or_insert_with(|| format!("{owner}::{tag}"));
                        self.walk(field_ty, &field_path, chain, found);
                    }
                    continue;
                }
                let variant_path = format!("{path}.{key}");
                // The enum is the named type being walked: `chain` holds the
                // alias chain, and its last entry is the type this union is
                // the body of. An inline union has none.
                let owner = chain.last().map_or_else(
                    || "(inline enum)".to_string(),
                    |name| name.trim_end_matches("_Serialize").to_string(),
                );
                found.insert(variant_path.clone(), owner);
                self.walk(&value, &variant_path, chain, found);
            }
            return;
        }
        let ty = real.first().map_or("", |member| member.trim());
        let ty = ty.trim_start_matches('(').trim_end_matches(')').trim();
        if let Some(inner) = ty.strip_suffix("[]") {
            self.walk(inner, &format!("{path}[]"), chain, found);
            return;
        }
        // `Partial<T>` only makes T's fields optional: the shape underneath is
        // what reaches the wire.
        if let Some(inner) = ty.strip_prefix("Partial<").and_then(|rest| rest.strip_suffix('>')) {
            self.walk(inner, path, chain, found);
            return;
        }
        if let Some(value) = map_value_type(ty) {
            self.walk(&value, &format!("{path}{{}}"), chain, found);
            return;
        }
        if ty.starts_with('{') {
            for (field, field_ty) in struct_fields(ty) {
                self.walk(&field_ty, &format!("{path}.{field}"), chain, found);
            }
            return;
        }
        // A tuple: serde writes it as a JSON array, and the fixture as `[]`.
        if let Some(elements) = ty.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
            for element in split_top_level(elements, ',') {
                let element = element.trim();
                if !element.is_empty() {
                    self.walk(element, &format!("{path}[]"), chain, found);
                }
            }
            return;
        }
        // A named type: resolve it, unless it is already being walked.
        let Some(body) = self.aliases.get(ty) else {
            return;
        };
        if chain.iter().any(|seen| seen == ty) {
            return;
        }
        chain.push(ty.to_string());
        let body = body.clone();
        self.walk(&body, path, chain, found);
        chain.pop();
    }
}

/// The index of the `;` that ends a declaration started at `from`.
fn end_of_declaration(text: &str, from: usize) -> Option<usize> {
    let mut depth = 0_i32;
    for (index, byte) in text.as_bytes().iter().enumerate().skip(from) {
        match byte {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth -= 1,
            b';' if depth == 0 => return Some(index),
            _ => {}
        }
    }
    None
}

/// Split an object body into its entries. specta writes a struct's fields
/// `a: A, b: B` but an internally tagged variant's `type: "Npm"; package: P`,
/// so both separators end an entry.
fn split_entries(body: &str) -> Vec<String> {
    split_top_level(body, ',')
        .iter()
        .flat_map(|part| split_top_level(part, ';'))
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

/// Split on `separator` at nesting depth zero, outside string literals.
fn split_top_level(text: &str, separator: char) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth = 0_i32;
    let mut quote = None::<char>;
    for ch in text.chars() {
        match (quote, ch) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(ch),
            (None, '{' | '(' | '[' | '<') => depth += 1,
            (None, '}' | ')' | ']' | '>') => depth -= 1,
            (None, c) if c == separator && depth == 0 => {
                parts.push(std::mem::take(&mut current));
                continue;
            }
            (None, _) => {}
        }
        current.push(ch);
    }
    parts.push(current);
    parts
}

/// `(key, value type)` of the first entry of an object body, given its inside.
///
/// The value ENDS at the object's own closing brace, not at the end of the
/// member: specta writes an exclusive union member as
/// `({ Sent: FleetActionReceipt[] }) & { Failed?: never }`, so everything from
/// the `&` on belongs to the exclusion, not to the payload.
fn first_entry(inner: &str) -> Option<(String, String)> {
    let entry = split_entries(inner).into_iter().next()?;
    let colon = split_top_level(&entry, ':');
    if colon.len() < 2 {
        return None;
    }
    let key = colon[0].trim().trim_matches('"').trim().to_string();
    let value = colon[1..].join(":");
    let value = value[..balanced_end(&value)].trim().to_string();
    Some((key, value))
}

/// The byte index at which `value` stops being the first object's content: the
/// `}` that closes the object the entry sits in, or the whole string when the
/// value is not wrapped in one.
fn balanced_end(value: &str) -> usize {
    let mut depth = 0_i32;
    let mut quote = None::<char>;
    for (index, ch) in value.char_indices() {
        match (quote, ch) {
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(ch),
            (None, '{' | '(' | '[' | '<') => depth += 1,
            (None, '}' | ')' | ']' | '>') => {
                if depth == 0 {
                    return index;
                }
                depth -= 1;
            }
            (None, _) => {}
        }
    }
    value.len()
}

/// Every `(field, type)` of an object type, optional markers stripped.
fn struct_fields(ty: &str) -> Vec<(String, String)> {
    let inner = ty.trim().trim_start_matches('{').trim_end().trim_end_matches('}');
    split_entries(inner)
        .into_iter()
        .filter_map(|entry| {
            let parts = split_top_level(&entry, ':');
            if parts.len() < 2 {
                return None;
            }
            let field = parts[0].trim().trim_matches('"').trim_end_matches('?').trim();
            if field.is_empty() || field.starts_with('[') {
                return None;
            }
            Some((field.to_string(), parts[1..].join(":").trim().to_string()))
        })
        .collect()
}

/// The value type of a map type (`Record<string, T>`, `{ [key in string]: T }`).
fn map_value_type(ty: &str) -> Option<String> {
    let ty = ty.trim();
    if let Some(args) = ty.strip_prefix("Record<").and_then(|rest| rest.strip_suffix('>')) {
        let parts = split_top_level(args, ',');
        return parts.get(1).map(|value| value.trim().to_string());
    }
    let inner = ty.strip_prefix('{')?.trim_end().strip_suffix('}')?;
    let inner = inner.trim();
    if !inner.starts_with('[') {
        return None;
    }
    let colon = inner.find(']')?;
    let value = inner[colon + 1..].trim().strip_prefix(':')?;
    Some(value.trim().trim_end_matches(',').trim().to_string())
}

/// Drop `/* ... */` blocks so prose never parses as structure.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find("*/") else {
            return out;
        };
        rest = &rest[start + end + 2..];
    }
    out.push_str(rest);
    out
}
