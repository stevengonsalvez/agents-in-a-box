//! Custom issue properties + the agent metadata scratch bag (multica parity
//! gap #17).
//!
//! Two deliberately separate surfaces, mirroring the reference's two
//! migrations:
//!
//! * **`issue.properties`** (reference `191_issue_properties`) — USER-facing
//!   typed custom fields, validated against the per-workspace `issue_property`
//!   catalog and keyed by **definition id**, never by name, so renaming a
//!   property's display label is a catalog-only write that touches zero issue
//!   rows.
//! * **`issue.metadata`** (reference `105_issue_metadata`) — AGENT-internal
//!   flat KV scratch (`pr_number`, `pipeline_status`, `waiting_on`, …). No
//!   catalog, primitives only, single-key atomic mutations.
//!
//! # Numbers are held as canonical decimal TEXT
//!
//! The one deliberate divergence from the reference's Go types: a numeric value
//! is carried as the caller's decimal string, never as `f64`. This buys exactly
//! what the reference's `json.RawMessage` buys — `42` never becomes `42.0` on
//! the way to the DB — and it additionally keeps `Eq` derivable on every value
//! type, so the wire's `IssueRow` keeps its `Eq` derive.
//!
//! # Tolerant decode
//!
//! [`properties_from_json`] and [`metadata_from_json`] NEVER fail: malformed
//! JSON, a non-object document, or an element of an unsupported shape all decode
//! to "not present", exactly as [`crate::acceptance`]'s codec is tolerant of the
//! legacy column shape. A stored bag can therefore never wedge a read path.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Maximum number of ACTIVE custom property definitions per workspace
/// (reference: 20 active defs / workspace).
pub const MAX_ACTIVE_PROPERTIES: usize = 20;
/// Maximum serialised size, in bytes, of one issue's custom-property value bag
/// (reference: 16 KB).
pub const MAX_PROPERTY_BYTES: usize = 16_384;
/// Maximum number of metadata keys on one issue (reference:
/// `maxIssueMetadataKeys = 50`).
pub const MAX_METADATA_KEYS: usize = 50;
/// Maximum serialised size, in bytes, of one issue's metadata bag (reference:
/// `pg_column_size(metadata) <= 8192`).
pub const MAX_METADATA_BYTES: usize = 8_192;

/// Everything that can go wrong authoring a custom property or a metadata key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyError {
    /// The property / metadata key was blank after trimming.
    BlankKey,
    /// The metadata key did not match `^[a-zA-Z_][a-zA-Z0-9_.-]{0,63}$`.
    BadKey(String),
    /// The supplied property kind token is not one this build knows and is not
    /// acceptable in the position it was used (definition time).
    UnknownKind(String),
    /// The value's shape does not match its definition's kind.
    KindMismatch {
        /// The kind the catalog declares for this property.
        expected: String,
        /// What the caller actually supplied.
        got: String,
    },
    /// A `select` / `multi_select` value that is not one of the catalogued
    /// options.
    NotAnOption(String),
    /// A `select` / `multi_select` definition was created with no options.
    OptionsRequired,
    /// More keys than the surface's cap allows.
    TooManyKeys,
    /// The serialised bag exceeded its byte cap.
    TooLarge,
    /// A metadata value that is an array or an object rather than a primitive.
    NotPrimitive,
    /// A metadata value that is JSON `null`.
    NullValue,
}

impl fmt::Display for PropertyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlankKey => write!(f, "key must not be blank"),
            Self::BadKey(k) => write!(
                f,
                "invalid key {k:?}: must match ^[a-zA-Z_][a-zA-Z0-9_.-]{{0,63}}$"
            ),
            Self::UnknownKind(k) => write!(f, "unknown property kind {k:?}"),
            Self::KindMismatch { expected, got } => {
                write!(f, "value must be a {expected}, got {got}")
            }
            Self::NotAnOption(v) => write!(f, "{v:?} is not one of this property's options"),
            Self::OptionsRequired => {
                write!(
                    f,
                    "a select / multi_select property requires at least one option"
                )
            }
            Self::TooManyKeys => write!(f, "too many keys"),
            Self::TooLarge => write!(f, "value bag is too large"),
            Self::NotPrimitive => {
                write!(f, "value must be a primitive: string, number, or bool")
            }
            Self::NullValue => write!(f, "value cannot be null (use DELETE to remove a key)"),
        }
    }
}

