// ABOUTME: The copilot pane's engine / model / mode header — what is in force,
// what it can be cycled to, and what happened the last time it was turned.
//
// Its own module rather than fields on `ChatHost` because the two answer
// different questions. The host owns ONE CONVERSATION and its timeline; this
// owns the SETTINGS behind whichever session that conversation is talking to,
// and turning the engine dial replaces that session outright. Folding it into
// the host would make "the conversation" and "the thing the conversation runs
// on" one object, and the swap is precisely the moment they differ.

use std::sync::{Arc, Mutex};

use ainb_hangar_proto::fleet::{FleetAdapter, FleetCopilotConfigureParams, FleetCopilotMode};

/// What the header is doing, and what it last failed at.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DialStatus {
    /// Nothing in flight; the header shows what is in force.
    #[default]
    Idle,
    /// A call is out. Carries the verb for the header's spinner line.
    Working(String),
    /// The last call failed. Carries the METHOD as well as the detail, because
    /// "which call failed" is the actionable half: `adapter_list` failing means
    /// no engines, `copilot_configure` failing means the engine did not change.
    Failed { call: String, detail: String },
}

/// One landed effect.
#[derive(Debug, Clone)]
pub enum DialOutcome {
    /// The registry answered.
    Adapters(Vec<FleetAdapter>),
    /// The channel list answered, broadcast channels only.
    Channels(Vec<String>),
    /// A configure landed; the settings are now these.
    Applied {
        provider: String,
        mode: FleetCopilotMode,
        model: Option<String>,
        replaced: bool,
    },
    /// A call failed, naming the method.
    Failed { call: String, detail: String },
}

/// The copilot header's state and its in-flight effects.
#[derive(Debug)]
pub struct CopilotDial {
    adapters: Vec<FleetAdapter>,
    /// The named broadcast channels, by name. A durable conversation an
    /// operator can come back to, which is the thing the checkbox broadcast is
    /// deliberately not.
    channels: Vec<String>,
    engine: Option<String>,
    model: Option<String>,
    mode: FleetCopilotMode,
    status: DialStatus,
    /// True once the registry has been asked for, so a per-frame tick does not
    /// spawn a worker per repaint.
    asked: bool,
    /// Set by the swap, cleared by the next render pass that has shown it.
    replaced_notice: bool,
    /// Whether `fleet/channel_list` has answered at least once.
    channels_listed: bool,
    inbox: Arc<Mutex<Vec<DialOutcome>>>,
}

impl Default for CopilotDial {
    fn default() -> Self {
        Self::new()
    }
}

impl CopilotDial {
    /// A header that has not yet read the registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            adapters: Vec::new(),
            channels: Vec::new(),
            engine: None,
            model: None,
            mode: FleetCopilotMode::default(),
            status: DialStatus::Idle,
            asked: false,
            replaced_notice: false,
            channels_listed: false,
            inbox: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The adapter in force, or `None` before the registry has answered.
    #[must_use]
    pub fn engine(&self) -> Option<&str> {
        self.engine.as_deref()
    }

    /// The model in force. `None` means the adapter runs its own default.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The guardrail dial in force.
    #[must_use]
    pub const fn mode(&self) -> FleetCopilotMode {
        self.mode
    }

    /// What the header is doing.
    #[must_use]
    pub const fn status(&self) -> &DialStatus {
        &self.status
    }

    /// Every adapter the daemon can spawn.
    #[must_use]
    pub fn adapters(&self) -> &[FleetAdapter] {
        &self.adapters
    }

    /// The named broadcast channels, in creation order.
    #[must_use]
    pub fn channels(&self) -> &[String] {
        &self.channels
    }

    /// Whether the channel list has come back yet.
    ///
    /// Distinguishes "no channels exist" from "not asked yet", which render
    /// differently: the first is a fact, the second is a pending read that
    /// would otherwise look like the first.
    #[must_use]
    pub const fn channels_listed(&self) -> bool {
        self.channels_listed
    }

    /// Whether the last swap replaced the session, for the header's notice.
    #[must_use]
    pub const fn session_replaced(&self) -> bool {
        self.replaced_notice
    }

    /// The models the CURRENT engine declares, in picker order.
    #[must_use]
    pub fn models(&self) -> &[String] {
        self.engine
            .as_deref()
            .and_then(|engine| self.adapters.iter().find(|adapter| adapter.name == engine))
            .map_or(&[], |adapter| adapter.models.as_slice())
    }

