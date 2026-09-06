// ABOUTME: The ATC supervisor's provider gate — which agents can actually host
// a heartbeat brain, and the refusal when one cannot.
//
// This module used to own a MODE as well: two controllers, `LiteScanner` and
// `FullHeartbeat`, with a persisted `AtcMeta::mode` and a `may_act` gate
// consulted before every send, because both auto-continued into the same panes
// and the only thing keeping them apart had been a start-time refusal a race
// could walk past.
//
// Lite is gone, so there is one controller and nothing to gate. What lite did —
// LLM-free auto-continue of known transient API errors, inside a per-session
// retry cap — is now the hangar daemon's own retry sweep. That sweep needs no
// ATC instance at all and excludes any session an instance owns, checked every
// tick, so the exclusivity this module enforced is now a property of the
// sweep's roster rather than a mode an operator has to hold correct.
//
// The ledger moved with it. Both modes used to spend the code-owned
// `continue_counts` in `heartbeat-state.json`, with a fail-closed handover
// whenever the hangar daemon owned the durable `atc_retry` table instead. The
// sweep spends `atc_retry` directly, under its own reserved instance, so there
// is one ledger and no handover to get wrong.

use serde::{Deserialize, Serialize};

use crate::providers::{AtcControl, ProviderRegistry};

/// The default full-mode provider. Every pre-existing instance predates the
/// provider field, so this is also what they deserialize to — full mode keeps
/// behaving exactly as it did.
pub const DEFAULT_PROVIDER: &str = "claude";

// SupervisorMode, Controller, `may_act` and `stand_down_reason` stood here.
//
// All four existed for ONE reason: to keep two controllers from sending actions
// to the same fleet, because lite mode's scanner and full mode's heartbeat both
// typed into the same panes. With lite deleted there is one controller, so an
// exclusivity gate has nothing to exclude and a "mode" with a single value is a
// setting that cannot be set.
//
// What lite did is now the hangar daemon's own retry sweep. It shares no pane
// with the heartbeat: it acts on sessions no ATC instance owns, checked every
// tick, so the invariant these types enforced is now a property of the sweep's
// roster rather than a mode an operator has to hold correct.

// ── Provider capability gate ────────────────────────────────────────────────

/// Look up what ainb can drive for `provider_id`, or `None` when no such
/// provider is registered at all.
#[must_use]
pub fn provider_control(provider_id: &str) -> Option<AtcControl> {
    ProviderRegistry::built_ins().get(provider_id).map(|p| p.atc_control())
}

/// Every provider id that can currently host a full-mode brain, in registry order.
///
/// Read off the capability, never hard-coded, so a provider that implements
/// `atc_control` becomes selectable everywhere at once.
#[must_use]
pub fn supported_full_providers() -> Vec<&'static str> {
    let registry = ProviderRegistry::built_ins();
    registry
        .iter()
        .filter(|p| p.atc_control().is_supported())
        .map(|p| p.id())
        .collect()
}

/// Validate a full-mode provider choice, returning its capability.
///
/// Errors rather than degrading: provisioning a full-mode ATC on a provider ainb
/// cannot nudge produces a session that boots, reads nothing, and is never woken
/// — which looks healthy on every surface. Refusing is the honest answer.
pub fn resolve_full_provider(provider_id: &str) -> anyhow::Result<AtcControl> {
    let supported = supported_full_providers();
    let Some(control) = provider_control(provider_id) else {
        anyhow::bail!(
            "unknown provider '{provider_id}' — full mode supports: {}",
            supported.join(", ")
        );
    };
    if !control.is_supported() {
        anyhow::bail!(
            "provider '{provider_id}' cannot drive ATC full mode: ainb has no way to \
{}. Full mode supports: {}. Lite mode works regardless of provider — it sends no \
prompts to a brain at all.",
            missing_capability(control),
            supported.join(", ")
        );
    }
    Ok(control)
}

