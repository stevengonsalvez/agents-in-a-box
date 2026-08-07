//! Bounded, daemon-owned Fleet Usage projection.
//!
//! Provider logs and canonical model-rate parsing stay behind this module. The
//! public RPC receives only aggregates, never paths, transcripts, or calls.

use std::collections::HashMap;
use std::time::{Duration as StdDuration, SystemTime};

use ainb_hangar_proto::fleet::{
    FLEET_USAGE_MAX_BREAKDOWN_BUCKETS, FLEET_USAGE_MAX_DAILY_BUCKETS, FleetUsageBucket,
    FleetUsageDailyBucket, FleetUsageModelBucket, FleetUsagePeriod, FleetUsageProjectBucket,
    FleetUsageProviderBucket, FleetUsageSummaryResult, FleetUsageSummaryState,
};
use ainb_plugin_session_reader::scanner::{self, ProviderRoots};
use ainb_plugin_types_sessions::{ProviderCall, TokenBucket, UsageData};
use chrono::{DateTime, Duration, NaiveDate, Utc};

/// Scan canonical provider histories then expose the requested bounded window.
///
/// This runs on a blocking worker owned by the RPC handler. The scanner itself
/// owns provider parsing and rate lookup, so this module does not duplicate
/// provider-specific cost logic.
#[must_use]
pub fn scan_summary(period: FleetUsagePeriod) -> FleetUsageSummaryResult {
    let roots = ProviderRoots::defaults();
    if roots.claude_projects.is_none()
        && roots.codex_sessions.is_none()
        && roots.gemini_sessions.is_none()
        && roots.copilot_sessions.is_none()
        && roots.cursor_sessions.is_none()
    {
        return FleetUsageSummaryResult {
            state: FleetUsageSummaryState::Unavailable,
            generated_at: Some(Utc::now().timestamp_millis()),
            start_at: None,
            end_at: None,
            totals: None,
            daily: Vec::new(),
            providers: Vec::new(),
            models: Vec::new(),
            projects: Vec::new(),
            detail: Some("provider history roots are unavailable".to_string()),
        };
    }
    let now = Utc::now();
    let (start, _) = window(period, now);
    let since = SystemTime::UNIX_EPOCH
        + StdDuration::from_millis(u64::try_from(start.timestamp_millis()).unwrap_or_default());
    summary_from_usage(scanner::scan_since(&roots, since), period, now)
}

fn summary_from_usage(
    usage: UsageData,
    period: FleetUsagePeriod,
    now: DateTime<Utc>,
) -> FleetUsageSummaryResult {
    let (start, end) = window(period, now);
    let calls: Vec<_> = usage
        .calls
        .into_iter()
        .filter(|call| call.timestamp >= start && call.timestamp < end)
        .collect();
    let projected = scanner::aggregate(calls.clone());
    let complete_costs = completeness(&calls);

    let mut daily: Vec<_> = projected
        .daily
        .into_iter()
        .map(|(date, bucket)| FleetUsageDailyBucket {
            date: date.to_string(),
            bucket: bucket_from(
                &bucket,
                complete_costs.daily.get(&date).copied().unwrap_or(true),
            ),
        })
        .collect();
    daily.truncate(FLEET_USAGE_MAX_DAILY_BUCKETS);

    let mut providers: Vec<_> = grouped_usage(&calls, |call| call.provider.as_str().to_string())
        .into_iter()
        .map(|(provider, usage)| FleetUsageProviderBucket {
            bucket: bucket_from(&usage.bucket, usage.complete_cost),
            provider,
        })
        .collect();
    sort_and_cap(&mut providers, |row| &row.bucket);

    let mut models: Vec<_> = projected
        .models
        .into_iter()
        .map(|row| FleetUsageModelBucket {
            bucket: bucket_from(
                &row.bucket,
                complete_costs.models.get(&row.model).copied().unwrap_or(true),
            ),
            model: row.model,
        })
        .collect();
    sort_and_cap(&mut models, |row| &row.bucket);

    let mut projects: Vec<_> = projected
        .projects
        .into_iter()
        .map(|row| FleetUsageProjectBucket {
            bucket: bucket_from(
                &row.bucket,
                complete_costs.projects.get(&row.name).copied().unwrap_or(true),
            ),
            project: row.name,
            repo: row.repo,
        })
        .collect();
    sort_and_cap(&mut projects, |row| &row.bucket);

    FleetUsageSummaryResult {
        state: FleetUsageSummaryState::Ready,
        generated_at: Some(now.timestamp_millis()),
        start_at: Some(start.timestamp_millis()),
        end_at: Some(end.timestamp_millis()),
        totals: Some(bucket_from(
            &projected.grand_total,
            calls.iter().all(|call| call.cost_usd.is_some()),
        )),
        daily,
        providers,
        models,
        projects,
        detail: None,
    }
}

