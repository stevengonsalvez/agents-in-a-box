#![allow(missing_docs)]

// ABOUTME: Section 21 `usage` is a fold of the daemon's `fleet/usage_summary`
// reply (D3p-e). It computes nothing: what the daemon counted crosses as it
// counted it, bounded by the verb's own caps whatever the daemon sends, with
// every cut counted, the free text scrubbed, and the states drawn as states.

use ainb_app::AppState;
use ainb_app::SectionId;
use ainb_app::wire::frame::HostId;
use ainb_app::wire::section_json;
use ainb_app::wire::usage::{
    USAGE_DETAIL_MAX_BYTES, USAGE_FRAME_MAX_BYTES, USAGE_MAX_BREAKDOWN, USAGE_MAX_DAILY,
    USAGE_MAX_NAME_CHARS, USAGE_REASON_MAX_CHARS,
};
use ainb_hangar_proto::fleet::{
    FleetUsageBucket, FleetUsageDailyBucket, FleetUsageModelBucket, FleetUsageProjectBucket,
    FleetUsageProviderBucket, FleetUsageSummaryResult, FleetUsageSummaryState,
};

fn bucket(input: u64, cost: Option<f64>) -> FleetUsageBucket {
    FleetUsageBucket {
        input_tokens: input,
        cache_creation_tokens: 2,
        cache_read_tokens: 3,
        output_tokens: 4,
        reasoning_tokens: 5,
        call_count: 6,
        session_count: 7,
        project_count: 8,
        cost_usd: cost,
    }
}

fn ready() -> FleetUsageSummaryResult {
    FleetUsageSummaryResult {
        state: FleetUsageSummaryState::Ready,
        generated_at: Some(1_700_000_000_000),
        start_at: Some(1_697_408_000_000),
        end_at: Some(1_700_000_000_000),
        totals: Some(bucket(100, Some(1.25))),
        daily: vec![FleetUsageDailyBucket {
            date: "2026-09-19".to_string(),
            bucket: bucket(40, None),
        }],
        providers: vec![FleetUsageProviderBucket {
            provider: "claude".to_string(),
            bucket: bucket(90, Some(1.0)),
        }],
        models: vec![FleetUsageModelBucket {
            model: "claude-opus-5".to_string(),
            bucket: bucket(80, Some(0.9)),
        }],
        projects: vec![FleetUsageProjectBucket {
            project: "agents-in-a-box".to_string(),
            repo: Some("stevengonsalvez/agents-in-a-box".to_string()),
            bucket: bucket(70, None),
        }],
        detail: None,
    }
}

fn framed(state: &AppState) -> serde_json::Value {
    section_json(state, SectionId::Usage, &HostId::local())
}

#[test]
fn before_any_read_the_section_holds_nothing() {
    let state = AppState::new();
    let body = framed(&state);
    assert!(body["summary"].is_null(), "{body}");
    assert!(body["absent"].is_null(), "{body}");
}

#[test]
fn a_read_frames_its_totals_days_and_breakdowns_as_the_daemon_counted_them() {
    let mut state = AppState::new();
    assert!(state.apply_usage_read(ready(), 42));
    let body = framed(&state);
    let summary = &body["summary"];
    assert_eq!(body["received_at_ms"], 42);
    assert_eq!(summary["state"], "ready");
    assert_eq!(summary["generated_at"], 1_700_000_000_000_i64);
    assert_eq!(summary["totals"]["input_tokens"], 100);
    assert_eq!(summary["totals"]["cost_usd"], 1.25);
    assert_eq!(summary["daily"][0]["date"], "2026-09-19");
    assert!(
        summary["daily"][0]["bucket"]["cost_usd"].is_null(),
        "an unpriced day crosses without a cost, never as zero: {summary}"
    );
    assert_eq!(summary["providers"][0]["name"], "claude");
    assert_eq!(summary["models"][0]["name"], "claude-opus-5");
    // Every project key frames as its leaf segment, so a hyphenated name
    // shortens to its last word; the repo still names it whole.
    assert_eq!(summary["projects"][0]["name"], "box");
    assert_eq!(
        summary["projects"][0]["repo"],
        "stevengonsalvez/agents-in-a-box"
    );
    for cut in ["daily_cut", "providers_cut", "models_cut", "projects_cut"] {
        assert_eq!(summary[cut], 0, "{cut}");
    }
}

#[test]
fn the_same_reply_again_moves_no_version() {
    let mut state = AppState::new();
    assert!(state.apply_usage_read(ready(), 1));
    let before = state.versions();
    assert!(
        !state.apply_usage_read(ready(), 2),
        "same counters, no change"
    );
    assert_eq!(before, state.versions());
}

