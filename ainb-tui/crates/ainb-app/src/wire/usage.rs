// ABOUTME: Section 21 `usage` as a frame carries it: a fold of the daemon's
// `fleet/usage_summary` reply (D3p-e), bounded whatever the daemon sent.
//
//   reply ──bound (section)──▶ held ──scrub──▶ cut ──▶ frame
//                 │
//      daily_cut, providers_cut, models_cut, projects_cut
//
// The verb already caps its lists (30 days, 10 per breakdown) and its detail
// (1,024 bytes); the section applies the same caps again, because a frame's
// bound cannot rest on the peer keeping its word, and counts what each list
// lost. Every string here is free text from the producer's view (a model id, a
// project key, a repo, a detail message), so each is scrubbed and then cut,
// never cut first: a cut through a credential leaves a prefix no shape
// matches. Names also lose their control characters, which draw nothing and
// cost six bytes each once escaped. With every field capped the section has a
// ceiling, [`USAGE_FRAME_MAX_BYTES`], that a test builds the worst case of.
//
// The projection is owned rather than borrowed so the same types carry the
// TypeScript bindings.

use crate::app::sections::{HeldUsage, UsageSection};

/// The most daily buckets a frame carries: the verb's own cap.
pub const USAGE_MAX_DAILY: usize = ainb_hangar_proto::fleet::FLEET_USAGE_MAX_DAILY_BUCKETS;

/// The most rows a frame carries per breakdown (providers, models, projects):
/// the verb's own cap.
pub const USAGE_MAX_BREAKDOWN: usize = ainb_hangar_proto::fleet::FLEET_USAGE_MAX_BREAKDOWN_BUCKETS;

/// The most characters a name (a date, a provider, a model, a project, a repo)
/// carries once scrubbed.
pub const USAGE_MAX_NAME_CHARS: usize = 120;

/// The most UTF-8 bytes the detail carries once scrubbed: the verb's own cap.
pub const USAGE_DETAIL_MAX_BYTES: usize = ainb_hangar_proto::fleet::FLEET_USAGE_DETAIL_MAX_BYTES;

/// How far past a cut the section keeps text, so the scrub sees a credential
/// that straddles the cut whole.
pub const USAGE_SCRUB_WINDOW: usize = 256;

/// The encoded bytes section 21 can reach with every list and string at its
/// cap and every character at its most expensive encoding.
pub const USAGE_FRAME_MAX_BYTES: usize = 80 * 1024;

/// Section 21 on the wire.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct UsageView {
    /// Why there is no summary: the daemon does not serve `fleet.usage.read`,
    /// or nothing can be read. Scrubbed.
    pub absent: Option<String>,
    /// Why the last read failed while the last summary is still drawn.
    /// Scrubbed.
    pub failure: Option<String>,
    /// The local epoch-ms clock the summary was received at.
    pub received_at_ms: Option<i64>,
    pub summary: Option<UsageSummaryFrame>,
}

/// Whether the daemon's summary is complete, as the daemon says.
#[derive(serde::Serialize, Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum UsageState {
    /// Still building a summary: no totals yet, and none to draw as zero.
    Scanning,
    /// Complete for every configured source.
    Ready,
    /// Useful, with one or more sources not fully read.
    Partial,
    /// The daemon cannot provide a summary.
    Unavailable,
}

/// One `fleet/usage_summary` reply, bounded.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct UsageSummaryFrame {
    pub state: UsageState,
    /// When the daemon generated the summary, epoch ms.
    pub generated_at: Option<i64>,
    /// Inclusive window start, epoch ms.
    pub start_at: Option<i64>,
    /// Exclusive window end, epoch ms.
    pub end_at: Option<i64>,
    /// `None` while scanning: never a synthesised zero.
    pub totals: Option<UsageBucketFrame>,
    /// Oldest first, at most [`USAGE_MAX_DAILY`].
    pub daily: Vec<UsageDayFrame>,
    /// Days the frame did not carry.
    pub daily_cut: usize,
    pub providers: Vec<UsageNamedFrame>,
    pub providers_cut: usize,
    pub models: Vec<UsageNamedFrame>,
    pub models_cut: usize,
    pub projects: Vec<UsageProjectFrame>,
    pub projects_cut: usize,
    /// The daemon's own word on a partial or unavailable summary, scrubbed and
    /// cut to [`USAGE_DETAIL_MAX_BYTES`].
    pub detail: Option<String>,
}

