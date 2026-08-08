// ABOUTME: Send-routing, transport-configurable; tmux-first by default, broker optional.

use anyhow::Result;

use crate::fleet::send::tmux::{SendError, SendFailure};
use crate::fleet::send::{broker_health, broker_send, tmux_send, tmux_session_exists};
use crate::fleet::types::{SendOutcome, Session};

const PEER_ID_ENV: &str = "AINB_FLEET_PEER_ID";
const PEER_ID_DEFAULT: &str = "ainb-fleet-cp";
const TRANSPORT_ENV: &str = "AINB_FLEET_TRANSPORT";

/// Which channel to prefer when sending to a session.
///
/// The claude-peers broker has proven flaky, so tmux send-keys is the default
/// primary path; the broker is opt-in or a fallback. Selected via
/// `AINB_FLEET_TRANSPORT`:
///
/// | value | behaviour |
/// |---|---|
/// | unset / `tmux` / `tmux-first` | tmux send-keys first, broker fallback (default) |
/// | `tmux-only` | tmux send-keys only, never touch the broker |
/// | `peers` / `broker` / `peers-first` | legacy: broker first, tmux fallback |
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Transport {
    TmuxFirst,
    TmuxOnly,
    PeersFirst,
}

fn parse_transport(raw: Option<&str>) -> Transport {
    match raw.map(str::trim) {
        Some("peers" | "broker" | "peers-first") => Transport::PeersFirst,
        Some("tmux-only") => Transport::TmuxOnly,
        // default (unset, "tmux", "tmux-first", or anything unrecognised): tmux-first
        _ => Transport::TmuxFirst,
    }
}

fn transport() -> Transport {
    parse_transport(std::env::var(TRANSPORT_ENV).ok().as_deref())
}

fn from_peer_id() -> String {
    std::env::var(PEER_ID_ENV).unwrap_or_else(|_| PEER_ID_DEFAULT.to_string())
}

/// Outcome of a tmux delivery attempt.
///
/// `Delivered` and `NotSubmitted` are distinct on purpose. Collapsing them (the
/// old `.is_ok()`) told the caller "no live tmux session found" for a session
/// that demonstrably existed and into whose composer the text had already been
/// typed. That is factually wrong, and it also invited the broker fallback to deliver
/// the SAME text a second time.
enum TmuxAttempt {
    Delivered(SendOutcome),
    /// The text reached the pane but the turn was not submitted (or could not
    /// be verified). Nothing else may be sent: the payload is already parked.
    NotSubmitted(String),
    /// The send-keys call itself failed, so NOT ONE BYTE reached the pane (a
    /// pane that died between the liveness check and the first write). Another
    /// transport must be allowed to try, or that message is delivered by no
    /// transport at all.
    WriteFailed(String),
    /// Nothing was written: no tmux session on the record, or it is not live.
    NoSession,
}

/// `None` means "nothing was written and there is no reason worth reporting,
/// another transport may try".
fn attempt_outcome(attempt: TmuxAttempt) -> Option<SendOutcome> {
    match attempt {
        TmuxAttempt::Delivered(outcome) => Some(outcome),
        // Surfacing the real submit error keeps "typed but not submitted"
        // distinguishable from "no session" at the `SendOutcome` level, and
        // stops the broker from re-delivering a payload that is already parked
        // in the composer.
        TmuxAttempt::NotSubmitted(reason) | TmuxAttempt::WriteFailed(reason) => {
            Some(SendOutcome::Failed { reason })
        }
        TmuxAttempt::NoSession => None,
    }
}

/// May a second transport deliver the same text after this attempt?
///
/// Only when tmux is PROVEN to have written nothing. An unknown error shape
/// fails closed into "written": a duplicate delivery of a nudge is noise, while
/// re-sending a payload that is already sitting in the composer means the
/// session receives it twice as soon as that composer flushes.
const fn allows_fallback(attempt: &TmuxAttempt) -> bool {
    matches!(
        attempt,
        TmuxAttempt::NoSession | TmuxAttempt::WriteFailed(_)
    )
}

async fn try_tmux(session: &Session, text: &str) -> TmuxAttempt {
    let Some(name) = session.tmux_session.as_deref() else {
        return TmuxAttempt::NoSession;
    };
    if !tmux_session_exists(name).await {
        return TmuxAttempt::NoSession;
    }
    match tmux_send(name, text).await {
        Ok(()) => TmuxAttempt::Delivered(SendOutcome::Tmux {
            tmux_session: name.to_string(),
        }),
        // The distinction is read off a TYPED error, never off the wording of
        // the message: a string match here would silently stop working the
        // first time someone reworded a `bail!`.
        Err(error) => {
            let reason = format!("{error:#}");
            match error.downcast_ref::<SendError>().map(SendError::failure) {
                Some(SendFailure::NothingWritten) => TmuxAttempt::WriteFailed(reason),
                Some(SendFailure::Written) | None => TmuxAttempt::NotSubmitted(reason),
            }
        }
    }
}

/// Try a broker (claude-peers) delivery. `None` if the session has no peer id,
/// the broker is unhealthy, or the broker rejected the message.
async fn try_broker(session: &Session, text: &str) -> Option<SendOutcome> {
    let peer_id = session.peer_id.as_deref()?;
    if broker_health().await {
        if let Ok(res) = broker_send(&from_peer_id(), peer_id, text).await {
            if res.ok {
                return Some(SendOutcome::Broker {
                    peer_id: peer_id.to_string(),
                });
            }
        }
    }
    None
}

