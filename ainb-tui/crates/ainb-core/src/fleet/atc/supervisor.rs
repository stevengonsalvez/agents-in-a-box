// ABOUTME: The ATC supervisor — one operator-visible controller per fleet, in
// exactly one mode at a time.
//
// Before this module there were two independent watchers that both auto-continued
// into the same panes: `ainb fleet daemon` (deterministic pane scan, uncapped)
// and ATC's LLM heartbeat (capped, but only when it was the one running). The
// only thing keeping them apart was a start-time refusal in the daemon, which a
// `--force-race` — or simply starting the daemon first — walked straight past.
//
// The replacement is a single persisted fact, `AtcMeta::mode`, and one rule:
//
//   mode  ──▶  owner  ──▶  the ONLY controller allowed to send an action
//
//   ┌──────────────┐        ┌──────────────────────────────────────────┐
//   │ mode = Lite  │ ──────▶│ LiteScanner  (no LLM, deterministic)     │
//   └──────────────┘        └──────────────────────────────────────────┘
//   ┌──────────────┐        ┌──────────────────────────────────────────┐
//   │ mode = Full  │ ──────▶│ FullHeartbeat (LLM session, triage)      │
//   └──────────────┘        └──────────────────────────────────────────┘
//
// Both controllers consult [`may_act`] immediately before every send, against the
// mode re-read from disk. A mode flip therefore silences the losing controller on
// its next action, without needing the flip to win a race against a process that
// is already mid-tick. Neither controller is trusted to remember which mode it
// started in.
//
// Both modes share ONE safety ledger — the code-owned `continue_counts` in
// `heartbeat-state.json` ([`super::HeartbeatState`]) — so the ERR retry cap is
// spent from the same budget whichever controller is holding the fleet. Switching
// modes does not hand a broken session a fresh set of retries.

use serde::{Deserialize, Serialize};

use crate::providers::{AtcControl, ProviderRegistry};

/// The default full-mode provider. Every pre-existing instance predates the
/// provider field, so this is also what they deserialize to — full mode keeps
/// behaving exactly as it did.
pub const DEFAULT_PROVIDER: &str = "claude";

/// Which supervisor is driving this fleet. Exactly one is active per instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SupervisorMode {
    /// No LLM at all. A deterministic scan of the same LLM-free `fleet needs`
    /// read, auto-continuing only *known* transient errors, inside the same
    /// per-session retry cap. It never reasons about an ambiguous session: an
    /// ASK / WAIT / IDLE row is reported, never answered.
    Lite,

    /// The existing ATC: a scheduled heartbeat wakes an LLM session that triages
    /// the ambiguous work and coordinates the fleet.
    ///
    /// `Default` on purpose — an instance provisioned before modes existed has
    /// no `mode` field, and it was a full LLM ATC. Deserializing it as anything
    /// else would silently downgrade a running fleet.
    #[default]
    Full,
}

impl SupervisorMode {
    /// Stable CLI / JSON spelling.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Lite => "lite",
            Self::Full => "full",
        }
    }

    /// Short display label for the Daemons row and CLI text output.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lite => "LITE",
            Self::Full => "FULL",
        }
    }

    /// Parse a CLI value.
    #[must_use]
    pub fn from_id(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "lite" => Some(Self::Lite),
            "full" => Some(Self::Full),
            _ => None,
        }
    }

    /// The other mode — what a toggle switches to.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Lite => Self::Full,
            Self::Full => Self::Lite,
        }
    }

    /// The controller that owns the fleet in this mode.
    #[must_use]
    pub const fn owner(self) -> Controller {
        match self {
            Self::Lite => Controller::LiteScanner,
            Self::Full => Controller::FullHeartbeat,
        }
    }
}

/// One of the two things that can send an action to a monitored session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Controller {
    /// `ainb fleet atc supervise` — the LLM-free scan loop.
    LiteScanner,
    /// `ainb fleet atc heartbeat` — the beat that wakes the LLM session. This is
    /// the single verb BOTH schedulers (the local launchd/systemd timer and the
    /// daemon cron) invoke, so gating it gates every full-mode action path.
    FullHeartbeat,
}