    /// Fold in what landed, and ask for the registry the first time.
    ///
    /// Returns `true` when anything changed, so the caller marks the frame
    /// dirty without diffing the header.
    pub fn tick(&mut self) -> bool {
        let landed: Vec<DialOutcome> = self
            .inbox
            .lock()
            .map(|mut inbox| inbox.drain(..).collect())
            .unwrap_or_else(|poisoned| poisoned.into_inner().drain(..).collect());
        let mut changed = !landed.is_empty();
        for outcome in landed {
            match outcome {
                DialOutcome::Adapters(adapters) => {
                    // The engine is only DEFAULTED here, never overwritten: a
                    // registry refresh that landed after a swap must not put the
                    // header back on the adapter the operator just left.
                    if self.engine.is_none() {
                        self.engine = adapters.first().map(|adapter| adapter.name.clone());
                    }
                    self.adapters = adapters;
                    self.status = DialStatus::Idle;
                }
                DialOutcome::Applied {
                    provider,
                    mode,
                    model,
                    replaced,
                } => {
                    self.engine = Some(provider);
                    self.mode = mode;
                    self.model = model;
                    self.replaced_notice = replaced;
                    self.status = DialStatus::Idle;
                }
                DialOutcome::Channels(channels) => {
                    self.channels = channels;
                    self.channels_listed = true;
                }
                DialOutcome::Failed { call, detail } => {
                    self.status = DialStatus::Failed { call, detail };
                }
            }
        }
        if !self.asked {
            self.asked = true;
            self.status = DialStatus::Working("reading the adapter registry".to_string());
            self.load_adapters();
            self.load_channels();
            changed = true;
        }
        changed
    }

    /// Fold these outcomes in WITHOUT touching the daemon.
    ///
    /// The header's render tests need a dial in a known state, and the only
    /// other route to one is a live `fleet/adapter_list`. `asked` is latched
    /// here so the fold cannot also spawn a worker against a daemon the test
    /// does not have.
    #[cfg(test)]
    pub(crate) fn seed_for_test(&mut self, outcomes: Vec<DialOutcome>) {
        self.asked = true;
        if let Ok(mut inbox) = self.inbox.lock() {
            inbox.extend(outcomes);
        }
        self.tick();
    }

    /// Re-read the registry and retry whatever failed. The pane's `r`.
    pub fn retry(&mut self) {
        self.status = DialStatus::Working("reading the adapter registry".to_string());
        self.load_adapters();
        self.load_channels();
    }

    /// Move to the next engine in the registry and apply it.
    ///
    /// A no-op with a spoken reason when the registry holds one adapter or
    /// none: silently doing nothing is the failure mode this whole surface
    /// exists to remove.
    pub fn cycle_engine(&mut self) {
        if self.adapters.len() < 2 {
            self.status = DialStatus::Failed {
                call: "engine".to_string(),
                detail: if self.adapters.is_empty() {
                    "no adapters in the registry; check [acp.adapters] in config.toml".to_string()
                } else {
                    "only one adapter is configured; add another under [acp.adapters]".to_string()
                },
            };
            return;
        }
        let next = self.next_after(self.engine.as_deref());
        self.apply(next, self.mode, None, "swapping the engine");
    }

    /// Move to the next model the current engine declares and apply it.
    pub fn cycle_model(&mut self) {
        let models = self.models().to_vec();
        if models.is_empty() {
            self.status = DialStatus::Failed {
                call: "model".to_string(),
                detail: format!(
                    "{} declares no models; list them under [acp.adapters.{}].models",
                    self.engine.as_deref().unwrap_or("this adapter"),
                    self.engine.as_deref().unwrap_or("<name>")
                ),
            };
            return;
        }
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let index = self
            .model
            .as_deref()
            .and_then(|model| models.iter().position(|candidate| candidate == model))
            .map_or(0, |index| (index + 1) % models.len());
        let next_model = models[index].clone();
        self.apply(engine, self.mode, Some(next_model), "setting the model");
    }

    /// Turn the guardrail dial one step and apply it.
    pub fn cycle_mode(&mut self) {
        let Some(engine) = self.engine.clone() else {
            self.status = DialStatus::Failed {
                call: "mode".to_string(),
                detail: "the engine is not known yet; press r to read the registry".to_string(),
            };
            return;
        };
        let next = self.mode.cycle();
        let model = self.model.clone();
        self.apply(engine, next, model, "turning the guardrail dial");
    }

    fn next_after(&self, current: Option<&str>) -> String {
        let index = current
            .and_then(|name| self.adapters.iter().position(|adapter| adapter.name == name))
            .map_or(0, |index| (index + 1) % self.adapters.len());
        self.adapters[index].name.clone()
    }

