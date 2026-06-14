// ABOUTME: `ainb fleet cost` — fleet-shaped spend rollups + budget caps.
//
// ainb already prices every provider call (`cost_usd` on `ProviderCall` /
// `TokenBucket`, aggregated by the burndown plugin). This verb does NOT
// re-price anything: it fetches burndown's `usage report --format json`
// payload live (via `capture_usage_via_plugin`, the same retry/init path
// `ainb usage` uses), reshapes it into per-session / per-model / per-day /
// per-group rollups joined to the live fleet, evaluates configured budget
// caps, and fires a notifyd alert for every breach.
//
// The rollup + budget logic is factored into pure functions
// (`build_cost_report`, `evaluate_budgets`) that take already-parsed
// fixture data, so the unit tests exercise aggregation and threshold logic
// with zero live API spend.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::OutputFormat;
use crate::config::{AppConfig, CostBudgetConfig};
use crate::fleet::discover::{
    discover_from_ainb, discover_from_jobs, discover_from_peers, merge_sessions,
};
use crate::fleet::types::Session;

use super::budget_alert::{self, BudgetBreach};

// ---------------------------------------------------------------------------
// Input shapes — a structural subset of the burndown plugin's
// `usage report --format json` payload (the `report_json` shape).
// Only the fields `fleet cost` consumes are modelled; unknown keys are
// ignored by serde, so additions to burndown's payload don't break us.
// ---------------------------------------------------------------------------

/// Token + cost counts, mirroring `models::usage::TokenBucket`. Re-declared
/// here (rather than imported) so the verb stays decoupled from the bincode
/// cache layout and only depends on the stable JSON wire shape.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct CostBucket {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default)]
    pub call_count: usize,
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

impl CostBucket {
    /// Total tokens across every category.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            + self.cache_creation_tokens
            + self.cache_read_tokens
            + self.output_tokens
            + self.reasoning_tokens
    }

    /// Fold `other` into `self`, summing tokens/calls and adding cost when
    /// either side carries a priced value.
    fn merge(&mut self, other: &CostBucket) {
        self.input_tokens += other.input_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.call_count += other.call_count;
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (Some(a), Some(b)) => Some(a + b),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
    }

    fn cost(&self) -> f64 {
        self.cost_usd.unwrap_or(0.0)
    }
}

/// Project rollup row. Only `name` + `path` matter here — they supply the
/// project-name → cwd join. The `bucket` is intentionally not modelled (we
/// derive project/group spend from the session rows instead), and serde
/// ignores the extra key on the wire.
#[derive(Debug, Clone, Deserialize)]
struct ProjectRow {
    name: String,
    path: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionRow {
    #[serde(default)]
    provider: String,
    project: String,
    session_id: String,
    bucket: CostBucket,
}

#[derive(Debug, Clone, Deserialize)]
struct ModelRow {
    model: String,
    bucket: CostBucket,
}

/// The structural subset of burndown's `report_json` we consume. `daily`
/// arrives as `[[date, bucket], ...]` tuples; everything else is a list of
/// keyed rows.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UsageReportView {
    #[serde(default)]
    daily: Vec<(String, CostBucket)>,
    #[serde(default)]
    projects: Vec<ProjectRow>,
    #[serde(default)]
    sessions: Vec<SessionRow>,
    #[serde(default)]
    models: Vec<ModelRow>,
}

// ---------------------------------------------------------------------------
// Output shapes — the documented `ainb --format json fleet cost` contract.
// ---------------------------------------------------------------------------

/// Per-session spend, joined to the live fleet by cwd where possible.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionCost {
    pub session_id: String,
    pub provider: String,
    /// Burndown project name (display label / aggregation key).
    pub project: String,
    /// Working directory resolved via the project rollup, when known. This
    /// is the cross-system join key to the fleet `Session`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Workspace/group this session belongs to, resolved from the live
    /// fleet by cwd. `None` when the session isn't in the current fleet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub cost_usd: f64,
    pub bucket: CostBucket,
}

/// Per-model spend across the whole reporting window.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ModelCost {
    pub model: String,
    pub cost_usd: f64,
    pub bucket: CostBucket,
}

