//! Which EXECUTOR a claimed task's provider work takes, and the precedence that
//! picks it (spine A5's flag, A8's per-agent override).
//!
//! ```text
//!   agent.task_executor  ─▶ recognised?  ─yes─▶ that executor
//!            │ NULL / blank / unrecognised
//!            ▼
//!   HANGAR_TASK_EXECUTOR ─▶ recognised?  ─yes─▶ that executor
//!            │ unset / blank / unrecognised
//!            ▼
//!                              process
//! ```
//!
//! Two shapes here are load-bearing.
//!
//! **An agent contributes an executor only when it NAMES a recognised one.** A
//! NULL column, a blank string and a typo all mean the same thing — no override
//! — so an agent that says nothing usable dispatches exactly as it did before
//! migration 0095 existed. The typo warns, because a misspelled opt-in that
//! silently does nothing is the failure mode this daemon already guards on the
//! environment variable.
//!
//! **[`DaemonDefaultExecutor`] is a distinct TYPE, not another
//! [`TaskExecutor`].** With per-agent selection the daemon-wide value is only an
//! INPUT to the decision: the executor a task actually runs on lives on its
//! `ResolvedDispatch`, and `execute_claimed` has three branches that spawn work
//! from it. A bare `TaskExecutor` parameter made `daemon_default ==
//! TaskExecutor::Acp` compile and read plausibly at every one of them, each
//! routing a task to an executor it was not assigned. Wrapped, and with the
//! wrapper's field private to THIS module, that comparison does not compile in
//! `run_loop` at all.

use ainb_hangar_store::bootstrap::{TASK_EXECUTOR_ACP, TASK_EXECUTOR_PROCESS};

/// Which executor runs a claimed task's provider work.
///
/// A second FIRST-CLASS executor, not a mode: `Process` spawns the provider CLI
/// (`claude -p`, `codex exec`) and keeps its jsonl transcript; `Acp` prompts an
/// ACP adapter over JSON-RPC and keeps its transcript in `fleet_provider_event`.
/// Deliberately a different axis from [`Mode`](crate::runner::Mode): executor and
/// interactivity are two questions, and conflating them is the bug
/// `interactive_command` was extracted to prevent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TaskExecutor {
    /// Spawn the provider CLI. Today's path, and the default.
    #[default]
    Process,
    /// Prompt an ACP adapter through [`crate::acp_task`].
    Acp,
}

impl TaskExecutor {
    /// Every variant, so a test enumerates the executor axis mechanically
    /// rather than sampling the two a human happened to think of.
    pub const ALL: [Self; 2] = [Self::Process, Self::Acp];

    /// The token this executor is spelled with, on the agent row and in
    /// `HANGAR_TASK_EXECUTOR` alike.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Process => TASK_EXECUTOR_PROCESS,
            Self::Acp => TASK_EXECUTOR_ACP,
        }
    }
}

/// Parse one executor token, case-insensitively, or `None` when it names
/// neither executor.
///
/// The ONE place a token becomes a [`TaskExecutor`]: the environment variable
/// and the agent column carry the same vocabulary
/// ([`ainb_hangar_store::bootstrap::SUPPORTED_TASK_EXECUTORS`]), and two parsers
/// would be two places for `acp` to stop meaning ACP.
#[must_use]
pub fn parse(value: &str) -> Option<TaskExecutor> {
    let value = value.trim();
    if value.eq_ignore_ascii_case(TASK_EXECUTOR_ACP) {
        Some(TaskExecutor::Acp)
    } else if value.eq_ignore_ascii_case(TASK_EXECUTOR_PROCESS) {
        Some(TaskExecutor::Process)
    } else {
        None
    }
}

/// The daemon-wide `HANGAR_TASK_EXECUTOR` value: the executor a task takes when
/// its agent asks for none.
///
/// Not a [`TaskExecutor`], on purpose — see the module docs. The only thing that
/// can be done with one is ask it to resolve a specific agent's executor, which
/// is the only correct use of it once per-agent selection exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonDefaultExecutor(TaskExecutor);

impl DaemonDefaultExecutor {
    /// Wrap the daemon-wide default (`DaemonConfig::task_executor`).
    #[must_use]
    pub const fn new(default: TaskExecutor) -> Self {
        Self(default)
    }

