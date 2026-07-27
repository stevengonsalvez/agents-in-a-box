//! Last-writer-wins conflict resolution + the `reconcile` command (P2.5).
//!
//! Where the inbound poll loop ([`super::inbound`]) only ever lands `bd`-side
//! changes *into* Hangar, [`ReconcileService`] is the full two-way repair pass
//! invoked on demand by `ainb hangar beads reconcile`. It walks every mapped
//! pair, decides which side wrote last, and drags the loser to match — plus two
//! housekeeping cases the steady-state loops don't handle: deleting dangling
//! mappings and adopting orphan `bd` issues into Hangar.
//!
//! # Last-writer-wins rule
//!
//! Each `beads_mapping` row carries one `last_synced` instant: the wall-clock
//! moment Hangar and `bd` were last reconciled. Each `bd` issue carries its own
//! `updated_at`. The resolver compares them per pair (see [`decide_winner`]):
//!
//! - `bd.updated_at > mapping.last_synced` → `bd` changed since the last sync →
//!   **`bd` wins**, Hangar's lifecycle state follows.
//! - otherwise (the `bd` side has *not* moved since the last sync, so any drift
//!   is a Hangar-side edit) → **Hangar wins**, the change is mirrored out with a
//!   `bd close` / no-op.
//! - equal timestamps are a **tie**, resolved in Hangar's favour (Hangar is the
//!   local source of intent; `bd` is the mirror).
//!
//! Only `Hangar`-sourced mappings are reconciled; `Swarm`-sourced rows belong to
//! a leader's own `bd` lifecycle and are skipped (per
//! `feedback_swarm_leader_owns_bd_tracking`).
//!
//! # `--dry-run`
//!
//! [`ReconcileOpts::dry_run`] performs the full diff and returns the same
//! [`ReconcileReport`] (conflicts, adoptions, would-be deletions) while skipping
//! **every** write — both the `bd` side effects and the `beads_mapping` / issue
//! writes — so the user can preview drift repair before committing to it.

pub mod cli;

use std::collections::HashSet;

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use ainb_hangar_store::repo::beads_mapping::{
    BeadsMappingRepo, BeadsMappingRow, MappingKind, MappingSource,
};
use ainb_hangar_store::repo::issue::{IssueRepo, NewIssue};
use chrono::{DateTime, TimeZone, Utc};

use crate::beads_adapter::{BdClient, BdId, BdIssue, BdListFilter, BdStatus};

use super::inbound::SYNC_LABEL;
use super::outbound::SyncError;

/// The Hangar issue lifecycle state a closed `bd` issue maps to.
const HANGAR_STATE_DONE: &str = "done";
/// The Hangar issue lifecycle state any other non-closed `bd` issue maps to.
const HANGAR_STATE_OPEN: &str = "open";
/// The Hangar issue lifecycle state a `bd`-blocked issue maps to (migration
/// 0049): `bd` has a first-class `blocked` status and hangar now has its twin,
/// so the bridge stops collapsing it into `open`.
const HANGAR_STATE_BLOCKED: &str = "blocked";
/// The actor recorded as creator on an adopted (orphan-`bd`) Hangar issue.
const ADOPTED_CREATOR_ID: &str = "stevie";

/// Which side of a mapped pair wrote last, and therefore wins the reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictWinner {
    /// Hangar's lifecycle state is authoritative; `bd` is dragged to match.
    Hangar,
    /// `bd`'s lifecycle status is authoritative; Hangar is dragged to match.
    Bd,
    /// Both sides changed to the same timestamp; resolved to Hangar.
    Tie,
}

impl ConflictWinner {
    /// Whether the resolution requires Hangar's issue row to be rewritten.
    const fn writes_hangar(self) -> bool {
        matches!(self, Self::Bd)
    }
}

