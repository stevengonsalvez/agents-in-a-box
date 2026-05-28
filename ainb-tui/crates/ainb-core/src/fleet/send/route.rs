// ABOUTME: Send-routing — peers-first, tmux fallback.

use anyhow::Result;

use crate::fleet::send::{broker_health, broker_send, tmux_send, tmux_session_exists};
use crate::fleet::types::{SendOutcome, Session};

const POPA_PEER_ID_ENV: &str = "AINB_FLEET_PEER_ID";
const POPA_PEER_ID_DEFAULT: &str = "ainb-fleet-cp";

fn from_peer_id() -> String {
    std::env::var(POPA_PEER_ID_ENV).unwrap_or_else(|_| POPA_PEER_ID_DEFAULT.to_string())
}

pub async fn send(session: &Session, text: &str) -> Result<SendOutcome> {
    if let Some(peer_id) = session.peer_id.as_deref() {
        if broker_health().await {
            match broker_send(&from_peer_id(), peer_id, text).await {
                Ok(res) if res.ok => {
                    return Ok(SendOutcome::Broker {
                        peer_id: peer_id.to_string(),
                    });
                }
                Ok(_) | Err(_) => { /* fall through to tmux fallback */ }
            }
        }
    }

    if let Some(name) = session.tmux_session.as_deref() {
        if tmux_session_exists(name).await {
            tmux_send(name, text).await?;
            return Ok(SendOutcome::Tmux {
                tmux_session: name.to_string(),
            });
        }
    }

    Ok(SendOutcome::Failed {
        reason: "no peer registered and no tmux session found".to_string(),
    })
}
