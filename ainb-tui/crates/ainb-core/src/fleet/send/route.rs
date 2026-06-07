// ABOUTME: Send-routing — transport-configurable; tmux-first by default, broker optional.

use anyhow::Result;

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

/// Try a tmux send-keys delivery. `None` if the session has no live tmux pane
/// or the keystrokes could not be written.
async fn try_tmux(session: &Session, text: &str) -> Option<SendOutcome> {
    let name = session.tmux_session.as_deref()?;
    if tmux_session_exists(name).await && tmux_send(name, text).await.is_ok() {
        return Some(SendOutcome::Tmux {
            tmux_session: name.to_string(),
        });
    }
    None
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
        Transport::TmuxFirst => match try_tmux(session, text).await {
            Some(o) => Some(o),
            None => try_broker(session, text).await,
        },
        Transport::TmuxOnly => try_tmux(session, text).await,
        Transport::PeersFirst => match try_broker(session, text).await {
            Some(o) => Some(o),
            None => try_tmux(session, text).await,
        },
    };

    Ok(outcome.unwrap_or_else(|| SendOutcome::Failed {
        reason: match mode {
            Transport::TmuxOnly => {
                "no live tmux session found (transport=tmux-only)".to_string()
            }
            _ => "no live tmux session and no reachable broker peer".to_string(),
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::{Transport, parse_transport};

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
