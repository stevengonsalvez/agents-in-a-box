//! Wire schema for the `sessions.usage_data` event.
//!
//! Two plugins share this crate:
//!
//! * `ainb-plugin-session-reader` (Phase 6b) — owns parsing, publishes
//!   `sessions.usage_data` carrying a [`UsageDataEvent`].
//! * `ainb-plugin-burndown` (Phase 6c onwards) — subscribes to that
//!   topic and renders / formats from the same types.
//!
//! Wire encoding is rmp-serde (msgpack) with **externally-tagged**
//! enums (no `#[serde(tag = "kind")]`) — that combination round-trips
//! cleanly through rmp-serde for unit, struct, and newtype variants
//! alike. See `reference_rmp_serde_internally_tagged_primitives` and
//! `reference_serde_nested_tag_collision` for why the alternative
//! shapes blow up at encode/decode.
//!
//! Schema versioning lives on the envelope ([`UsageDataEvent::version`])
//! and is bumped whenever a field is added/removed/renamed in a way
//! consumers must care about. Decoders refuse mismatched versions.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![allow(clippy::module_name_repetitions)]

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// Wire-format schema version for [`UsageDataEvent`]. Bump on any
/// breaking change. Consumers compare `event.version == WIRE_VERSION`
/// before trusting the payload.
///
/// v2: added `chunk_index` + `is_final` so a single logical snapshot
/// can be split into N envelopes — required because the full snapshot
/// of a real `$HOME` (~2 GB / ~40k provider calls) encodes to ~110 MB
/// of msgpack, blowing past the host framer's 16 MiB body cap. Older
/// peers reading the new wire see defaults (`chunk_index = 0`,
/// `is_final = true`) — which is exactly the single-chunk path.
///
/// v3: added `Provider::Cursor` variant. Producer→consumer messages
/// from a v3 publisher carrying `"cursor"`-tagged calls fail to
/// deserialise on v2 consumers (the externally-tagged enum rejects
/// unknown variants), so receivers MUST check
/// `event.version == WIRE_VERSION` before trusting the payload.
///
/// v4: per-chunk `sessions` and `shell_commands` slices. Previously
/// chunk 0 carried the entire aggregate tree (including the full
/// `sessions` vec and the full `shell_commands` vec), which for large
/// `$HOME` datasets pushed chunk 0 past the framer's 16 MiB
/// `MAX_BODY_BYTES` cap (observed: 17 MiB for ~128k calls / ~4k
/// sessions / many distinct bash commands). Under v4, `sessions` and
/// `shell_commands` are tail-chunkable: chunk 0 leaves them empty;
/// follow-on chunks carry slices that the consumer accumulator
/// `extend`s into the in-flight `UsageData` just like `calls`.
pub const WIRE_VERSION: u32 = 4;

/// Per-file scan progress payload published on the
/// `sessions.scan_progress` topic.
///
/// `scanned` is the running count of files visited; `total` is the
/// pre-walk file count when known, or `0` during the initial walk (the
/// scanner emits `total = 0` until/unless it has cheaply enumerated the
/// full set). `current_project` is the project label of the most
/// recently scanned file — burndown surfaces it in the skeleton line.
///
/// Schema is intentionally unversioned: it's a soft-realtime
/// notification, not an authoritative snapshot. New fields are added
/// with `#[serde(default)]` so older subscribers keep decoding cleanly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanProgressEvent {
    /// Number of files visited so far. Monotonic within one scan.
    pub scanned: u32,
    /// Total file count if known, else `0` (UI renders without the
    /// `/M` suffix).
    pub total: u32,
    /// Project label (basename of the project dir or rollout date) for
    /// the most recently scanned file.
    pub current_project: String,
}

