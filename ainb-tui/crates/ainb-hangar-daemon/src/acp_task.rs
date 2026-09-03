//! Run one hangar task over ACP instead of spawning a provider CLI.
//!
//! The third arm of [`crate::run_loop::execute_claimed`]'s exec-path branch,
//! selected by `HANGAR_TASK_EXECUTOR=acp` and returning the same
//! [`RunOutcome`] the two process arms do, so everything below `let outcome =`
//! (finalize, PR capture, board advance, retry) is untouched.
//!
//! ```text
//!   register claude-agent-acp#task:<id>  ── per-task adapter PROCESS
//!            │  cwd + agent_env + permission mode + OS sandbox are per process
//!            ▼
//!   acp_session::ensure(scope_key = "task:<id>")  ── no spawn, one txn
//!            ▼
//!   acp_session::enqueue  ─▶  pool.submit_prompt  ─▶  the adapter's turn
//!            ▼
//!   POLL the delivery leg (the pool resolves it on EVERY path)
//!            ▼
//!   leg state + detail token set ──▶ RunOutcome  (unmapped ⇒ contract drift)
//! ```
//!
//! Two shapes here are load-bearing and are not preferences.
//!
//! **The leg is polled, never awaited.** The pool resolves the PENDING delivery
//! leg in one transaction on turn end, on adapter exit, on the deadline sweep
//! and on boot convergence ([`crate::acp_pool`]), so it is the one signal that
//! is always written. A `oneshot` on the prompt job would be ~40 lines and one
//! missed send would freeze the task at `running` until the multi-hour TTL —
//! the exact black hole `execute_claimed`'s spawn-setup umbrella exists to
//! close.
//!
//! **The adapter process is per TASK, not the shared pool's.** The permission
//! mode, the child environment and the OS sandbox are all per PROCESS
//! ([`ainb_acp::config::AdapterConfig`]), so a shared `claude-agent-acp` cannot
//! host one task's `agent_env` without handing it to every other tenant, and
//! its `max_in_flight_per_process: 4` would cap the whole fleet's concurrent
//! task turns at four.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ainb_acp::config::AdapterConfig;
use ainb_hangar_store::repo::fleet_acp_session::FleetAcpSessionRepo;
use ainb_hangar_store::repo::fleet_message::FleetMessageRepo;
use ainb_hangar_store::repo::task::Task;
use ainb_hangar_store::service::fail::FailureReason;
use sqlx::SqlitePool;

use crate::acp_pool::{AcpPool, ConvergeCause, SubmitOutcome};
use crate::events::EventSink;
use crate::execenv::ExecEnv;
use crate::runner::{Backend, RunLocation, RunOutcome, RunnerResult};

/// How often the delivery leg is re-read while the turn is open.
///
/// One indexed read per second per running ACP task, bounded by the agent's
/// `max_concurrent_tasks`. The pool's own deadline sweep runs at 15 s, so a
/// tighter poll buys nothing an operator can see.
const LEG_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How many transcript replies are scanned for the run's final message.
///
/// A turn writes exactly one `agent` reply threaded to its prompt
/// (`acp_pool::finish_turn`), so this is a bound on a pathological store, not a
/// window that has to be tuned.
const REPLY_SCAN_LIMIT: i64 = 8;

/// The `fleet_acp_session.scope_key` a task's session is created under.
///
/// Deliberately derivable from the task id alone: the cancel arm, the durable
/// timeline read and this module all resolve the same session without a new
/// column on either side, and `acp_session::ensure` is already idempotent per
/// live scope.
#[must_use]
pub fn scope_key(task_id: &str) -> String {
    format!("task:{task_id}")
}

/// The workspace an ACP session's scope belongs to, or `None` when the scope
/// is not a task's.
///
/// The one place the `task:<id>` scope convention is read back, so the pool
/// does not have to know it. An attention row raised by a task's session lands
/// unscoped without this, and a workspace-filtered inbox — which is every
/// operator surface — never shows it.
pub async fn workspace_for_scope(pool: &SqlitePool, scope: &str) -> Option<String> {
    let task_id = scope.strip_prefix("task:")?;
    ainb_hangar_store::repo::task::TaskRepo::get_by_id(pool, task_id)
        .await
        .ok()
        .flatten()
        .map(|task| task.workspace_id)
}