/// Decide which side of a drifted mapped pair wrote last.
///
/// Pure: compares the `bd` issue's `updated_at` against the mapping's
/// `last_synced` per the last-writer-wins rule documented on the module. A `bd`
/// `updated_at` *strictly after* the last sync means `bd` moved since → `bd`
/// wins; an equal timestamp is a tie (→ Hangar); anything earlier means the
/// drift is a Hangar-side edit → Hangar wins.
///
/// This function does not look at whether the two sides actually disagree — the
/// caller filters to drifted pairs first; here we only assign the winner.
#[must_use]
pub fn decide_winner(mapping: &BeadsMappingRow, bd_issue: &BdIssue) -> ConflictWinner {
    match bd_issue.updated_at.cmp(&mapping.last_synced) {
        std::cmp::Ordering::Greater => ConflictWinner::Bd,
        std::cmp::Ordering::Equal => ConflictWinner::Tie,
        std::cmp::Ordering::Less => ConflictWinner::Hangar,
    }
}

/// Options controlling a reconcile pass.
#[derive(Debug, Clone)]
pub struct ReconcileOpts {
    /// Diff only — skip every write (both `bd` side effects and store writes).
    pub dry_run: bool,
    /// Restrict the `bd list` to issues carrying these labels (in addition to
    /// the always-present [`SYNC_LABEL`]).
    pub label_filter: Vec<String>,
    /// Workspace an adopted orphan-`bd` issue is created under.
    pub workspace_id: String,
}

impl Default for ReconcileOpts {
    fn default() -> Self {
        Self {
            dry_run: false,
            label_filter: Vec::new(),
            workspace_id: "ws-1".to_string(),
        }
    }
}

/// Counters describing what one [`ReconcileService::run`] pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ReconcileStats {
    /// Total `bd` issues the list returned.
    pub scanned: usize,
    /// Mapped pairs whose drift was resolved (a side was rewritten).
    pub updated: usize,
    /// Orphan `bd` issues imported into Hangar.
    pub adopted: usize,
    /// Dangling mapping rows deleted (both sides gone).
    pub deleted: usize,
    /// Mapped pairs that already agreed (no action).
    pub in_sync: usize,
    /// Pairs skipped as out-of-scope (swarm-sourced).
    pub skipped: usize,
    /// Per-pair errors that did not abort the pass.
    pub errors: usize,
}

/// One resolved (or, under `--dry-run`, would-be-resolved) conflict, carrying
/// enough context to JSON-log for the user.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ConflictRecord {
    /// The Hangar issue id of the drifted pair.
    pub hangar_id: String,
    /// The `bd` issue id of the drifted pair.
    pub bd_id: String,
    /// Hangar's lifecycle state before resolution.
    pub hangar_state: String,
    /// `bd`'s lifecycle status before resolution (as a token).
    pub bd_status: String,
    /// Which side won.
    pub winner: Winner,
}

/// Serializable mirror of [`ConflictWinner`] for the JSON report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Winner {
    /// Hangar's state won (includes ties).
    Hangar,
    /// `bd`'s status won.
    Bd,
}

impl From<ConflictWinner> for Winner {
    fn from(w: ConflictWinner) -> Self {
        match w {
            ConflictWinner::Bd => Self::Bd,
            ConflictWinner::Hangar | ConflictWinner::Tie => Self::Hangar,
        }
    }
}

/// The full result of a reconcile pass — stats plus per-conflict detail and the
/// list of adopted orphan ids, suitable for `--format json`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReconcileReport {
    /// Aggregate counters.
    pub stats: ReconcileStats,
    /// Every drifted pair the pass resolved (or would, under `--dry-run`).
    pub conflicts: Vec<ConflictRecord>,
    /// Hangar ids minted for adopted orphan-`bd` issues.
    pub adopted: Vec<String>,
}

/// On-demand two-way drift repair between Hangar and `bd`.
///
/// Holds borrows (`bd`, `mapping`, `pool`, `clock`) so the daemon composes it
/// per-operation from its long-lived service registry; it is `Send + Sync`.
pub struct ReconcileService<'a> {
    bd: &'a BdClient,
    mapping: &'a BeadsMappingRepo<'a>,
    pool: &'a sqlx::SqlitePool,
    clock: &'a dyn HangarClock,
    idgen: &'a dyn IdGen,
}

