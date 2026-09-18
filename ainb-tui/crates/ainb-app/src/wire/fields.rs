// ABOUTME: Field-level serializers the redaction layer is built from. They sit
// on the TYPES as `#[serde(...)]` attributes, so no call site can forget them.
//
// Two families:
//
// * Frame-only (`*_in_frame`). For types that ALSO serialise to disk or to a
//   CLI: `AppConfig` (config.toml), `RepositoryPreset` (presets.toml),
//   `Session` (the session store), `LogEntry`, `DaemonStatus`. Their normal
//   `Serialize` must keep writing the real value, so these helpers redact only
//   while [`serialize_section`](super::serialize_section) is running, which
//   sets a thread-local flag for exactly that call. A disk write never sees it.
//
// * Always. For renderer state that exists only to be drawn (`ConfigScreenState`,
//   the onboarding forms, the composers). It never touches disk, so a credential
//   buffer serialises as its length and captured text through `redact::scrub`
//   unconditionally.

use crate::fleet::bridge::redact::{REDACTED, scrub, scrub_lines as scrub_text_lines};
use serde::ser::SerializeMap;
use serde::{Serialize, Serializer};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

thread_local! {
    static IN_FRAME: Cell<bool> = const { Cell::new(false) };
}

/// Marks the current thread as building a mirror frame until dropped.
pub(super) struct FrameScope {
    was: bool,
}

impl FrameScope {
    pub(super) fn enter() -> Self {
        Self {
            was: IN_FRAME.with(|flag| flag.replace(true)),
        }
    }
}

impl Drop for FrameScope {
    fn drop(&mut self) {
        IN_FRAME.with(|flag| flag.set(self.was));
    }
}

/// True while a section frame is being serialised on this thread.
#[must_use]
pub fn in_frame() -> bool {
    IN_FRAME.with(Cell::get)
}

// ---- frame-only --------------------------------------------------------------

/// `skip_serializing_if`: leave the field out of a frame, keep it on disk.
#[must_use]
pub fn omit_in_frame<T>(_: &T) -> bool {
    in_frame()
}

/// `skip_serializing_if` for an optional field that is also omitted when empty.
#[must_use]
pub fn omit_in_frame_or_none<T>(value: &Option<T>) -> bool {
    in_frame() || value.is_none()
}

/// `skip_serializing_if`: carry the field in a frame only, never to disk.
#[must_use]
pub fn omit_outside_frame<T>(_: &T) -> bool {
    !in_frame()
}

/// A session's merged attention chips, as each chip's kind and scrubbed detail
/// ([`AttentionMark`](crate::fleet::attention::AttentionMark)).
pub fn attention_marks<S: Serializer>(
    chips: &[crate::fleet::attention::SessionAttention],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(chips.iter().map(crate::fleet::attention::AttentionMark::from))
}

/// Captured text: scrubbed in a frame, verbatim elsewhere.
pub fn scrub_in_frame<S: Serializer>(value: &str, serializer: S) -> Result<S::Ok, S::Error> {
    if in_frame() {
        serializer.serialize_str(&scrub(value))
    } else {
        serializer.serialize_str(value)
    }
}

/// Optional captured text: scrubbed in a frame, verbatim elsewhere.
pub fn scrub_opt_in_frame<S: Serializer>(
    value: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(text) => scrub_in_frame(text, serializer),
        None => serializer.serialize_none(),
    }
}

/// A list of arguments or lines: each scrubbed in a frame, verbatim elsewhere.
pub fn scrub_vec_in_frame<S: Serializer>(
    value: &[String],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if in_frame() {
        serializer.collect_seq(scrub_text_lines(value))
    } else {
        serializer.collect_seq(value)
    }
}

/// An environment map. In a frame the keys stay (a renderer shows which
/// variables are set) and every value is [`REDACTED`]; elsewhere verbatim.
pub fn env_values_in_frame<H: std::hash::BuildHasher, S: Serializer>(
    env: &HashMap<String, String, H>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if !in_frame() {
        return env.serialize(serializer);
    }
    let mut keys: Vec<&String> = env.keys().collect();
    keys.sort();
    let mut map = serializer.serialize_map(Some(keys.len()))?;
    for key in keys {
        map.serialize_entry(key, REDACTED)?;
    }
    map.end()
}

// ---- always -------------------------------------------------------------------

/// A credential or private buffer, as its character count. A renderer draws a
/// masked run or a caret from it; the text never serialises.
pub fn char_count<S: Serializer>(value: &str, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u64(value.chars().count() as u64)
}

/// An optional private buffer, as its character count (`null` when closed).
pub fn opt_char_count<S: Serializer>(
    value: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(text) => char_count(text, serializer),
        None => serializer.serialize_none(),
    }
}

/// An open-or-closed private buffer, as whether it is open.
pub fn is_some<T, S: Serializer>(value: &Option<T>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_bool(value.is_some())
}

/// A private list (a conversation, a transcript), as how many entries it has.
pub fn len_of<T, S: Serializer>(value: &[T], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u64(value.len() as u64)
}

