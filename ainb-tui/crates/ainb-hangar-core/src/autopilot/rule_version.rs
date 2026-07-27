//! The autopilot rule-version domain: what counts as a SUBSTANTIVE publish
//! (multica parity #14, migration 0061).
//!
//! multica's `autopilot_rule_version` is an accountability ledger, not a change
//! log: one append-only row per *substantive* publish, each naming the human who
//! published it. The load-bearing distinction is that **cosmetic edits (a
//! rename) explicitly do NOT create a version** — a title tweak must never
//! re-assign blame for an unattended run.
//!
//! This module is the single, IO-free definition of that rule:
//!
//! - [`RuleChangeKind`] — the ledger vocabulary, with a TOLERANT parse (an
//!   unknown token written by a newer daemon decodes to `None` and renders as
//!   raw text rather than poisoning the read path; migration 0061 decision 1).
//! - [`AutopilotConfigSnapshot`] — the rule as published, the shape that is
//!   serialised into `config_summary`.
//! - [`classify`] — the ONLY definition of "substantive". `None` means cosmetic
//!   (no version row).
//!
//! Keeping the classifier here (rather than inline in the sqlx repo) means the
//! rename rule is unit-testable without a database and cannot be re-derived
//! differently by a second call site.

use std::fmt;

/// Why a rule version was published — the `autopilot_rule_version.change_kind`
/// column.
///
/// The column carries NO `CHECK` (migration 0061 decision 1): SQLite cannot
/// widen a `CHECK` without a full table rebuild and this vocabulary is
/// append-only by design, so the domain lives here and
/// [`from_db_str`](RuleChangeKind::from_db_str) is deliberately tolerant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleChangeKind {
    /// The rule was created. Always version 1, written in the SAME transaction
    /// as the `autopilot` insert so there is no window in which an autopilot
    /// exists with no accountable human.
    Created,
    /// The agent's instructions changed.
    Instructions,
    /// The cron expression changed (and `next_tick_at` was recomputed).
    Schedule,
    /// The dispatch TARGET changed (a different agent now runs unattended).
    Target,
    /// A policy knob changed (`max_concurrent_runs` / `execution_mode` /
    /// `concurrency_policy`).
    Policy,
    /// The rule was disabled — the scheduler stops considering it.
    Paused,
    /// The rule was re-enabled (`next_tick_at` recomputed from now).
    Resumed,
    /// A trigger surface was armed or disarmed. hangar collapses trigger config
    /// into columns on `autopilot`, so multica's per-trigger publisher
    /// (migration 189) folds into the per-RULE version chain here.
    Trigger,
}

impl RuleChangeKind {
    /// The literal stored in the `change_kind` column.
    #[must_use]
    pub const fn as_db_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Instructions => "instructions",
            Self::Schedule => "schedule",
            Self::Target => "target",
            Self::Policy => "policy",
            Self::Paused => "paused",
            Self::Resumed => "resumed",
            Self::Trigger => "trigger",
        }
    }

    /// Parse a stored `change_kind` value.
    ///
    /// **Tolerant by design**: an unrecognised token (written by a newer daemon
    /// against the same file) yields `None` so the caller renders the raw text,
    /// rather than erroring and poisoning the whole read path. This is what lets
    /// migration 0061 omit the `CHECK` constraint safely.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        Some(match s {
            "created" => Self::Created,
            "instructions" => Self::Instructions,
            "schedule" => Self::Schedule,
            "target" => Self::Target,
            "policy" => Self::Policy,
            "paused" => Self::Paused,
            "resumed" => Self::Resumed,
            "trigger" => Self::Trigger,
            _ => return None,
        })
    }
}

impl fmt::Display for RuleChangeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_db_str())
    }
}

/// The rule as published — the snapshot serialised into
/// `autopilot_rule_version.config_summary` and the input to [`classify`].
///
/// Deliberately stringly-typed for `execution_mode` / `concurrency_policy`: the
/// typed enums live in the store crate (which depends on core, not the other way
/// round), and the ledger only ever needs to compare and serialise them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutopilotConfigSnapshot {
    /// Display name. **Cosmetic** — a change here alone mints no version.
    pub name: String,
    /// The agent dispatched to at each tick.
    pub agent_id: String,
    /// Instructions handed to the agent; `None` when unset.
    pub instructions: Option<String>,
    /// The validated UTC cron expression.
    pub cron_expr: String,
    /// Maximum simultaneous in-flight runs.
    pub max_concurrent_runs: i64,
    /// What a fired tick materialises (`run_only` / `create_issue`).
    pub execution_mode: String,
    /// What the scheduler does at the in-flight limit (`skip` / `queue` /
    /// `replace`).
    pub concurrency_policy: String,
    /// Whether the scheduler considers this rule.
    pub enabled: bool,
    /// Whether the bare programmatic `api` trigger is armed.
    pub api_trigger_enabled: bool,
}