/// Token counts and, when every call was priced, the cost.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct UsageBucketFrame {
    pub input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub call_count: u64,
    pub session_count: u64,
    pub project_count: u64,
    /// `None` when a call in the bucket had no canonical rate: draw the
    /// tokens, never a zero cost.
    pub cost_usd: Option<f64>,
}

/// One day.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct UsageDayFrame {
    /// `YYYY-MM-DD`, UTC.
    pub date: String,
    pub bucket: UsageBucketFrame,
}

/// One provider or model.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct UsageNamedFrame {
    pub name: String,
    pub bucket: UsageBucketFrame,
}

/// One project, with its upstream repository when the daemon resolved one.
#[derive(serde::Serialize, Debug, Clone)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
pub struct UsageProjectFrame {
    pub name: String,
    pub repo: Option<String>,
    pub bucket: UsageBucketFrame,
}

impl From<&UsageSection> for UsageView {
    fn from(section: &UsageSection) -> Self {
        let scrub = |text: &str| crate::fleet::bridge::redact::scrub(text);
        Self {
            absent: section.absent.as_deref().map(scrub),
            failure: section.failure.as_deref().map(scrub),
            received_at_ms: section.received_at_ms,
            summary: section.summary.as_ref().map(summary),
        }
    }
}

fn summary(held: &HeldUsage) -> UsageSummaryFrame {
    use ainb_hangar_proto::fleet::FleetUsageSummaryState;
    let reply = &held.reply;
    UsageSummaryFrame {
        state: match reply.state {
            FleetUsageSummaryState::Scanning => UsageState::Scanning,
            FleetUsageSummaryState::Ready => UsageState::Ready,
            FleetUsageSummaryState::Partial => UsageState::Partial,
            FleetUsageSummaryState::Unavailable => UsageState::Unavailable,
        },
        generated_at: reply.generated_at,
        start_at: reply.start_at,
        end_at: reply.end_at,
        totals: reply.totals.as_ref().map(bucket),
        daily: reply
            .daily
            .iter()
            .map(|day| UsageDayFrame {
                date: name(&day.date),
                bucket: bucket(&day.bucket),
            })
            .collect(),
        daily_cut: held.daily_cut,
        providers: reply
            .providers
            .iter()
            .map(|row| UsageNamedFrame {
                name: name(&row.provider),
                bucket: bucket(&row.bucket),
            })
            .collect(),
        providers_cut: held.providers_cut,
        models: reply
            .models
            .iter()
            .map(|row| UsageNamedFrame {
                name: name(&row.model),
                bucket: bucket(&row.bucket),
            })
            .collect(),
        models_cut: held.models_cut,
        projects: reply
            .projects
            .iter()
            .map(|row| UsageProjectFrame {
                name: name(&row.project),
                repo: row.repo.as_deref().map(name),
                bucket: bucket(&row.bucket),
            })
            .collect(),
        projects_cut: held.projects_cut,
        detail: reply.detail.as_deref().map(detail),
    }
}

const fn bucket(bucket: &ainb_hangar_proto::fleet::FleetUsageBucket) -> UsageBucketFrame {
    UsageBucketFrame {
        input_tokens: bucket.input_tokens,
        cache_creation_tokens: bucket.cache_creation_tokens,
        cache_read_tokens: bucket.cache_read_tokens,
        output_tokens: bucket.output_tokens,
        reasoning_tokens: bucket.reasoning_tokens,
        call_count: bucket.call_count,
        session_count: bucket.session_count,
        project_count: bucket.project_count,
        cost_usd: bucket.cost_usd,
    }
}

/// A name, scrubbed, stripped of what draws nothing, then cut.
fn name(text: &str) -> String {
    let scrubbed = crate::fleet::bridge::redact::scrub(text);
    scrubbed
        .chars()
        .filter(|c| !c.is_control())
        .take(USAGE_MAX_NAME_CHARS)
        .collect()
}

/// The detail, scrubbed, then cut to [`USAGE_DETAIL_MAX_BYTES`] on a character
/// boundary.
fn detail(text: &str) -> String {
    let mut scrubbed = crate::fleet::bridge::redact::scrub(text);
    if scrubbed.len() > USAGE_DETAIL_MAX_BYTES {
        let mut end = USAGE_DETAIL_MAX_BYTES;
        while !scrubbed.is_char_boundary(end) {
            end -= 1;
        }
        scrubbed.truncate(end);
    }
    scrubbed
}