/// Text a frame must not carry in any form, as [`REDACTED`].
pub fn withheld<S: Serializer>(_: &str, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(REDACTED)
}

/// Captured text in renderer-only state, scrubbed.
pub fn scrub_str<S: Serializer>(value: &str, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&scrub(value))
}

/// Optional captured text in renderer-only state, scrubbed.
pub fn scrub_opt<S: Serializer>(value: &Option<String>, serializer: S) -> Result<S::Ok, S::Error> {
    value.as_deref().map(scrub).serialize(serializer)
}

/// Captured lines in renderer-only state, scrubbed as one text so a key block
/// spanning lines is redacted whole.
pub fn scrub_lines<S: Serializer>(value: &[String], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(scrub_text_lines(value))
}

/// Arbitrary JSON a plugin or daemon published: every string (keys included)
/// scrubbed, the structure kept.
pub fn scrub_json<S: Serializer>(
    value: &serde_json::Value,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    fn walk(value: &serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::String(text) => serde_json::Value::String(scrub(text)),
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(walk).collect())
            }
            serde_json::Value::Object(map) => serde_json::Value::Object(
                map.iter().map(|(key, item)| (scrub(key), walk(item))).collect(),
            ),
            other => other.clone(),
        }
    }
    walk(value).serialize(serializer)
}

/// A multi-line editor's text, scrubbed line by line; the cursor stays private.
pub fn scrub_editor<S: Serializer>(
    editor: &crate::text_editor::TextEditor,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    scrub_lines(editor.get_lines(), serializer)
}

/// `SecretValue.reference` as its SOURCE, never its value: `""` when unset, the
/// reference itself for `$ENV_VAR` and `keychain:<service>`, and `<literal>`
/// for a plaintext secret sitting in config.toml.
pub fn secret_source<S: Serializer>(reference: &str, serializer: S) -> Result<S::Ok, S::Error> {
    let shown = if reference.trim().is_empty() {
        ""
    } else if reference.starts_with('$') || reference.starts_with("keychain:") {
        reference
    } else {
        "<literal>"
    };
    serializer.serialize_str(shown)
}

/// Broadcast receipts with each leg's `detail` scrubbed: the daemon echoes
/// transport errors there.
pub fn scrub_receipts<S: Serializer>(
    receipts: &[ainb_hangar_proto::fleet::FleetActionReceipt],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.collect_seq(receipts.iter().map(|receipt| {
        ainb_hangar_proto::fleet::FleetActionReceipt {
            detail: receipt.detail.as_deref().map(scrub),
            ..receipt.clone()
        }
    }))
}

/// A poller-published cell: serialise the value it holds right now. A poisoned
/// lock still holds the last complete value, so it is read anyway.
pub fn locked_shared<T: Serialize, S: Serializer>(
    cell: &Option<Arc<Mutex<T>>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match cell {
        Some(cell) => cell
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .serialize(serializer),
        None => serializer.serialize_none(),
    }
}

/// A status line with the local instant it expires at: the text is the frame,
/// the `Instant` means nothing off this process.
pub fn text_of_timed<S: Serializer>(
    status: &Option<(String, std::time::Instant)>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    status.as_ref().map(|(text, _)| scrub(text)).serialize(serializer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Persisted {
        #[serde(serialize_with = "env_values_in_frame")]
        env: HashMap<String, String>,
        #[serde(skip_serializing_if = "omit_in_frame")]
        table: String,
    }

    #[test]
    fn frame_only_redaction_leaves_disk_output_alone() {
        let value = Persisted {
            env: HashMap::from([("TOKEN".to_string(), "v".to_string())]),
            table: "t".to_string(),
        };
        let disk = serde_json::to_value(&value).unwrap();
        assert_eq!(
            disk,
            serde_json::json!({ "env": { "TOKEN": "v" }, "table": "t" })
        );

        let frame = {
            let _scope = FrameScope::enter();
            serde_json::to_value(&value).unwrap()
        };
        assert_eq!(frame, serde_json::json!({ "env": { "TOKEN": REDACTED } }));
        assert!(!in_frame(), "the scope ends with the frame");
    }

    #[test]
    fn a_secret_reference_serialises_as_its_source() {
        let source = |r: &str| secret_source(r, serde_json::value::Serializer).unwrap();
        assert_eq!(source(""), "");
        assert_eq!(source("$BOT_TOKEN"), "$BOT_TOKEN");
        assert_eq!(source("keychain:ainb-bot"), "keychain:ainb-bot");
        assert_eq!(source("123456:plaintext"), "<literal>");
    }
}

/// The length of `value` as compact JSON, counted without building the string.
///
/// Here, inside the wire seam, because it serialises: a held tool call can
/// carry a whole file, and the conversation projection needs its size to
/// decide whether to carry it, without copying it to find out.
#[must_use]
pub fn compact_json_len(value: &serde_json::Value) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    // Writing a `Value` to a sink that never fails cannot fail.
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}
