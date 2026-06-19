// ABOUTME: Idempotent read-preserve-modify-write merge of the ATC lifecycle hook
// set into Claude Code's `~/.claude/settings.json`.
//
// agent-deck injects its orchestration hooks by *merging* a managed block into
// the user's existing settings, never by overwriting — so a host that already
// runs the reflect plugin's `Stop`/`PreCompact`/`SessionStart`/`UserPromptSubmit`
// hooks and the ainb-hooks/notifyd `Notification`/`Stop` hooks keeps all of them.
// This module mirrors that contract for ainb:
//
//   * Every ATC-managed hook entry is tagged `"_ainb_atc_managed": true` so a
//     re-install replaces exactly our prior entries and nothing else
//     (idempotent), and uninstall strips exactly ours.
//   * User-authored hooks AND other tools' hooks (reflect, notifyd) on the same
//     event are preserved verbatim — we append our entry to the event's array,
//     we never replace the array.
//   * The full lifecycle event set is added: `SessionStart` → waiting,
//     `UserPromptSubmit` → running (+ budget reset), `Stop` → waiting + drain,
//     `Notification`, `SessionEnd` → dead.
//
// The Stop hook is the load-bearing one: it runs the synchronous drain that
// turns child completions into the parent's next turn (see `drain.rs`). All
// other events just write the session's status file.

use serde_json::{Map, Value, json};

/// JSON key tagging a hook entry as ATC-managed (for idempotent re-merge +
/// clean uninstall). Stable — never rename without a migration.
pub const ATC_MANAGED_KEY: &str = "_ainb_atc_managed";

/// The lifecycle events ATC injects, in stable order, each paired with its
/// matcher. Most lifecycle events use the all-matcher `""`; `PreToolUse` is
/// scoped to the `AskUserQuestion` tool so ASK is hook-native (a PreToolUse with
/// `""` would fire on EVERY tool call — far too hot). `StopFailure` reports an
/// exhausted-retry API error (observational; it cannot block). Each maps to a
/// command that invokes the canonical hook script with the matching
/// `AINB_HOOK_EVENT` so the script forwards the right event + matcher.
pub const ATC_EVENTS: &[(&str, &str)] = &[
    ("SessionStart", ""),
    ("UserPromptSubmit", ""),
    ("Stop", ""),
    ("Notification", ""),
    ("SessionEnd", ""),
    ("PreToolUse", "AskUserQuestion"),
    ("StopFailure", ""),
];

/// Build the ATC-managed hook entry for `event` (scoped to `matcher`), pointing
/// at `hook_script`.
///
/// The command sets `AINB_HOOK_EVENT=<event>` so the shared script branches
/// correctly, `AINB_HOOK_MATCHER=<matcher>` so the appended event line records
/// its discriminator (e.g. `AskUserQuestion`), and `AINB_MANAGED=atc` so it
/// knows it is running under ATC management (events.jsonl append + inbox writes).
/// Returns a single matcher block carrying one command hook — the Claude
/// settings shape (matcher `""` = all events of this type).
#[must_use]
pub fn managed_entry(event: &str, matcher: &str, hook_script: &str) -> Value {
    let command = format!(
        "AINB_HOOK_EVENT={event} AINB_HOOK_MATCHER={matcher} AINB_MANAGED=atc {hook_script}"
    );
    json!({
        ATC_MANAGED_KEY: true,
        "matcher": matcher,
        "hooks": [
            { "type": "command", "command": command, "timeout": 10 }
        ]
    })
}