fn window(period: FleetUsagePeriod, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let end_day = now.date_naive().succ_opt().expect("valid UTC date");
    let days = match period {
        FleetUsagePeriod::Today => 1,
        FleetUsagePeriod::Trailing7Days => 7,
        FleetUsagePeriod::Trailing30Days => 30,
    };
    let start_day = end_day - Duration::days(days);
    (
        start_day.and_hms_opt(0, 0, 0).expect("midnight UTC").and_utc(),
        end_day.and_hms_opt(0, 0, 0).expect("midnight UTC").and_utc(),
    )
}

#[derive(Default)]
struct CostCompleteness {
    daily: HashMap<NaiveDate, bool>,
    models: HashMap<String, bool>,
    projects: HashMap<String, bool>,
}

fn completeness(calls: &[ProviderCall]) -> CostCompleteness {
    let mut result = CostCompleteness::default();
    for call in calls {
        let priced = call.cost_usd.is_some();
        mark(&mut result.daily, call.timestamp.date_naive(), priced);
        mark(&mut result.models, call.model.clone(), priced);
        mark(&mut result.projects, call.project.clone(), priced);
    }
    result
}

fn mark<K: std::hash::Hash + Eq>(map: &mut HashMap<K, bool>, key: K, priced: bool) {
    map.entry(key).and_modify(|complete| *complete &= priced).or_insert(priced);
}

struct GroupedUsage {
    bucket: TokenBucket,
    complete_cost: bool,
}

fn grouped_usage<F>(calls: &[ProviderCall], key: F) -> HashMap<String, GroupedUsage>
where
    F: Fn(&ProviderCall) -> String,
{
    let mut groups: HashMap<String, Vec<ProviderCall>> = HashMap::new();
    for call in calls {
        groups.entry(key(call)).or_default().push(call.clone());
    }
    groups
        .into_iter()
        .map(|(key, calls)| {
            let complete_cost = calls.iter().all(|call| call.cost_usd.is_some());
            let bucket = scanner::aggregate(calls).grand_total;
            (
                key,
                GroupedUsage {
                    bucket,
                    complete_cost,
                },
            )
        })
        .collect()
}

fn bucket_from(bucket: &TokenBucket, complete_cost: bool) -> FleetUsageBucket {
    FleetUsageBucket {
        input_tokens: bucket.input_tokens,
        cache_creation_tokens: bucket.cache_creation_tokens,
        cache_read_tokens: bucket.cache_read_tokens,
        output_tokens: bucket.output_tokens,
        reasoning_tokens: bucket.reasoning_tokens,
        call_count: u64::try_from(bucket.call_count).unwrap_or(u64::MAX),
        session_count: u64::try_from(bucket.session_count).unwrap_or(u64::MAX),
        project_count: u64::try_from(bucket.project_count).unwrap_or(u64::MAX),
        cost_usd: complete_cost.then_some(bucket.cost_usd).flatten(),
    }
}

fn sort_and_cap<T>(rows: &mut Vec<T>, bucket: impl Fn(&T) -> &FleetUsageBucket) {
    rows.sort_by(|left, right| {
        let left_total = bucket(left).input_tokens
            + bucket(left).cache_creation_tokens
            + bucket(left).cache_read_tokens
            + bucket(left).output_tokens
            + bucket(left).reasoning_tokens;
        let right_total = bucket(right).input_tokens
            + bucket(right).cache_creation_tokens
            + bucket(right).cache_read_tokens
            + bucket(right).output_tokens
            + bucket(right).reasoning_tokens;
        right_total.cmp(&left_total)
    });
    rows.truncate(FLEET_USAGE_MAX_BREAKDOWN_BUCKETS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_windows_are_bounded_and_utc_aligned() {
        let now = "2026-08-06T15:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let (start, end) = window(FleetUsagePeriod::Trailing7Days, now);
        assert_eq!(start.to_rfc3339(), "2026-07-31T00:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-08-07T00:00:00+00:00");
    }

    #[test]
    fn unknown_cost_never_becomes_zero() {
        let bucket = TokenBucket {
            input_tokens: 10,
            cost_usd: Some(1.25),
            ..TokenBucket::default()
        };
        assert_eq!(bucket_from(&bucket, false).cost_usd, None);
        assert_eq!(bucket_from(&bucket, false).input_tokens, 10);
    }
}