/// Name the half that is missing, so the refusal says what would have to exist.
const fn missing_capability(control: AtcControl) -> &'static str {
    match (control.resident_session, control.heartbeat_injection) {
        (false, false) => "host a resident supervisor session for it, or inject a heartbeat turn",
        (true, false) => "inject a heartbeat turn into its session",
        (false, true) => "host a resident supervisor session for it",
        (true, true) => "drive it", // unreachable: that combination is supported
    }
}

// ── Operator-facing help ────────────────────────────────────────────────────

/// The concise inline help shown beside an instance: what the heartbeat does,
/// and what it costs.
///
/// Returned as lines rather than one blob so the TUI can style them and the CLI
/// can print them unchanged — the two surfaces must never describe an instance
/// differently.
#[must_use]
pub fn mode_help(provider: &str) -> Vec<String> {
    vec![
        format!("brain: {provider}"),
        "scheduled heartbeat wakes an LLM session that triages the ambiguous \
work and coordinates the fleet."
            .to_string(),
        format!(
            "     limits: spends tokens every beat; needs a provider ainb can \
drive ({}).",
            supported_full_providers().join(" / ")
        ),
        // Named here because an operator reading this help is deciding whether
        // they need an instance at all: transient-error auto-continue no longer
        // requires one, and provisioning an LLM brain to get it would be paying
        // tokens for what the daemon already does for free.
        "transient API errors are auto-continued by the hangar daemon's retry \
sweep, with no instance and no LLM."
            .to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Exclusivity ─────────────────────────────────────────────────────────

    // ── Persistence / parsing ───────────────────────────────────────────────

    // ── Provider capability gating ──────────────────────────────────────────

    #[test]
    fn claude_and_codex_can_drive_full_mode_today() {
        let supported = supported_full_providers();
        assert!(supported.contains(&"claude"), "got {supported:?}");
        assert!(supported.contains(&"codex"), "got {supported:?}");
        assert!(resolve_full_provider("claude").is_ok());
        assert!(resolve_full_provider("codex").is_ok());
    }

    #[test]
    fn claude_is_the_default_provider_and_reads_claude_md() {
        assert_eq!(DEFAULT_PROVIDER, "claude");
        let control = resolve_full_provider(DEFAULT_PROVIDER).unwrap();
        assert_eq!(control.policy_file, "CLAUDE.md");
    }

    #[test]
    fn codex_policy_lands_in_the_file_codex_actually_reads() {
        // A CLAUDE.md would be provisioned, ignored, and look fine.
        assert_eq!(
            resolve_full_provider("codex").unwrap().policy_file,
            "AGENTS.md"
        );
    }

    #[test]
    fn a_provider_without_real_control_is_refused_not_faked() {
        for id in ["copilot", "antigravity", "gemini"] {
            let control = provider_control(id).unwrap_or(AtcControl::UNSUPPORTED);
            assert!(
                !control.is_supported(),
                "{id} claims full-mode control it has not implemented"
            );
            let err = resolve_full_provider(id).expect_err("must refuse, not fake");
            let msg = err.to_string();
            assert!(msg.contains(id), "refusal must name the provider: {msg}");
            assert!(
                msg.contains("lite") || msg.contains("Lite"),
                "refusal must point at the mode that still works: {msg}"
            );
        }
    }

    #[test]
    fn an_unregistered_provider_is_refused_with_the_supported_list() {
        let err = resolve_full_provider("mystery-llm").unwrap_err().to_string();
        assert!(err.contains("mystery-llm"), "{err}");
        assert!(err.contains("claude"), "must list what does work: {err}");
    }

    #[test]
    fn supported_list_is_derived_from_capabilities_not_hard_coded() {
        // Every id the list offers must independently resolve, and every
        // registered provider that resolves must be on the list. This is what
        // makes adding a provider a one-method change.
        let listed = supported_full_providers();
        for id in &listed {
            assert!(resolve_full_provider(id).is_ok(), "{id} listed but refused");
        }
        for p in ProviderRegistry::built_ins().iter() {
            assert_eq!(
                listed.contains(&p.id()),
                p.atc_control().is_supported(),
                "{} listed/capability mismatch",
                p.id()
            );
        }
    }

    // ── Help text ───────────────────────────────────────────────────────────
}