/// Merge the ATC lifecycle hooks into a parsed `settings.json` value, preserving
/// every existing hook (user-authored, reflect, notifyd). Idempotent: a second
/// call with the same `hook_script` produces an identical result.
///
/// Pure on `Value` so it is exhaustively unit-testable without touching disk.
#[must_use]
pub fn merge_into(mut settings: Value, hook_script: &str) -> Value {
    // Ensure a top-level object.
    if !settings.is_object() {
        settings = Value::Object(Map::new());
    }
    let root = settings.as_object_mut().expect("ensured object");

    // Ensure a `hooks` object.
    let hooks = root.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    if !hooks.is_object() {
        *hooks = Value::Object(Map::new());
    }
    let hooks = hooks.as_object_mut().expect("ensured object");

    for (event, matcher) in ATC_EVENTS {
        let entry = managed_entry(event, matcher, hook_script);
        let arr = hooks.entry((*event).to_string()).or_insert_with(|| Value::Array(Vec::new()));
        if !arr.is_array() {
            *arr = Value::Array(Vec::new());
        }
        let arr = arr.as_array_mut().expect("ensured array");
        // Drop any prior ATC-managed entry for this event (idempotent re-merge),
        // keep everything else (reflect / notifyd / user hooks).
        arr.retain(|e| !is_atc_managed(e));
        arr.push(entry);
    }

    settings
}

/// Strip every ATC-managed hook entry from a parsed `settings.json` value,
/// leaving all other hooks (reflect, notifyd, user) intact. Empty event arrays
/// are removed so uninstall leaves no noise.
#[must_use]
pub fn strip_from(mut settings: Value) -> Value {
    let Some(root) = settings.as_object_mut() else {
        return settings;
    };
    let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) else {
        return settings;
    };
    for (_event, arr) in hooks.iter_mut() {
        if let Some(a) = arr.as_array_mut() {
            a.retain(|e| !is_atc_managed(e));
        }
    }
    // Drop now-empty event arrays.
    hooks.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    settings
}