impl std::error::Error for PropertyError {}

/// The reference's seven custom-property kinds.
///
/// **APPEND-ONLY vocabulary**: [`PropertyKind::parse`] is tolerant (an unknown
/// token becomes [`PropertyKind::Unknown`] and renders as raw text) and
/// [`PropertyKind::as_db_str`] is the only writer, so a newer daemon's kind
/// never wedges an older reader — the 0058/0062 precedent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropertyKind {
    /// Free text.
    Text,
    /// A decimal number, carried as canonical decimal TEXT.
    Number,
    /// One of a catalogued option list.
    Select,
    /// Zero or more of a catalogued option list.
    MultiSelect,
    /// An ISO-8601 date (`YYYY-MM-DD`) or an RFC-3339 timestamp.
    Date,
    /// A boolean tick-box.
    Checkbox,
    /// An absolute URL.
    Url,
    /// A kind token this build does not know; rendered as raw text.
    Unknown(String),
}

impl PropertyKind {
    /// The stable token written to `issue_property.kind`.
    #[must_use]
    pub const fn as_db_str(&self) -> &str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Select => "select",
            Self::MultiSelect => "multi_select",
            Self::Date => "date",
            Self::Checkbox => "checkbox",
            Self::Url => "url",
            Self::Unknown(raw) => raw.as_str(),
        }
    }

    /// Tolerant decode of a stored / caller-supplied kind token.
    ///
    /// Never fails: an unrecognised token becomes [`PropertyKind::Unknown`].
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "text" => Self::Text,
            "number" => Self::Number,
            "select" => Self::Select,
            "multi_select" | "multiselect" => Self::MultiSelect,
            "date" => Self::Date,
            "checkbox" | "bool" | "boolean" => Self::Checkbox,
            "url" => Self::Url,
            other => Self::Unknown(other.to_string()),
        }
    }

    /// Strict decode for the DEFINITION path — an unknown token is rejected so
    /// a typo in `property define --kind` does not silently create an
    /// unvalidated field.
    ///
    /// # Errors
    ///
    /// [`PropertyError::UnknownKind`] when `raw` is not one of the seven kinds.
    pub fn parse_strict(raw: &str) -> Result<Self, PropertyError> {
        match Self::parse(raw) {
            Self::Unknown(other) => Err(PropertyError::UnknownKind(other)),
            known => Ok(known),
        }
    }

    /// Whether this kind draws its values from a catalogued option list.
    #[must_use]
    pub const fn needs_options(&self) -> bool {
        matches!(self, Self::Select | Self::MultiSelect)
    }
}

/// One stored custom-property value.
///
/// `Eq`-safe on purpose: numbers are the caller's canonical decimal TEXT, never
/// `f64` (see the module header).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PropertyValue {
    /// A text / date / url value.
    Text(String),
    /// A numeric value as canonical decimal text.
    Number(String),
    /// A checkbox value.
    Bool(bool),
    /// A `multi_select` value.
    List(Vec<String>),
}

impl PropertyValue {
    /// The token used in [`PropertyError::KindMismatch::got`].
    #[must_use]
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Text(_) => "text",
            Self::Number(_) => "number",
            Self::Bool(_) => "bool",
            Self::List(_) => "list",
        }
    }
}

/// One stored metadata value: primitives only, exactly as the reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetadataValue {
    /// A string value.
    Text(String),
    /// A numeric value as canonical decimal text.
    Number(String),
    /// A boolean value.
    Bool(bool),
}

// ─────────────────────────────── properties ──────────────────────────────────

fn value_from_json(v: &serde_json::Value) -> Option<PropertyValue> {
    match v {
        serde_json::Value::String(s) => Some(PropertyValue::Text(s.clone())),
        serde_json::Value::Number(n) => Some(PropertyValue::Number(n.to_string())),
        serde_json::Value::Bool(b) => Some(PropertyValue::Bool(*b)),
        serde_json::Value::Array(items) => Some(PropertyValue::List(
            items.iter().filter_map(|i| i.as_str().map(str::to_string)).collect(),
        )),
        serde_json::Value::Null | serde_json::Value::Object(_) => None,
    }
}

