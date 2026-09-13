// ABOUTME: Daemon-side pane binding for hook-sourced Fleet rows (spec D14,
// issue #916). Resolves which tmux pane a managed session occupies when the
// hook process could not tell us itself.
//
// Tier-0 identity never rides on launcher environment. A provider may run its
// hooks from a long-lived shared daemon whose environment predates the pane:
// Codex 0.154 attaches the interactive TUI to a shared app-server, so the hook
// process inherits that daemon's environment and `$TMUX_PANE` is absent. The
// `session_id` and `cwd` in the payload still identify the session, so the
// store key survives; what is lost is the pane.
//
// A null `tmux_target` is not cosmetic. It skips legacy-row retirement
// (`fleet::retire_correlated_legacy`), so one agent shows as two rows, a
// tier-5 discovered pane row and a tier-0 hook row, and it strips the exact
// target the send-keys answer path needs. Measured on the ainb-owned
// app-server: 1,215 of 1,215 sampled hook lines carried a null target.
//
// The daemon can recover the pane the hook could not name, because it already
// holds the tier-5 discovery scan. Correlate `(provider, cwd)` against the
// discovered panes running that provider in that directory and bind when
// EXACTLY ONE matches. Zero or two is not a coin toss: binding the wrong pane
// types an answer into an agent that never asked, so an unresolved row stays
// `pane_unbound` and says so, on the fleet panel and in `ainb doctor`.

use ainb_hangar_store::repo::fleet::FleetRepoError;
use sqlx::SqlitePool;

/// Why a managed row could not be bound to a pane. Carried for the operator
/// surfaces (`ainb doctor`, the fleet panel detail): the two cases have
/// different fixes, so they are never collapsed into one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnboundReason {
    /// No discovered pane runs this provider in this directory. The agent is
    /// most likely not in tmux at all, or its pane has not been scanned yet.
    NoCandidate,
    /// More than one discovered pane runs this provider in this directory, so
    /// no single pane can be attributed. Carries the candidate targets in
    /// scan order so the operator can see the collision.
    Ambiguous(Vec<String>),
    /// The pane this row was bound to is now running a different process
    /// (#961). Carries the target that was invalidated and the candidates
    /// available now, because the operator's question is "where did my agent
    /// go", not "why did this fail".
    Invalidated {
        /// The pane the row had been bound to.
        previous_target: String,
        /// Discovered panes for this `(provider, cwd)` right now, in scan
        /// order. Empty is normal and means the agent is no longer in tmux.
        candidates: Vec<String>,
    },
}

impl UnboundReason {
    /// Operator-facing sentence. Names the directory and, when the problem is
    /// a collision, the exact panes that collided: "names the pane it could
    /// not find" rather than failing silently.
    #[must_use]
    pub fn describe(&self, provider: &str, cwd: &str) -> String {
        match self {
            Self::NoCandidate => format!(
                "pane_unbound: no discovered {provider} pane in {cwd} (the hook carried no tmux target and nothing matched)"
            ),
            Self::Ambiguous(candidates) => format!(
                "pane_unbound: {} discovered {provider} panes in {cwd} ({}), no single pane can be attributed",
                candidates.len(),
                candidates.join(", ")
            ),
            Self::Invalidated {
                previous_target,
                candidates,
            } => {
                let now = if candidates.is_empty() {
                    format!("no {provider} pane in {cwd} now")
                } else {
                    format!("{provider} panes in {cwd} now: {}", candidates.join(", "))
                };
                format!(
                    "pane_unbound: {previous_target} is running a different process than the one bound to this session, so the binding was dropped rather than typed into ({now})"
                )
            }
        }
    }
}

/// Outcome of resolving one hook observation onto a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneBinding {
    /// The hook named its own pane. Nothing was inferred.
    FromHook {
        /// `session:window.pane`.
        target: String,
        /// `pane=…;pid=…;session_started=…`, when the hook carried one.
        fingerprint: Option<String>,
    },
    /// Exactly one discovered pane matched `(provider, cwd)`.
    Correlated {
        /// `session:window.pane` taken from the discovered row.
        target: String,
        /// The discovered row's fingerprint, when it had one.
        fingerprint: Option<String>,
        /// The discovered row's key, retired onto the managed key by the caller.
        legacy_key: String,
    },
    /// Nothing could be attributed. The row renders `pane_unbound`.
    Unbound(UnboundReason),
}