    fn apply(
        &mut self,
        provider: String,
        mode: FleetCopilotMode,
        model: Option<String>,
        verb: &str,
    ) {
        self.status = DialStatus::Working(verb.to_string());
        self.replaced_notice = false;
        let inbox = Arc::clone(&self.inbox);
        // `model` is sent as-is: `None` means "leave the stored one alone", and
        // the daemon's config write treats it the same way, so a mode-only
        // change cannot clear a model the operator set.
        let params = FleetCopilotConfigureParams {
            provider,
            copilot_mode: Some(mode),
            model,
            reasoning_effort: None,
            persona: None,
        };
        Self::spawn(
            inbox,
            move |publish| match crate::fleet::control::copilot_configure_blocking(params) {
                Ok(result) => publish(DialOutcome::Applied {
                    provider: result.provider,
                    mode: result.copilot_mode,
                    model: result.model,
                    replaced: result.session_replaced,
                }),
                Err(detail) => publish(DialOutcome::Failed {
                    call: "fleet/copilot_configure".to_string(),
                    detail,
                }),
            },
        );
    }

    fn load_adapters(&self) {
        let inbox = Arc::clone(&self.inbox);
        Self::spawn(
            inbox,
            |publish| match crate::fleet::control::adapter_list_blocking() {
                Ok(adapters) => publish(DialOutcome::Adapters(adapters)),
                Err(detail) => publish(DialOutcome::Failed {
                    call: "fleet/adapter_list".to_string(),
                    detail,
                }),
            },
        );
    }

    /// Page the named broadcast channels.
    ///
    /// A FAILURE here does not take the header's status: the engine dial still
    /// works without a channel list, and hijacking the one status line would
    /// hide an engine failure behind a channel one.
    fn load_channels(&self) {
        let inbox = Arc::clone(&self.inbox);
        Self::spawn(inbox, |publish| {
            if let Ok(channels) = crate::fleet::control::broadcast_channels_blocking() {
                publish(DialOutcome::Channels(channels));
            }
        });
    }