fn value_to_json(v: &PropertyValue) -> serde_json::Value {
    match v {
        PropertyValue::Text(s) => serde_json::Value::String(s.clone()),
        PropertyValue::Number(n) => serde_json::from_str::<serde_json::Value>(n)
            .ok()
            .filter(serde_json::Value::is_number)
            .unwrap_or_else(|| serde_json::Value::String(n.clone())),
        PropertyValue::Bool(b) => serde_json::Value::Bool(*b),
        PropertyValue::List(items) => serde_json::Value::Array(
            items.iter().map(|i| serde_json::Value::String(i.clone())).collect(),
        ),
    }
}

/// Tolerant decode of the `issue.properties` column into a
/// `definition id -> value` map.
///
/// Bad JSON, a non-object document, and elements of an unsupported shape all
/// decode to "absent" — this NEVER errors, matching
/// [`crate::acceptance::criteria_from_json`]'s contract.
#[must_use]
pub fn properties_from_json(raw: &str) -> BTreeMap<String, PropertyValue> {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(raw) else {
        return BTreeMap::new();
    };
    map.iter()
        .filter_map(|(k, v)| value_from_json(v).map(|v| (k.clone(), v)))
        .collect()
}

/// Encode a value bag for the `issue.properties` column.
///
/// Always a JSON object (`{}` when empty) so the column's
/// `NOT NULL DEFAULT '{}'` invariant holds.
#[must_use]
pub fn properties_to_json(map: &BTreeMap<String, PropertyValue>) -> String {
    let obj: serde_json::Map<String, serde_json::Value> =
        map.iter().map(|(k, v)| (k.clone(), value_to_json(v))).collect();
    serde_json::to_string(&serde_json::Value::Object(obj)).unwrap_or_else(|_| "{}".to_string())
}

/// Whether a definition's `kind` + `options` pair is coherent at DEFINE time.
///
/// # Errors
///
/// [`PropertyError::OptionsRequired`] when a `select` / `multi_select` is
/// defined with no options.
pub fn validate_definition(kind: &PropertyKind, options: &[String]) -> Result<(), PropertyError> {
    if kind.needs_options() && options.iter().all(|o| o.trim().is_empty()) {
        return Err(PropertyError::OptionsRequired);
    }
    Ok(())
}

fn is_absolute_url(raw: &str) -> bool {
    let Some((scheme, rest)) = raw.split_once("://") else {
        return false;
    };
    !rest.trim().is_empty()
        && !scheme.is_empty()
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

fn is_date_like(raw: &str) -> bool {
    let raw = raw.trim();
    chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").is_ok()
        || chrono::DateTime::parse_from_rfc3339(raw).is_ok()
}

fn mismatch(expected: &PropertyKind, got: &PropertyValue) -> PropertyError {
    PropertyError::KindMismatch {
        expected: expected.as_db_str().to_string(),
        got: got.type_name().to_string(),
    }
}

/// Reject a value that does not match its definition's kind / options.
///
/// # Errors
///
/// [`PropertyError::KindMismatch`], [`PropertyError::NotAnOption`] or
/// [`PropertyError::OptionsRequired`] per the definition.
pub fn validate_value(
    kind: &PropertyKind,
    options: &[String],
    value: &PropertyValue,
) -> Result<(), PropertyError> {
    match kind {
        PropertyKind::Text | PropertyKind::Unknown(_) => match value {
            PropertyValue::Text(_) => Ok(()),
            other => Err(mismatch(kind, other)),
        },
        PropertyKind::Number => match value {
            PropertyValue::Number(n) if n.trim().parse::<f64>().is_ok() => Ok(()),
            other => Err(mismatch(kind, other)),
        },
        PropertyKind::Checkbox => match value {
            PropertyValue::Bool(_) => Ok(()),
            other => Err(mismatch(kind, other)),
        },
        PropertyKind::Date => match value {
            PropertyValue::Text(s) if is_date_like(s) => Ok(()),
            other => Err(mismatch(kind, other)),
        },
        PropertyKind::Url => match value {
            PropertyValue::Text(s) if is_absolute_url(s) => Ok(()),
            other => Err(mismatch(kind, other)),
        },
        PropertyKind::Select => {
            validate_definition(kind, options)?;
            match value {
                PropertyValue::Text(s) if options.iter().any(|o| o == s) => Ok(()),
                PropertyValue::Text(s) => Err(PropertyError::NotAnOption(s.clone())),
                other => Err(mismatch(kind, other)),
            }
        }
        PropertyKind::MultiSelect => {
            validate_definition(kind, options)?;
            match value {
                PropertyValue::List(items) => items.iter().try_for_each(|item| {
                    if options.iter().any(|o| o == item) {
                        Ok(())
                    } else {
                        Err(PropertyError::NotAnOption(item.clone()))
                    }
                }),
                other => Err(mismatch(kind, other)),
            }
        }
    }
}

/// Turn caller-supplied strings into the typed value a definition's kind wants.
///
/// One seam shared by the CLI and the RPC handler so the two cannot coerce
/// differently. `values` is the repeatable `--value` list; a `multi_select`
/// consumes all of them, every other kind consumes the first.
///
/// # Errors
///
/// [`PropertyError::KindMismatch`] when a value cannot be coerced (a `checkbox`
/// set to `"maybe"`, a `number` set to `"soon"`), or [`PropertyError::BlankKey`]
/// when `values` is empty.
pub fn coerce_value(
    kind: &PropertyKind,
    values: &[String],
) -> Result<PropertyValue, PropertyError> {
    if matches!(kind, PropertyKind::MultiSelect) {
        return Ok(PropertyValue::List(values.to_vec()));
    }
    let Some(first) = values.first() else {
        return Err(PropertyError::BlankKey);
    };
    match kind {
        PropertyKind::Number => {
            let trimmed = first.trim();
            if trimmed.parse::<f64>().is_ok() {
                Ok(PropertyValue::Number(trimmed.to_string()))
            } else {
                Err(PropertyError::KindMismatch {
                    expected: "number".to_string(),
                    got: "text".to_string(),
                })
            }
        }
        PropertyKind::Checkbox => match first.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Ok(PropertyValue::Bool(true)),
            "false" | "no" | "0" => Ok(PropertyValue::Bool(false)),
            _ => Err(PropertyError::KindMismatch {
                expected: "checkbox".to_string(),
                got: "text".to_string(),
            }),
        },
        _ => Ok(PropertyValue::Text(first.clone())),
    }
}

