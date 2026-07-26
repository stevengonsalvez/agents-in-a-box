//! Per-agent environment variables with a REDACT-BY-CONSTRUCTION contract
//! (multica parity #30, `server/internal/handler/agent.go:552-562`).
//!
//! # The invariant
//!
//! > The plaintext of an [`AgentEnv`] leaves the process by exactly one route:
//! > the environment of the provider child process, via
//! > [`AgentEnv::expose_for_child_env`]. Every other route — `Debug`,
//! > `Display`, `Serialize`, the RPC wire, the CLI renderers, the TUI, tracing
//! > — is redacted by construction. Adding a new egress means calling
//! > `expose_for_child_env`, which is grep-able and reviewable.
//!
//! The mask itself is multica's: **keys are preserved, every value becomes
//! [`REDACTED_VALUE`]**, and the accompanying wire flag (multica's
//! `custom_env_redacted`) says the caller is looking at a masked map.
//!
//! # Deviations from multica (deliberate)
//!
//! * **D-1 — hangar masks unconditionally.** Multica has a `canViewAgentEnv()`
//!   owner/admin branch that serves PLAINTEXT to an authorised viewer. That is
//!   a multi-tenant server concept; hangar is a local, single-user control
//!   plane where the operator can already `sqlite3` the DB. A `--reveal`
//!   affordance would re-open exactly the hole this type exists to close, so
//!   there is none.
//! * **D-2 — the authoritative `agent.agent_env` column keeps plaintext.** The
//!   exec seam needs the value and there is no session key to encrypt with; the
//!   DB lives under the operator's own account. What the redaction contract
//!   buys is that *no other persisted artefact* (another table, the daemon log,
//!   a per-task log dir, the interactive wrapper after teardown, any rendered
//!   output) ever contains the value. A keychain-backed value-of-record is a
//!   follow-up, out of scope here.

use std::fmt;

use serde::{Deserialize, Serialize};

/// The mask multica writes (`agent.go:558`). Byte-identical on purpose so a
/// reader that knows multica's contract reads hangar's output unchanged.
pub const REDACTED_VALUE: &str = "****";

/// An agent's per-agent environment: plaintext INSIDE, redacted on every egress
/// except [`AgentEnv::expose_for_child_env`].
///
/// Ordered key-value list rather than a map so the DB encoding is deterministic
/// (this preserves the exact `0015_agent_archive_and_config.sql` column shape —
/// a JSON object of string→string).
///
/// `Debug` / `Display` / `Serialize` are hand-written and all mask values.
/// There is deliberately **no `Deserialize`**: `****` must never round-trip back
/// into the DB. The write path uses [`AgentEnvInput`] instead.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct AgentEnv {
    pairs: Vec<(String, String)>,
}