#[test]
fn scanning_and_partial_frame_as_states_not_as_zeros() {
    let mut state = AppState::new();
    state.apply_usage_read(
        FleetUsageSummaryResult {
            state: FleetUsageSummaryState::Scanning,
            generated_at: None,
            start_at: None,
            end_at: None,
            totals: None,
            daily: Vec::new(),
            providers: Vec::new(),
            models: Vec::new(),
            projects: Vec::new(),
            detail: Some("scanning provider logs".to_string()),
        },
        1,
    );
    let body = framed(&state);
    assert_eq!(body["summary"]["state"], "scanning");
    assert!(
        body["summary"]["totals"].is_null(),
        "no synthesised zero: {body}"
    );
    assert_eq!(body["summary"]["detail"], "scanning provider logs");

    let mut partial = ready();
    partial.state = FleetUsageSummaryState::Partial;
    state.apply_usage_read(partial, 2);
    assert_eq!(framed(&state)["summary"]["state"], "partial");
}

#[test]
fn a_refused_capability_frames_absent_with_the_reason() {
    let mut state = AppState::new();
    assert!(state.usage_absent("daemon does not serve fleet.usage.read"));
    let body = framed(&state);
    assert_eq!(body["absent"], "daemon does not serve fleet.usage.read");
    assert!(body["summary"].is_null());

    assert!(state.apply_usage_read(ready(), 5), "a later read clears it");
    assert!(framed(&state)["absent"].is_null());
}

#[test]
fn a_failed_read_keeps_the_last_summary_and_says_why() {
    let mut state = AppState::new();
    state.apply_usage_read(ready(), 5);
    assert!(state.usage_read_failed("daemon not reachable"));
    let body = framed(&state);
    assert_eq!(body["failure"], "daemon not reachable");
    assert_eq!(
        body["summary"]["totals"]["input_tokens"], 100,
        "the numbers stay"
    );
    assert!(
        !state.usage_read_failed("daemon not reachable"),
        "same reason, no change"
    );

    state.apply_usage_read(ready(), 6);
    assert!(
        framed(&state)["failure"].is_null(),
        "a read clears the failure"
    );
}

#[test]
fn the_free_text_is_scrubbed() {
    let token = format!("ghp_{}", "C".repeat(36));
    let mut reply = ready();
    reply.detail = Some(format!("source failed: {token}"));
    reply.providers[0].provider = format!("p {token}");
    reply.models[0].model = format!("m {token}");
    reply.projects[0].project = format!("proj {token}");
    reply.projects[0].repo = Some(format!("https://x:{token}@github.com/o/r"));
    reply.daily[0].date = format!("d {token}");
    let mut state = AppState::new();
    state.apply_usage_read(reply, 1);
    state.usage_read_failed(format!("rpc said {token}"));
    let text = serde_json::to_string(&framed(&state)).expect("encodes");
    assert!(!text.contains(&token), "a credential left: {text}");
    assert!(text.contains("<redacted>"), "{text}");
}

/// A daemon that ignores its own caps still frames inside them: every list is
/// cut to the verb's cap with the loss counted, every name to its character
/// cap after the scrub, the detail to its byte cap, and the whole section
/// encodes under a fixed ceiling.
#[test]
fn a_reply_past_every_cap_is_cut_counted_and_bounded_in_bytes() {
    let long = "\u{1f4a5}".repeat(20_000);
    let named = |n: usize| format!("{n}{long}");
    let mut state = AppState::new();
    state.apply_usage_read(
        FleetUsageSummaryResult {
            state: FleetUsageSummaryState::Ready,
            generated_at: Some(1),
            start_at: Some(1),
            end_at: Some(2),
            totals: Some(bucket(u64::MAX, Some(f64::MAX))),
            daily: (0..500)
                .map(|n| FleetUsageDailyBucket {
                    date: named(n),
                    bucket: bucket(1, None),
                })
                .collect(),
            providers: (0..50)
                .map(|n| FleetUsageProviderBucket {
                    provider: named(n),
                    bucket: bucket(1, None),
                })
                .collect(),
            models: (0..50)
                .map(|n| FleetUsageModelBucket {
                    model: named(n),
                    bucket: bucket(1, None),
                })
                .collect(),
            projects: (0..50)
                .map(|n| FleetUsageProjectBucket {
                    project: named(n),
                    repo: Some(named(n)),
                    bucket: bucket(1, None),
                })
                .collect(),
            detail: Some("\"".repeat(100_000)),
        },
        1,
    );
    state.usage_read_failed("\"".repeat(100_000));
    let body = framed(&state);
    let summary = &body["summary"];
    assert_eq!(
        summary["daily"].as_array().expect("days").len(),
        USAGE_MAX_DAILY
    );
    assert_eq!(summary["daily_cut"], 500 - USAGE_MAX_DAILY);
    // `daily` is oldest first, so the cut drops the oldest: the frame keeps
    // the thirty days a person is looking at, ending today.
    let first = summary["daily"][0]["date"].as_str().expect("a date");
    let last = summary["daily"][USAGE_MAX_DAILY - 1]["date"].as_str().expect("a date");
    assert!(
        first.starts_with("470"),
        "the oldest kept day is day 470: {first:.8}"
    );
    assert!(last.starts_with("499"), "the newest day is kept: {last:.8}");
    for (list, cut) in [
        ("providers", "providers_cut"),
        ("models", "models_cut"),
        ("projects", "projects_cut"),
    ] {
        assert_eq!(
            summary[list].as_array().expect(list).len(),
            USAGE_MAX_BREAKDOWN,
            "{list}"
        );
        assert_eq!(summary[cut], 50 - USAGE_MAX_BREAKDOWN, "{cut}");
    }
    let name = summary["models"][0]["name"].as_str().expect("a name");
    assert!(
        name.chars().count() <= USAGE_MAX_NAME_CHARS,
        "{} chars",
        name.chars().count()
    );
    let detail = summary["detail"].as_str().expect("a detail");
    assert!(
        detail.len() <= USAGE_DETAIL_MAX_BYTES,
        "{} bytes",
        detail.len()
    );
    let failure = body["failure"].as_str().expect("a failure");
    assert!(
        failure.chars().count() <= USAGE_REASON_MAX_CHARS + 8,
        "the failure is cut: {} chars",
        failure.chars().count()
    );
    let encoded = serde_json::to_vec(&body).expect("encodes").len();
    assert!(
        encoded <= USAGE_FRAME_MAX_BYTES,
        "{encoded} bytes past {USAGE_FRAME_MAX_BYTES}"
    );
}