/// Render one property value for display.
///
/// The single source of truth for the wire's `IssuePropertyRow.value` AND the
/// CLI's `issue show` line, so the two can never drift.
#[must_use]
pub fn render_value(value: &PropertyValue) -> String {
    match value {
        PropertyValue::Text(s) => s.clone(),
        PropertyValue::Number(n) => n.clone(),
        PropertyValue::Bool(true) => "yes".to_string(),
        PropertyValue::Bool(false) => "no".to_string(),
        PropertyValue::List(items) => items.join(", "),
    }
}

// ──────────────────────────────── metadata ───────────────────────────────────

/// Validate a metadata key against the reference's
/// `^[a-zA-Z_][a-zA-Z0-9_.-]{0,63}$`.
///
/// # Errors
///
/// [`PropertyError::BlankKey`] for an empty key, [`PropertyError::BadKey`]
/// otherwise.
pub fn validate_metadata_key(key: &str) -> Result<(), PropertyError> {
    if key.is_empty() {
        return Err(PropertyError::BlankKey);
    }
    if key.len() > 64 {
        return Err(PropertyError::BadKey(key.to_string()));
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap_or('\0');
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(PropertyError::BadKey(key.to_string()));
    }
    if chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')) {
        Ok(())
    } else {
        Err(PropertyError::BadKey(key.to_string()))
    }
}

/// Tolerant decode of the `issue.metadata` column.
///
/// Non-primitive entries (arrays, objects, nulls) are DROPPED rather than
/// erroring, so a bag written by a future/foreign writer can never wedge a read.
#[must_use]
pub fn metadata_from_json(raw: &str) -> BTreeMap<String, MetadataValue> {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(raw) else {
        return BTreeMap::new();
    };
    map.iter()
        .filter_map(|(k, v)| match v {
            serde_json::Value::String(s) => Some((k.clone(), MetadataValue::Text(s.clone()))),
            serde_json::Value::Number(n) => Some((k.clone(), MetadataValue::Number(n.to_string()))),
            serde_json::Value::Bool(b) => Some((k.clone(), MetadataValue::Bool(*b))),
            _ => None,
        })
        .collect()
}

/// Encode a metadata bag for the `issue.metadata` column.
#[must_use]
pub fn metadata_to_json(map: &BTreeMap<String, MetadataValue>) -> String {
    let obj: serde_json::Map<String, serde_json::Value> =
        map.iter().map(|(k, v)| (k.clone(), metadata_value_to_json(v))).collect();
    serde_json::to_string(&serde_json::Value::Object(obj)).unwrap_or_else(|_| "{}".to_string())
}