impl AgentEnv {
    /// Build from an ordered key-value list (the DB / CLI / RPC write shape).
    #[must_use]
    pub const fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        Self { pairs }
    }

    /// `true` when the agent carries no per-agent env at all.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// How many variables are set (the only count any renderer needs).
    #[must_use]
    pub const fn len(&self) -> usize {
        self.pairs.len()
    }

    /// The variable NAMES, in stored order. Names are not secret — multica
    /// ships them too — so this is the safe read for every UI.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.pairs.iter().map(|(k, _)| k.as_str())
    }

    /// Multica's `redactEnv()` map: keys preserved, every value replaced with
    /// [`REDACTED_VALUE`].
    #[must_use]
    pub fn redacted_pairs(&self) -> Vec<(&str, &'static str)> {
        self.pairs.iter().map(|(k, _)| (k.as_str(), REDACTED_VALUE)).collect()
    }

    /// **The ONE plaintext escape.**
    ///
    /// The only call site permitted is the exec seam that builds a provider
    /// child process's environment — grep this name in review; any other hit is
    /// a leak.
    #[must_use]
    pub fn expose_for_child_env(self) -> Vec<(String, String)> {
        self.pairs
    }

    /// Encode into the JSON-object text the `agent_env` column stores. An empty
    /// env yields `"{}"` (the column default), matching the pre-`AgentEnv`
    /// bytes exactly — this is why item 30 needs no migration.
    #[must_use]
    pub fn to_db_json(&self) -> String {
        let map: serde_json::Map<String, serde_json::Value> = self
            .pairs
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        serde_json::to_string(&serde_json::Value::Object(map)).unwrap_or_else(|_| "{}".to_string())
    }

    /// Decode the `agent_env` column's JSON-object text, preserving the stored
    /// object's key order.
    ///
    /// # Errors
    ///
    /// Returns a short, VALUE-FREE reason string when the cell is not a JSON
    /// object of string→string. The message never echoes the offending input:
    /// `serde_json` errors quote scalars, which for this column is the secret.
    pub fn try_from_db_json(raw: &str) -> Result<Self, &'static str> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| "stored env is not valid JSON")?;
        let obj = value.as_object().ok_or("stored env is not a JSON object")?;
        let pairs = obj
            .iter()
            .map(|(k, v)| {
                v.as_str().map(|s| (k.clone(), s.to_string())).ok_or("env value is not a string")
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { pairs })
    }

    /// Lossy decode: a corrupt cell degrades to an empty env rather than taking
    /// the caller down. Used where a broken row must not be fatal.
    #[must_use]
    pub fn from_db_json(raw: &str) -> Self {
        Self::try_from_db_json(raw).unwrap_or_default()
    }
}

impl From<Vec<(String, String)>> for AgentEnv {
    fn from(pairs: Vec<(String, String)>) -> Self {
        Self::from_pairs(pairs)
    }
}

/// Redacted: `AgentEnv { SECRET_TOKEN: "****" }`. This is what structurally
/// kills the `tracing::debug!(?dispatch)` / `anyhow` context / panic-message
/// leak class — no reviewer vigilance required.
impl fmt::Debug for AgentEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut dbg = f.debug_struct("AgentEnv");
        for key in self.keys() {
            dbg.field(key, &REDACTED_VALUE);
        }
        dbg.finish()
    }
}

/// Redacted: `2 keys (hidden)`.
impl fmt::Display for AgentEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} keys (hidden)", self.pairs.len())
    }
}

/// Emits the REDACTED map, so an accidental `serde_json::to_*` of a row that
/// embeds an `AgentEnv` is safe by construction.
impl Serialize for AgentEnv {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut map = serializer.serialize_map(Some(self.pairs.len()))?;
        for key in self.keys() {
            map.serialize_entry(key, REDACTED_VALUE)?;
        }
        map.end()
    }
}

/// The WRITE-side carrier (RPC params, CLI flags).
///
/// Serialises PLAINTEXT — it is a PUT body, the value has to reach the daemon —
/// but its `Debug` is redacted, so a params dump or an error context can't leak
/// it. `#[serde(transparent)]` over `Vec<(String, String)>` keeps the EXISTING
/// wire bytes (`"agent_env": [["FOO","bar"]]`): this is a pure type change with
/// zero wire break.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentEnvInput(Vec<(String, String)>);

impl AgentEnvInput {
    /// Build from an ordered key-value list.
    #[must_use]
    pub const fn from_pairs(pairs: Vec<(String, String)>) -> Self {
        Self(pairs)
    }

    /// Promote a validated write into the redact-by-construction domain type.
    #[must_use]
    pub fn into_agent_env(self) -> AgentEnv {
        AgentEnv::from_pairs(self.0)
    }

    /// Borrow the raw pairs (the CLI needs them to build the RPC body).
    #[must_use]
    pub fn as_pairs(&self) -> &[(String, String)] {
        &self.0
    }

    /// `true` when the write carries no variables (an explicit CLEAR).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many variables the write carries.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }
}