    /// Run `work` on a detached worker, guaranteeing the inbox gets SOMETHING.
    ///
    /// A worker that returned silently would leave the header spinning on a
    /// `Working` status forever, which is the same never-resolving spinner the
    /// chat host's dispatch is built to avoid.
    fn spawn<F>(inbox: Arc<Mutex<Vec<DialOutcome>>>, work: F)
    where
        F: FnOnce(&dyn Fn(DialOutcome)) + Send + 'static,
    {
        let publish_inbox = Arc::clone(&inbox);
        let spawned =
            std::thread::Builder::new().name("ainb-copilot-dial".into()).spawn(move || {
                let publish = |outcome: DialOutcome| {
                    if let Ok(mut inbox) = publish_inbox.lock() {
                        inbox.push(outcome);
                    }
                };
                work(&publish);
            });
        if let Err(error) = spawned {
            if let Ok(mut inbox) = inbox.lock() {
                inbox.push(DialOutcome::Failed {
                    call: "worker".to_string(),
                    detail: format!("the copilot dial worker did not start: {error}"),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(name: &str, models: &[&str]) -> FleetAdapter {
        FleetAdapter {
            name: name.to_string(),
            command: name.to_string(),
            permission_mode: "default".to_string(),
            built_in: true,
            models: models.iter().map(ToString::to_string).collect(),
        }
    }

    fn dial_with(adapters: Vec<FleetAdapter>) -> CopilotDial {
        let mut dial = CopilotDial::new();
        dial.inbox.lock().unwrap().push(DialOutcome::Adapters(adapters));
        // `asked` first, so the tick folds the seeded registry in without also
        // spawning a real worker at a daemon this test does not have.
        dial.asked = true;
        dial.tick();
        dial
    }

    /// The engine list is the DAEMON's, so a cycle walks whatever it returned,
    /// not a list compiled in here.
    #[test]
    fn the_engine_cycles_through_the_registry_and_wraps() {
        let dial = dial_with(vec![
            adapter("claude-agent-acp", &[]),
            adapter("codex-acp", &[]),
            adapter("some-vendor-acp", &[]),
        ]);
        assert_eq!(dial.engine(), Some("claude-agent-acp"));
        assert_eq!(dial.next_after(Some("claude-agent-acp")), "codex-acp");
        assert_eq!(dial.next_after(Some("some-vendor-acp")), "claude-agent-acp");
        // An engine the registry has never heard of restarts at the top rather
        // than panicking on a missing index.
        assert_eq!(dial.next_after(Some("deleted-acp")), "claude-agent-acp");
    }

    /// A dial that cannot move says WHY. Doing nothing quietly is the exact
    /// failure this pane replaces.
    #[test]
    fn a_cycle_with_nothing_to_cycle_to_speaks() {
        let mut empty = dial_with(vec![]);
        empty.cycle_engine();
        assert!(
            matches!(empty.status(), DialStatus::Failed { call, detail }
                if call == "engine" && detail.contains("no adapters")),
            "got {:?}",
            empty.status()
        );

        let mut one = dial_with(vec![adapter("claude-agent-acp", &[])]);
        one.cycle_engine();
        assert!(
            matches!(one.status(), DialStatus::Failed { detail, .. }
                if detail.contains("only one adapter")),
            "got {:?}",
            one.status()
        );

        // No models declared: ACP cannot be asked what a model is, so the
        // header names the config key instead of offering a guess.
        let mut modelless = dial_with(vec![adapter("claude-agent-acp", &[])]);
        modelless.cycle_model();
        assert!(
            matches!(modelless.status(), DialStatus::Failed { call, detail }
                if call == "model" && detail.contains("[acp.adapters.claude-agent-acp].models")),
            "got {:?}",
            modelless.status()
        );
    }

    #[test]
    fn the_model_cycles_within_the_current_engine_only() {
        let mut dial = dial_with(vec![
            adapter("claude-agent-acp", &["sonnet-5", "opus-5"]),
            adapter("codex-acp", &["gpt-5"]),
        ]);
        assert_eq!(dial.models(), ["sonnet-5", "opus-5"]);
        dial.engine = Some("codex-acp".to_string());
        assert_eq!(
            dial.models(),
            ["gpt-5"],
            "the model list must follow the engine, not stay on the first adapter's"
        );
    }

    /// A landed swap moves the header to the adapter the DAEMON confirmed, not
    /// the one that was asked for, and flags the replaced session.
    #[test]
    fn an_applied_swap_takes_the_daemons_answer() {
        let mut dial = dial_with(vec![
            adapter("claude-agent-acp", &[]),
            adapter("codex-acp", &[]),
        ]);
        dial.inbox.lock().unwrap().push(DialOutcome::Applied {
            provider: "codex-acp".to_string(),
            mode: FleetCopilotMode::Yolo,
            model: Some("gpt-5".to_string()),
            replaced: true,
        });
        assert!(dial.tick());
        assert_eq!(dial.engine(), Some("codex-acp"));
        assert_eq!(dial.mode(), FleetCopilotMode::Yolo);
        assert_eq!(dial.model(), Some("gpt-5"));
        assert!(dial.session_replaced());
        assert_eq!(dial.status(), &DialStatus::Idle);
    }

    /// A registry refresh landing after a swap must not drag the header back to
    /// the adapter the operator just left.
    #[test]
    fn a_late_registry_refresh_does_not_overwrite_a_chosen_engine() {
        let mut dial = dial_with(vec![
            adapter("claude-agent-acp", &[]),
            adapter("codex-acp", &[]),
        ]);
        dial.inbox.lock().unwrap().push(DialOutcome::Applied {
            provider: "codex-acp".to_string(),
            mode: FleetCopilotMode::Guarded,
            model: None,
            replaced: true,
        });
        dial.tick();
        dial.inbox.lock().unwrap().push(DialOutcome::Adapters(vec![
            adapter("claude-agent-acp", &[]),
            adapter("codex-acp", &[]),
        ]));
        dial.tick();
        assert_eq!(dial.engine(), Some("codex-acp"));
    }

    /// The failure names the CALL. "which call failed" is the actionable half.
    /// "no channels exist" and "the list has not come back" are different
    /// facts and must not render the same: the second one resolving into the
    /// first is how an operator concludes their channels were deleted.
    #[test]
    fn an_unanswered_channel_list_is_not_an_empty_one() {
        let mut dial = dial_with(vec![]);
        assert!(!dial.channels_listed());
        assert!(dial.channels().is_empty());

        dial.inbox.lock().unwrap().push(DialOutcome::Channels(vec![]));
        dial.tick();
        assert!(dial.channels_listed(), "an empty answer is still an answer");

        dial.inbox
            .lock()
            .unwrap()
            .push(DialOutcome::Channels(vec!["release".into(), "qa".into()]));
        dial.tick();
        assert_eq!(dial.channels(), ["release", "qa"]);
    }

    #[test]
    fn a_failure_names_the_method_that_failed() {
        let mut dial = dial_with(vec![]);
        dial.inbox.lock().unwrap().push(DialOutcome::Failed {
            call: "fleet/adapter_list".to_string(),
            detail: "daemon is not running".to_string(),
        });
        dial.tick();
        assert_eq!(
            dial.status(),
            &DialStatus::Failed {
                call: "fleet/adapter_list".to_string(),
                detail: "daemon is not running".to_string()
            }
        );
    }
}