/// An absent reason is the host's text and has no ceiling of its own either.
#[test]
fn an_absent_reason_is_cut() {
    let mut state = AppState::new();
    state.usage_absent("x".repeat(100_000));
    let absent = framed(&state)["absent"].as_str().expect("absent").to_string();
    assert!(
        absent.chars().count() <= USAGE_REASON_MAX_CHARS + 8,
        "{} chars",
        absent.chars().count()
    );
}

/// A project's key is the producer's aggregation key, and for a provider that
/// keys by working directory it is that path with its separators dashed
/// (`-home-<user>-src-app`, `-Volumes-Work-<user>-src-app`). Every key frames
/// as its leaf segment, split on `/`, `\\` and `-`, so no root, user or
/// parent directory crosses whatever the root is (the #1260 review's call).
#[test]
fn every_project_key_frames_as_its_leaf_segment() {
    let home = dirs::home_dir().expect("a home");
    let dashed_home = home.to_string_lossy().replace('/', "-");
    let mut reply = ready();
    let keys = [
        format!("{dashed_home}-src-api"),
        "-Volumes-Work-alice-src-web".to_string(),
        "-Users-sample-user-work-cli".to_string(),
        format!("{}/src/tool", home.display()),
        r"C:\Users\bob\code\site".to_string(),
    ];
    reply.projects = keys
        .iter()
        .map(|key| FleetUsageProjectBucket {
            project: key.clone(),
            repo: None,
            bucket: bucket(1, None),
        })
        .collect();
    let mut state = AppState::new();
    state.apply_usage_read(reply, 1);
    let body = framed(&state);
    let names: Vec<&str> = body["summary"]["projects"]
        .as_array()
        .expect("projects")
        .iter()
        .map(|row| row["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(names, ["api", "web", "cli", "tool", "site"]);
    // Dashed, since the section's other rows name the `claude` provider.
    let dashed_user = format!("-{}-", home.file_name().expect("a user").to_string_lossy());
    let text = serde_json::to_string(&body).expect("encodes");
    for leaked in [
        dashed_home.as_str(),
        "Volumes",
        "alice",
        "sample",
        "bob",
        dashed_user.as_str(),
    ] {
        assert!(!text.contains(leaked), "`{leaked}` framed: {text}");
    }
}

/// Two keys that fold to one label are one row: their counts added, a cost
/// only when both were priced, a repo only when both named the same one, in
/// the place the first of them held.
#[test]
fn keys_that_fold_to_one_label_merge_into_one_row() {
    let mut reply = ready();
    let row =
        |key: &str, repo: Option<&str>, input: u64, cost: Option<f64>| FleetUsageProjectBucket {
            project: key.to_string(),
            repo: repo.map(str::to_string),
            bucket: bucket(input, cost),
        };
    reply.projects = vec![
        row("-home-a-src-app", Some("o/app"), 10, Some(1.0)),
        row("-home-b-lib", None, 5, Some(0.5)),
        row("-Volumes-x-app", Some("o/app"), 20, Some(2.0)),
        row("-home-c-lib", Some("o/lib"), 7, None),
    ];
    let mut state = AppState::new();
    state.apply_usage_read(reply, 1);
    let body = framed(&state);
    let projects = body["summary"]["projects"].as_array().expect("projects");
    assert_eq!(projects.len(), 2, "{body}");
    assert_eq!(projects[0]["name"], "app");
    assert_eq!(projects[0]["bucket"]["input_tokens"], 30);
    assert_eq!(projects[0]["bucket"]["call_count"], 12);
    assert_eq!(
        projects[0]["bucket"]["cost_usd"], 3.0,
        "both priced: the sum"
    );
    assert_eq!(projects[0]["repo"], "o/app", "the same repo: kept");
    assert_eq!(projects[1]["name"], "lib");
    assert_eq!(projects[1]["bucket"]["input_tokens"], 12);
    assert!(
        projects[1]["bucket"]["cost_usd"].is_null(),
        "one unpriced: unpriced"
    );
    assert!(projects[1]["repo"].is_null(), "a repo on one only: none");
    assert_eq!(body["summary"]["projects_cut"], 0, "a merge is not a cut");
}