impl AutopilotConfigSnapshot {
    /// The JSON object written to `config_summary`, with `changed` listing every
    /// field that differs from `before` (`[]` for a create).
    ///
    /// Stable and additive: readers must tolerate new keys.
    #[must_use]
    pub fn to_config_summary(&self, before: Option<&Self>) -> serde_json::Value {
        let changed: Vec<&'static str> =
            before.map(|b| changed_fields(b, self)).unwrap_or_default();
        serde_json::json!({
            "name": self.name,
            "agent_id": self.agent_id,
            "cron_expr": self.cron_expr,
            "instructions": self.instructions,
            "max_concurrent_runs": self.max_concurrent_runs,
            "execution_mode": self.execution_mode,
            "concurrency_policy": self.concurrency_policy,
            "enabled": self.enabled,
            "api_trigger_enabled": self.api_trigger_enabled,
            "changed": changed,
        })
    }
}

/// Every field that differs between two snapshots, in a stable order.
///
/// Carried verbatim in `config_summary.changed`, so a multi-field edit loses
/// nothing even though it mints exactly one version row.
#[must_use]
pub fn changed_fields(
    before: &AutopilotConfigSnapshot,
    after: &AutopilotConfigSnapshot,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    if before.name != after.name {
        out.push("name");
    }
    if before.agent_id != after.agent_id {
        out.push("agent_id");
    }
    if before.cron_expr != after.cron_expr {
        out.push("cron_expr");
    }
    if before.instructions != after.instructions {
        out.push("instructions");
    }
    if before.max_concurrent_runs != after.max_concurrent_runs {
        out.push("max_concurrent_runs");
    }
    if before.execution_mode != after.execution_mode {
        out.push("execution_mode");
    }
    if before.concurrency_policy != after.concurrency_policy {
        out.push("concurrency_policy");
    }
    if before.enabled != after.enabled {
        out.push("enabled");
    }
    if before.api_trigger_enabled != after.api_trigger_enabled {
        out.push("api_trigger_enabled");
    }
    out
}