/// Per-day spend across the whole reporting window.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DayCost {
    pub date: String,
    pub cost_usd: f64,
    pub bucket: CostBucket,
}

/// Per-group (workspace) spend, summed from its member sessions.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GroupCost {
    pub group: String,
    pub cost_usd: f64,
    pub session_count: usize,
    pub bucket: CostBucket,
}

/// Fleet-wide totals.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct CostTotals {
    pub cost_usd: f64,
    pub bucket: CostBucket,
    pub session_count: usize,
    pub model_count: usize,
}

/// The complete `ainb --format json fleet cost` object.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CostReport {
    pub totals: CostTotals,
    pub sessions: Vec<SessionCost>,
    pub models: Vec<ModelCost>,
    pub daily: Vec<DayCost>,
    pub groups: Vec<GroupCost>,
    /// Budget breaches detected this run (empty when no caps configured or
    /// none crossed). Mirrors what was delivered to notifyd.
    pub budget_breaches: Vec<BudgetBreach>,
}

// ---------------------------------------------------------------------------
// Pure rollup core.
// ---------------------------------------------------------------------------

/// Build the fleet-shaped cost report from burndown's parsed usage view and
/// the live fleet sessions, then evaluate budget caps.
///
/// Pure — no I/O, no clock. `fleet_sessions` supplies the cwd→group join:
/// a burndown session is attributed to a fleet group when its resolved cwd
/// matches a fleet `Session.cwd`, using that session's `workspace_name`
/// (falling back to the project name) as the group label.
pub fn build_cost_report(
    view: &UsageReportView,
    fleet_sessions: &[Session],
    budget: &CostBudgetConfig,
) -> CostReport {
    // project name -> cwd, from the project rollup (the only place cost
    // data is tied to a path).
    let project_cwd: BTreeMap<&str, &str> =
        view.projects.iter().map(|p| (p.name.as_str(), p.path.as_str())).collect();

    // cwd -> group label, from the live fleet. workspace_name is the
    // group/workspace identity; fall back to the cwd basename when absent.
    let cwd_group: BTreeMap<&str, String> = fleet_sessions
        .iter()
        .map(|s| {
            let group =
                s.workspace_name.clone().unwrap_or_else(|| cwd_basename(&s.cwd).to_string());
            (s.cwd.as_str(), group)
        })
        .collect();

    let mut sessions: Vec<SessionCost> = view
        .sessions
        .iter()
        .map(|row| {
            let cwd = project_cwd.get(row.project.as_str()).map(|s| s.to_string());
            let group = cwd.as_deref().and_then(|c| cwd_group.get(c).cloned());
            SessionCost {
                session_id: row.session_id.clone(),
                provider: row.provider.clone(),
                project: row.project.clone(),
                cwd,
                group,
                cost_usd: row.bucket.cost(),
                bucket: row.bucket.clone(),
            }
        })
        .collect();
    sessions.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });

    let mut models: Vec<ModelCost> = view
        .models
        .iter()
        .map(|row| ModelCost {
            model: row.model.clone(),
            cost_usd: row.bucket.cost(),
            bucket: row.bucket.clone(),
        })
        .collect();
    models.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.model.cmp(&b.model))
    });

    let daily: Vec<DayCost> = view
        .daily
        .iter()
        .map(|(date, bucket)| DayCost {
            date: date.clone(),
            cost_usd: bucket.cost(),
            bucket: bucket.clone(),
        })
        .collect();

    // Group rollup — sum member sessions by resolved group label.
    let mut group_acc: BTreeMap<String, (CostBucket, usize)> = BTreeMap::new();
    for s in &sessions {
        if let Some(group) = &s.group {
            let entry = group_acc.entry(group.clone()).or_default();
            entry.0.merge(&s.bucket);
            entry.1 += 1;
        }
    }
    let mut groups: Vec<GroupCost> = group_acc
        .into_iter()
        .map(|(group, (bucket, session_count))| GroupCost {
            group,
            cost_usd: bucket.cost(),
            session_count,
            bucket,
        })
        .collect();
    groups.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.group.cmp(&b.group))
    });

    let mut totals = CostTotals {
        session_count: sessions.len(),
        model_count: models.len(),
        ..CostTotals::default()
    };
    for s in &sessions {
        totals.bucket.merge(&s.bucket);
    }
    totals.cost_usd = totals.bucket.cost();

    let budget_breaches = evaluate_budgets(&sessions, &groups, budget);

    CostReport {
        totals,
        sessions,
        models,
        daily,
        groups,
        budget_breaches,
    }
}