impl PaneBinding {
    /// The resolved target, whatever resolved it. `None` is `pane_unbound`.
    #[must_use]
    pub fn target(&self) -> Option<&str> {
        match self {
            Self::FromHook { target, .. } | Self::Correlated { target, .. } => Some(target),
            Self::Unbound(_) => None,
        }
    }

    /// The resolved fingerprint. `None` when unbound, or when the source knew
    /// a target but no fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> Option<&str> {
        match self {
            Self::FromHook { fingerprint, .. } | Self::Correlated { fingerprint, .. } => {
                fingerprint.as_deref()
            }
            Self::Unbound(_) => None,
        }
    }

    /// True when this row has no pane.
    #[must_use]
    pub fn is_unbound(&self) -> bool {
        matches!(self, Self::Unbound(_))
    }
}

/// One tier-5 discovered pane row considered as a binding candidate.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PaneCandidate {
    /// The discovered row's own key.
    pub session_key: String,
    /// `session:window.pane`.
    pub tmux_target: String,
    /// The scan's fingerprint for the pane, when it had one.
    pub process_start_fingerprint: Option<String>,
}

/// Resolve the pane for one hook observation.
///
/// `hook_target` wins outright when present: the hook process saw its own
/// `$TMUX_PANE` and no inference can beat that. Otherwise correlate.
///
/// # Errors
/// Propagates a store fault from the candidate query. A caller that cannot
/// tolerate one should treat the failure as `Unbound` rather than dropping the
/// event: the row is still correct, only its pane is missing.
pub async fn resolve(
    pool: &SqlitePool,
    managed_key: &str,
    provider: &str,
    cwd: &str,
    hook_target: Option<String>,
    hook_fingerprint: Option<String>,
) -> Result<PaneBinding, FleetRepoError> {
    // A binding this row already made is re-confirmed before anything else
    // (#961). Until now a correlated decision was never checked again, so a
    // pane that had been reused by a different agent kept receiving this
    // session's send-keys.
    if let Some(decision) = bound_decision(pool, managed_key).await? {
        match confirm(pool, managed_key, provider, cwd, &decision, hook_fingerprint.as_deref())
            .await?
        {
            Confirmation::Holds => {
                return Ok(PaneBinding::Correlated {
                    target: decision.target,
                    fingerprint: decision.fingerprint,
                    // Nothing to retire: the legacy row was retired when this
                    // binding was first made.
                    legacy_key: String::new(),
                });
            }
            Confirmation::Broken(candidates) => {
                return Ok(PaneBinding::Unbound(UnboundReason::Invalidated {
                    previous_target: decision.target,
                    candidates,
                }));
            }
            // Nothing observed this pane on this pass, so there is no evidence
            // either way. A binding is not dropped on silence: an unscanned
            // pane and a reused one look identical from here, and only one of
            // them is a reason to stop delivering.
            Confirmation::Unobserved => {
                return Ok(PaneBinding::Correlated {
                    target: decision.target,
                    fingerprint: decision.fingerprint,
                    legacy_key: String::new(),
                });
            }
        }
    }

    if let Some(target) = hook_target {
        return Ok(PaneBinding::FromHook {
            target,
            fingerprint: hook_fingerprint,
        });
    }
    let candidates = discovered_candidates(pool, managed_key, provider, cwd).await?;
    Ok(bind(candidates))
}

/// The binding decision a row already holds.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BoundDecision {
    target: String,
    fingerprint: Option<String>,
}

/// What this pass can say about a binding the row already holds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Confirmation {
    /// The bound pane still runs the process it was bound to.
    Holds,
    /// It runs a different one. Carries the candidates available now.
    Broken(Vec<String>),
    /// Nothing observed the pane on this pass.
    Unobserved,
}

/// Read the written-once decision, if this row has made one.
///
/// `bound_fingerprint IS NULL` is a decision with nothing to re-confirm
/// against, which is the pre-0099 shape and the shape a hook that knew its
/// target but not its process produces. Those are returned too, so the target
/// is still carried, and `confirm` answers `Unobserved` for them rather than
/// inventing a mismatch.
async fn bound_decision(
    pool: &SqlitePool,
    managed_key: &str,
) -> Result<Option<BoundDecision>, FleetRepoError> {
    let row = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT bound_target, bound_fingerprint FROM fleet_session WHERE session_key = ?",
    )
    .bind(managed_key)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(target, fingerprint)| {
        target.map(|target| BoundDecision {
            target,
            fingerprint,
        })
    }))
}