/// The ONLY definition of "substantive".
///
/// Returns the single [`RuleChangeKind`] one edit publishes, or `None` when the
/// edit is **cosmetic** (a rename, or a no-op) and therefore mints no version
/// row — multica: *"cosmetic edits like a rename don't count"*.
///
/// `name` is deliberately absent from every arm: it is the cosmetic field.
///
/// # Precedence
///
/// One edit ⇒ exactly one row, so when several fields move together the highest
/// precedence wins:
///
/// `Paused`/`Resumed` > `Trigger` > `Target` > `Schedule` > `Instructions` >
/// `Policy`
///
/// The full changed-field list is preserved in `config_summary.changed`
/// ([`AutopilotConfigSnapshot::to_config_summary`]), so nothing is lost.
#[must_use]
pub fn classify(
    before: &AutopilotConfigSnapshot,
    after: &AutopilotConfigSnapshot,
) -> Option<RuleChangeKind> {
    if before.enabled != after.enabled {
        return Some(if after.enabled {
            RuleChangeKind::Resumed
        } else {
            RuleChangeKind::Paused
        });
    }
    if before.api_trigger_enabled != after.api_trigger_enabled {
        return Some(RuleChangeKind::Trigger);
    }
    if before.agent_id != after.agent_id {
        return Some(RuleChangeKind::Target);
    }
    if before.cron_expr != after.cron_expr {
        return Some(RuleChangeKind::Schedule);
    }
    if before.instructions != after.instructions {
        return Some(RuleChangeKind::Instructions);
    }
    if before.max_concurrent_runs != after.max_concurrent_runs
        || before.execution_mode != after.execution_mode
        || before.concurrency_policy != after.concurrency_policy
    {
        return Some(RuleChangeKind::Policy);
    }
    // Name-only, or nothing at all: cosmetic.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> AutopilotConfigSnapshot {
        AutopilotConfigSnapshot {
            name: "nightly".into(),
            agent_id: "agent-1".into(),
            instructions: Some("do the thing".into()),
            cron_expr: "0 9 * * 1-5".into(),
            max_concurrent_runs: 1,
            execution_mode: "run_only".into(),
            concurrency_policy: "skip".into(),
            enabled: true,
            api_trigger_enabled: false,
        }
    }

    #[test]
    fn db_str_roundtrips_for_every_kind() {
        for k in [
            RuleChangeKind::Created,
            RuleChangeKind::Instructions,
            RuleChangeKind::Schedule,
            RuleChangeKind::Target,
            RuleChangeKind::Policy,
            RuleChangeKind::Paused,
            RuleChangeKind::Resumed,
            RuleChangeKind::Trigger,
        ] {
            assert_eq!(RuleChangeKind::from_db_str(k.as_db_str()), Some(k));
        }
    }

    #[test]
    fn from_db_str_is_tolerant_of_an_unknown_token() {
        // A newer daemon's vocabulary must decode to None, never panic — this is
        // what lets migration 0061 omit the CHECK constraint.
        assert_eq!(RuleChangeKind::from_db_str("teleported"), None);
        assert_eq!(RuleChangeKind::from_db_str(""), None);
        assert_eq!(RuleChangeKind::from_db_str("CREATED"), None);
    }

    #[test]
    fn a_rename_is_cosmetic_and_mints_no_version() {
        let before = base();
        let mut after = before.clone();
        after.name = "nightly-renamed".into();
        assert_eq!(classify(&before, &after), None);
        // ...but the change is still RECORDED in the changed list.
        assert_eq!(changed_fields(&before, &after), vec!["name"]);
    }

    #[test]
    fn a_no_op_edit_mints_no_version() {
        let before = base();
        assert_eq!(classify(&before, &before.clone()), None);
        assert!(changed_fields(&before, &before.clone()).is_empty());
    }

    #[test]
    fn each_substantive_field_maps_to_its_kind() {
        let before = base();

        let mut target = before.clone();
        target.agent_id = "agent-2".into();
        assert_eq!(classify(&before, &target), Some(RuleChangeKind::Target));

        let mut sched = before.clone();
        sched.cron_expr = "0 10 * * *".into();
        assert_eq!(classify(&before, &sched), Some(RuleChangeKind::Schedule));

        let mut instr = before.clone();
        instr.instructions = Some("something else".into());
        assert_eq!(
            classify(&before, &instr),
            Some(RuleChangeKind::Instructions)
        );

        // Clearing instructions is substantive too.
        let mut cleared = before.clone();
        cleared.instructions = None;
        assert_eq!(
            classify(&before, &cleared),
            Some(RuleChangeKind::Instructions)
        );

        for mutate in [
            (|s: &mut AutopilotConfigSnapshot| s.max_concurrent_runs = 4)
                as fn(&mut AutopilotConfigSnapshot),
            |s: &mut AutopilotConfigSnapshot| s.execution_mode = "create_issue".into(),
            |s: &mut AutopilotConfigSnapshot| s.concurrency_policy = "queue".into(),
        ] {
            let mut policy = before.clone();
            mutate(&mut policy);
            assert_eq!(classify(&before, &policy), Some(RuleChangeKind::Policy));
        }

        let mut paused = before.clone();
        paused.enabled = false;
        assert_eq!(classify(&before, &paused), Some(RuleChangeKind::Paused));
        assert_eq!(classify(&paused, &before), Some(RuleChangeKind::Resumed));

        let mut trig = before.clone();
        trig.api_trigger_enabled = true;
        assert_eq!(classify(&before, &trig), Some(RuleChangeKind::Trigger));
    }

    #[test]
    fn precedence_picks_one_kind_but_changed_keeps_everything() {
        let before = base();
        let mut after = before.clone();
        after.name = "renamed".into();
        after.cron_expr = "0 10 * * *".into();
        after.instructions = Some("new".into());
        after.max_concurrent_runs = 9;

        // Schedule outranks Instructions and Policy; name never wins.
        assert_eq!(classify(&before, &after), Some(RuleChangeKind::Schedule));
        assert_eq!(
            changed_fields(&before, &after),
            vec!["name", "cron_expr", "instructions", "max_concurrent_runs"]
        );

        // Target outranks Schedule.
        let mut target_too = after.clone();
        target_too.agent_id = "agent-2".into();
        assert_eq!(classify(&before, &target_too), Some(RuleChangeKind::Target));
    }

    #[test]
    fn config_summary_carries_the_changed_list_and_the_published_config() {
        let before = base();
        let mut after = before.clone();
        after.instructions = Some("v2".into());

        let created = before.to_config_summary(None);
        assert_eq!(created["changed"], serde_json::json!([]));
        assert_eq!(created["cron_expr"], "0 9 * * 1-5");

        let edited = after.to_config_summary(Some(&before));
        assert_eq!(edited["changed"], serde_json::json!(["instructions"]));
        assert_eq!(edited["instructions"], "v2");
    }
}