impl Controller {
    /// Human name used in refusals and in the Daemons help text.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LiteScanner => "lite scanner",
            Self::FullHeartbeat => "full heartbeat",
        }
    }
}

/// **The exclusivity rule.** May `controller` send an action, given the mode
/// persisted in `meta.json` right now?
///
/// Called immediately before every send by both controllers. That is what makes
/// concurrency impossible rather than merely unlikely: a lite supervisor left
/// running after a switch to full, or a timer still firing after a switch to
/// lite, reads the current mode and declines instead of doubling up on the
/// pane.
#[must_use]
pub const fn may_act(mode: SupervisorMode, controller: Controller) -> bool {
    matches!(
        (mode, controller),
        (SupervisorMode::Lite, Controller::LiteScanner)
            | (SupervisorMode::Full, Controller::FullHeartbeat)
    )
}

/// The refusal a stood-down controller prints — it names the owner, so an
/// operator reading a log knows which half to look at.
#[must_use]
pub fn stand_down_reason(mode: SupervisorMode, controller: Controller) -> String {
    format!(
        "{} is standing down: this fleet is in {} mode, owned by the {}. \
Exactly one controller sends actions; switch with `ainb fleet atc mode <name> --set {}`.",
        controller.label(),
        mode.id(),
        mode.owner().label(),
        controller_mode(controller).id(),
    )
}

/// The mode under which `controller` would be the owner.
const fn controller_mode(controller: Controller) -> SupervisorMode {
    match controller {
        Controller::LiteScanner => SupervisorMode::Lite,
        Controller::FullHeartbeat => SupervisorMode::Full,
    }
}

// ── Provider capability gate ────────────────────────────────────────────────

/// Look up what ainb can drive for `provider_id`, or `None` when no such
/// provider is registered at all.
#[must_use]
pub fn provider_control(provider_id: &str) -> Option<AtcControl> {
    ProviderRegistry::built_ins().get(provider_id).map(|p| p.atc_control())
}

/// Every provider id that can currently host a full-mode brain, in registry
/// order. This is read off the capability, never hard-coded, so a provider that
/// implements `atc_control` becomes selectable everywhere at once.
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
fn missing_capability(control: AtcControl) -> &'static str {
    match (control.resident_session, control.heartbeat_injection) {
        (false, false) => "host a resident supervisor session for it, or inject a heartbeat turn",
        (true, false) => "inject a heartbeat turn into its session",
        (false, true) => "host a resident supervisor session for it",
        (true, true) => "drive it", // unreachable: that combination is supported
    }
}

// ── Operator-facing help ────────────────────────────────────────────────────