/// Compare the bound pane against what is in it now.
///
/// The hook's own fingerprint is preferred when the hook named the same pane:
/// it is this agent reporting its own process, which is better evidence than a
/// scan. Otherwise the tier-5 scan for `(provider, cwd)` is consulted, and a
/// pane absent from it is `Unobserved` rather than broken.
async fn confirm(
    pool: &SqlitePool,
    managed_key: &str,
    provider: &str,
    cwd: &str,
    decision: &BoundDecision,
    hook_fingerprint: Option<&str>,
) -> Result<Confirmation, FleetRepoError> {
    let Some(bound) = decision.fingerprint.as_deref() else {
        return Ok(Confirmation::Unobserved);
    };
    if let Some(observed) = hook_fingerprint {
        return Ok(if observed == bound {
            Confirmation::Holds
        } else {
            Confirmation::Broken(
                discovered_candidates(pool, managed_key, provider, cwd)
                    .await?
                    .into_iter()
                    .map(|candidate| candidate.tmux_target)
                    .collect(),
            )
        });
    }
    let live = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT tmux_target, process_start_fingerprint FROM fleet_session \
         WHERE tmux_target = ? AND visible = 1 AND superseded_by IS NULL \
           AND session_key != ? \
         ORDER BY last_observed_at DESC LIMIT 1",
    )
    .bind(&decision.target)
    .bind(managed_key)
    .fetch_optional(pool)
    .await?;
    let Some((_, Some(observed))) = live else {
        return Ok(Confirmation::Unobserved);
    };
    if observed == bound {
        return Ok(Confirmation::Holds);
    }
    Ok(Confirmation::Broken(
        discovered_candidates(pool, managed_key, provider, cwd)
            .await?
            .into_iter()
            .map(|candidate| candidate.tmux_target)
            .collect(),
    ))
}

/// Choose a binding from the candidate set. Split from the query so the
/// 1 / 0 / 2 decision is testable without a store.
#[must_use]
pub fn bind(mut candidates: Vec<PaneCandidate>) -> PaneBinding {
    match candidates.len() {
        1 => {
            let only = candidates.remove(0);
            PaneBinding::Correlated {
                target: only.tmux_target,
                fingerprint: only.process_start_fingerprint,
                legacy_key: only.session_key,
            }
        }
        0 => PaneBinding::Unbound(UnboundReason::NoCandidate),
        _ => PaneBinding::Unbound(UnboundReason::Ambiguous(
            candidates.into_iter().map(|c| c.tmux_target).collect(),
        )),
    }
}