/// The adapter-registry key a task's OWN adapter process is registered under.
///
/// `<base>#task:<id>`, so the pool's one-process-per-key rule is the isolation
/// and the base adapter every chat tenant shares is never reconfigured.
fn adapter_key(base: &str, task_id: &str) -> String {
    format!("{base}#task:{task_id}")
}

/// The `CLAUDE_CONFIG_DIR` a task's adapter is pointed at, inside the task's
/// own execenv root (so the OS sandbox already allows writes to it).
///
/// Without it the adapter merges the OPERATOR's `~/.claude`: proven live on a
/// dev box, a `settings.json` carrying `permissions.defaultMode: auto` makes
/// `session/new` fail outright with `-32603 Invalid permissions.defaultMode`,
/// and even when it parses the task inherits every globally-installed skill and
/// the global `CLAUDE.md`. The operator's own directory is never touched.
fn task_config_dir(env: &ExecEnv) -> std::path::PathBuf {
    env.root().join("claude-config")
}

/// Which ACP adapter serves `backend`.
///
/// Refused rather than defaulted for anything else: silently prompting
/// `claude-agent-acp` for a task whose agent asked for copilot is the same
/// class of bug as spawning the headless argv into an interactive pane.
const fn adapter_for(backend: Backend) -> Option<&'static str> {
    match backend {
        Backend::Claude => Some(ainb_acp::config::CLAUDE_ADAPTER),
        Backend::Codex => Some(ainb_acp::config::CODEX_ADAPTER),
        Backend::Copilot | Backend::Antigravity => None,
    }
}

/// Unregisters a task's adapter key (and stops its process) on EVERY exit path,
/// including the cancel that DROPS [`run_acp`] mid-poll.
///
/// `Drop` cannot await, so the removal rides a detached task.
/// `AcpPool::unregister_adapter` is idempotent, so the ordinary end-of-run path
/// needs no separate call and a double drop is benign.
struct AdapterLease {
    pool: Arc<AcpPool>,
    key: String,
}

impl Drop for AdapterLease {
    fn drop(&mut self) {
        let pool = Arc::clone(&self.pool);
        let key = std::mem::take(&mut self.key);
        tokio::spawn(async move {
            if pool.unregister_adapter(&key).await {
                tracing::debug!(adapter = %key, "released a task's adapter process");
            }
        });
    }
}