    /// Resolve the executor for one agent: its own `task_executor` when that
    /// names a recognised executor, else this daemon-wide default.
    ///
    /// `None`, blank and unrecognised all inherit, so an agent created before
    /// migration 0095 — or one whose value was hand-edited into nonsense — runs
    /// exactly where it ran before. The unrecognised case warns rather than
    /// failing the task: refusing to dispatch would turn a typo in a cosmetic
    /// override into an unrunnable agent.
    #[must_use]
    pub fn for_agent(self, agent_value: Option<&str>) -> TaskExecutor {
        let Some(raw) = agent_value.map(str::trim).filter(|v| !v.is_empty()) else {
            return self.0;
        };
        parse(raw).unwrap_or_else(|| {
            tracing::warn!(
                value = raw,
                inherited = self.0.as_str(),
                "unknown agent.task_executor; inheriting the daemon's HANGAR_TASK_EXECUTOR"
            );
            self.0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{DaemonDefaultExecutor, TaskExecutor, parse};
    use ainb_hangar_store::bootstrap::SUPPORTED_TASK_EXECUTORS;

    /// Every executor the daemon can run is nameable by an operator, and every
    /// name an operator may write resolves to an executor.
    ///
    /// The two sides are enumerated from their own sources — the enum's `ALL`
    /// and the store constant the create paths validate against — so a variant
    /// added without a token (an executor nobody can select) and a token added
    /// without a variant (a value `agent create` accepts and dispatch ignores)
    /// both fail here. A count would have caught neither.
    #[test]
    fn the_token_vocabulary_and_the_executor_set_cover_each_other() {
        // `map` + panic, never `filter_map`: a token that resolves to NO
        // executor is the whole failure this direction exists to catch, and
        // filtering it out is how the guard silently passed with a bogus
        // `"tmux"` in the vocabulary.
        let from_tokens: Vec<TaskExecutor> = SUPPORTED_TASK_EXECUTORS
            .iter()
            .map(|token| {
                parse(token).unwrap_or_else(|| {
                    panic!("`{token}` is offered to operators but resolves to no executor")
                })
            })
            .collect();
        assert_eq!(
            from_tokens,
            TaskExecutor::ALL.to_vec(),
            "every supported token must resolve to a distinct executor, in the same order"
        );
        for executor in TaskExecutor::ALL {
            assert!(
                SUPPORTED_TASK_EXECUTORS.contains(&executor.as_str()),
                "{executor:?} has no token an operator can write"
            );
        }
    }

    /// The FULL precedence matrix: every daemon-wide default crossed with every
    /// shape an agent's column can take.
    ///
    /// Both axes are enumerated rather than sampled: the executor axis from
    /// [`TaskExecutor::ALL`], the "agent asks for X" axis from the same
    /// [`SUPPORTED_TASK_EXECUTORS`] the write paths validate against (so a new
    /// token joins this matrix automatically), and the "agent asks for nothing"
    /// axis from every spelling of absence the column admits.
    #[test]
    fn the_agent_wins_when_it_names_an_executor_and_inherits_otherwise() {
        // Every way the column says "no override": NULL, and the blank shapes a
        // hand-edited row or a whitespace-only CLI argument can leave behind.
        let no_override: [Option<&str>; 4] = [None, Some(""), Some("   "), Some("\t")];
        // Every way it says something the daemon cannot honour.
        let unrecognised: [&str; 4] = ["acpp", "tmux", "ac p", "0"];

        for daemon_default in TaskExecutor::ALL {
            let default = DaemonDefaultExecutor::new(daemon_default);

            for absent in no_override {
                assert_eq!(
                    default.for_agent(absent),
                    daemon_default,
                    "{absent:?} must inherit the daemon default {daemon_default:?}"
                );
            }

            for token in SUPPORTED_TASK_EXECUTORS {
                let asked = parse(token).expect("a supported token parses");
                assert_eq!(
                    default.for_agent(Some(token)),
                    asked,
                    "the agent's `{token}` must win over the daemon default {daemon_default:?}"
                );
                // The same token as an operator may realistically have typed it.
                assert_eq!(
                    default.for_agent(Some(&token.to_uppercase())),
                    asked,
                    "`{token}` must resolve case-insensitively"
                );
                assert_eq!(
                    default.for_agent(Some(&format!("  {token} "))),
                    asked,
                    "`{token}` must resolve with surrounding whitespace"
                );
            }

            for garbage in unrecognised {
                assert_eq!(
                    default.for_agent(Some(garbage)),
                    daemon_default,
                    "`{garbage}` must inherit {daemon_default:?}, not pick an executor"
                );
            }
        }
    }
}