impl<'a> ReconcileService<'a> {
    /// Compose the service from its borrowed dependencies.
    ///
    /// Both the clock and the id generator are injected (rather than defaulting
    /// to the production `SystemClock` / `SystemIdGen`) so adoption of an orphan
    /// `bd` issue mints a deterministic Hangar id under a seeded
    /// [`ainb_hangar_core::idgen::FixedIdGen`] in tests.
    #[must_use]
    pub const fn new(
        bd: &'a BdClient,
        mapping: &'a BeadsMappingRepo<'a>,
        pool: &'a sqlx::SqlitePool,
        clock: &'a dyn HangarClock,
        idgen: &'a dyn IdGen,
    ) -> Self {
        Self {
            bd,
            mapping,
            pool,
            clock,
            idgen,
        }
    }

    /// Run one reconcile pass and return its [`ReconcileReport`].
    ///
    /// Pulls `bd`'s current view (filtered to [`SYNC_LABEL`] plus any
    /// `opts.label_filter`), resolves drift per the last-writer-wins rule,
    /// adopts orphan `bd` issues, and deletes dangling mappings. Under
    /// `opts.dry_run` the diff still runs but no write lands.
    ///
    /// # Errors
    ///
    /// Returns [`SyncError::Bd`] if the `bd list` invocation fails, or
    /// [`SyncError::BadClock`] if the injected clock is out of range. Per-pair
    /// store errors are counted in `stats.errors` and logged, not propagated.
    pub async fn run(&self, opts: ReconcileOpts) -> Result<ReconcileReport, SyncError> {
        let mut labels = vec![SYNC_LABEL.to_string()];
        labels.extend(opts.label_filter.iter().cloned());
        let bd_issues = self.bd.list(BdListFilter {
            labels,
            since: None,
            // Reconcile resolves drift in both directions, including a `bd`-side
            // close that Hangar has not yet followed. `bd list` hides closed
            // issues by default, so `--all` is required or a closed-on-bd pair is
            // never even scanned.
            include_closed: true,
        })?;
        let now = self.now()?;

        let mut report = ReconcileReport {
            stats: ReconcileStats {
                scanned: bd_issues.len(),
                ..ReconcileStats::default()
            },
            conflicts: Vec::new(),
            adopted: Vec::new(),
        };

        // Track which mapping rows a bd issue covered, so the dangling sweep can
        // skip any mapping whose bd side is still present in the list.
        let mut seen_bd: HashSet<String> = HashSet::new();

        for bd_issue in &bd_issues {
            seen_bd.insert(bd_issue.id.to_string());
            match self.reconcile_pair(bd_issue, now, &opts, &mut report).await {
                Ok(()) => {}
                Err(e) => {
                    report.stats.errors += 1;
                    tracing::warn!(bd_id = %bd_issue.id, error = %e, "reconcile failed for pair");
                }
            }
        }

        self.sweep_dangling(&seen_bd, &opts, &mut report).await;

        tracing::info!(
            scanned = report.stats.scanned,
            updated = report.stats.updated,
            adopted = report.stats.adopted,
            deleted = report.stats.deleted,
            in_sync = report.stats.in_sync,
            skipped = report.stats.skipped,
            errors = report.stats.errors,
            dry_run = opts.dry_run,
            "reconcile pass complete"
        );
        Ok(report)
    }