/// Optional payload for the `sessions.refresh_request` topic.
///
/// Historically the topic was a bare ping (empty payload) and most
/// publishers — the host FS watcher, burndown s `r` key — still send
/// exactly that, which decodes to the default: a normal incremental
/// refresh. A `hard` refresh (msgpack `{hard: true}`) tells
/// session-reader to distrust every cache layer and rebuild from
/// source — the CPU-heavy path, so publishers gate it behind explicit
/// user intent (`R` confirm in the TUI, `--hard` on the CLI).
///
/// Schema is intentionally unversioned, mirroring
/// [`ScanProgressEvent`]: new fields arrive with `#[serde(default)]`
/// so older publishers payloads keep decoding cleanly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshRequest {
    /// `true` = full rebuild from source (wipe parse cache + stable
    /// rollup first). `false`/absent = incremental refresh.
    #[serde(default)]
    pub hard: bool,
}

/// Top-level envelope published on the `sessions.usage_data` topic.
///
/// `published_ns` is filled in from the host's wall-clock at publish
/// time (via `ainb_now_ms` or equivalent). `partial = true` flags a
/// snapshot whose scan was budget-truncated — burndown shows a
/// "partial" badge and re-asks on the next refresh.
///
/// **Chunked publishes**: a single logical snapshot may be split into
/// N envelopes. Chunk 0 carries the bounded aggregates (`daily`,
/// `weekly`, `projects`, `grand_total`, `models`, `activities`,
/// `tools`, `mcp_servers`, `branches`, `model_project_counts`); chunks
/// 1..N carry slices of the unbounded vecs (`calls`, `sessions`,
/// `shell_commands`). The consumer accumulator concats each slice into
/// the in-flight `UsageData`. The last chunk sets `is_final = true`;
/// consumers treat the snapshot as live once they see `is_final`.
///
/// Single-chunk publishes use `chunk_index = 0, is_final = true` — the
/// defaults — so old publishers and small snapshots keep working
/// unchanged.
///
/// Wire-format note: chunks 1..N also leave every bounded aggregate
/// at default (empty vec / zero counters); only `calls`, `sessions`,
/// and `shell_commands` are populated. The consumer reads these three
/// fields with `extend`, ignoring the others on follow-on chunks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageDataEvent {
    /// Schema version. Equals [`WIRE_VERSION`] when the publisher and
    /// this crate agree.
    pub version: u32,
    /// Unix nanoseconds at publish.
    pub published_ns: u64,
    /// `true` if scan was truncated by fuel budget; `false` if complete.
    pub partial: bool,
    /// 0 = first chunk (carries aggregates); increments per follow-on
    /// chunk. Optional on the wire — defaults to `0` for old
    /// publishers / single-chunk snapshots.
    #[serde(default)]
    pub chunk_index: u32,
    /// `true` on the last chunk of a publish sequence (or on a
    /// single-chunk snapshot). Optional on the wire — defaults to
    /// `true` so a v1-era publisher reads as a complete single chunk.
    #[serde(default = "default_true")]
    pub is_final: bool,
    /// Aggregated snapshot. On chunks `> 0`, only the [`UsageData::calls`]
    /// vec is populated; every other field is default/empty.
    pub data: UsageData,
}

const fn default_true() -> bool {
    true
}

/// Provider — externally-tagged so unit variants serialise as plain
/// strings (`"claude"`, `"codex"`, …) which is the cheapest msgpack
/// encoding and avoids the internally-tagged-primitive panic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// Claude / claude.ai sessions (JSONL under `~/.claude/projects/`).
    Claude,
    /// Codex transcripts (JSONL under `~/.codex/sessions/`).
    Codex,
    /// Gemini Code Assist sessions.
    Gemini,
    /// GitHub Copilot Chat sessions.
    Copilot,
    /// Cursor IDE chat sessions.
    Cursor,
}

impl Provider {
    /// Stable lowercase identifier (matches msgpack/json wire form).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Copilot => "copilot",
            Self::Cursor => "cursor",
        }
    }
}

/// Aggregate counters for a slice of [`ProviderCall`]s. Cost is
/// optional because some providers don't surface unit pricing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenBucket {
    /// Sum of `input_tokens` across the slice.
    pub input_tokens: u64,
    /// Sum of `cache_creation_tokens`.
    pub cache_creation_tokens: u64,
    /// Sum of `cache_read_tokens`.
    pub cache_read_tokens: u64,
    /// Sum of `output_tokens`.
    pub output_tokens: u64,
    /// Sum of `reasoning_tokens` (Claude / o-series style).
    pub reasoning_tokens: u64,
    /// Distinct sessions contributing.
    pub session_count: usize,
    /// Distinct projects contributing.
    pub project_count: usize,
    /// Number of calls in the bucket.
    pub call_count: usize,
    /// Sum of `cost_usd` when at least one call had a price; `None`
    /// otherwise.
    pub cost_usd: Option<f64>,
}