/// Tier-5 discovered panes running `provider` in `cwd`.
///
/// Scoped to rows the scan owns and nothing else claims: `DEGRADED` management
/// (a `MANAGED` row is another agent's hook row, never a free pane), a live
/// target, not superseded, still visible, and not already exited. `cwd` is
/// matched exactly: a hook whose agent has `cd`-ed below its session root
/// reports the subdirectory, and binding on containment would let one pane
/// swallow every session beneath it.
///
/// `provider_session_id IS NULL` is what makes a candidate genuinely tier 5,
/// and `management_state` alone is not enough to establish that. `apply_hook`
/// promotes a row to `MANAGED` only for Claude, so another CODEX session's hook
/// row sits at `DEGRADED` with a resolved pane and used to qualify. Two Codex
/// sessions in one directory, one on 0.148 whose pane is visible and one on
/// 0.154 whose pane is null, then correlated onto each other: the unbound row
/// took the bound row's pane and `supersede_session` retired the row that
/// actually owned it. Two live agents merged into one and an answer typed into
/// the agent that had not asked.
///
/// A discovery scan cannot learn a provider's own session id, so a row that has
/// one was written by a hook and belongs to an agent. This is the same test
/// `tier_of` already makes.
///
/// # Errors
/// Propagates the store fault.
pub async fn discovered_candidates(
    pool: &SqlitePool,
    managed_key: &str,
    provider: &str,
    cwd: &str,
) -> Result<Vec<PaneCandidate>, FleetRepoError> {
    if cwd.is_empty() {
        // An empty cwd matches every unrooted scan row at once. Refusing here
        // is what keeps "we know nothing" from resolving to "bind anything".
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, PaneCandidate>(
        "SELECT session_key, tmux_target, process_start_fingerprint \
         FROM fleet_session \
         WHERE session_key != ? AND provider = ? AND cwd = ? \
           AND management_state = 'DEGRADED' \
           AND provider_session_id IS NULL \
           AND tmux_target IS NOT NULL AND tmux_target != '' \
           AND lifecycle_state != 'EXITED' \
           AND superseded_by IS NULL AND visible = 1 \
         ORDER BY session_key",
    )
    .bind(managed_key)
    .bind(provider)
    .bind(cwd)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Explain, for an answer that found no delivery target, whether the reason is
/// that the raising session has no pane bound (D14, issue #916).
///
/// The answer router discovers its target live, so a `pane_unbound` row fails
/// there with the router's generic "no live session matched", true, but it
/// hides the actual cause and gives the operator nothing to act on. This
/// recomputes the binding for the raising session and returns the same sentence
/// the fleet panel and `ainb doctor` show, so all three name the same pane.
///
/// `None` means the row is bound, absent, or not hook-sourced: the router's own
/// reason is then the accurate one and must not be overwritten.
pub async fn unbound_answer_reason(pool: &SqlitePool, provider_session_id: &str) -> Option<String> {
    if provider_session_id.is_empty() {
        return None;
    }
    let row = sqlx::query_as::<_, (String, String, String, Option<String>)>(
        "SELECT session_key, provider, cwd, tmux_target FROM fleet_session \
         WHERE provider_session_id = ? AND superseded_by IS NULL AND visible = 1 \
         ORDER BY last_observed_at DESC LIMIT 1",
    )
    .bind(provider_session_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()?;
    let (session_key, provider, cwd, tmux_target) = row;
    if tmux_target.is_some_and(|target| !target.is_empty()) {
        return None;
    }
    let candidates = discovered_candidates(pool, &session_key, &provider, &cwd).await.ok()?;
    let reason = match bind(candidates) {
        PaneBinding::Unbound(reason) => reason,
        // The binding resolves NOW even though the row is still null: a later
        // event will adopt it. Saying "no pane" would be wrong, so say nothing.
        PaneBinding::Correlated { .. } | PaneBinding::FromHook { .. } => return None,
    };
    Some(reason.describe(&provider, &cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(key: &str, target: &str) -> PaneCandidate {
        PaneCandidate {
            session_key: key.to_string(),
            tmux_target: target.to_string(),
            process_start_fingerprint: Some(format!("pane=%1;pid=1;session_started={key}")),
        }
    }

    /// The whole point of #916: one match is an attribution, not a guess.
    #[test]
    fn exactly_one_candidate_binds_and_names_the_row_to_retire() {
        let binding = bind(vec![candidate("tmux:dev:1.0", "dev:1.0")]);
        assert_eq!(binding.target(), Some("dev:1.0"));
        assert!(matches!(
            binding,
            PaneBinding::Correlated { ref legacy_key, .. } if legacy_key == "tmux:dev:1.0"
        ));
    }

    #[test]
    fn no_candidate_is_unbound_not_a_guess() {
        let binding = bind(Vec::new());
        assert!(binding.is_unbound());
        assert_eq!(binding.target(), None);
        assert!(matches!(
            binding,
            PaneBinding::Unbound(UnboundReason::NoCandidate)
        ));
    }

    /// Two panes in one directory is the case that would type an answer into
    /// the wrong agent. It must refuse, and it must say which panes collided.
    #[test]
    fn two_candidates_are_unbound_and_name_both_panes() {
        let binding = bind(vec![
            candidate("tmux:dev:1.0", "dev:1.0"),
            candidate("tmux:dev:2.0", "dev:2.0"),
        ]);
        assert!(binding.is_unbound());
        let PaneBinding::Unbound(reason) = &binding else {
            panic!("two candidates must be unbound");
        };
        let text = reason.describe("codex", "/w/app");
        assert!(text.contains("dev:1.0"), "{text}");
        assert!(text.contains("dev:2.0"), "{text}");
        assert!(text.contains("/w/app"), "{text}");
    }

    #[test]
    fn no_candidate_message_names_the_provider_and_directory() {
        let text = UnboundReason::NoCandidate.describe("codex", "/w/app");
        assert!(text.starts_with("pane_unbound:"), "{text}");
        assert!(text.contains("codex"), "{text}");
        assert!(text.contains("/w/app"), "{text}");
    }

    /// A hook that named its own pane is never second-guessed, even when the
    /// scan holds candidates that would have correlated differently.
    #[tokio::test]
    async fn a_hook_provided_target_wins_without_consulting_the_scan() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.expect("open store");
        let binding = resolve(
            store.pool(),
            "claude:sid-1",
            "claude",
            "/w/app",
            Some("dev:1.0".to_string()),
            Some("pane=%7;pid=9;session_started=1".to_string()),
        )
        .await
        .expect("resolve");
        assert_eq!(binding.target(), Some("dev:1.0"));
        assert_eq!(
            binding.fingerprint(),
            Some("pane=%7;pid=9;session_started=1")
        );
        assert!(matches!(binding, PaneBinding::FromHook { .. }));
    }

    /// Another agent's hook row is never a free pane, whatever its
    /// `management_state` says.
    ///
    /// THE #916 hazard. `apply_hook` promotes a row to `MANAGED` only for
    /// Claude, so a second CODEX session's hook row sits at `DEGRADED` with a
    /// resolved pane and used to qualify as a tier-5 candidate. Two Codex
    /// sessions in one directory, one on 0.148 whose pane is visible and one on
    /// 0.154 whose pane is null, then correlated onto each other: the unbound
    /// row took the bound row's pane and the bound row was retired behind it.
    /// Two live agents merged into one, and an answer typed into the agent that
    /// had not asked.
    ///
    /// A discovery scan cannot learn a provider's own session id, so carrying
    /// one is proof a hook wrote the row.
    #[tokio::test]
    async fn another_agents_hook_row_is_not_a_free_pane() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.expect("open store");

        // The 0.148 session: its own hook line named its pane, and being Codex
        // it stays DEGRADED. Only `provider_session_id` distinguishes it from a
        // pane the discovery scan found on its own.
        ainb_hangar_store::repo::fleet::FleetRepo::apply_event(
            store.pool(),
            &ainb_hangar_store::repo::fleet::NewFleetEvent {
                event_id: "hook:codex-a".to_string(),
                session_key: "codex:sid-a".to_string(),
                observed_at: 1,
                authority: ainb_hangar_store::repo::fleet::ObservationAuthority::Authoritative,
                event_type: "SessionStart".to_string(),
                payload: "{}".to_string(),
                patch: ainb_hangar_store::repo::fleet::FleetSessionPatch {
                    provider: Some("codex".to_string()),
                    provider_session_id: Some("sid-a".to_string()),
                    tmux_target: Some("dev:1.0".to_string()),
                    cwd: Some("/w/app".to_string()),
                    lifecycle_state: Some("RUNNING".to_string()),
                    ..ainb_hangar_store::repo::fleet::FleetSessionPatch::default()
                },
            },
        )
        .await
        .expect("the other agent's hook row lands");

        // The 0.154 session asks who owns a pane in the same directory.
        let candidates = discovered_candidates(store.pool(), "codex:sid-b", "codex", "/w/app")
            .await
            .expect("query");
        assert!(
            candidates.is_empty(),
            "a row carrying a provider session id was written by a hook and \
             belongs to an agent, so it is not a pane to hand out: {candidates:?}"
        );

        // And with no candidate the binding says so rather than guessing.
        let binding = resolve(store.pool(), "codex:sid-b", "codex", "/w/app", None, None)
            .await
            .expect("resolve");
        assert!(
            matches!(binding, PaneBinding::Unbound(UnboundReason::NoCandidate)),
            "zero candidates is `pane_unbound`, not a guess: {binding:?}"
        );
    }

    /// THE #961 hazard: a pane is reused by a different agent, and the row
    /// bound to it keeps routing there.
    ///
    /// A correlated binding was never re-confirmed, so `tmux_target` survived
    /// the pane changing hands and send-keys typed this session's answer into
    /// whichever agent holds the pane now. The binding is dropped instead, and
    /// the row says which pane it lost and what is available.
    #[tokio::test]
    async fn a_reused_pane_invalidates_the_binding_instead_of_typing_into_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.expect("open store");

        apply(
            &store,
            "hook:bind",
            "claude:sid-1",
            1,
            bound_patch("sid-1", "pane=%1;pid=1"),
        )
        .await;

        // The same pane, now running a different process.
        apply(
            &store,
            "scan:reused",
            "tmux:dev:1.0",
            2,
            ainb_hangar_store::repo::fleet::FleetSessionPatch {
                provider: Some("claude".to_string()),
                cwd: Some("/w/app".to_string()),
                tmux_target: Some("dev:1.0".to_string()),
                process_start_fingerprint: Some("pane=%1;pid=999".to_string()),
                ..Default::default()
            },
        )
        .await;

        let binding = resolve(store.pool(), "claude:sid-1", "claude", "/w/app", None, None)
            .await
            .expect("resolve");
        let PaneBinding::Unbound(reason) = &binding else {
            panic!("a reused pane must invalidate the binding, got {binding:?}");
        };
        let UnboundReason::Invalidated {
            previous_target, ..
        } = reason
        else {
            panic!("and say so as an invalidation, got {reason:?}");
        };
        assert_eq!(previous_target, "dev:1.0");
        assert_eq!(
            binding.target(),
            None,
            "the row must stop offering a target to send keys to"
        );
        assert!(
            reason.describe("claude", "/w/app").contains("dev:1.0"),
            "the operator is told which pane was lost: {}",
            reason.describe("claude", "/w/app")
        );
    }

    /// The agent's own hook re-confirms its pane, and the binding stands.
    #[tokio::test]
    async fn a_matching_fingerprint_confirms_the_binding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.expect("open store");
        apply(
            &store,
            "hook:bind",
            "claude:sid-2",
            1,
            bound_patch("sid-2", "pane=%1;pid=1"),
        )
        .await;

        let binding = resolve(
            store.pool(),
            "claude:sid-2",
            "claude",
            "/w/app",
            None,
            Some("pane=%1;pid=1".to_string()),
        )
        .await
        .expect("resolve");
        assert_eq!(binding.target(), Some("dev:1.0"), "{binding:?}");
    }

    /// Silence is not evidence of reuse. An unscanned pane and a stolen one
    /// look identical from here, and only one is a reason to stop delivering.
    #[tokio::test]
    async fn an_unobserved_pane_keeps_its_binding() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.expect("open store");
        apply(
            &store,
            "hook:bind",
            "claude:sid-3",
            1,
            bound_patch("sid-3", "pane=%1;pid=1"),
        )
        .await;

        let binding = resolve(store.pool(), "claude:sid-3", "claude", "/w/app", None, None)
            .await
            .expect("resolve");
        assert_eq!(binding.target(), Some("dev:1.0"), "{binding:?}");
    }

    /// A managed row bound to `dev:1.0` while `fingerprint` was in it.
    fn bound_patch(
        session_id: &str,
        fingerprint: &str,
    ) -> ainb_hangar_store::repo::fleet::FleetSessionPatch {
        ainb_hangar_store::repo::fleet::FleetSessionPatch {
            provider: Some("claude".to_string()),
            provider_session_id: Some(session_id.to_string()),
            cwd: Some("/w/app".to_string()),
            management_state: Some("MANAGED".to_string()),
            tmux_target: Some("dev:1.0".to_string()),
            process_start_fingerprint: Some(fingerprint.to_string()),
            bound: Some(("dev:1.0".to_string(), Some(fingerprint.to_string()))),
            ..Default::default()
        }
    }

    /// Apply one event, for the fixtures above.
    async fn apply(
        store: &ainb_hangar_store::Store,
        event_id: &str,
        session_key: &str,
        observed_at: i64,
        patch: ainb_hangar_store::repo::fleet::FleetSessionPatch,
    ) {
        ainb_hangar_store::repo::fleet::FleetRepo::apply_event(
            store.pool(),
            &ainb_hangar_store::repo::fleet::NewFleetEvent {
                event_id: event_id.to_string(),
                session_key: session_key.to_string(),
                observed_at,
                authority: ainb_hangar_store::repo::fleet::ObservationAuthority::Authoritative,
                event_type: "SessionStart".to_string(),
                payload: "{}".to_string(),
                patch,
            },
        )
        .await
        .expect("fixture event applies");
    }

    /// An empty cwd carries no information. Matching on it would correlate a
    /// hook to whichever unrooted scan row happened to sort first.
    #[tokio::test]
    async fn an_empty_cwd_never_correlates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ainb_hangar_store::Store::open_in(dir.path()).await.expect("open store");
        let candidates = discovered_candidates(store.pool(), "claude:sid-1", "claude", "")
            .await
            .expect("query");
        assert!(candidates.is_empty());
    }
}