    /// Reconcile one `bd` issue against its mapped Hangar issue (or adopt it).
    async fn reconcile_pair(
        &self,
        bd_issue: &BdIssue,
        now: DateTime<Utc>,
        opts: &ReconcileOpts,
        report: &mut ReconcileReport,
    ) -> Result<(), SyncError> {
        let Some(row) = self.mapping.find_by_bd(bd_issue.id.as_str()).await? else {
            // Orphan bd issue tagged for sync → adopt into Hangar.
            return self.adopt_orphan(bd_issue, now, opts, report).await;
        };

        // Swarm-sourced rows belong to a leader's bd lifecycle — never reconcile.
        if row.source == MappingSource::Swarm {
            report.stats.skipped += 1;
            tracing::debug!(bd_id = %bd_issue.id, "swarm-sourced mapping; skipping reconcile");
            return Ok(());
        }

        // The Hangar issue must still exist. A missing one means the pair is
        // half-dangling (the `bd` side is still present in this list, so the
        // dangling sweep will *not* touch it — it skips any row whose bd id is
        // in `seen_bd`). This is intentionally a no-op for P2: `bd` is still
        // alive, so there is nothing safe to delete and re-adoption is out of
        // scope here. The pair is simply left as-is for a later phase / human.
        let Some(issue) = IssueRepo::get_by_id(self.pool, &row.hangar_id).await? else {
            return Ok(());
        };

        let bd_state = hangar_state_for(&bd_issue.status);

        // Already in agreement → nothing to do (still bump nothing).
        if issue.state == bd_state {
            report.stats.in_sync += 1;
            return Ok(());
        }

        // Drift: resolve by last-writer-wins.
        let winner = decide_winner(&row, bd_issue);

        // Detect the one resolution `bd` cannot represent BEFORE recording it as
        // a resolved conflict: Hangar wins (or ties) but the winning Hangar state
        // is `open` while `bd` is `closed`. Mirroring that out would require a
        // `bd reopen`, which `BdClient` does not expose. Counting it as `updated`
        // and bumping `last_synced` would (a) falsely report the drift repaired
        // and (b) permanently suppress re-detection (bd.updated_at would stay
        // below the bumped last_synced on every future pass, flipping the winner
        // to Bd and dragging Hangar to done — the opposite of the resolution).
        // So surface it as an error; the caller counts it and the pair stays
        // drifted for a human to resolve.
        let needs_bd_reopen = !winner.writes_hangar()
            && issue.state == HANGAR_STATE_OPEN
            && matches!(bd_issue.status, BdStatus::Closed);
        if needs_bd_reopen {
            tracing::warn!(
                hangar_id = %row.hangar_id,
                bd_id = %bd_issue.id,
                ?winner,
                "drift unrepairable: Hangar is open but bd is closed and bd has no reopen verb; leaving both sides untouched (last_synced not bumped)"
            );
            return Err(SyncError::Unreconcilable {
                hangar_id: row.hangar_id.clone(),
                bd_id: row.bd_id.clone(),
            });
        }

        report.conflicts.push(ConflictRecord {
            hangar_id: row.hangar_id.clone(),
            bd_id: row.bd_id.clone(),
            hangar_state: issue.state.clone(),
            bd_status: status_token(&bd_issue.status),
            winner: winner.into(),
        });

        if opts.dry_run {
            // Count the resolution but apply nothing.
            report.stats.updated += 1;
            return Ok(());
        }

        if winner.writes_hangar() {
            // bd won → drag the Hangar issue to bd's state.
            IssueRepo::update_state(self.pool, &row.hangar_id, bd_state).await?;
        } else {
            // Hangar won (or tie) → mirror Hangar's state out to bd. The only
            // representable case here is Hangar = `done`, which closes the bd
            // issue; the open-Hangar-over-closed-bd case was already rejected as
            // `Unreconcilable` above (bd has no reopen verb).
            if issue.state == HANGAR_STATE_DONE {
                self.bd.close(&BdId::from(row.bd_id.as_str()), None)?;
            }
        }

        self.mapping.update_last_synced(&row.hangar_id, now).await?;
        report.stats.updated += 1;
        tracing::info!(
            hangar_id = %row.hangar_id,
            bd_id = %bd_issue.id,
            ?winner,
            "reconciled drifted pair"
        );
        Ok(())
    }

    /// Import an orphan `bd` issue (tagged for sync, no mapping row) into Hangar.
    async fn adopt_orphan(
        &self,
        bd_issue: &BdIssue,
        now: DateTime<Utc>,
        opts: &ReconcileOpts,
        report: &mut ReconcileReport,
    ) -> Result<(), SyncError> {
        if opts.dry_run {
            report.adopted.push(bd_issue.id.to_string());
            report.stats.adopted += 1;
            return Ok(());
        }

        let hangar_id = self.idgen.new_ulid();
        let new = NewIssue {
            id: hangar_id.clone(),
            workspace_id: opts.workspace_id.clone(),
            title: bd_issue.title.clone(),
            description: None,
            state: hangar_state_for(&bd_issue.status).to_string(),
            assignee: None,
            creator: ainb_hangar_core::actor::ActorRef::new(
                ainb_hangar_core::actor::ActorKind::Member,
                ADOPTED_CREATOR_ID,
            )
            .expect("adopted creator actor is non-empty"),
            created_at: now.timestamp_millis(),
            priority: 0,
            due_date: None,
            labels: Vec::new(),
            acceptance_criteria: Vec::new(),
            context_refs: Vec::new(),
            parent_issue_id: None,
            stage: None,
        };
        IssueRepo::insert(self.pool, &new).await?;
        self.mapping
            .insert(&BeadsMappingRow {
                hangar_id: hangar_id.clone(),
                bd_id: bd_issue.id.to_string(),
                hangar_kind: MappingKind::Issue,
                bd_kind: MappingKind::Issue,
                source: MappingSource::Hangar,
                last_synced: now,
            })
            .await?;

        report.adopted.push(hangar_id.clone());
        report.stats.adopted += 1;
        tracing::info!(hangar_id = %hangar_id, bd_id = %bd_issue.id, "adopted orphan bd issue into Hangar");
        Ok(())
    }