/// Run `task`'s brief as one ACP turn and map the delivery leg onto a
/// [`RunOutcome`].
///
/// Never returns `Err` for an agent-side failure: every refusal, timeout and
/// adapter fault is a terminal outcome the caller finalizes through the same
/// seam the process executor uses. `Err` is reserved for a store fault that
/// leaves the run undecidable.
///
/// # Errors
///
/// Returns an error only when the session or the prompt cannot be written to
/// the store, so the caller terminalises rather than freezing at `running`.
// One over the lint's 7, and the same shape the sibling run functions in
// `run_loop` carry: every argument is a distinct collaborator the run needs and
// bundling them into a context struct is a wider refactor than this arm wants.
// `task_env` is the map `prepare_spawn_inputs` returns verbatim; generalising
// its hasher would only make the one call site spell out a type parameter.
#[allow(clippy::too_many_arguments, clippy::implicit_hasher)]
pub async fn run_acp(
    pool: &SqlitePool,
    events: &EventSink,
    task: &Task,
    dispatch: &crate::run_loop::ResolvedDispatch,
    env: &ExecEnv,
    location: &RunLocation,
    task_env: HashMap<String, String>,
    max_runtime: Duration,
) -> anyhow::Result<RunOutcome> {
    let Some(acp) = crate::acp_pool::active_handle().await else {
        tracing::error!(task_id = %task.id, "no acp pool installed; cannot run this task over acp");
        return Ok(spawn_error());
    };
    let Some(base) = adapter_for(dispatch.backend) else {
        tracing::error!(
            task_id = %task.id,
            provider = dispatch.backend.name(),
            "no acp adapter serves this provider; run it under HANGAR_TASK_EXECUTOR=process"
        );
        return Ok(RunOutcome::Failed {
            reason: FailureReason::ProvisionError,
            result: RunnerResult::default(),
        });
    };
    let Some(recipe) = acp.config().adapters.get(base).cloned() else {
        tracing::error!(task_id = %task.id, adapter = base, "adapter is not in the registry");
        return Ok(spawn_error());
    };

    let key = adapter_key(base, &task.id);
    let cwd = location.cwd.clone();
    acp.register_adapter(
        key.clone(),
        task_adapter(
            &recipe,
            &key,
            env,
            location,
            task_env,
            dispatch.agent_env.clone().expose_for_child_env(),
            sandbox_enabled(),
        ),
    );
    // Held from BEFORE the session exists: a store fault below still has to
    // release the key, and an early `?` would otherwise leak a registry entry.
    let _lease = AdapterLease {
        pool: Arc::clone(&acp),
        key: key.clone(),
    };

    // The session is minted against the TASK's key, not the base adapter, so
    // its first prompt spawns the process this run just registered.
    let scope = scope_key(&task.id);
    let session =
        crate::acp_session::ensure(pool, events, &key, &cwd.to_string_lossy(), Some(&scope))
            .await?;
    let session_key = session.session_key;
    // Written the moment it exists, so a run that later fails or is cancelled
    // still points at the transcript it produced; the success path would
    // otherwise be the only one that records it (`CompleteTaskService`).
    if let Err(error) =
        ainb_hangar_store::repo::task::TaskRepo::set_session_id(pool, &task.id, &session_key).await
    {
        tracing::warn!(task_id = %task.id, %error, "could not record the acp session on the task");
    }

    let message_id =
        crate::acp_session::enqueue(pool, &session_key, &scope, &dispatch.invocation.prompt)
            .await?;
    if let SubmitOutcome::Rejected(detail) =
        acp.submit_prompt(&session_key, &message_id, &dispatch.invocation.prompt).await
    {
        tracing::warn!(task_id = %task.id, detail, "the acp pool refused the task's prompt");
        return Ok(outcome_for(
            "REJECTED",
            Some(detail),
            RunnerResult::default(),
        ));
    }

    let (state, detail) = await_leg(pool, &acp, &session_key, &message_id, max_runtime).await;
    let result = build_result(pool, &session_key, &message_id).await;
    Ok(outcome_for(&state, detail.as_deref(), result))
}