/// Whether a hook entry is one ATC manages (tagged, or — defensively — a command
/// that invokes the script with our `AINB_MANAGED=atc` marker).
#[must_use]
pub fn is_atc_managed(entry: &Value) -> bool {
    if entry.get(ATC_MANAGED_KEY).and_then(Value::as_bool).unwrap_or(false) {
        return true;
    }
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(Value::as_str)
                    .map(|s| s.contains("AINB_MANAGED=atc"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settings.json carrying BOTH the reflect plugin's hooks and the
    /// ainb-hooks/notifyd hooks — the exact coexistence the goal requires.
    fn settings_with_reflect_and_notifyd() -> Value {
        json!({
            "hooks": {
                "SessionStart": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/skills/recall/hooks/session_start_recall.py" }
                    ]}
                ],
                "UserPromptSubmit": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/skills/recall/hooks/user_prompt_submit_recall.py" }
                    ]}
                ],
                "PostToolUse": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/hooks/posttooluse_minilearning.py" }
                    ]}
                ],
                "Stop": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/hooks/stop_reflect.py" }
                    ]},
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "AINB_AGENT=claude /x/notify.sh" }
                    ]}
                ],
                "PreCompact": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "uv run ${CLAUDE_PLUGIN_ROOT}/hooks/precompact_reflect.py --auto --verbose" }
                    ]}
                ],
                "Notification": [
                    { "matcher": "", "hooks": [
                        { "type": "command", "command": "AINB_AGENT=claude /x/notify.sh" }
                    ]}
                ]
            }
        })
    }

    fn commands_for(settings: &Value, event: &str) -> Vec<String> {
        settings["hooks"][event]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .flat_map(|e| e["hooks"].as_array().cloned().unwrap_or_default())
                    .filter_map(|h| h["command"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn merge_preserves_reflect_and_notifyd_hooks() {
        let merged = merge_into(settings_with_reflect_and_notifyd(), "/x/notify.sh");

        // Reflect's SessionStart survives, ours is added.
        let ss = commands_for(&merged, "SessionStart");
        assert!(
            ss.iter().any(|c| c.contains("session_start_recall.py")),
            "reflect lost: {ss:?}"
        );
        assert!(
            ss.iter().any(|c| c.contains("AINB_HOOK_EVENT=SessionStart")),
            "ours missing: {ss:?}"
        );

        // Reflect's PostToolUse + PreCompact are untouched (we don't manage them).
        assert!(
            commands_for(&merged, "PostToolUse")
                .iter()
                .any(|c| c.contains("posttooluse_minilearning.py"))
        );
        assert!(
            commands_for(&merged, "PreCompact")
                .iter()
                .any(|c| c.contains("precompact_reflect.py"))
        );

        // Stop: reflect's stop_reflect AND notifyd's notify.sh AND ours all present.
        let stop = commands_for(&merged, "Stop");
        assert!(
            stop.iter().any(|c| c.contains("stop_reflect.py")),
            "reflect Stop lost: {stop:?}"
        );
        assert!(
            stop.iter().any(|c| c.contains("AINB_AGENT=claude /x/notify.sh")),
            "notifyd Stop lost: {stop:?}"
        );
        assert!(
            stop.iter().any(|c| c.contains("AINB_HOOK_EVENT=Stop")),
            "ATC Stop drain missing: {stop:?}"
        );

        // SessionEnd is added fresh (no prior hooks).
        assert!(
            commands_for(&merged, "SessionEnd")
                .iter()
                .any(|c| c.contains("AINB_HOOK_EVENT=SessionEnd"))
        );
    }

    #[test]
    fn merge_is_idempotent() {
        let once = merge_into(settings_with_reflect_and_notifyd(), "/x/notify.sh");
        let twice = merge_into(once.clone(), "/x/notify.sh");
        assert_eq!(once, twice, "second merge changed the result");
        // And exactly one ATC-managed Stop entry exists after re-merge.
        let atc_stop = twice["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| is_atc_managed(e))
            .count();
        assert_eq!(atc_stop, 1, "duplicate ATC Stop entries after re-merge");
    }

    #[test]
    fn merge_adds_full_lifecycle_event_set() {
        let merged = merge_into(json!({}), "/x/notify.sh");
        for (event, matcher) in ATC_EVENTS {
            let arr = merged["hooks"][event].as_array().expect("event added");
            let entry = arr.iter().find(|e| is_atc_managed(e)).expect("ATC entry present");
            assert_eq!(
                entry["matcher"].as_str(),
                Some(*matcher),
                "event {event} matcher mismatch"
            );
        }
        // PreToolUse is scoped to AskUserQuestion (NOT the all-matcher), so it
        // doesn't fire on every tool call.
        let pre = &merged["hooks"]["PreToolUse"];
        assert!(pre.is_array(), "PreToolUse installed");
        let cmd = pre[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("AINB_HOOK_MATCHER=AskUserQuestion"),
            "PreToolUse forwards its matcher: {cmd}"
        );
        // StopFailure installed (observational ERR source).
        assert!(merged["hooks"]["StopFailure"].is_array());
    }

    #[test]
    fn merge_on_empty_or_non_object_settings() {
        // Null / non-object settings are coerced to an object with hooks.
        let merged = merge_into(Value::Null, "/x/notify.sh");
        assert!(merged["hooks"]["Stop"].is_array());
        let merged2 = merge_into(json!("garbage-string"), "/x/notify.sh");
        assert!(merged2["hooks"]["Stop"].is_array());
    }

    #[test]
    fn strip_removes_only_atc_entries() {
        let merged = merge_into(settings_with_reflect_and_notifyd(), "/x/notify.sh");
        let stripped = strip_from(merged);

        // ATC entries gone everywhere.
        for (event, _matcher) in ATC_EVENTS {
            let atc = stripped["hooks"][event]
                .as_array()
                .map(|a| a.iter().filter(|e| is_atc_managed(e)).count())
                .unwrap_or(0);
            assert_eq!(atc, 0, "ATC entry survived strip on {event}");
        }
        // But reflect + notifyd survive.
        assert!(
            commands_for(&stripped, "SessionStart")
                .iter()
                .any(|c| c.contains("session_start_recall.py"))
        );
        assert!(commands_for(&stripped, "Stop").iter().any(|c| c.contains("stop_reflect.py")));
        assert!(commands_for(&stripped, "Stop").iter().any(|c| c.contains("notify.sh")));
        assert!(commands_for(&stripped, "Notification").iter().any(|c| c.contains("notify.sh")));
    }

    #[test]
    fn strip_then_merge_round_trips_to_merged() {
        // merge → strip → merge yields the same as a single merge (no drift).
        let base = settings_with_reflect_and_notifyd();
        let merged_once = merge_into(base.clone(), "/x/notify.sh");
        let round = merge_into(strip_from(merged_once.clone()), "/x/notify.sh");
        assert_eq!(merged_once, round);
    }
}