/// The concise inline help shown beside the mode toggle: what each mode does,
/// what it will NOT do, and who owns the fleet right now.
///
/// Returned as lines rather than one blob so the TUI can style them and the CLI
/// can print them unchanged — the two surfaces must never describe the modes
/// differently.
#[must_use]
pub fn mode_help(mode: SupervisorMode, provider: &str) -> Vec<String> {
    let mut lines = vec![
        format!(
            "owner: {} ({} mode){}",
            mode.owner().label(),
            mode.id(),
            match mode {
                SupervisorMode::Full => format!(" · brain: {provider}"),
                SupervisorMode::Lite => String::new(),
            }
        ),
        "lite — no LLM. Scans, auto-continues known transient errors within the \
retry cap, reports everything else."
            .to_string(),
        "     limits: never answers an ASK, never resolves an ambiguous session, \
no fleet coordination."
            .to_string(),
        "full — scheduled heartbeat wakes an LLM session that triages the \
ambiguous work and coordinates the fleet."
            .to_string(),
        format!(
            "     limits: spends tokens every beat; needs a provider ainb can \
drive ({}).",
            supported_full_providers().join(" / ")
        ),
        "one owner at a time — switching stops the other controller before the \
new one sends anything."
            .to_string(),
    ];
    lines.push(format!(
        "switch: `ainb fleet atc mode <name> --set {}`",
        mode.other().id()
    ));
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Exclusivity ─────────────────────────────────────────────────────────

    #[test]
    fn exactly_one_controller_may_act_in_each_mode() {
        // The load-bearing property: for every mode, precisely one of the two
        // controllers is permitted. Never both, never neither.
        for mode in [SupervisorMode::Lite, SupervisorMode::Full] {
            let permitted: Vec<Controller> = [Controller::LiteScanner, Controller::FullHeartbeat]
                .into_iter()
                .filter(|c| may_act(mode, *c))
                .collect();
            assert_eq!(
                permitted.len(),
                1,
                "{} mode permitted {permitted:?}, expected exactly one owner",
                mode.id()
            );
            assert_eq!(permitted[0], mode.owner());
        }
    }

    #[test]
    fn a_controller_from_the_other_mode_is_refused() {
        assert!(!may_act(SupervisorMode::Lite, Controller::FullHeartbeat));
        assert!(!may_act(SupervisorMode::Full, Controller::LiteScanner));
    }

    #[test]
    fn stand_down_reason_names_the_owner_and_the_flag_that_would_fix_it() {
        let msg = stand_down_reason(SupervisorMode::Lite, Controller::FullHeartbeat);
        assert!(msg.contains("lite scanner"), "must name the owner: {msg}");
        assert!(msg.contains("--set full"), "must name the fix: {msg}");
    }

    #[test]
    fn other_round_trips_and_owners_differ() {
        assert_eq!(SupervisorMode::Lite.other(), SupervisorMode::Full);
        assert_eq!(SupervisorMode::Full.other(), SupervisorMode::Lite);
        assert_ne!(SupervisorMode::Lite.owner(), SupervisorMode::Full.owner());
    }

    // ── Persistence / parsing ───────────────────────────────────────────────

    #[test]
    fn default_mode_is_full_so_existing_instances_are_not_downgraded() {
        assert_eq!(SupervisorMode::default(), SupervisorMode::Full);
    }

    #[test]
    fn mode_ids_round_trip() {
        for mode in [SupervisorMode::Lite, SupervisorMode::Full] {
            assert_eq!(SupervisorMode::from_id(mode.id()), Some(mode));
        }
        assert_eq!(SupervisorMode::from_id("FULL"), Some(SupervisorMode::Full));
        assert_eq!(SupervisorMode::from_id(" lite "), Some(SupervisorMode::Lite));
        assert_eq!(SupervisorMode::from_id("hybrid"), None);
    }

    #[test]
    fn mode_serializes_as_its_stable_id() {
        // The JSON spelling is the wire format in meta.json; it must match the
        // CLI value so a hand-edited file and a `--set` agree.
        let json = serde_json::to_string(&SupervisorMode::Lite).unwrap();
        assert_eq!(json, "\"lite\"");
        let back: SupervisorMode = serde_json::from_str("\"full\"").unwrap();
        assert_eq!(back, SupervisorMode::Full);
    }

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
        assert_eq!(resolve_full_provider("codex").unwrap().policy_file, "AGENTS.md");
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

    #[test]
    fn help_names_the_current_owner_both_behaviours_and_both_limits() {
        let help = mode_help(SupervisorMode::Full, "claude").join("\n");
        assert!(help.contains("full heartbeat"), "current owner: {help}");
        assert!(help.contains("brain: claude"), "current provider: {help}");
        assert!(help.contains("no LLM"), "lite behaviour: {help}");
        assert!(help.contains("never answers an ASK"), "lite limits: {help}");
        assert!(help.contains("spends tokens"), "full limits: {help}");
        assert!(help.contains("--set lite"), "the switch: {help}");

        let lite = mode_help(SupervisorMode::Lite, "claude").join("\n");
        assert!(lite.contains("lite scanner"), "current owner: {lite}");
        assert!(lite.contains("--set full"), "the switch: {lite}");
        // Lite runs no brain, so naming a provider there would be misleading.
        assert!(!lite.contains("brain:"), "{lite}");
    }
}