impl From<Vec<(String, String)>> for AgentEnvInput {
    fn from(pairs: Vec<(String, String)>) -> Self {
        Self::from_pairs(pairs)
    }
}

/// Redacted, exactly like [`AgentEnv`] — a `Debug` of the RPC params struct is
/// a real leak vector (it lands in `INVALID_PARAMS` contexts and trace spans).
impl fmt::Debug for AgentEnvInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut dbg = f.debug_struct("AgentEnvInput");
        for (key, _) in &self.0 {
            dbg.field(key, &REDACTED_VALUE);
        }
        dbg.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "sk-live-DEADBEEF01";

    fn one() -> AgentEnv {
        AgentEnv::from_pairs(vec![("SECRET_TOKEN".into(), SECRET.into())])
    }

    #[test]
    fn debug_masks_values_and_keeps_keys() {
        let rendered = format!("{:?}", one());
        assert!(!rendered.contains(SECRET), "Debug leaked the value: {rendered}");
        assert!(rendered.contains("SECRET_TOKEN"), "{rendered}");
        assert!(rendered.contains(REDACTED_VALUE), "{rendered}");
    }

    #[test]
    fn display_is_key_count() {
        assert_eq!(one().to_string(), "1 keys (hidden)");
        assert_eq!(AgentEnv::default().to_string(), "0 keys (hidden)");
    }

    #[test]
    fn serialize_emits_the_mask() {
        let out = serde_json::to_string(&one()).unwrap();
        assert_eq!(out, r#"{"SECRET_TOKEN":"****"}"#);
        assert!(!out.contains(SECRET));
    }

    #[test]
    fn redacted_pairs_keep_keys_and_mask_values() {
        assert_eq!(one().redacted_pairs(), vec![("SECRET_TOKEN", "****")]);
    }

    #[test]
    fn exec_seam_is_the_one_plaintext_escape() {
        assert_eq!(
            one().expose_for_child_env(),
            vec![("SECRET_TOKEN".to_string(), SECRET.to_string())]
        );
    }

    #[test]
    fn input_serialize_is_plaintext_and_wire_compatible() {
        let input = AgentEnvInput::from_pairs(vec![("FOO".into(), "bar".into())]);
        assert_eq!(serde_json::to_string(&input).unwrap(), r#"[["FOO","bar"]]"#);
        let back: AgentEnvInput = serde_json::from_str(r#"[["FOO","bar"]]"#).unwrap();
        assert_eq!(back, input);
    }

    #[test]
    fn input_debug_is_redacted() {
        let input = AgentEnvInput::from_pairs(vec![("SECRET_TOKEN".into(), SECRET.into())]);
        let rendered = format!("{input:?}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(rendered.contains("SECRET_TOKEN"), "{rendered}");
    }

    #[test]
    fn db_json_round_trips() {
        let env = one();
        let json = env.to_db_json();
        assert_eq!(json, r#"{"SECRET_TOKEN":"sk-live-DEADBEEF01"}"#);
        assert_eq!(AgentEnv::from_db_json(&json), env);
        assert_eq!(AgentEnv::default().to_db_json(), "{}");
    }

    #[test]
    fn corrupt_db_json_degrades_to_empty() {
        assert!(AgentEnv::from_db_json("not json").is_empty());
        assert!(AgentEnv::from_db_json("[1,2]").is_empty());
        assert!(AgentEnv::from_db_json(r#"{"K":31337}"#).is_empty());
    }

    #[test]
    fn decode_errors_never_echo_the_input() {
        let err = AgentEnv::try_from_db_json(&format!(r#"{{"K":"{SECRET}"#)).unwrap_err();
        assert!(!err.contains(SECRET), "{err}");
        let err = AgentEnv::try_from_db_json(r#"{"K":31337}"#).unwrap_err();
        assert!(!err.contains("31337"), "{err}");
    }
}