fn metadata_value_to_json(v: &MetadataValue) -> serde_json::Value {
    match v {
        MetadataValue::Text(s) => serde_json::Value::String(s.clone()),
        MetadataValue::Number(n) => serde_json::from_str::<serde_json::Value>(n)
            .ok()
            .filter(serde_json::Value::is_number)
            .unwrap_or_else(|| serde_json::Value::String(n.clone())),
        MetadataValue::Bool(b) => serde_json::Value::Bool(*b),
    }
}

/// The canonical JSON TEXT of one metadata value (`42`, `"open"`, `true`).
///
/// This is what travels the wire as `IssueMetadataRow.value_json`, so numeric
/// vs string typing survives without an `Eq`-breaking `serde_json::Value`.
#[must_use]
pub fn metadata_value_json(v: &MetadataValue) -> String {
    metadata_value_to_json(v).to_string()
}

/// Render one metadata value for display (unquoted).
#[must_use]
pub fn render_metadata(v: &MetadataValue) -> String {
    match v {
        MetadataValue::Text(s) => s.clone(),
        MetadataValue::Number(n) => n.clone(),
        MetadataValue::Bool(b) => b.to_string(),
    }
}

/// Coerce a caller-supplied metadata value, honouring the reference's
/// `--type` override and its default value-sniffing.
///
/// `value_type` is `string` / `number` / `bool`; absent ⇒ sniff (`true`/`false`
/// → bool, a valid decimal → number, else string).
///
/// # Errors
///
/// [`PropertyError::UnknownKind`] for an unrecognised `value_type`, and
/// [`PropertyError::KindMismatch`] when the value does not fit the requested
/// type.
pub fn coerce_metadata_value(
    value: &str,
    value_type: Option<&str>,
) -> Result<MetadataValue, PropertyError> {
    match value_type.map(|t| t.trim().to_ascii_lowercase()) {
        None => Ok(sniff_metadata_value(value)),
        Some(t) if t == "string" || t == "text" => Ok(MetadataValue::Text(value.to_string())),
        Some(t) if t == "number" => {
            let trimmed = value.trim();
            if trimmed.parse::<f64>().is_ok() {
                Ok(MetadataValue::Number(trimmed.to_string()))
            } else {
                Err(PropertyError::KindMismatch {
                    expected: "number".to_string(),
                    got: "text".to_string(),
                })
            }
        }
        Some(t) if t == "bool" || t == "boolean" => {
            match value.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Ok(MetadataValue::Bool(true)),
                "false" | "no" | "0" => Ok(MetadataValue::Bool(false)),
                _ => Err(PropertyError::KindMismatch {
                    expected: "bool".to_string(),
                    got: "text".to_string(),
                }),
            }
        }
        Some(other) => Err(PropertyError::UnknownKind(other)),
    }
}

/// The reference's default value-sniffing: `true`/`false` → bool, a valid
/// decimal → number, everything else → string.
#[must_use]
pub fn sniff_metadata_value(value: &str) -> MetadataValue {
    let trimmed = value.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "true" => return MetadataValue::Bool(true),
        "false" => return MetadataValue::Bool(false),
        _ => {}
    }
    if !trimmed.is_empty() && trimmed.parse::<f64>().is_ok() {
        return MetadataValue::Number(trimmed.to_string());
    }
    MetadataValue::Text(value.to_string())
}

