//! ABOUTME: Composer-classification tests for the verified tmux send path.
//!
//! FIXTURE PROVENANCE. Every `pane_fixtures/*.txt` is verbatim, unedited
//! `tmux capture-pane` output from a real Claude Code v2.1.224 session on
//! darwin 25.4.0. Nothing here is hand-written pane art except the four tiny
//! synthetic panes used for region-geometry cases, each labelled as such.
//!
//!   * `busy-queued`, `clean-submit`, `ghost-suggestion`,
//!     `human-typed-unsubmitted`, `idle-empty-composer`, `model-running`,
//!     `parked-placeholder`, `stranded-*`, `submitted-paste-transcript`:
//!     captured at 200x50.
//!   * `x80-*`: captured at 80x24, which is the geometry `ainb run` actually
//!     creates (`ainb-core/src/tmux/session.rs`), by parking payloads in a
//!     scratch session's composer with NO Enter (parking costs no API tokens).
//!
//! A `.ansi.txt` file is the SAME pane captured with `-e`, i.e. attributes
//! kept. It is not a duplicate: the DIM attribute is the only thing separating
//! a ghost suggestion from real typed text, and `-p` strips it.

use super::{
    Gate, GateAction, Verdict, apply_sgr, composer_pending, composer_region, composer_state,
    gate_action, is_rule_row, payload_needle, region_is_clear, region_shows_payload_tail,
    region_shows_placeholder, squeeze,
};

// ---- 200x50 captures -------------------------------------------------------

const IDLE_EMPTY: &str = include_str!("pane_fixtures/idle-empty-composer.txt");
const GHOST: &str = include_str!("pane_fixtures/ghost-suggestion.ansi.txt");
const GHOST_PLAIN: &str = include_str!("pane_fixtures/ghost-suggestion.txt");
const HUMAN_TYPED: &str = include_str!("pane_fixtures/human-typed-unsubmitted.ansi.txt");
const HUMAN_TYPED_PLAIN: &str = include_str!("pane_fixtures/human-typed-unsubmitted.txt");
const MODEL_RUNNING: &str = include_str!("pane_fixtures/model-running.txt");
const BUSY_QUEUED: &str = include_str!("pane_fixtures/busy-queued.ansi.txt");
const PARKED_PLACEHOLDER: &str = include_str!("pane_fixtures/parked-placeholder.ansi.txt");
const STRANDED_PLACEHOLDERS: &str =
    include_str!("pane_fixtures/stranded-placeholders-only.ansi.txt");
const STRANDED_MIXED_3400: &str = include_str!("pane_fixtures/stranded-mixed-tail-3400.txt");
const STRANDED_MIXED_2200: &str = include_str!("pane_fixtures/stranded-mixed-tail-2200.txt");
const CLEAN_SUBMIT: &str = include_str!("pane_fixtures/clean-submit.ansi.txt");
const SUBMITTED_PASTE: &str = include_str!("pane_fixtures/submitted-paste-transcript.txt");

// ---- 80x24 captures, the geometry `ainb run` creates -----------------------

const X80_EMPTY: &str = include_str!("pane_fixtures/x80-empty.ansi.txt");
const X80_PARKED_HEARTBEAT: &str = include_str!("pane_fixtures/x80-parked-heartbeat.ansi.txt");
const X80_PARKED_1546: &str = include_str!("pane_fixtures/x80-parked-1546.ansi.txt");
const X80_PARKED_TINY: &str = include_str!("pane_fixtures/x80-parked-tiny.ansi.txt");
const X80_HIDDEN_BY_PLACEHOLDER: &str =
    include_str!("pane_fixtures/x80-hidden-by-one-placeholder.txt");
const X80_MIXED_PLACEHOLDER: &str = include_str!("pane_fixtures/x80-parked-mixed-placeholder.txt");

/// The filler sentence every 200x50 fixture payload is built from.
const ACK_UNIT: &str = "test payload, reply with the single word ACK and do nothing else. ";
/// The filler for the 80x24 placeholder probe.
const PROBE_UNIT: &str = "probe payload alpha bravo charlie delta echo foxtrot golf. ";

/// Rebuild a fixture's payload: the unit sentence repeated and cut at
/// `bytes`, exactly as the capture harness produced it.
fn repeat_to(unit: &str, bytes: usize) -> String {
    let mut out = String::new();
    while out.len() < bytes {
        out.push_str(unit);
    }
    out.truncate(bytes);
    out
}

