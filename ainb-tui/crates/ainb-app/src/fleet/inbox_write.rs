//! The inbox's one write, `hangar/inbox_mark_read`, as the D18 mutation it is
//! (D3-prime).
//!
//! The verb is a whole-inbox sweep (`InboxRepo::mark_all_read`); there is no
//! per-entry verb, so every surface calls this "mark all read". Its params
//! type already embeds the mutation envelope and the daemon's ledger guard is
//! generic at dispatch, so what this module adds is the client half: an op id
//! minted ONCE per effect from this process's CSPRNG, sent again unchanged on
//! a retry, so a lost reply is answered from the ledger (`replayed`) rather
//! than by running the sweep a second time. The registry fences this method
//! on nothing (`Fk::None`), and this module does not invent a fence.
//!
//! ```text
//!  effect ──mint op id──▶ send ──lost reply──▶ send SAME op id ──▶ replayed
//! ```

use ainb_hangar_client::{DaemonClient, DaemonError};
use ainb_hangar_proto::methods;
use ainb_hangar_proto::mutation::{MutationEnvelope, OpId};
use ainb_hangar_proto::snapshots::{InboxMarkReadResult, InboxScopedParams};
use serde::{Deserialize, Serialize};

use crate::app::sections::{INBOX_RECIPIENT, INBOX_WORKSPACE_ID};

/// How many times one op id is sent before the effect gives up: the first
/// send and one retry after a transport failure.
pub const SENDS: usize = 2;

/// One "mark all read" operation: its op id, minted once.
#[derive(Debug, Clone)]
pub struct MarkAllRead {
    op_id: OpId,
}

impl MarkAllRead {
    /// Mint a fresh op id from the process CSPRNG. Every send of this value
    /// carries the same id.
    #[must_use]
    pub fn mint() -> Self {
        Self {
            op_id: OpId::from_bytes(rand::random::<[u8; 16]>()),
        }
    }

    /// The id this operation is deduplicated by.
    #[must_use]
    pub const fn op_id(&self) -> &OpId {
        &self.op_id
    }

    /// The params every send carries: the daemon's default workspace, the
    /// local human, and this operation's op id. Identical on every call, which
    /// is what makes a retry a replay and not a second sweep.
    #[must_use]
    pub fn params(&self) -> InboxScopedParams {
        InboxScopedParams {
            workspace_id: INBOX_WORKSPACE_ID.to_string(),
            recipient: Some(INBOX_RECIPIENT.to_string()),
            mutation: MutationEnvelope::with_op_id(self.op_id.clone()),
        }
    }

    /// Send once over `client`.
    pub async fn send(&self, client: &DaemonClient) -> Result<InboxMarkReadResult, DaemonError> {
        client
            .call_typed::<InboxScopedParams, InboxMarkReadResult>(
                methods::HANGAR_INBOX_MARK_READ,
                &self.params(),
            )
            .await
    }

    /// Send through `send`, retrying once with the SAME params after a
    /// transport failure; a daemon refusal is final. What the sweep did, or
    /// why it did not, as the report the reducer folds.
    pub fn deliver(
        &self,
        mut send: impl FnMut(&InboxScopedParams) -> Result<InboxMarkReadResult, DaemonError>,
    ) -> MarkAllReadOutcome {
        let params = self.params();
        let mut last = None;
        for _ in 0..SENDS {
            match send(&params) {
                Ok(result) => {
                    return MarkAllReadOutcome {
                        op_id: self.op_id.as_str().to_string(),
                        ok: true,
                        marked: result.marked,
                        unread: result.unread,
                        error: None,
                    };
                }
                Err(error @ DaemonError::Rpc { .. }) => {
                    last = Some(error);
                    break;
                }
                Err(error) => last = Some(error),
            }
        }
        MarkAllReadOutcome {
            op_id: self.op_id.as_str().to_string(),
            ok: false,
            marked: 0,
            unread: 0,
            error: Some(last.map_or_else(|| "not sent".to_string(), |e| e.to_string())),
        }
    }
}