    /// Delete mapping rows whose `bd` side did not appear in the list **and**
    /// whose Hangar issue no longer exists (both sides gone).
    async fn sweep_dangling(
        &self,
        seen_bd: &HashSet<String>,
        opts: &ReconcileOpts,
        report: &mut ReconcileReport,
    ) {
        // Walk all Hangar-sourced mappings since epoch; cheap for the table sizes
        // Hangar holds, and avoids a bespoke "list all" query.
        let epoch = Utc.timestamp_millis_opt(0).single().expect("epoch ts");
        let rows = match self.mapping.list_since(epoch).await {
            Ok(rows) => rows,
            Err(e) => {
                report.stats.errors += 1;
                tracing::warn!(error = %e, "dangling sweep: failed to list mappings");
                return;
            }
        };

        for row in rows {
            if row.source == MappingSource::Swarm || seen_bd.contains(&row.bd_id) {
                continue;
            }
            // bd side gone — check the Hangar side.
            match IssueRepo::get_by_id(self.pool, &row.hangar_id).await {
                Ok(Some(_)) => {} // Hangar side present; not dangling.
                Ok(None) => {
                    if !opts.dry_run {
                        if let Err(e) = self.mapping.delete_by_hangar(&row.hangar_id).await {
                            report.stats.errors += 1;
                            tracing::warn!(hangar_id = %row.hangar_id, error = %e, "failed to delete dangling mapping");
                            continue;
                        }
                    }
                    report.stats.deleted += 1;
                    tracing::info!(hangar_id = %row.hangar_id, bd_id = %row.bd_id, "deleted dangling mapping");
                }
                Err(e) => {
                    report.stats.errors += 1;
                    tracing::warn!(hangar_id = %row.hangar_id, error = %e, "dangling sweep: failed to read Hangar issue");
                }
            }
        }
    }

    /// The injected clock's current instant as a UTC timestamp.
    fn now(&self) -> Result<DateTime<Utc>, SyncError> {
        let ms = self.clock.now_ms();
        match Utc.timestamp_millis_opt(ms) {
            chrono::LocalResult::Single(dt) => Ok(dt),
            _ => Err(SyncError::BadClock(ms)),
        }
    }
}

/// Run `ainb hangar beads reconcile` end-to-end: resolve `BEADS_DIR`, open the
/// store, run a reconcile pass, and render the report to stdout in the chosen
/// format.
///
/// `BEADS_DIR` is read from the environment — it **must** be set explicitly in a
/// git worktree (per `reference_beads_worktree`); the function fails fast if it
/// is unset. The workspace an adopted orphan issue lands under is the first
/// workspace row (Hangar is single-workspace for P2).
///
/// # Errors
///
/// Returns an error if `BEADS_DIR` is unset, the `bd` binary cannot be resolved,
/// the store cannot be opened, no workspace exists, the reconcile pass fails, or
/// rendering fails.
pub async fn dispatch(args: &cli::ReconcileArgs) -> anyhow::Result<()> {
    use ainb_hangar_core::clock::SystemClock;

    let beads_dir = std::env::var_os("BEADS_DIR").filter(|p| !p.is_empty()).ok_or_else(|| {
        anyhow::anyhow!(
            "BEADS_DIR is not set; export it to the beads root (required in a worktree)"
        )
    })?;
    let bd = BdClient::new("bd", std::path::PathBuf::from(beads_dir))?;

    let store = ainb_hangar_store::Store::open_default().await?;
    let pool = store.pool();
    let workspace_id: String =
        sqlx::query_scalar("SELECT id FROM workspace ORDER BY created_at LIMIT 1")
            .fetch_optional(pool)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no workspace exists; create one before reconciling"))?;

    let mapping = BeadsMappingRepo::new(pool);
    let clock = SystemClock;
    let idgen = ainb_hangar_core::idgen::SystemIdGen;
    let service = ReconcileService::new(&bd, &mapping, pool, &clock, &idgen);
    let report = service.run(args.to_opts(workspace_id)).await?;

    println!("{}", args.format.render(&report)?);
    Ok(())
}