impl TokenBucket {
    /// Total tokens (sum of every per-bucket field).
    #[must_use]
    pub fn total(&self) -> u64 {
        self.input_tokens
            + self.cache_creation_tokens
            + self.cache_read_tokens
            + self.output_tokens
            + self.reasoning_tokens
    }
}

/// Single provider call (one assistant turn).
///
/// `id` is FNV-1a 64 of `format!("{path}:{offset}")` — non-cryptographic
/// dedupe key, no crate dependency. See
/// `reference_inline_fnv_drop_blake3` for why we don't reach for blake3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCall {
    /// Stable per-call identifier (FNV-1a u64 of `path:offset`).
    pub id: u64,
    /// Source provider.
    pub provider: Provider,
    /// Model name as reported by the provider.
    pub model: String,
    /// Provider-assigned session id.
    pub session_id: String,
    /// Display-friendly project label (resolved upstream where possible).
    pub project: String,
    /// Working directory the turn was logged from.
    pub project_path: String,
    /// UTC timestamp.
    pub timestamp: DateTime<Utc>,
    /// Per-call input tokens.
    pub input_tokens: u64,
    /// Per-call cache-creation tokens (Claude prompt-cache writes).
    pub cache_creation_tokens: u64,
    /// Per-call cache-read tokens (Claude prompt-cache hits).
    pub cache_read_tokens: u64,
    /// Per-call output tokens.
    pub output_tokens: u64,
    /// Per-call reasoning tokens (e.g. extended-thinking for Claude).
    pub reasoning_tokens: u64,
    /// Per-call cost when the provider exposes pricing.
    pub cost_usd: Option<f64>,
    /// Tools used during the turn (display labels, deduped).
    pub tools: Vec<String>,
    /// Bash commands run during the turn (raw, deduped).
    pub bash_commands: Vec<String>,
    /// First user message that opened the turn.
    pub user_message: String,
    /// `gitBranch` if the provider records it; otherwise `None`.
    pub branch: Option<String>,
}

/// Per-day dashboard row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyUsage {
    /// Calendar day in UTC.
    pub date: NaiveDate,
    /// Aggregated bucket.
    pub bucket: TokenBucket,
}

/// Per-project summary (one row per upstream remote / fallback folder).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectUsage {
    /// Display label and aggregation key.
    pub name: String,
    /// Working directory.
    pub path: String,
    /// Aggregated bucket.
    pub bucket: TokenBucket,
    /// `Some(owner/repo)` when the cwd resolved to an upstream remote.
    pub repo: Option<String>,
}

/// Per-session summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUsage {
    /// Source provider.
    pub provider: Provider,
    /// Project label this session belongs to.
    pub project: String,
    /// Provider-assigned session id.
    pub session_id: String,
    /// Earliest call timestamp.
    pub first_timestamp: DateTime<Utc>,
    /// Latest call timestamp.
    pub last_timestamp: DateTime<Utc>,
    /// Aggregated bucket.
    pub bucket: TokenBucket,
}

/// Per-model summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelUsage {
    /// Model name (provider-reported).
    pub model: String,
    /// Aggregated bucket.
    pub bucket: TokenBucket,
}

/// Per-branch summary. Only Claude turns with a non-empty `gitBranch`
/// contribute; other providers / non-git turns are dropped from this view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BranchUsage {
    /// Branch name.
    pub branch: String,
    /// Aggregated bucket.
    pub bucket: TokenBucket,
}

/// High-level activity classification for a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityCategory {
    /// Writing / editing application code.
    Code,
    /// Documentation, READMEs, comments.
    Docs,
    /// Investigating / fixing bugs.
    Debug,
    /// Refactoring without behaviour change.
    Refactor,
    /// Authoring / running tests.
    Test,
    /// Anything that doesn't match the above.
    Other,
}

