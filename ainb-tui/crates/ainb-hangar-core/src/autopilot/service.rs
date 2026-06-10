//! P7.2 — the IO-free autopilot service the daemon RPC handlers build on.
//!
//! Like [`crate::skill_service`], the value here is the *invariant*: a
//! [`WorkspaceId`] threads through **every** by-id read and mutation, so a
//! handler physically cannot forget to scope a lookup to the caller's tenant.
//! The (stale) P7 plan sketched an `async fn create(&self, …) -> Result<…>` that
//! issued sqlx directly; that cannot live here because this crate is IO-free (no
//! `tokio`/`sqlx`). Instead the orchestration is a pure [`AutopilotService`] over
//! an [`AutopilotBackend`] trait that the store crate's `AutopilotRepo` implements
//! with real sqlx, and tests fake in memory.
//!
//! # Cron validation happens before any insert
//!
//! [`AutopilotService::create`] parses `cron_expr` via [`crate::autopilot::cron`]
//! **before** delegating to the backend, returning [`AutopilotServiceError::Cron`]
//! with no row written on a bad expression. The validated schedule is used to
//! compute the cached `next_tick_at` (epoch-ms) the backend persists.
//!
//! # Enable recomputes the next tick from *now*
//!
//! [`AutopilotService::enable`] does not replay missed ticks: it recomputes
//! `next_tick_at` strictly after the current clock instant, so an autopilot that
//! was disabled across several scheduled ticks fires once going forward, never a
//! burst of catch-ups. [`AutopilotService::disable`] leaves `next_tick_at`
//! untouched (it is simply ignored while `enabled = 0`).

use crate::autopilot::cron::{
    CronError, millis_to_utc, next_tick_after, parse_cron, utc_to_millis,
};
use crate::clock::HangarClock;
use crate::ids::{AgentId, AutopilotId, AutopilotRunId, WorkspaceId};

/// A stored, cron-scheduled autopilot.
///
/// Mirrors the `autopilot` row one-to-one. `next_tick_at` is the cached
/// next-firing instant in epoch milliseconds (`None` when the schedule has no
/// future match); `enabled` reflects the `0/1` column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Autopilot {
    /// Primary key.
    pub id: AutopilotId,
    /// Owning workspace.
    pub workspace_id: WorkspaceId,
    /// The agent this autopilot dispatches to at each tick.
    pub agent_id: AgentId,
    /// Human-readable name, unique within the workspace.
    pub name: String,
    /// Instructions handed to the agent at each firing; `None` when unset.
    pub instructions: Option<String>,
    /// The (validated) cron expression, UTC.
    pub cron_expr: String,
    /// Maximum simultaneous in-flight runs before a tick is skipped.
    pub max_concurrent_runs: i64,
    /// Cached next-firing instant (epoch-ms); `None` when no future match.
    pub next_tick_at: Option<i64>,
    /// Whether the scheduler considers this autopilot.
    pub enabled: bool,
    /// Creation time (epoch-ms).
    pub created_at: i64,
}

/// A single firing of an [`Autopilot`].
///
/// Mirrors the `autopilot_run` row. `completed_at` is `None` while the run is
/// in flight; `status` is one of `running` / `completed` / `failed` /
/// `cancelled`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutopilotRun {
    /// Primary key.
    pub id: AutopilotRunId,
    /// The autopilot this run belongs to.
    pub autopilot_id: AutopilotId,
    /// When the run was created (epoch-ms).
    pub started_at: i64,
    /// When the run finished (epoch-ms); `None` while in flight.
    pub completed_at: Option<i64>,
    /// Lifecycle status.
    pub status: String,
}

/// The validated inputs to [`AutopilotService::create`].
///
/// `cron_expr` is *not* yet validated when this struct is built — the service
/// validates it (and rejects on failure before any insert).
#[derive(Debug, Clone)]
pub struct CreateAutopilot {
    /// Owning workspace.
    pub workspace_id: WorkspaceId,
    /// Agent to dispatch to.
    pub agent_id: AgentId,
    /// Name, unique within the workspace.
    pub name: String,
    /// Optional instructions handed to the agent.
    pub instructions: Option<String>,
    /// Cron expression (UTC), validated on create.
    pub cron_expr: String,
    /// Maximum simultaneous in-flight runs (defaults to 1 at the SQL layer).
    pub max_concurrent_runs: i64,
}