/// `session/cancel` the task's open turn — the ACP analogue of the interactive
/// arm's `kill_session`.
///
/// Dropping [`run_acp`] stops POLLING; it does not stop the agent, which lives
/// in another process that has not been told anything. Called from the cancel
/// arm of `execute_claimed`'s `select!` for exactly that reason.
///
/// Returns after the actor has HANDLED the cancel (its convergence resolves
/// every pending leg on the session), not merely after the message was sent:
/// the lease drop that follows kills the adapter process, and killing it first
/// would mean the adapter never saw the `session/cancel` it is being stopped
/// by. Bounded, so a wedged actor delays a cancel by seconds rather than
/// forever.
pub async fn cancel_run(pool: &SqlitePool, task_id: &str) {
    let Some(acp) = crate::acp_pool::active_handle().await else {
        return;
    };
    let session = FleetAcpSessionRepo::get_live_by_scope(pool, &scope_key(task_id)).await;
    let Ok(Some(session)) = session else {
        return;
    };
    if !acp.cancel(&session.session_key, ConvergeCause::OperatorStop).await {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match FleetMessageRepo::pending_deliveries_for_session(pool, &session.session_key).await {
            Ok(legs) if legs.is_empty() => return,
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    tracing::warn!(
        task_id,
        session_key = %session.session_key,
        "acp cancel did not converge in time; tearing the adapter down anyway"
    );
}

/// The per-task adapter recipe: the base adapter's program, this task's
/// environment, permission mode and confinement.
fn task_adapter(
    base: &AdapterConfig,
    key: &str,
    env: &ExecEnv,
    location: &RunLocation,
    task_env: HashMap<String, String>,
    agent_env: Vec<(String, String)>,
    sandbox: bool,
) -> AdapterConfig {
    let config_dir = task_config_dir(env);
    // Best-effort: an adapter handed a config dir it has to create itself still
    // gets a CLEAN one, which is the whole point of pointing it here.
    if let Err(error) = std::fs::create_dir_all(&config_dir) {
        tracing::warn!(%error, dir = %config_dir.display(), "could not pre-create the task config dir");
    }
    let mut extra_env: Vec<(String, String)> = task_env.into_iter().collect();
    // Deterministic, so a leaked-env assertion reads the same list every run.
    extra_env.sort();
    // The ONE permitted plaintext escape: the child env. Layered last so the
    // agent's own values win, matching `compose_child_env` on the process path.
    extra_env.extend(agent_env);
    extra_env.push((
        "CLAUDE_CONFIG_DIR".to_string(),
        config_dir.to_string_lossy().into_owned(),
    ));

    let mut adapter = AdapterConfig {
        name: key.to_string(),
        command: base.command.clone(),
        args: base.args.clone(),
        permission_mode: base.permission_mode.clone(),
        // The adapter authenticates itself; this names the variable it reads
        // WITHOUT the daemon minting one, so a token that is present in the
        // daemon's environment is honoured and an absent one is not faked.
        env_passthrough: base.env_passthrough.clone(),
        extra_env,
        config_options: base.config_options.clone(),
        sandbox: None,
    };
    if sandbox {
        let mut policy = ainb_hangar_sandbox::SandboxPolicy::confined_to(env.root());
        if let Some(root) = location.extra_root.as_deref() {
            policy = policy.allow_read(root).allow_write(root);
        }
        adapter.sandbox = Some(policy);
    }
    adapter
}

/// The headless OS sandbox posture, read through the daemon's own pure
/// resolver so this path and the process path cannot disagree about what
/// `HANGAR_DAEMON_DISABLE_SANDBOX` means.
fn sandbox_enabled() -> bool {
    crate::run_loop::DaemonConfig::resolve_sandbox(
        std::env::var_os("HANGAR_DAEMON_DISABLE_SANDBOX").as_deref(),
    )
}

/// Poll the delivery leg until it leaves `PENDING`, or the run outlives
/// `max_runtime`, and answer the `(state, detail)` the outcome mapping reads.
///
/// On the deadline the turn is CANCELLED on the way out — the adapter would
/// otherwise keep working on a run nobody is waiting for — and the pair
/// returned is the same one the pool's own deadline sweep would have written,
/// so both routes to a timed-out task map identically.
async fn await_leg(
    pool: &SqlitePool,
    acp: &Arc<AcpPool>,
    session_key: &str,
    message_id: &str,
    max_runtime: Duration,
) -> (String, Option<String>) {
    let deadline = Instant::now() + max_runtime;
    loop {
        match FleetMessageRepo::deliveries_for_message(pool, message_id).await {
            Ok(legs) => match legs.into_iter().find(|leg| leg.session_key == session_key) {
                Some(leg) if leg.state != "PENDING" => return (leg.state, leg.detail),
                Some(_) => {}
                // The leg is written in the same transaction as the message, so
                // its absence is not a race: it is a store that lost a row.
                // Answered as drift by the caller rather than polled forever.
                None => return ("MISSING".to_string(), None),
            },
            // Transient: keep polling, the deadline is still the bound.
            Err(error) => tracing::warn!(%session_key, %error, "acp delivery leg read failed"),
        }
        if Instant::now() >= deadline {
            acp.cancel(session_key, ConvergeCause::TurnDeadline).await;
            return (
                "UNKNOWN".to_string(),
                Some(crate::acp_pool::DELIVERY_TURN_DEADLINE.to_string()),
            );
        }
        tokio::time::sleep(LEG_POLL_INTERVAL).await;
    }
}

/// The run artefacts a finalize reads: the session key as the run's session id,
/// and the turn's final agent message as the stdout tail PR capture scans.
async fn build_result(pool: &SqlitePool, session_key: &str, message_id: &str) -> RunnerResult {
    // The turn's final message is already persisted as a threaded `agent` reply
    // in the task's own scope (`acp_pool::finish_turn`), so this reads ONE row
    // rather than re-deriving it from the transcript chunk stream.
    let stdout_tail = FleetMessageRepo::list_by_origin(pool, message_id, 0, REPLY_SCAN_LIMIT)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.kind == "agent")
        .map(|row| row.body)
        .collect::<Vec<_>>()
        .join("\n");
    RunnerResult {
        // An ACP turn has no process and so no exit code; the leg carries the
        // outcome instead.
        exit_code: None,
        session_id: Some(session_key.to_string()),
        // A7 parses the `acp.usage` rows; until then an ACP run reports none
        // rather than a fabricated zero.
        usage: None,
        stdout_tail,
        // The adapter's stderr is INHERITED by the daemon (`ainb_acp::client`),
        // so there is no tail to capture here.
        stderr_tail: String::new(),
    }
}

/// [`FailureReason::SpawnError`] with no captured output — the pool or its
/// registry could not produce an adapter at all.
fn spawn_error() -> RunOutcome {
    RunOutcome::Failed {
        reason: FailureReason::SpawnError,
        result: RunnerResult::default(),
    }
}

/// Map a resolved delivery leg onto the outcome the task FSM finalizes.
///
/// The detail is read as a `; `-joined TOKEN SET, never by equality: a plain
/// success can read `resume=loaded`, a budget stop `stop=max_tokens;
/// resume=reprimed`, and an adapter exit appends the raw error text after its
/// token. An unmapped detail is
/// [`FailureReason::ProviderContractDrift`] on purpose — an unknown token means
/// the pool's taxonomy grew, not that the agent succeeded.
#[must_use]
pub fn outcome_for(state: &str, detail: Option<&str>, result: RunnerResult) -> RunOutcome {
    use crate::acp_pool as tokens;

    let set: Vec<&str> = detail
        .unwrap_or_default()
        .split("; ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();
    let has = |token: &str| set.contains(&token);
    let stop = set.iter().find_map(|token| token.strip_prefix(tokens::DELIVERY_STOP_PREFIX));

    let failed = |reason: FailureReason| RunOutcome::Failed {
        reason,
        result: result.clone(),
    };
    match state {
        // No `stop=` token on a DELIVERED leg IS the ordinary `EndTurn`: the
        // pool writes the token only when the reason is worth naming.
        "DELIVERED" => match stop {
            None => RunOutcome::Success(result),
            Some("max_tokens" | "max_turn_requests") => failed(FailureReason::IterationLimit),
            Some(_) => failed(FailureReason::ProviderContractDrift),
        },
        "FAILED" if has(tokens::DELIVERY_TURN_FAILED) => match stop {
            Some("refusal") => failed(FailureReason::AgentError),
            Some("cancelled") => RunOutcome::Cancelled(result),
            _ => failed(FailureReason::ProviderContractDrift),
        },
        "FAILED" if has(tokens::DELIVERY_OPERATOR_STOP) => RunOutcome::Cancelled(result),
        "FAILED"
            if has(tokens::DELIVERY_SPAWN_FAILED)
                || has(tokens::DELIVERY_MODE_UNPROVEN)
                || has(tokens::DELIVERY_TURN_UNRECORDED) =>
        {
            failed(FailureReason::SpawnError)
        }
        "FAILED" if has(tokens::DELIVERY_SESSION_GONE) => failed(FailureReason::ProvisionError),
        "FAILED"
            if has(tokens::DELIVERY_BREAKER_OPEN)
                || has(tokens::DELIVERY_PROVIDER_AT_CAPACITY)
                || has(tokens::DELIVERY_QUEUE_FULL) =>
        {
            failed(FailureReason::RuntimeOffline)
        }
        "UNKNOWN" if has(tokens::DELIVERY_ADAPTER_EXIT) => failed(FailureReason::RuntimeOffline),
        "UNKNOWN" if has(tokens::DELIVERY_TURN_DEADLINE) => failed(FailureReason::Timeout),
        "UNKNOWN" if has(tokens::DELIVERY_DAEMON_RESTART) => failed(FailureReason::RuntimeRecovery),
        // Refused at the door: nothing reached the adapter, and the refusal
        // reason is a standing condition a re-dispatch will meet again.
        "REJECTED" => failed(FailureReason::SpawnError),
        _ => failed(FailureReason::ProviderContractDrift),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn exec_env(root: &Path) -> ExecEnv {
        ExecEnv {
            workdir: root.join("workdir"),
            output: root.join("output"),
            logs: root.join("logs"),
            gc_meta: root.join(".gc_meta.json"),
        }
    }

    #[test]
    fn scope_and_adapter_keys_name_the_task() {
        assert_eq!(scope_key("t-1"), "task:t-1");
        assert_eq!(
            adapter_key(ainb_acp::config::CLAUDE_ADAPTER, "t-1"),
            "claude-agent-acp#task:t-1"
        );
    }

    #[test]
    fn only_the_two_acp_backends_are_servable() {
        assert_eq!(adapter_for(Backend::Claude), Some("claude-agent-acp"));
        assert_eq!(adapter_for(Backend::Codex), Some("codex-acp"));
        assert_eq!(adapter_for(Backend::Copilot), None);
        assert_eq!(adapter_for(Backend::Antigravity), None);
    }

    #[test]
    fn the_per_task_adapter_carries_this_task_and_no_other() {
        let dir = tempfile::tempdir().expect("tempdir");
        let env = exec_env(dir.path());
        let base = AdapterConfig::new(ainb_acp::config::CLAUDE_ADAPTER, "bypassPermissions")
            .command("/opt/claude-agent-acp")
            .env_passthrough(vec!["CLAUDE_CODE_OAUTH_TOKEN".to_string()]);
        let location = RunLocation {
            cwd: dir.path().join("worktree"),
            extra_root: Some(dir.path().join("worktree")),
        };
        let adapter = task_adapter(
            &base,
            "claude-agent-acp#task:t-1",
            &env,
            &location,
            HashMap::from([("PATH".to_string(), "/usr/bin".to_string())]),
            vec![("SECRET_TOKEN".to_string(), "sk-live-1".to_string())],
            false,
        );

        assert_eq!(adapter.name, "claude-agent-acp#task:t-1");
        assert_eq!(
            adapter.command,
            std::path::PathBuf::from("/opt/claude-agent-acp")
        );
        // The mode is the base's, not a default: an unpinned adapter has been
        // observed inheriting `bypassPermissions` from ambient state.
        assert_eq!(adapter.permission_mode, "bypassPermissions");
        assert_eq!(
            adapter.env_passthrough,
            vec!["CLAUDE_CODE_OAUTH_TOKEN".to_string()]
        );

        let env_map: HashMap<&str, &str> = adapter
            .extra_env
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        assert_eq!(env_map.get("SECRET_TOKEN"), Some(&"sk-live-1"));
        assert_eq!(env_map.get("PATH"), Some(&"/usr/bin"));
        // Pointed at the task's OWN config tree, inside the execenv root the
        // sandbox already allows writes to, so the adapter never merges the
        // operator's `~/.claude`.
        let config_dir = env.root().join("claude-config");
        assert_eq!(
            env_map.get("CLAUDE_CONFIG_DIR").map(std::path::Path::new),
            Some(config_dir.as_path())
        );
        assert!(
            config_dir.is_dir(),
            "the task config dir is created up front"
        );
    }

    #[test]
    fn the_sandbox_policy_widens_to_the_run_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let env = exec_env(dir.path());
        let worktree = dir.path().join("worktree");
        let location = RunLocation {
            cwd: worktree.clone(),
            extra_root: Some(worktree.clone()),
        };
        let base = AdapterConfig::new(ainb_acp::config::CLAUDE_ADAPTER, "default");
        let build = |sandbox| {
            task_adapter(
                &base,
                "k",
                &env,
                &location,
                HashMap::new(),
                Vec::new(),
                sandbox,
            )
        };

        // Sandbox OFF is an explicit posture, not an accident of construction.
        assert!(build(false).sandbox.is_none());

        let policy = build(true).sandbox.expect("a confined per-task adapter");
        assert!(
            policy.write_roots.contains(&env.root().to_path_buf()),
            "the task tree must be writable: {:?}",
            policy.write_roots
        );
        // The provisioned worktree lives OUTSIDE the task tree, so without the
        // widening the confined agent could not touch its own checkout.
        assert!(
            policy.write_roots.contains(&worktree),
            "the run worktree must be writable: {:?}",
            policy.write_roots
        );
        assert!(policy.read_roots.contains(&worktree));
    }

    #[test]
    fn a_delivered_leg_with_no_stop_token_is_the_ordinary_success() {
        assert!(matches!(
            outcome_for("DELIVERED", None, RunnerResult::default()),
            RunOutcome::Success(_)
        ));
        // `resume=loaded` rides on an ORDINARY success, so equality on the
        // detail would read a healthy turn as drift.
        assert!(matches!(
            outcome_for("DELIVERED", Some("resume=loaded"), RunnerResult::default()),
            RunOutcome::Success(_)
        ));
    }
}