/// Activity-level summary including retry / single-shot metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityUsage {
    /// Activity category.
    pub category: ActivityCategory,
    /// Aggregated bucket.
    pub bucket: TokenBucket,
    /// Total turns classified as this activity.
    pub turns: usize,
    /// Turns that immediately re-prompted after a failure.
    pub retries: usize,
    /// Turns that produced an edit.
    pub edit_turns: usize,
    /// Turns that completed in one shot (no retry).
    pub one_shot_turns: usize,
}

/// Generic name + count row used for tools / MCP servers / shell
/// commands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NamedUsage {
    /// Display name.
    pub name: String,
    /// Number of calls referencing this name.
    pub calls: usize,
}

/// Complete parsed snapshot. Sortable / deterministic so msgpack
/// encodes the same bytes for the same input — burndown's tripwire
/// asserts byte-identity over this.
///
/// Note: `model_project_counts` is `Vec<(String, Vec<(String, usize)>)>`
/// (sorted by model name) instead of a `HashMap` so the msgpack
/// representation is stable across runs. (Rust's default `HashMap`
/// hash order is non-deterministic.)
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageData {
    /// Daily totals, oldest → newest.
    pub daily: Vec<(NaiveDate, TokenBucket)>,
    /// ISO-week totals, oldest → newest (week start = Monday).
    pub weekly: Vec<(NaiveDate, TokenBucket)>,
    /// Per-project rows, sorted by total tokens descending.
    pub projects: Vec<ProjectUsage>,
    /// Sum across all calls.
    pub grand_total: TokenBucket,
    /// All calls, oldest → newest.
    pub calls: Vec<ProviderCall>,
    /// Per-session rows, sorted by `last_timestamp` descending.
    pub sessions: Vec<SessionUsage>,
    /// Per-model rows, sorted by total tokens descending.
    pub models: Vec<ModelUsage>,
    /// Per-activity rows.
    pub activities: Vec<ActivityUsage>,
    /// Per-tool rows.
    pub tools: Vec<NamedUsage>,
    /// MCP-server breakdown.
    pub mcp_servers: Vec<NamedUsage>,
    /// Shell-command breakdown.
    pub shell_commands: Vec<NamedUsage>,
    /// Per-branch rows (Claude-only contributors).
    pub branches: Vec<BranchUsage>,
    /// `model -> [(project, call_count), …]` precomputed for the
    /// "top N projects for model X" lookup. Sorted by model name
    /// (deterministic msgpack order). Each inner vec is sorted by
    /// count descending.
    pub model_project_counts: Vec<(String, Vec<(String, usize)>)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_call() -> ProviderCall {
        ProviderCall {
            id: 0xDEAD_BEEF,
            provider: Provider::Claude,
            model: "claude-3-5-sonnet".into(),
            session_id: "s1".into(),
            project: "ainb".into(),
            project_path: "/tmp/ainb".into(),
            timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
            input_tokens: 100,
            cache_creation_tokens: 0,
            cache_read_tokens: 50,
            output_tokens: 200,
            reasoning_tokens: 0,
            cost_usd: Some(0.005),
            tools: vec!["Read".into(), "Edit".into()],
            bash_commands: vec!["ls".into()],
            user_message: "do thing".into(),
            branch: Some("main".into()),
        }
    }

    fn fixture_data() -> UsageData {
        let bucket = TokenBucket {
            input_tokens: 100,
            cache_read_tokens: 50,
            output_tokens: 200,
            session_count: 1,
            project_count: 1,
            call_count: 1,
            cost_usd: Some(0.005),
            ..TokenBucket::default()
        };
        UsageData {
            daily: vec![(NaiveDate::from_ymd_opt(2026, 5, 9).unwrap(), bucket)],
            weekly: vec![(NaiveDate::from_ymd_opt(2026, 5, 4).unwrap(), bucket)],
            projects: vec![ProjectUsage {
                name: "ainb".into(),
                path: "/tmp/ainb".into(),
                bucket,
                repo: Some("stevengonsalvez/agents-in-a-box".into()),
            }],
            grand_total: bucket,
            calls: vec![fixture_call()],
            sessions: vec![SessionUsage {
                provider: Provider::Claude,
                project: "ainb".into(),
                session_id: "s1".into(),
                first_timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
                last_timestamp: DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap(),
                bucket,
            }],
            models: vec![ModelUsage {
                model: "claude-3-5-sonnet".into(),
                bucket,
            }],
            activities: vec![ActivityUsage {
                category: ActivityCategory::Code,
                bucket,
                turns: 1,
                retries: 0,
                edit_turns: 1,
                one_shot_turns: 1,
            }],
            tools: vec![NamedUsage {
                name: "Read".into(),
                calls: 1,
            }],
            mcp_servers: vec![],
            shell_commands: vec![NamedUsage {
                name: "ls".into(),
                calls: 1,
            }],
            branches: vec![BranchUsage {
                branch: "main".into(),
                bucket,
            }],
            model_project_counts: vec![("claude-3-5-sonnet".into(), vec![("ainb".into(), 1)])],
        }
    }

    #[test]
    fn provider_variants_roundtrip() {
        for p in [
            Provider::Claude,
            Provider::Codex,
            Provider::Gemini,
            Provider::Copilot,
        ] {
            let bytes = rmp_serde::to_vec_named(&p).unwrap();
            let back: Provider = rmp_serde::from_slice(&bytes).unwrap();
            assert_eq!(p, back, "round-trip failed for {p:?}");
        }
    }

    #[test]
    fn activity_category_variants_roundtrip() {
        for c in [
            ActivityCategory::Code,
            ActivityCategory::Docs,
            ActivityCategory::Debug,
            ActivityCategory::Refactor,
            ActivityCategory::Test,
            ActivityCategory::Other,
        ] {
            let bytes = rmp_serde::to_vec_named(&c).unwrap();
            let back: ActivityCategory = rmp_serde::from_slice(&bytes).unwrap();
            assert_eq!(c, back);
        }
    }

    #[test]
    fn provider_call_roundtrip() {
        let call = fixture_call();
        let bytes = rmp_serde::to_vec_named(&call).unwrap();
        let back: ProviderCall = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(call, back);
    }

    #[test]
    fn usage_data_default_roundtrip() {
        let data = UsageData::default();
        let bytes = rmp_serde::to_vec_named(&data).unwrap();
        let back: UsageData = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(data, back);
    }

    #[test]
    fn usage_data_full_fixture_roundtrip() {
        let data = fixture_data();
        let bytes = rmp_serde::to_vec_named(&data).unwrap();
        let back: UsageData = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(data, back);
    }

    #[test]
    fn refresh_request_roundtrip_and_default() {
        let bytes = rmp_serde::to_vec_named(&RefreshRequest { hard: true }).unwrap();
        let back: RefreshRequest = rmp_serde::from_slice(&bytes).unwrap();
        assert!(back.hard);

        // Empty map (a publisher encoding `{}`) → hard defaults off.
        let back: RefreshRequest = rmp_serde::from_slice(&[0x80]).unwrap();
        assert!(!back.hard);
    }

    /// Wire-format pin: the host CLI (`ainb-core/src/cli/registry.rs`,
    /// `--hard` branch of `dispatch_inner`) publishes this exact byte
    /// sequence WITHOUT linking this crate, to stay decoupled from the
    /// plugin codec. If the canonical `to_vec_named` encoding of
    /// `RefreshRequest { hard: true }` ever drifts from those
    /// hand-pinned bytes, this test fails and both sides must move
    /// together.
    #[test]
    fn refresh_request_hard_wire_bytes_are_pinned() {
        const HARD_REFRESH_MSGPACK: [u8; 7] = [0x81, 0xA4, b'h', b'a', b'r', b'd', 0xC3];
        let canonical = rmp_serde::to_vec_named(&RefreshRequest { hard: true }).unwrap();
        assert_eq!(
            canonical, HARD_REFRESH_MSGPACK,
            "host CLI pinned bytes drifted"
        );
    }

    #[test]
    fn envelope_roundtrip() {
        let env = UsageDataEvent {
            version: WIRE_VERSION,
            published_ns: 1_700_000_000_000_000_000,
            partial: false,
            chunk_index: 0,
            is_final: true,
            data: fixture_data(),
        };
        let bytes = rmp_serde::to_vec_named(&env).unwrap();
        let back: UsageDataEvent = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(env, back);
    }

    #[test]
    fn version_field_round_trips_arbitrary_value() {
        // Decoder doesn't enforce version equality at the type level —
        // consumers must check `event.version == WIRE_VERSION` and
        // refuse mismatches before trusting the payload.
        let env = UsageDataEvent {
            version: WIRE_VERSION + 1,
            published_ns: 0,
            partial: true,
            chunk_index: 0,
            is_final: true,
            data: UsageData::default(),
        };
        let bytes = rmp_serde::to_vec_named(&env).unwrap();
        let back: UsageDataEvent = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(back.version, WIRE_VERSION + 1);
        assert!(back.partial);
    }

    #[test]
    fn older_wire_version_decodes_so_consumers_can_reject_it() {
        // An older publisher (v2 of the wire) may still be in flight
        // when burndown upgrades to v3. The envelope's type-level
        // decoder must succeed (otherwise the consumer never gets the
        // chance to compare `event.version` and gracefully fall back
        // to the schema-mismatch UI). This pins that contract so a
        // future WIRE_VERSION bump can't silently break it.
        assert!(
            WIRE_VERSION >= 2,
            "test assumes a previous wire generation exists"
        );
        let v2_env = UsageDataEvent {
            version: WIRE_VERSION - 1,
            published_ns: 0,
            partial: false,
            chunk_index: 0,
            is_final: true,
            data: UsageData::default(),
        };
        let bytes = rmp_serde::to_vec_named(&v2_env).unwrap();
        let back: UsageDataEvent =
            rmp_serde::from_slice(&bytes).expect("older wire version must decode cleanly");
        assert_eq!(back.version, WIRE_VERSION - 1);
        // Consumer-side gate: this is exactly what burndown's
        // handle_usage_data_event uses to latch `schema_mismatch`.
        assert_ne!(back.version, WIRE_VERSION);
    }

    #[test]
    fn chunked_envelope_roundtrip() {
        // Chunk 0 of a hypothetical 2-chunk publish.
        let chunk0 = UsageDataEvent {
            version: WIRE_VERSION,
            published_ns: 1_700_000_000_000_000_000,
            partial: false,
            chunk_index: 0,
            is_final: false,
            data: fixture_data(),
        };
        let bytes = rmp_serde::to_vec_named(&chunk0).unwrap();
        let back: UsageDataEvent = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(chunk0, back);
        assert_eq!(back.chunk_index, 0);
        assert!(!back.is_final);

        // Chunk 1 (final) carries only the remaining calls.
        let mut calls_only = UsageData::default();
        calls_only.calls = vec![fixture_call()];
        let chunk1 = UsageDataEvent {
            version: WIRE_VERSION,
            published_ns: 1_700_000_000_000_000_000,
            partial: false,
            chunk_index: 1,
            is_final: true,
            data: calls_only,
        };
        let bytes = rmp_serde::to_vec_named(&chunk1).unwrap();
        let back: UsageDataEvent = rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(chunk1, back);
    }

    #[test]
    fn token_bucket_total_sums_all_token_fields() {
        let b = TokenBucket {
            input_tokens: 1,
            cache_creation_tokens: 2,
            cache_read_tokens: 3,
            output_tokens: 4,
            reasoning_tokens: 5,
            ..TokenBucket::default()
        };
        assert_eq!(b.total(), 15);
    }

    #[test]
    fn provider_as_str_is_stable_lowercase() {
        assert_eq!(Provider::Claude.as_str(), "claude");
        assert_eq!(Provider::Codex.as_str(), "codex");
        assert_eq!(Provider::Gemini.as_str(), "gemini");
        assert_eq!(Provider::Copilot.as_str(), "copilot");
    }
}