/// Decode a caller-supplied RAW JSON metadata value, reproducing the
/// reference's exact primitive-only contract.
///
/// # Errors
///
/// [`PropertyError::NullValue`] for `null` (*"value cannot be null (use DELETE
/// to remove a key)"*), [`PropertyError::NotPrimitive`] for arrays and objects.
pub fn metadata_value_from_json(raw: &str) -> Result<MetadataValue, PropertyError> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Ok(MetadataValue::Text(raw.to_string()));
    };
    match parsed {
        serde_json::Value::Null => Err(PropertyError::NullValue),
        serde_json::Value::String(s) => Ok(MetadataValue::Text(s)),
        serde_json::Value::Number(n) => Ok(MetadataValue::Number(n.to_string())),
        serde_json::Value::Bool(b) => Ok(MetadataValue::Bool(b)),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
            Err(PropertyError::NotPrimitive)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn kind_round_trips_and_unknown_is_tolerant() {
        for k in [
            PropertyKind::Text,
            PropertyKind::Number,
            PropertyKind::Select,
            PropertyKind::MultiSelect,
            PropertyKind::Date,
            PropertyKind::Checkbox,
            PropertyKind::Url,
        ] {
            assert_eq!(PropertyKind::parse(k.as_db_str()), k);
        }
        assert_eq!(
            PropertyKind::parse("hologram"),
            PropertyKind::Unknown("hologram".to_string())
        );
        assert_eq!(
            PropertyKind::parse_strict("hologram"),
            Err(PropertyError::UnknownKind("hologram".to_string()))
        );
    }

    #[test]
    fn numbers_keep_integer_fidelity_across_a_round_trip() {
        let mut map = BTreeMap::new();
        map.insert("p1".to_string(), PropertyValue::Number("42".to_string()));
        let json = properties_to_json(&map);
        assert_eq!(json, r#"{"p1":42}"#, "got {json}");
        let back = properties_from_json(&json);
        assert_eq!(back.get("p1"), Some(&PropertyValue::Number("42".into())));
    }

    #[test]
    fn properties_decode_is_tolerant_of_garbage() {
        assert!(properties_from_json("not json").is_empty());
        assert!(properties_from_json("[1,2]").is_empty());
        assert!(properties_from_json("").is_empty());
        // A null / object entry is dropped, the sibling survives.
        let map = properties_from_json(r#"{"a":null,"b":{"x":1},"c":"keep"}"#);
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("c"), Some(&PropertyValue::Text("keep".into())));
        assert_eq!(properties_to_json(&BTreeMap::new()), "{}");
    }

    #[test]
    fn validate_value_enforces_each_kind() {
        let sel = PropertyKind::Select;
        let options = opts(&["S1", "S2"]);
        assert!(validate_value(&sel, &options, &PropertyValue::Text("S2".into())).is_ok());
        assert_eq!(
            validate_value(&sel, &options, &PropertyValue::Text("S9".into())),
            Err(PropertyError::NotAnOption("S9".into()))
        );
        assert_eq!(
            validate_value(&sel, &[], &PropertyValue::Text("S2".into())),
            Err(PropertyError::OptionsRequired)
        );

        let check = PropertyKind::Checkbox;
        assert!(validate_value(&check, &[], &PropertyValue::Bool(true)).is_ok());
        assert!(matches!(
            validate_value(&check, &[], &PropertyValue::Text("maybe".into())),
            Err(PropertyError::KindMismatch { .. })
        ));

        let num = PropertyKind::Number;
        assert!(validate_value(&num, &[], &PropertyValue::Number("3.5".into())).is_ok());
        assert!(matches!(
            validate_value(&num, &[], &PropertyValue::Text("soon".into())),
            Err(PropertyError::KindMismatch { .. })
        ));

        let multi = PropertyKind::MultiSelect;
        assert!(
            validate_value(&multi, &options, &PropertyValue::List(opts(&["S1", "S2"]))).is_ok()
        );
        assert_eq!(
            validate_value(&multi, &options, &PropertyValue::List(opts(&["S1", "S9"]))),
            Err(PropertyError::NotAnOption("S9".into()))
        );

        let date = PropertyKind::Date;
        assert!(validate_value(&date, &[], &PropertyValue::Text("2026-07-24".into())).is_ok());
        assert!(matches!(
            validate_value(&date, &[], &PropertyValue::Text("someday".into())),
            Err(PropertyError::KindMismatch { .. })
        ));

        let url = PropertyKind::Url;
        assert!(validate_value(&url, &[], &PropertyValue::Text("https://x.dev/a".into())).is_ok());
        assert!(matches!(
            validate_value(&url, &[], &PropertyValue::Text("x.dev".into())),
            Err(PropertyError::KindMismatch { .. })
        ));

        // An unknown kind accepts text and never wedges a read.
        let unknown = PropertyKind::Unknown("hologram".into());
        assert!(validate_value(&unknown, &[], &PropertyValue::Text("whatever".into())).is_ok());
    }

    #[test]
    fn coerce_value_matches_the_definition_kind() {
        assert_eq!(
            coerce_value(&PropertyKind::Number, &opts(&["42"])),
            Ok(PropertyValue::Number("42".into()))
        );
        assert!(coerce_value(&PropertyKind::Number, &opts(&["soon"])).is_err());
        assert_eq!(
            coerce_value(&PropertyKind::Checkbox, &opts(&["yes"])),
            Ok(PropertyValue::Bool(true))
        );
        assert!(coerce_value(&PropertyKind::Checkbox, &opts(&["maybe"])).is_err());
        assert_eq!(
            coerce_value(&PropertyKind::MultiSelect, &opts(&["a", "b"])),
            Ok(PropertyValue::List(opts(&["a", "b"])))
        );
        assert_eq!(
            coerce_value(&PropertyKind::Text, &opts(&["hi"])),
            Ok(PropertyValue::Text("hi".into()))
        );
        assert_eq!(
            coerce_value(&PropertyKind::Text, &[]),
            Err(PropertyError::BlankKey)
        );
    }

    #[test]
    fn render_value_is_the_one_display_seam() {
        assert_eq!(render_value(&PropertyValue::Text("S2".into())), "S2");
        assert_eq!(render_value(&PropertyValue::Number("42".into())), "42");
        assert_eq!(render_value(&PropertyValue::Bool(true)), "yes");
        assert_eq!(render_value(&PropertyValue::Bool(false)), "no");
        assert_eq!(
            render_value(&PropertyValue::List(opts(&["a", "b"]))),
            "a, b"
        );
    }

    #[test]
    fn metadata_keys_follow_the_reference_regex() {
        for good in ["pr_number", "_x", "a.b-c", "A9"] {
            assert!(validate_metadata_key(good).is_ok(), "{good} should pass");
        }
        assert_eq!(validate_metadata_key(""), Err(PropertyError::BlankKey));
        for bad in ["9lives", "a b", "a/b", "-x"] {
            assert!(
                matches!(validate_metadata_key(bad), Err(PropertyError::BadKey(_))),
                "{bad} should fail"
            );
        }
        let too_long = "a".repeat(65);
        assert!(matches!(
            validate_metadata_key(&too_long),
            Err(PropertyError::BadKey(_))
        ));
        assert!(validate_metadata_key(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn metadata_primitive_contract_matches_the_reference_wording() {
        assert_eq!(
            metadata_value_from_json("null"),
            Err(PropertyError::NullValue)
        );
        assert_eq!(
            PropertyError::NullValue.to_string(),
            "value cannot be null (use DELETE to remove a key)"
        );
        assert_eq!(
            metadata_value_from_json("[1]"),
            Err(PropertyError::NotPrimitive)
        );
        assert_eq!(
            metadata_value_from_json("{}"),
            Err(PropertyError::NotPrimitive)
        );
        assert_eq!(
            PropertyError::NotPrimitive.to_string(),
            "value must be a primitive: string, number, or bool"
        );
        assert_eq!(
            metadata_value_from_json("42"),
            Ok(MetadataValue::Number("42".into()))
        );
    }

    #[test]
    fn metadata_sniffing_and_type_override() {
        assert_eq!(sniff_metadata_value("true"), MetadataValue::Bool(true));
        assert_eq!(
            sniff_metadata_value("471"),
            MetadataValue::Number("471".into())
        );
        assert_eq!(
            sniff_metadata_value("open"),
            MetadataValue::Text("open".into())
        );
        // --type string forces a numeric-looking value to stay text.
        assert_eq!(
            coerce_metadata_value("471", Some("string")),
            Ok(MetadataValue::Text("471".into()))
        );
        assert!(coerce_metadata_value("soon", Some("number")).is_err());
        assert!(coerce_metadata_value("x", Some("wat")).is_err());
    }

    #[test]
    fn metadata_json_round_trip_keeps_typing() {
        let mut map = BTreeMap::new();
        map.insert("pr_number".to_string(), MetadataValue::Number("42".into()));
        map.insert("state".to_string(), MetadataValue::Text("open".into()));
        map.insert("done".to_string(), MetadataValue::Bool(false));
        let json = metadata_to_json(&map);
        assert_eq!(metadata_from_json(&json), map);
        assert_eq!(
            metadata_value_json(&MetadataValue::Number("42".into())),
            "42"
        );
        assert_eq!(
            metadata_value_json(&MetadataValue::Text("open".into())),
            "\"open\""
        );
        assert_eq!(render_metadata(&MetadataValue::Number("42".into())), "42");
        assert_eq!(render_metadata(&MetadataValue::Bool(true)), "true");
        assert!(metadata_from_json(r#"{"a":[1]}"#).is_empty());
    }
}