/// Detect every session / group whose spend crosses its configured USD
/// ceiling. Pure — the caller delivers the breaches to notifyd.
pub fn evaluate_budgets(
    sessions: &[SessionCost],
    groups: &[GroupCost],
    budget: &CostBudgetConfig,
) -> Vec<BudgetBreach> {
    let mut breaches = Vec::new();
    for s in sessions {
        if let Some(limit) = budget.session_limit(&s.session_id) {
            if s.cost_usd > limit {
                breaches.push(BudgetBreach::session(
                    s.session_id.clone(),
                    s.cwd.clone(),
                    s.cost_usd,
                    limit,
                ));
            }
        }
    }
    for g in groups {
        if let Some(limit) = budget.group_limit(&g.group) {
            if g.cost_usd > limit {
                breaches.push(BudgetBreach::group(g.group.clone(), g.cost_usd, limit));
            }
        }
    }
    breaches
}

fn cwd_basename(cwd: &str) -> &str {
    cwd.trim_end_matches('/').rsplit('/').next().unwrap_or(cwd)
}

// ---------------------------------------------------------------------------
// Live entry point.
// ---------------------------------------------------------------------------

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let period = matches.get_one::<String>("period").map(String::as_str).unwrap_or("month");

    // Fetch burndown's priced usage data and discover the live fleet in
    // parallel — neither depends on the other.
    let argv = vec![
        "--format".to_string(),
        "json".to_string(),
        "report".to_string(),
        "--period".to_string(),
        period.to_string(),
    ];
    let (usage_json, ainb_res, jobs_res) = tokio::join!(
        crate::cli::registry::capture_usage_via_plugin(argv),
        discover_from_ainb(),
        async { discover_from_jobs() }
    );
    let usage_json = usage_json.context("failed to fetch usage data from the burndown plugin")?;
    let view: UsageReportView = serde_json::from_str(&usage_json)
        .context("burndown usage payload did not match the expected report shape")?;

    let ainb = ainb_res.unwrap_or_default();
    let peers = discover_from_peers().unwrap_or_default();
    let jobs = jobs_res.unwrap_or_default();
    let fleet = merge_sessions(vec![ainb, peers, jobs]);

    let budget = AppConfig::load().unwrap_or_default().fleet.cost;
    let report = build_cost_report(&view, &fleet, &budget);

    // Deliver an alert for every breach. Best-effort: a notifyd write
    // failure must not fail the command (the report is still useful).
    for breach in &report.budget_breaches {
        if let Err(e) = budget_alert::emit(breach) {
            eprintln!("warning: budget alert delivery failed: {e}");
        }
    }

    if matches!(format, OutputFormat::Text) {
        print_text(&report);
    } else {
        println!("{}", serde_json::to_string_pretty(&report)?);
    }
    Ok(())
}