/// The workspace-scoped autopilot persistence a backend must provide.
///
/// **Every by-id method takes a [`WorkspaceId`] explicitly** and the
/// implementation scopes its SQL `WHERE id = ? AND workspace_id = ?` (and the
/// run-history read joins through `autopilot` to verify the workspace). This is
/// the tenant-isolation contract the secured `SkillRepo` (P6.1) established, applied
/// up front here to avoid an IDOR re-finding.
///
/// The backend receives an already-validated `cron_expr` and the
/// already-computed `next_tick_at`; cron parsing and next-tick math live in the
/// service (so they are exercised without a database).
pub trait AutopilotBackend {
    /// The backend's error type (sqlx error in the daemon; a test error in unit
    /// tests). Cron-validation failures never reach the backend.
    type Error;

    /// Insert a new autopilot row with the supplied (already-computed)
    /// `next_tick_at` (epoch-ms, `None` when the schedule has no future match)
    /// and return the persisted [`Autopilot`].
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] on a backend failure — most notably a
    /// `(workspace_id, name)` uniqueness conflict (the duplicate-name case).
    fn insert(
        &mut self,
        req: &CreateAutopilot,
        next_tick_at: Option<i64>,
        now_ms: i64,
    ) -> Result<Autopilot, Self::Error>;

    /// List every autopilot in `workspace`, ordered by name.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] on a backend failure.
    fn list(&self, workspace: &WorkspaceId) -> Result<Vec<Autopilot>, Self::Error>;

    /// Fetch one autopilot by id, scoped to `workspace` (a foreign id → `None`).
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] on a backend failure.
    fn get(
        &self,
        workspace: &WorkspaceId,
        id: &AutopilotId,
    ) -> Result<Option<Autopilot>, Self::Error>;

    /// Set `enabled` for an autopilot scoped to `workspace`, also writing the
    /// supplied `next_tick_at` (the service passes the unchanged cached value on
    /// disable, and a freshly-recomputed value on enable). A foreign id touches
    /// no row.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] on a backend failure.
    fn set_enabled(
        &mut self,
        workspace: &WorkspaceId,
        id: &AutopilotId,
        enabled: bool,
        next_tick_at: Option<i64>,
    ) -> Result<(), Self::Error>;

    /// List the runs of an autopilot, latest-first, capped at `limit`. Scoped to
    /// `workspace` by joining through `autopilot`: a foreign autopilot id yields
    /// an empty set.
    ///
    /// # Errors
    ///
    /// Returns [`Self::Error`] on a backend failure.
    fn list_runs(
        &self,
        workspace: &WorkspaceId,
        autopilot_id: &AutopilotId,
        limit: u32,
    ) -> Result<Vec<AutopilotRun>, Self::Error>;
}

/// The pure autopilot service over any [`AutopilotBackend`].
///
/// Owns the cron-validation-before-insert and enable-recompute-from-now logic;
/// the backend owns the workspace-scoped row IO. Clock-injected so `next_tick_at`
/// is deterministic in tests.
pub struct AutopilotService<'a, B, C> {
    backend: &'a mut B,
    clock: &'a C,
}