pub async fn send(session: &Session, text: &str) -> Result<SendOutcome> {
    let mode = transport();
    let outcome = match mode {
        // On `NotSubmitted` the broker is deliberately NOT tried: the text is
        // already parked in the composer, so a second channel would deliver it
        // twice as soon as the composer flushes. A write that never happened
        // does fall through, and keeps its own reason if the broker also fails.
        Transport::TmuxFirst => {
            let attempt = try_tmux(session, text).await;
            if allows_fallback(&attempt) {
                try_broker(session, text).await.or_else(|| attempt_outcome(attempt))
            } else {
                attempt_outcome(attempt)
            }
        }
        Transport::TmuxOnly => attempt_outcome(try_tmux(session, text).await),
        Transport::PeersFirst => match try_broker(session, text).await {
            Some(outcome) => Some(outcome),
            None => attempt_outcome(try_tmux(session, text).await),
        },
    };

    Ok(outcome.unwrap_or_else(|| SendOutcome::Failed {
        reason: match mode {
            Transport::TmuxOnly => "no live tmux session found (transport=tmux-only)".to_string(),
            _ => "no live tmux session and no reachable broker peer".to_string(),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        SendOutcome, TmuxAttempt, Transport, allows_fallback, attempt_outcome, parse_transport,
    };

    #[test]
    fn an_unsubmitted_send_reports_the_real_reason_not_a_missing_session() {
        // Regression: `tmux_send(...).is_ok()` collapsed a failed submit into
        // `None`, which `send()` then rendered as "no live tmux session and no
        // reachable broker peer", a session that existed, into whose composer
        // the payload had already been typed.
        let reason = "send to sess was typed into the composer but NOT submitted";
        let outcome = attempt_outcome(TmuxAttempt::NotSubmitted(reason.to_string()));
        match outcome {
            Some(SendOutcome::Failed { reason: got }) => {
                assert_eq!(got, reason);
                assert!(
                    !got.contains("no live tmux session"),
                    "must not claim the session was missing"
                );
            }
            other => panic!("expected a Failed outcome carrying the submit error, got {other:?}"),
        }
    }

    #[test]
    fn only_a_parked_payload_blocks_the_broker_fallback() {
        // Round 1 mapped EVERY tmux error to `NotSubmitted` and suppressed the
        // broker for all of them, including the case where `send-keys` itself
        // failed and not one byte was written. A pane that dies between the
        // liveness check and the first write was then delivered by no transport
        // at all, where before the fix the broker covered it.
        assert!(
            !allows_fallback(&TmuxAttempt::NotSubmitted("parked".to_string())),
            "a payload sitting in the composer must not be sent a second way"
        );
        assert!(
            allows_fallback(&TmuxAttempt::WriteFailed("send-keys exited 1".to_string())),
            "nothing was written, so the broker MUST still be allowed to try"
        );
        assert!(allows_fallback(&TmuxAttempt::NoSession));
        assert!(!allows_fallback(&TmuxAttempt::Delivered(
            SendOutcome::Tmux {
                tmux_session: "sess".to_string(),
            }
        )));
    }

    #[test]
    fn a_failed_write_still_reports_its_own_reason() {
        // It falls through to the broker, but if the broker is also unavailable
        // the caller must see the write error, not the generic "no live tmux
        // session and no reachable broker peer".
        let outcome = attempt_outcome(TmuxAttempt::WriteFailed("send-keys exited 1".to_string()));
        match outcome {
            Some(SendOutcome::Failed { reason }) => {
                assert!(reason.contains("send-keys exited 1"), "got: {reason}");
            }
            other => panic!("expected the write error to survive, got {other:?}"),
        }
        // Only "no session at all" is silent enough to be replaced by the
        // router's own summary.
        assert!(attempt_outcome(TmuxAttempt::NoSession).is_none());
    }

    #[test]
    fn a_delivered_send_passes_through() {
        let outcome = attempt_outcome(TmuxAttempt::Delivered(SendOutcome::Tmux {
            tmux_session: "sess".to_string(),
        }));
        assert!(matches!(
            outcome,
            Some(SendOutcome::Tmux { tmux_session }) if tmux_session == "sess"
        ));
    }

    #[test]
    fn defaults_to_tmux_first() {
        assert_eq!(parse_transport(None), Transport::TmuxFirst);
        assert_eq!(parse_transport(Some("tmux")), Transport::TmuxFirst);
        assert_eq!(parse_transport(Some("tmux-first")), Transport::TmuxFirst);
        assert_eq!(parse_transport(Some(" tmux ")), Transport::TmuxFirst);
        // unrecognised values fail safe to the tmux-first default
        assert_eq!(parse_transport(Some("garbage")), Transport::TmuxFirst);
        assert_eq!(parse_transport(Some("")), Transport::TmuxFirst);
    }

    #[test]
    fn tmux_only_opts_out_of_broker() {
        assert_eq!(parse_transport(Some("tmux-only")), Transport::TmuxOnly);
    }

    #[test]
    fn peers_values_select_legacy_broker_first() {
        assert_eq!(parse_transport(Some("peers")), Transport::PeersFirst);
        assert_eq!(parse_transport(Some("broker")), Transport::PeersFirst);
        assert_eq!(parse_transport(Some("peers-first")), Transport::PeersFirst);
        assert_eq!(parse_transport(Some(" peers ")), Transport::PeersFirst);
    }
}