/// The heartbeat-shaped payload parked in `x80-parked-heartbeat`: a distinctive
/// `[HEARTBEAT` head, filler, and a distinctive tail marker.
fn heartbeat_payload() -> String {
    let head = "[HEARTBEAT 1782949818090] 16 session(s) need attention. ";
    let unit = "session needs a nudge, reply with the single word ACK. ";
    let mut body = head.to_string();
    while body.len() < 1532 {
        body.push_str(unit);
    }
    body.truncate(1532);
    body.push_str("TAILMARKER9911");
    body
}

// ---------------------------------------------------------------------------
// The classifier, table-driven over every real composer state we captured
// ---------------------------------------------------------------------------

struct Case {
    name: &'static str,
    pane: &'static str,
    /// The payload that was written into that pane, for the strict check.
    payload: String,
    /// What the post-send verification must conclude.
    strict: Verdict,
    /// What the payload-less ATC pre-check must conclude.
    conservative: bool,
    why: &'static str,
}

/// One entry per composer state we have a real capture of. Long by design:
/// the point is that the table enumerates every state, not that it is short.
#[allow(clippy::too_many_lines)]
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "idle, empty composer",
            pane: IDLE_EMPTY,
            payload: repeat_to(ACK_UNIT, 2000),
            strict: Verdict::Submitted,
            conservative: false,
            why: "the composer holds nothing at all",
        },
        Case {
            name: "ghost suggestion of OUR OWN payload",
            pane: GHOST,
            // The nastiest shape: Claude's dim ghost is the previous prompt,
            // i.e. byte-for-byte the payload we just submitted. A needle-only
            // detector reports PENDING for it forever.
            payload: "test payload, reply with the single word ACK and do nothing else".to_string(),
            strict: Verdict::Submitted,
            conservative: false,
            why: "dim ghost text is an EMPTY composer",
        },
        Case {
            name: "human typing, unsubmitted",
            pane: HUMAN_TYPED,
            payload: "run the standup and summarise".to_string(),
            strict: Verdict::Unverified,
            conservative: false,
            why: "someone else's live text: not our send, and never Enter-able",
        },
        Case {
            name: "model running, composer empty",
            pane: MODEL_RUNNING,
            payload: "test payload: run the bash command 'sleep 45'".to_string(),
            strict: Verdict::Submitted,
            conservative: false,
            why: "accepted and being worked on",
        },
        Case {
            name: "busy session, message QUEUED",
            pane: BUSY_QUEUED,
            payload: "queued message number two, reply ACK".to_string(),
            strict: Verdict::Submitted,
            conservative: false,
            why: "'Press up to edit queued messages' is dim and means ACCEPTED",
        },
        Case {
            name: "parked paste placeholder, 2000 bytes",
            pane: PARKED_PLACEHOLDER,
            payload: repeat_to(ACK_UNIT, 2000),
            strict: Verdict::Pending,
            conservative: true,
            why: "a placeholder cannot survive a submit, so it is proof of parking",
        },
        Case {
            name: "partial submit, placeholder-only remainder",
            pane: STRANDED_PLACEHOLDERS,
            payload: repeat_to(ACK_UNIT, 4000),
            strict: Verdict::Pending,
            conservative: true,
            why: "symptom B: head accepted, tail stranded behind placeholders",
        },
        Case {
            name: "partial submit, placeholders + raw tail (3400b)",
            pane: STRANDED_MIXED_3400,
            payload: repeat_to(ACK_UNIT, 3400),
            strict: Verdict::Pending,
            conservative: true,
            why: "the stranded remainder ends exactly at the payload's tail",
        },
        Case {
            name: "partial submit, placeholders + raw tail (2200b)",
            pane: STRANDED_MIXED_2200,
            payload: repeat_to(ACK_UNIT, 2200),
            strict: Verdict::Pending,
            conservative: true,
            why: "same class, smaller payload",
        },
        Case {
            name: "clean submit of a 4000-byte payload",
            pane: CLEAN_SUBMIT,
            payload: repeat_to(ACK_UNIT, 4000),
            strict: Verdict::Submitted,
            conservative: false,
            why: "composer empty, zero placeholders anywhere on the pane",
        },
        Case {
            name: "submitted paste, expanded into the transcript",
            pane: SUBMITTED_PASTE,
            payload: repeat_to(ACK_UNIT, 2000),
            strict: Verdict::Submitted,
            conservative: false,
            why: "a submitted paste leaves no placeholder behind",
        },
        Case {
            name: "80x24, empty composer",
            pane: X80_EMPTY,
            payload: heartbeat_payload(),
            strict: Verdict::Submitted,
            conservative: false,
            why: "production geometry, nothing parked",
        },
        Case {
            name: "80x24, parked heartbeat, head OFF SCREEN",
            pane: X80_PARKED_HEARTBEAT,
            payload: heartbeat_payload(),
            strict: Verdict::Pending,
            // The `[HEARTBEAT` marker scrolled out of the viewport, so the
            // payload-less guard genuinely cannot see it. Asserted, not hidden.
            conservative: false,
            why: "1546 raw bytes, no placeholder: only the TAIL needle can see it",
        },
        Case {
            name: "80x24, parked 1546-byte payload",
            pane: X80_PARKED_1546,
            payload: repeat_to(ACK_UNIT, 1546),
            strict: Verdict::Pending,
            conservative: false,
            why: "same geometry, plain filler payload",
        },
        Case {
            name: "80x24, parked ONE-character payload",
            pane: X80_PARKED_TINY,
            payload: "2".to_string(),
            strict: Verdict::Pending,
            conservative: false,
            why: "a phone reply of '2' must be verified like any other payload",
        },
        Case {
            name: "80x24, whole payload hidden behind one placeholder",
            pane: X80_HIDDEN_BY_PLACEHOLDER,
            payload: repeat_to(PROBE_UNIT, 900),
            strict: Verdict::Pending,
            conservative: true,
            why: "900 bytes rendered as a bare placeholder: no payload text at all",
        },
        Case {
            name: "80x24, placeholders + raw tail",
            pane: X80_MIXED_PLACEHOLDER,
            payload: repeat_to(ACK_UNIT, 1546),
            strict: Verdict::Pending,
            conservative: true,
            why: "mixed rendering still parks",
        },
    ]
}