impl<'a, B, C> AutopilotService<'a, B, C>
where
    B: AutopilotBackend,
    C: HangarClock,
{
    /// Wrap a backend and a clock.
    pub const fn new(backend: &'a mut B, clock: &'a C) -> Self {
        Self { backend, clock }
    }

    /// Create an autopilot.
    ///
    /// Parses `req.cron_expr` first and returns [`AutopilotServiceError::Cron`]
    /// with **no row written** on a malformed/out-of-range expression. On a valid
    /// expression, computes `next_tick_at` strictly after the current clock
    /// instant and delegates the insert to the backend.
    ///
    /// # Errors
    ///
    /// - [`AutopilotServiceError::Cron`] when `cron_expr` fails to parse — before
    ///   any insert.
    /// - [`AutopilotServiceError::Backend`] on a backend failure (e.g. a
    ///   `(workspace_id, name)` uniqueness conflict).
    pub fn create(
        &mut self,
        req: &CreateAutopilot,
    ) -> Result<Autopilot, AutopilotServiceError<B::Error>> {
        let now_ms = self.clock.now_ms();
        // Validate (and reject) before any insert.
        let next_tick_at = compute_next_tick(&req.cron_expr, now_ms)?;
        self.backend
            .insert(req, next_tick_at, now_ms)
            .map_err(AutopilotServiceError::Backend)
    }

    /// List the workspace's autopilots, ordered by name.
    ///
    /// # Errors
    ///
    /// Propagates the backend error.
    pub fn list(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<Autopilot>, AutopilotServiceError<B::Error>> {
        self.backend.list(workspace).map_err(AutopilotServiceError::Backend)
    }

    /// Fetch one autopilot, scoped to `workspace`.
    ///
    /// # Errors
    ///
    /// Propagates the backend error.
    pub fn get(
        &self,
        workspace: &WorkspaceId,
        id: &AutopilotId,
    ) -> Result<Option<Autopilot>, AutopilotServiceError<B::Error>> {
        self.backend.get(workspace, id).map_err(AutopilotServiceError::Backend)
    }

    /// Disable an autopilot. Leaves `next_tick_at` untouched (it is ignored
    /// while disabled), so the cached value is preserved for inspection.
    ///
    /// Workspace-scoped: a foreign id touches no row.
    ///
    /// # Errors
    ///
    /// Propagates the backend error.
    pub fn disable(
        &mut self,
        workspace: &WorkspaceId,
        id: &AutopilotId,
    ) -> Result<(), AutopilotServiceError<B::Error>> {
        // Read the current cached next_tick_at and re-write it unchanged; this
        // keeps the value for re-enable inspection and avoids a partial-column
        // UPDATE surface on the backend.
        let next_tick_at = self
            .backend
            .get(workspace, id)
            .map_err(AutopilotServiceError::Backend)?
            .and_then(|a| a.next_tick_at);
        self.backend
            .set_enabled(workspace, id, false, next_tick_at)
            .map_err(AutopilotServiceError::Backend)
    }

    /// Enable an autopilot, recomputing `next_tick_at` strictly after *now* so a
    /// long-disabled autopilot fires once going forward rather than replaying
    /// every missed tick.
    ///
    /// Workspace-scoped: a foreign id touches no row. A no-op when the id is
    /// absent from the workspace (the recompute uses the stored `cron_expr`; an
    /// absent row simply yields nothing to update).
    ///
    /// # Errors
    ///
    /// - [`AutopilotServiceError::Cron`] if the stored `cron_expr` no longer
    ///   parses (a corrupt-row guard; impossible given [`create`] validates).
    /// - [`AutopilotServiceError::Backend`] on a backend failure.
    pub fn enable(
        &mut self,
        workspace: &WorkspaceId,
        id: &AutopilotId,
    ) -> Result<(), AutopilotServiceError<B::Error>> {
        let Some(autopilot) =
            self.backend.get(workspace, id).map_err(AutopilotServiceError::Backend)?
        else {
            // Foreign / absent id: nothing to enable, nothing to leak.
            return Ok(());
        };
        let now_ms = self.clock.now_ms();
        let next_tick_at = compute_next_tick(&autopilot.cron_expr, now_ms)?;
        self.backend
            .set_enabled(workspace, id, true, next_tick_at)
            .map_err(AutopilotServiceError::Backend)
    }

    /// List an autopilot's runs, latest-first, capped at `limit`, scoped to
    /// `workspace`.
    ///
    /// # Errors
    ///
    /// Propagates the backend error.
    pub fn list_runs(
        &self,
        workspace: &WorkspaceId,
        autopilot_id: &AutopilotId,
        limit: u32,
    ) -> Result<Vec<AutopilotRun>, AutopilotServiceError<B::Error>> {
        self.backend
            .list_runs(workspace, autopilot_id, limit)
            .map_err(AutopilotServiceError::Backend)
    }
}

/// Parse `cron_expr` and compute the next firing instant strictly after
/// `now_ms` as epoch milliseconds.
///
/// The single seam shared by `create` (validate-before-insert) and `enable`
/// (recompute-from-now). Returns `Ok(None)` when the schedule is valid but has
/// no future match (a one-shot expression already in the past).
///
/// # Errors
///
/// Returns [`AutopilotServiceError::Cron`] when `cron_expr` fails to parse.
fn compute_next_tick<E>(
    cron_expr: &str,
    now_ms: i64,
) -> Result<Option<i64>, AutopilotServiceError<E>> {
    let schedule = parse_cron(cron_expr)?;
    // `now_ms` is produced by `HangarClock::now_ms` (always in chrono's range),
    // so `millis_to_utc` only returns `None` on a corrupt stored value; treat
    // that as "no future tick" rather than panicking.
    let Some(after) = millis_to_utc(now_ms) else {
        return Ok(None);
    };
    Ok(next_tick_after(&schedule, after).map(utc_to_millis))
}

/// Error surface for [`AutopilotService`].
///
/// Splits a cron-validation failure (caller passed a malformed expression) from
/// a backend failure (uniqueness conflict, FK violation, IO). The backend error
/// is generic so the store crate plugs in `sqlx::Error` while unit tests plug in
/// their own.
#[derive(Debug, thiserror::Error)]
pub enum AutopilotServiceError<E> {
    /// The cron expression could not be parsed; no row was written.
    #[error(transparent)]
    Cron(#[from] CronError),
    /// An underlying backend failure (uniqueness conflict, FK violation, IO).
    #[error("autopilot backend error: {0}")]
    Backend(E),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    /// 2026-01-01T00:00:00Z in epoch-ms — the frozen `now` for these tests.
    const T0: i64 = 1_767_225_600_000;

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::from_str(id).unwrap()
    }
    fn ap_id(id: &str) -> AutopilotId {
        AutopilotId::from_str(id).unwrap()
    }

    /// In-memory backend proving the service threads `workspace` through every
    /// by-id call and never reaches the backend on a bad cron.
    #[derive(Default)]
    struct MemBackend {
        rows: Vec<Autopilot>,
        /// Sentinel: set true if a cron-rejected create ever reached `insert`.
        insert_called: bool,
    }

    #[derive(Debug, PartialEq, Eq)]
    struct MemError;

    impl AutopilotBackend for MemBackend {
        type Error = MemError;

        fn insert(
            &mut self,
            req: &CreateAutopilot,
            next_tick_at: Option<i64>,
            now_ms: i64,
        ) -> Result<Autopilot, MemError> {
            self.insert_called = true;
            let row = Autopilot {
                id: ap_id(&format!("ap-{}", self.rows.len())),
                workspace_id: req.workspace_id.clone(),
                agent_id: req.agent_id.clone(),
                name: req.name.clone(),
                instructions: req.instructions.clone(),
                cron_expr: req.cron_expr.clone(),
                max_concurrent_runs: req.max_concurrent_runs,
                next_tick_at,
                enabled: true,
                created_at: now_ms,
            };
            self.rows.push(row.clone());
            Ok(row)
        }

        fn list(&self, workspace: &WorkspaceId) -> Result<Vec<Autopilot>, MemError> {
            Ok(self.rows.iter().filter(|a| &a.workspace_id == workspace).cloned().collect())
        }

        fn get(
            &self,
            workspace: &WorkspaceId,
            id: &AutopilotId,
        ) -> Result<Option<Autopilot>, MemError> {
            Ok(self.rows.iter().find(|a| &a.workspace_id == workspace && &a.id == id).cloned())
        }

        fn set_enabled(
            &mut self,
            workspace: &WorkspaceId,
            id: &AutopilotId,
            enabled: bool,
            next_tick_at: Option<i64>,
        ) -> Result<(), MemError> {
            if let Some(row) =
                self.rows.iter_mut().find(|a| &a.workspace_id == workspace && &a.id == id)
            {
                row.enabled = enabled;
                row.next_tick_at = next_tick_at;
            }
            Ok(())
        }

        fn list_runs(
            &self,
            _workspace: &WorkspaceId,
            _autopilot_id: &AutopilotId,
            _limit: u32,
        ) -> Result<Vec<AutopilotRun>, MemError> {
            Ok(vec![])
        }
    }

    fn req(ws_id: &str, name: &str, cron: &str) -> CreateAutopilot {
        CreateAutopilot {
            workspace_id: ws(ws_id),
            agent_id: AgentId::from_str("agent-1").unwrap(),
            name: name.to_string(),
            instructions: None,
            cron_expr: cron.to_string(),
            max_concurrent_runs: 1,
        }
    }

    #[test]
    fn create_computes_next_tick_from_clock() {
        let mut backend = MemBackend::default();
        let clock = FixedClock(T0);
        let mut svc = AutopilotService::new(&mut backend, &clock);
        let row = svc.create(&req("ws-a", "daily", "0 */6 * * *")).unwrap();
        // T0 is exactly midnight, next 6h tick is 06:00Z == T0 + 6h.
        assert_eq!(row.next_tick_at, Some(T0 + 6 * 3_600_000));
    }

    #[test]
    fn create_rejects_bad_cron_without_touching_backend() {
        let mut backend = MemBackend::default();
        let clock = FixedClock(T0);
        let mut svc = AutopilotService::new(&mut backend, &clock);
        let err = svc.create(&req("ws-a", "bad", "0 25 * * *")).unwrap_err();
        assert!(matches!(err, AutopilotServiceError::Cron(_)));
        assert!(
            !backend.insert_called,
            "a cron-rejected create must never reach the backend insert"
        );
    }

    #[test]
    fn enable_recomputes_next_tick_from_now_not_replay() {
        // Seed a row whose cached next_tick_at is far in the past (a long
        // disabled stint). Enable must recompute from `now`, not replay.
        let mut backend = MemBackend::default();
        backend.rows.push(Autopilot {
            id: ap_id("ap-x"),
            workspace_id: ws("ws-a"),
            agent_id: AgentId::from_str("agent-1").unwrap(),
            name: "n".into(),
            instructions: None,
            cron_expr: "0 */6 * * *".into(),
            max_concurrent_runs: 1,
            next_tick_at: Some(0), // 1970 — a stale missed tick.
            enabled: false,
            created_at: 0,
        });
        let clock = FixedClock(T0);
        {
            let mut svc = AutopilotService::new(&mut backend, &clock);
            svc.enable(&ws("ws-a"), &ap_id("ap-x")).unwrap();
        }
        let row = &backend.rows[0];
        assert!(row.enabled);
        assert_eq!(
            row.next_tick_at,
            Some(T0 + 6 * 3_600_000),
            "enable must recompute strictly after now, not keep the stale 1970 tick"
        );
    }

    #[test]
    fn disable_keeps_next_tick_at() {
        let mut backend = MemBackend::default();
        let clock = FixedClock(T0);
        let id;
        {
            let mut svc = AutopilotService::new(&mut backend, &clock);
            id = svc.create(&req("ws-a", "n", "0 */6 * * *")).unwrap().id;
        }
        let before = backend.get(&ws("ws-a"), &id).unwrap().unwrap().next_tick_at;
        {
            let mut svc = AutopilotService::new(&mut backend, &clock);
            svc.disable(&ws("ws-a"), &id).unwrap();
        }
        let after = backend.get(&ws("ws-a"), &id).unwrap().unwrap();
        assert!(!after.enabled);
        assert_eq!(
            after.next_tick_at, before,
            "disable must preserve next_tick_at"
        );
    }

    #[test]
    fn by_id_methods_are_workspace_scoped() {
        let mut backend = MemBackend::default();
        let clock = FixedClock(T0);
        let id;
        {
            let mut svc = AutopilotService::new(&mut backend, &clock);
            id = svc.create(&req("ws-a", "n", "0 */6 * * *")).unwrap().id;
        }
        let svc = AutopilotService::new(&mut backend, &clock);
        // Workspace B cannot see workspace A's autopilot by id.
        assert!(svc.get(&ws("ws-b"), &id).unwrap().is_none());
    }
}