fn print_text(report: &CostReport) {
    println!(
        "ainb fleet cost — ${:.2} across {} sessions / {} models",
        report.totals.cost_usd, report.totals.session_count, report.totals.model_count
    );

    if !report.sessions.is_empty() {
        println!("\nBy session");
        println!("{:<28}  {:<20}  {:>10}", "session", "group", "cost");
        for s in report.sessions.iter().take(10) {
            let group = s.group.as_deref().unwrap_or("—");
            println!(
                "{:<28}  {:<20}  {:>9.2}$",
                truncate(&s.session_id, 28),
                truncate(group, 20),
                s.cost_usd
            );
        }
    }

    if !report.groups.is_empty() {
        println!("\nBy group");
        println!("{:<24}  {:>8}  {:>10}", "group", "sessions", "cost");
        for g in &report.groups {
            println!(
                "{:<24}  {:>8}  {:>9.2}$",
                truncate(&g.group, 24),
                g.session_count,
                g.cost_usd
            );
        }
    }

    if !report.models.is_empty() {
        println!("\nBy model");
        println!("{:<32}  {:>10}", "model", "cost");
        for m in report.models.iter().take(10) {
            println!("{:<32}  {:>9.2}$", truncate(&m.model, 32), m.cost_usd);
        }
    }

    if !report.budget_breaches.is_empty() {
        println!("\n⚠ Budget breaches");
        for b in &report.budget_breaches {
            println!(
                "  {} {} — ${:.2} over ${:.2} cap",
                b.scope_label(),
                b.subject,
                b.cost_usd,
                b.limit_usd
            );
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bucket(cost: f64, tokens: u64) -> CostBucket {
        CostBucket {
            input_tokens: tokens,
            output_tokens: 0,
            call_count: 1,
            cost_usd: Some(cost),
            ..CostBucket::default()
        }
    }

    fn fixture_view() -> UsageReportView {
        UsageReportView {
            daily: vec![
                ("2026-06-13".to_string(), bucket(2.0, 100)),
                ("2026-06-14".to_string(), bucket(4.0, 200)),
            ],
            projects: vec![
                ProjectRow {
                    name: "alpha".to_string(),
                    path: "/work/alpha".to_string(),
                },
                ProjectRow {
                    name: "beta".to_string(),
                    path: "/work/beta".to_string(),
                },
            ],
            sessions: vec![
                SessionRow {
                    provider: "claude".to_string(),
                    project: "alpha".to_string(),
                    session_id: "sess-a".to_string(),
                    bucket: bucket(4.0, 200),
                },
                SessionRow {
                    provider: "codex".to_string(),
                    project: "beta".to_string(),
                    session_id: "sess-b".to_string(),
                    bucket: bucket(2.0, 100),
                },
            ],
            models: vec![
                ModelRow {
                    model: "claude-opus-4-8".to_string(),
                    bucket: bucket(4.0, 200),
                },
                ModelRow {
                    model: "gpt-5".to_string(),
                    bucket: bucket(2.0, 100),
                },
            ],
        }
    }

    fn fleet_session(cwd: &str, workspace: &str) -> Session {
        Session {
            id: cwd.to_string(),
            cwd: cwd.to_string(),
            pid: None,
            git_root: None,
            tmux_session: None,
            workspace_name: Some(workspace.to_string()),
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![],
            summary: None,
            last_seen_ms: None,
        }
    }

    #[test]
    fn rollups_aggregate_sessions_models_and_days() {
        let view = fixture_view();
        let fleet = vec![
            fleet_session("/work/alpha", "ws-alpha"),
            fleet_session("/work/beta", "ws-beta"),
        ];
        let report = build_cost_report(&view, &fleet, &CostBudgetConfig::default());

        // Totals sum the per-session spend.
        assert_eq!(report.totals.cost_usd, 6.0);
        assert_eq!(report.totals.session_count, 2);
        assert_eq!(report.totals.model_count, 2);
        assert_eq!(report.totals.bucket.total_tokens(), 300);

        // Sessions sorted by descending cost, joined to cwd + group.
        assert_eq!(report.sessions[0].session_id, "sess-a");
        assert_eq!(report.sessions[0].cwd.as_deref(), Some("/work/alpha"));
        assert_eq!(report.sessions[0].group.as_deref(), Some("ws-alpha"));
        assert_eq!(report.sessions[1].group.as_deref(), Some("ws-beta"));

        // Models sorted by descending cost.
        assert_eq!(report.models[0].model, "claude-opus-4-8");
        assert_eq!(report.models[0].cost_usd, 4.0);

        // Daily rollup preserved.
        assert_eq!(report.daily.len(), 2);
        assert_eq!(report.daily[1].date, "2026-06-14");
        assert_eq!(report.daily[1].cost_usd, 4.0);

        // Group rollup: one session each.
        assert_eq!(report.groups.len(), 2);
        assert_eq!(report.groups[0].group, "ws-alpha");
        assert_eq!(report.groups[0].cost_usd, 4.0);
        assert_eq!(report.groups[0].session_count, 1);
    }

    #[test]
    fn sessions_without_fleet_match_have_no_group() {
        let view = fixture_view();
        // Empty fleet — nothing to join against.
        let report = build_cost_report(&view, &[], &CostBudgetConfig::default());
        assert!(report.sessions.iter().all(|s| s.group.is_none()));
        // cwd still resolves from the project rollup even without a fleet.
        assert_eq!(report.sessions[0].cwd.as_deref(), Some("/work/alpha"));
        assert!(report.groups.is_empty());
    }

    #[test]
    fn two_sessions_in_one_group_sum_into_one_group_row() {
        let mut view = fixture_view();
        // Point beta's session at alpha's project so both land in ws-alpha.
        view.sessions[1].project = "alpha".to_string();
        let fleet = vec![fleet_session("/work/alpha", "ws-alpha")];
        let report = build_cost_report(&view, &fleet, &CostBudgetConfig::default());
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].group, "ws-alpha");
        assert_eq!(report.groups[0].session_count, 2);
        assert_eq!(report.groups[0].cost_usd, 6.0);
    }

    #[test]
    fn session_budget_breach_detected_when_cost_exceeds_cap() {
        let view = fixture_view();
        let fleet = vec![fleet_session("/work/alpha", "ws-alpha")];
        let budget = CostBudgetConfig {
            session_usd: Some(3.0),
            ..CostBudgetConfig::default()
        };
        let report = build_cost_report(&view, &fleet, &budget);
        // sess-a ($4) breaches the $3 cap; sess-b ($2) does not.
        let breaches: Vec<_> = report.budget_breaches.iter().map(|b| &b.subject).collect();
        assert_eq!(breaches, vec!["sess-a"]);
        assert_eq!(report.budget_breaches[0].cost_usd, 4.0);
        assert_eq!(report.budget_breaches[0].limit_usd, 3.0);
    }

    #[test]
    fn group_budget_breach_detected_when_cost_exceeds_cap() {
        let mut view = fixture_view();
        view.sessions[1].project = "alpha".to_string();
        let fleet = vec![fleet_session("/work/alpha", "ws-alpha")];
        let budget = CostBudgetConfig {
            group_usd: Some(5.0),
            ..CostBudgetConfig::default()
        };
        let report = build_cost_report(&view, &fleet, &budget);
        // ws-alpha sums to $6 > $5 cap.
        let group_breach = report
            .budget_breaches
            .iter()
            .find(|b| b.scope_label() == "group")
            .expect("group breach");
        assert_eq!(group_breach.subject, "ws-alpha");
        assert_eq!(group_breach.cost_usd, 6.0);
    }

    #[test]
    fn per_session_override_takes_precedence_over_blanket_cap() {
        let view = fixture_view();
        let fleet = vec![fleet_session("/work/alpha", "ws-alpha")];
        let mut overrides = std::collections::HashMap::new();
        // Raise sess-a's ceiling above its $4 spend so it no longer breaches.
        overrides.insert("sess-a".to_string(), 10.0);
        let budget = CostBudgetConfig {
            session_usd: Some(1.0),
            session_overrides: overrides,
            ..CostBudgetConfig::default()
        };
        let report = build_cost_report(&view, &fleet, &budget);
        // sess-a exempt by override; sess-b ($2) still breaches the $1 blanket.
        let subjects: Vec<_> = report.budget_breaches.iter().map(|b| b.subject.as_str()).collect();
        assert_eq!(subjects, vec!["sess-b"]);
    }

    #[test]
    fn no_caps_yields_no_breaches() {
        let view = fixture_view();
        let fleet = vec![fleet_session("/work/alpha", "ws-alpha")];
        let report = build_cost_report(&view, &fleet, &CostBudgetConfig::default());
        assert!(report.budget_breaches.is_empty());
    }

    #[test]
    fn cost_bucket_merge_sums_costs_and_tokens() {
        let mut a = bucket(1.5, 50);
        a.merge(&bucket(2.5, 25));
        assert_eq!(a.cost_usd, Some(4.0));
        assert_eq!(a.total_tokens(), 75);
        assert_eq!(a.call_count, 2);
    }
}