#[test]
fn the_classifier_agrees_with_every_captured_composer_state() {
    let mut wrong = Vec::new();
    for case in cases() {
        let strict = composer_state(case.pane, Some(&case.payload));
        if strict != case.strict {
            wrong.push(format!(
                "STRICT  {}: want {:?}, got {:?} ({})",
                case.name, case.strict, strict, case.why
            ));
        }
        let conservative = composer_pending(case.pane, None);
        if conservative != case.conservative {
            wrong.push(format!(
                "PRECHK  {}: want {}, got {} ({})",
                case.name, case.conservative, conservative, case.why
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

#[test]
fn no_captured_state_is_ever_reported_as_submitted_while_parked() {
    // The P0 defect in one assertion: the only thing that must never happen is
    // a parked payload reported as delivered.
    for case in cases() {
        if case.strict == Verdict::Pending {
            assert_ne!(
                composer_state(case.pane, Some(&case.payload)),
                Verdict::Submitted,
                "{} would be reported as delivered while parked",
                case.name
            );
        }
    }
}

// ---------------------------------------------------------------------------
// CRITICAL-1: the head needle is invisible at production geometry
// ---------------------------------------------------------------------------

#[test]
fn at_80x24_the_payload_head_is_off_screen_and_the_tail_is_not() {
    // Captured live: one `send-keys -l` of 1546 bytes into a real 80x24 claude
    // pane, no Enter. The composer viewport is TAIL-anchored, so the head (and
    // with it the `[HEARTBEAT` marker the ATC guard used to look for) scrolls
    // out of view, and no placeholder is drawn at this size either.
    let payload = heartbeat_payload();
    assert!(
        !X80_PARKED_HEARTBEAT.contains("[Pasted text"),
        "precondition: this capture has no placeholder to fall back on"
    );
    assert!(
        !X80_PARKED_HEARTBEAT.contains("[HEARTBEAT"),
        "precondition: the payload HEAD is not on screen anywhere"
    );

    let region = composer_region(X80_PARKED_HEARTBEAT).expect("a claude composer");
    let head: String = squeeze(&payload).chars().take(32).collect();
    assert!(
        !super::region_squeezed(&region).contains(&head),
        "the round-1 HEAD needle is absent, which is why it reported Submitted"
    );
    assert!(
        region_shows_payload_tail(&region, &payload),
        "the TAIL needle must still find the parked payload"
    );
    assert_eq!(
        composer_state(X80_PARKED_HEARTBEAT, Some(&payload)),
        Verdict::Pending
    );
}

#[test]
fn the_needle_is_taken_from_the_tail() {
    let payload = "HEAD-MARKER filler filler filler filler TAIL-MARKER";
    let needle = payload_needle(payload).expect("a payload with content has a needle");
    assert!(needle.ends_with("TAIL-MARKER"), "got {needle}");
    assert!(!needle.contains("HEAD-MARKER"), "got {needle}");
}

#[test]
fn a_short_payload_still_gets_a_needle() {
    // HIGH-4: round 1 required 4 non-whitespace characters and returned
    // "submitted, unverified" below that. `outbound.rs` formats attention
    // pushes as numbered options and `relay.rs` forwards the reply verbatim, so
    // a phone answer of "2" took that path and blocked the session silently.
    assert_eq!(payload_needle("2").as_deref(), Some("2"));
    assert_eq!(payload_needle("y").as_deref(), Some("y"));
    assert_eq!(payload_needle(" ok ").as_deref(), Some("ok"));
    // Only a payload with nothing printable in it has no needle, and that one
    // cannot be recognised on a screen by construction.
    assert_eq!(payload_needle("   \n\t").as_deref(), None);
}

#[test]
fn a_one_character_payload_is_verified_against_the_pane() {
    // Real 80x24 capture of `2` parked in the composer, no Enter.
    assert_eq!(
        composer_state(X80_PARKED_TINY, Some("2")),
        Verdict::Pending,
        "a one-character payload must not skip verification"
    );
    assert_eq!(
        composer_state(X80_EMPTY, Some("2")),
        Verdict::Submitted,
        "and must be cleared by an empty composer"
    );
}

// ---------------------------------------------------------------------------
// CRITICAL-2 / rule 5: an unreadable pane is never proof of delivery
// ---------------------------------------------------------------------------

#[test]
fn a_pane_with_no_composer_is_unverified_not_submitted() {
    // Round 1 fell back to a last-prompt-line scan that dropped the payload and
    // could only match markers, then turned its `false` into `Submitted`.
    let bare = "plain shell output\n$ ls\nfile";
    assert_eq!(composer_region(bare), None);
    assert_eq!(composer_state(bare, Some("continue")), Verdict::Unverified);
    assert_eq!(
        composer_state("", Some("continue")),
        Verdict::Unverified,
        "an empty capture is the shape a dead pane produces"
    );
    // The conservative pre-check keeps its historical marker fallback: it is
    // idempotent, and reporting `true` for an unreadable pane would skip every
    // ATC tick forever.
    assert!(composer_pending(
        "transcript\n> [Pasted text #8 +5 lines]rest\nstatus line",
        None
    ));
    assert!(!composer_pending(bare, None));
}

#[test]
fn a_composer_holding_someone_elses_text_is_unverified() {
    // Real capture: `fix the login bug` typed by a human, never submitted. Our
    // payload is not there, but neither is proof that it went through, and
    // another Enter would submit THEIR text.
    assert_eq!(
        composer_state(HUMAN_TYPED, Some("run the standup and summarise")),
        Verdict::Unverified
    );
    assert!(
        !composer_pending(HUMAN_TYPED, None),
        "a human mid-keystroke is not a parked nudge"
    );
}

// ---------------------------------------------------------------------------
// Rule 4: emptiness is the primary signal, and it is DIM-aware
// ---------------------------------------------------------------------------

#[test]
fn dim_is_the_only_thing_separating_a_ghost_from_typed_text() {
    // The two plain captures are the same shape; `-p` strips the attribute that
    // tells them apart, which is why the send path captures with `-e`.
    let ghost = composer_region(GHOST_PLAIN).expect("composer");
    let typed = composer_region(HUMAN_TYPED_PLAIN).expect("composer");
    assert!(
        !region_is_clear(&ghost),
        "without ANSI, a ghost reads as text"
    );
    assert!(!region_is_clear(&typed));

    let ghost_ansi = composer_region(GHOST).expect("composer");
    let typed_ansi = composer_region(HUMAN_TYPED).expect("composer");
    assert!(
        region_is_clear(&ghost_ansi),
        "an entirely dim composer is EMPTY"
    );
    assert!(
        !region_is_clear(&typed_ansi),
        "real typed text is not dim and must stay pending-capable"
    );
}

#[test]
fn a_busy_session_that_queued_our_message_counts_as_submitted() {
    // Treating `Press up to edit queued messages` as pending makes every caller
    // skip a busy session forever.
    let region = composer_region(BUSY_QUEUED).expect("composer");
    assert!(region.contains("Press up to edit queued messages"));
    assert!(region_is_clear(&region), "the queued hint is dim");
}

#[test]
fn sgr_colour_codes_do_not_cancel_dim() {
    // The real busy-queued capture is `ESC[2m ESC[39m Press up ...`: a naive
    // "any SGR resets" parser loses the dim and the session is skipped forever.
    let mut dim = false;
    apply_sgr("2", &mut dim);
    assert!(dim);
    apply_sgr("39", &mut dim);
    assert!(dim, "a foreground colour must not clear DIM");
    apply_sgr("0", &mut dim);
    assert!(!dim, "reset clears it");
    apply_sgr("2;39", &mut dim);
    assert!(dim);
    apply_sgr("22", &mut dim);
    assert!(!dim, "normal intensity clears it");
    apply_sgr("", &mut dim);
    assert!(!dim, "a bare ESC[m is a reset");
}

// ---------------------------------------------------------------------------
// MED-7: the region must be the COMPOSER, not whatever the last rule pair is
// ---------------------------------------------------------------------------

#[test]
fn a_rule_pair_below_the_composer_does_not_steal_the_region() {
    // Synthetic pane (labelled): Claude draws a slash-command menu under the
    // composer, so the LAST two rules bracket the menu, where the payload can
    // never be. Taking them unconditionally reports Submitted for a parked
    // payload.
    let pane = "transcript\n\
                ────────────────────────────────────────────\n\
                ❯\u{a0}git pull --rebase && cargo test -p ainb-\n\
                  fleet-core\n\
                ────────────────────────────────────────────\n\
                  /clear    clear the conversation\n\
                  /compact  compact the context\n\
                ────────────────────────────────────────────\n\
                status line";
    let region = composer_region(pane).expect("the composer, not the menu");
    assert!(region.starts_with('❯'), "got {region:?}");
    assert_eq!(
        composer_state(
            pane,
            Some("git pull --rebase && cargo test -p ainb-fleet-core")
        ),
        Verdict::Pending
    );
}

#[test]
fn the_region_is_the_whole_wrapped_block_not_one_prompt_line() {
    // Synthetic pane (labelled): only the FIRST composer row carries the glyph,
    // so a single-line scan misses everything that wrapped.
    let pane = "transcript\n\
                ────────────────────────────────────────────\n\
                ❯\u{a0}first row of the payload\n\
                  continuation row holding TAILMARK\n\
                ────────────────────────────────────────────\n\
                status";
    let region = composer_region(pane).expect("composer");
    assert!(region_shows_payload_tail(
        &region,
        "first row of the payload continuation row holding TAILMARK"
    ));
}

#[test]
fn rule_rows_are_full_width_horizontal_rules_only() {
    assert!(is_rule_row(&"─".repeat(20)));
    assert!(is_rule_row(&format!("  {}  ", "─".repeat(40))));
    assert!(!is_rule_row(&"─".repeat(19)));
    assert!(!is_rule_row("──── menu ────"));
    assert!(!is_rule_row("plain text"));
}

// ---------------------------------------------------------------------------
// Placeholder handling
// ---------------------------------------------------------------------------

#[test]
fn a_placeholder_is_positive_evidence_even_with_no_payload_text_visible() {
    // Measured live at 80x24: a 900-byte write rendered as a bare
    // `[Pasted text #3]` and NOTHING of the payload was on screen, so the tail
    // needle can never match and the placeholder is the only signal left.
    let payload = repeat_to(PROBE_UNIT, 900);
    let region = composer_region(X80_HIDDEN_BY_PLACEHOLDER).expect("composer");
    assert!(!region_shows_payload_tail(&region, &payload));
    assert!(region_shows_placeholder(&region));
    assert_eq!(
        composer_state(X80_HIDDEN_BY_PLACEHOLDER, Some(&payload)),
        Verdict::Pending
    );
}

#[test]
fn a_placeholder_that_wrapped_across_rows_still_matches() {
    // Synthetic pane (labelled): at 80 columns a placeholder run wraps
    // mid-token, so the match has to run over whitespace-squeezed text.
    let pane = "transcript\n\
                ────────────────────────────────────────────\n\
                ❯\u{a0}[Pasted text #12][Pasted te\n\
                  xt #13]\n\
                ────────────────────────────────────────────\n\
                status";
    let region = composer_region(pane).expect("composer");
    assert!(region_shows_placeholder(&region));
    assert!(composer_pending(pane, None), "the ATC guard must see it");
}

#[test]
fn the_heartbeat_marker_is_pending_without_a_payload() {
    // Synthetic pane (labelled): the shape the ATC coalesce guard is for, at a
    // width where the marker is still on screen.
    let pane = "transcript\n\
                ──────────────────────────────────────────────\n\
                ❯\u{a0}[HEARTBEAT 1782949818090] 16 session(s)\n\
                ──────────────────────────────────────────────\n\
                status";
    assert!(composer_pending(pane, None));
}

// ---------------------------------------------------------------------------
// The gate contract: a composer-less pane is accepted, an unobserved composer
// is refused
// ---------------------------------------------------------------------------

#[test]
fn a_composerless_pane_is_delivered_while_an_unobserved_composer_is_refused() {
    // BOTH halves, in one assertion block, because the pair is the contract.
    //
    // `NoComposer` accepts: the pane has no composer, so the wire behaviour is
    // one write plus one Enter, byte for byte what `main` did. Failing it made
    // every hangar `fleet/message/send` to an unparseable pane report FAILED,
    // and left the ATC inbox undrained so completions were re-sent on every
    // heartbeat, forever.
    assert_eq!(
        gate_action(Gate::NoComposer),
        GateAction::PressAndAccept,
        "a pane with no composer must be reported as delivered, not failed"
    );
    // `NotObserved` refuses: a composer EXISTS and never showed the payload, so
    // the pane is still draining and an Enter would be swallowed into the paste
    // buffer. This is the CRITICAL-1 guarantee and it must not soften.
    assert_eq!(
        gate_action(Gate::NotObserved),
        GateAction::Refuse,
        "an unobserved composer is the CR-fusion condition: Enter must NOT be pressed"
    );
    assert_eq!(
        gate_action(Gate::PayloadVisible),
        GateAction::PressAndVerify
    );
}

// ---------------------------------------------------------------------------
// argv shape
// ---------------------------------------------------------------------------

#[test]
fn dash_prefixed_payload_sits_after_the_terminator() {
    // A `-`-prefixed option label must land AFTER `--` so tmux treats it as
    // literal keys, not a flag. (Regression: without the terminator `-y` was
    // parsed as an unknown tmux flag and the send silently failed.)
    let args = super::send_keys_literal_args("sess", "-y");
    assert_eq!(args, ["send-keys", "-t", "sess", "-l", "--", "-y"]);
    let term = args.iter().position(|a| *a == "--").expect("`--` present");
    let payload = args.iter().position(|a| *a == "-y").expect("payload present");
    assert!(term < payload, "payload must follow the `--` terminator");
}

#[test]
fn a_payload_goes_out_as_exactly_one_literal_write() {
    // Round 1 split the payload into sub-801-byte chunks to dodge the paste
    // classifier, which by design removed the `[Pasted text #N]` placeholder
    // the ATC coalesce guard reads, leaving a parked heartbeat invisible to it.
    // The argv builder takes the WHOLE text, and `tmux_send` calls it once.
    let long = repeat_to(ACK_UNIT, 8000);
    let args = super::send_keys_literal_args("sess", &long);
    assert_eq!(args[5].len(), 8000, "the payload must not be split");
}

#[test]
fn picker_keys_use_non_literal_constrained_routing() {
    assert_eq!(
        super::picker_key_args("sess:0.0", "1"),
        Some(["send-keys", "-t", "sess:0.0", "1"])
    );
    assert_eq!(
        super::picker_key_args("sess:0.0", "Enter"),
        Some(["send-keys", "-t", "sess:0.0", "Enter"])
    );
    assert_eq!(super::picker_key_args("sess:0.0", "option label"), None);
    assert_eq!(super::picker_key_args("sess:0.0", "C-c"), None);
}