/// Map a `bd` lifecycle status onto the Hangar issue state string.
const fn hangar_state_for(status: &BdStatus) -> &'static str {
    match status {
        BdStatus::Closed => HANGAR_STATE_DONE,
        // 0049: bd's `blocked` now has a hangar twin — stop throwing it away.
        BdStatus::Blocked => HANGAR_STATE_BLOCKED,
        _ => HANGAR_STATE_OPEN,
    }
}

/// Render a `bd` status as its stable token (for the conflict record).
fn status_token(status: &BdStatus) -> String {
    match status {
        BdStatus::Open => "open".to_string(),
        BdStatus::InProgress => "in_progress".to_string(),
        BdStatus::Blocked => "blocked".to_string(),
        BdStatus::Closed => "closed".to_string(),
        BdStatus::Other(s) => s.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::repo::beads_mapping::{BeadsMappingRow, MappingKind, MappingSource};

    fn mapping_at(ms: i64) -> BeadsMappingRow {
        BeadsMappingRow {
            hangar_id: "h".into(),
            bd_id: "b".into(),
            hangar_kind: MappingKind::Issue,
            bd_kind: MappingKind::Issue,
            source: MappingSource::Hangar,
            last_synced: Utc.timestamp_millis_opt(ms).single().unwrap(),
        }
    }

    fn bd_at(status: BdStatus, ms: i64) -> BdIssue {
        BdIssue {
            id: BdId::from("b"),
            title: "t".into(),
            status,
            assignee: None,
            labels: vec![],
            updated_at: Utc.timestamp_millis_opt(ms).single().unwrap(),
        }
    }

    /// Table-driven cover of the *pure* winner decision across the timestamp
    /// ordering × bd-status axes. [`decide_winner`] in isolation only depends on
    /// the timestamp ordering (the status decides which *state* is applied, not
    /// who wins), so the doc's "9 combos" reduce here to the three orderings ×
    /// two statuses. The application-level consequence of the status — including
    /// the open-Hangar-over-closed-`bd` case that `decide_winner` cannot see but
    /// `reconcile_pair` must reject — is covered in `tests/reconcile.rs`
    /// (`test_hangar_open_wins_over_closed_bd_is_unreconcilable_not_silently_repaired`).
    #[test]
    fn decide_winner_covers_timestamp_orderings() {
        let cases = [
            // (bd_updated_ms, last_synced_ms, status, expected)
            (200, 100, BdStatus::Closed, ConflictWinner::Bd),
            (200, 100, BdStatus::Open, ConflictWinner::Bd),
            (100, 100, BdStatus::Closed, ConflictWinner::Tie),
            (100, 100, BdStatus::Open, ConflictWinner::Tie),
            (100, 200, BdStatus::Closed, ConflictWinner::Hangar),
            (100, 200, BdStatus::Open, ConflictWinner::Hangar),
        ];
        for (bd_ms, sync_ms, status, expected) in cases {
            let got = decide_winner(&mapping_at(sync_ms), &bd_at(status.clone(), bd_ms));
            assert_eq!(got, expected, "bd={bd_ms} sync={sync_ms} status={status:?}");
        }
    }

    #[test]
    fn tie_maps_to_hangar_winner() {
        assert_eq!(Winner::from(ConflictWinner::Tie), Winner::Hangar);
        assert_eq!(Winner::from(ConflictWinner::Hangar), Winner::Hangar);
        assert_eq!(Winner::from(ConflictWinner::Bd), Winner::Bd);
    }

    /// [`ReconcileService`] must stay `Send + Sync` so the daemon can hold it in
    /// an `Arc<>` on its shared service registry.
    #[test]
    fn reconcile_service_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ReconcileService<'static>>();
    }
}