/// Mint an operation, dial through `dialer`, and deliver it on the current
/// thread, blocking. For a host's worker thread.
pub fn mark_all_read_blocking(
    dialer: impl Fn() -> Result<DaemonClient, DaemonError>,
) -> MarkAllReadOutcome {
    let op = MarkAllRead::mint();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
    let Ok(runtime) = runtime else {
        return MarkAllReadOutcome {
            op_id: op.op_id.as_str().to_string(),
            ok: false,
            marked: 0,
            unread: 0,
            error: Some("the worker runtime did not start".into()),
        };
    };
    op.deliver(|params| {
        let client = dialer()?;
        runtime.block_on(client.call_typed::<InboxScopedParams, InboxMarkReadResult>(
            methods::HANGAR_INBOX_MARK_READ,
            params,
        ))
    })
}

/// How a "mark all read" ended, reported to the reducer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkAllReadOutcome {
    /// The op id every send carried, for the ledger row it left.
    pub op_id: String,
    /// The daemon answered the sweep.
    pub ok: bool,
    /// Rows the sweep flipped (zero on a replay, or when nothing was unread).
    pub marked: i64,
    /// The unread count after the sweep.
    pub unread: i64,
    /// Why it did not land, when it did not.
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_op_id_is_32_hex_characters_and_differs_per_mint() {
        let a = MarkAllRead::mint();
        let b = MarkAllRead::mint();
        assert_eq!(a.op_id().as_str().len(), 32);
        assert!(a.op_id().as_str().chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a.op_id(), b.op_id());
    }

    #[test]
    fn every_send_carries_the_same_params() {
        let op = MarkAllRead::mint();
        let first = serde_json::to_value(op.params()).unwrap();
        let second = serde_json::to_value(op.params()).unwrap();
        assert_eq!(first, second);
        assert_eq!(first["op_id"], op.op_id().as_str());
        assert_eq!(first["workspace_id"], INBOX_WORKSPACE_ID);
        assert_eq!(first["recipient"], INBOX_RECIPIENT);
        assert!(first.get("fence").is_none(), "the registry fences this on nothing");
    }

    #[test]
    fn a_transport_failure_is_retried_once_with_the_same_op_id() {
        let op = MarkAllRead::mint();
        let mut sent = Vec::new();
        let outcome = op.deliver(|params| {
            sent.push(params.mutation.op_id.clone());
            if sent.len() == 1 {
                Err(DaemonError::Io("broken pipe".into()))
            } else {
                Ok(InboxMarkReadResult {
                    marked: 3,
                    unread: 0,
                })
            }
        });
        assert_eq!(sent.len(), SENDS);
        assert_eq!(sent[0], sent[1]);
        assert_eq!(sent[0].as_ref(), Some(op.op_id()));
        assert!(outcome.ok);
        assert_eq!((outcome.marked, outcome.unread), (3, 0));
    }

    #[test]
    fn a_daemon_refusal_is_final() {
        let op = MarkAllRead::mint();
        let mut sends = 0;
        let outcome = op.deliver(|_| {
            sends += 1;
            Err(DaemonError::Rpc {
                code: -32602,
                message: "invalid params".into(),
            })
        });
        assert_eq!(sends, 1, "a refusal is not retried");
        assert!(!outcome.ok);
        assert!(outcome.error.as_deref().unwrap().contains("invalid params"));
    }

    #[test]
    fn two_transport_failures_give_up_with_the_reason() {
        let op = MarkAllRead::mint();
        let outcome = op.deliver(|_| Err(DaemonError::Io("gone".into())));
        assert!(!outcome.ok);
        assert!(outcome.error.as_deref().unwrap().contains("gone"));
        assert_eq!(outcome.op_id, op.op_id().as_str());
    }
}
