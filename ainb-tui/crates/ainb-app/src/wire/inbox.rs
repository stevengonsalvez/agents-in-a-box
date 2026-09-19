// ABOUTME: The inbox section on the wire (D3-prime): the daemon's notification
// rows as the fold bounded them, with the free-text field scrubbed again on
// the frame, and the cut counters that say what the fold dropped. Owned types
// deriving both `Serialize` and `specta::Type`, so the TypeScript the window
// reads is generated from the same shape the frame writes.

use serde::Serialize;

use crate::app::sections::{InboxSection, MAX_INBOX_BYTES};
use crate::fleet::bridge::redact::scrub;

/// Section 16 (inbox) on the wire.
///
/// Every field is either an id, an enum token, a number, or text the fold
/// scrubbed before it cut. `summary` is scrubbed again here, as the ACP
/// transcript's chunks are, because the frame is the boundary and the fold is
/// not. `absent` and `unreachable` are the host's own reasons, which can carry
/// a socket path or a daemon error, so they are scrubbed too.
///
/// The byte budget is enforced here, not only claimed: rows are added in the
/// daemon's order while their encoded bytes fit under [`MAX_INBOX_BYTES`]
/// less the reserve the scalar fields need, and the rest are counted in
/// `rows_cut`. A section past `MAX_FRAME_BYTES` is withheld whole, and a
/// withheld inbox has no counter to say why.
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[derive(Debug, Serialize)]
pub struct InboxView {
    /// The rows a surface draws, newest first, at most `MAX_INBOX_ROWS`.
    pub entries: Vec<InboxRowFrame>,
    /// The daemon's unread count for `recipient`.
    pub unread: i64,
    /// The actor whose inbox this is.
    pub recipient: String,
    /// Why there are no rows, when the host knows.
    pub absent: Option<String>,
    /// The last read failed for this reason; the rows are the last that landed.
    pub unreachable: Option<String>,
    /// Rows the daemon sent that the fold did not keep.
    pub rows_cut: usize,
    /// Summaries the fold cut to `MAX_INBOX_SUMMARY_CHARS`.
    pub summaries_cut: usize,
    /// The local clock when the last read landed, epoch milliseconds.
    pub received_at_ms: i64,
}

/// One inbox row on the wire: `InboxEntryRow` as the fold bounded it.
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[derive(Debug, Serialize)]
pub struct InboxRowFrame {
    /// The entry id (a ULID), the stable id a surface keys the row by.
    pub id: String,
    /// The entity family (`issue`, `comment`, `task`).
    pub kind: String,
    /// The event that produced the entry (`issue_created`, ...).
    pub event: String,
    /// The id of the issue, comment or task the entry addresses.
    pub subject_id: String,
    /// The pre-rendered human line, scrubbed then cut by the fold, scrubbed again here.
    pub summary: String,
    /// The actor the entry is addressed to.
    pub recipient: String,
    /// Creation time, epoch milliseconds.
    pub created_at: i64,
    /// When the entry was marked read; absent when unread.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_at: Option<i64>,
}

/// Bytes held back from the row budget for the scalar fields: the two
/// reasons at their cap (512 characters, up to six bytes each encoded), the
/// recipient, the counters, and the envelope.
const SCALAR_RESERVE: usize = 8 * 1024;

impl From<&InboxSection> for InboxView {
    fn from(section: &InboxSection) -> Self {
        let budget = MAX_INBOX_BYTES.saturating_sub(SCALAR_RESERVE);
        let mut spent: usize = 0;
        let mut rows_cut = section.rows_cut;
        let mut entries = Vec::with_capacity(section.entries.len());
        for row in &section.entries {
            let frame = InboxRowFrame {
                id: row.id.clone(),
                kind: row.kind.clone(),
                event: row.event.clone(),
                subject_id: row.subject_id.clone(),
                summary: scrub(&row.summary),
                recipient: row.recipient.clone(),
                created_at: row.created_at,
                read_at: row.read_at,
            };
            // One byte for the separator; the encoded row is what the wire pays.
            let cost = serde_json::to_vec(&frame).map_or(usize::MAX, |bytes| bytes.len() + 1);
            if spent.saturating_add(cost) > budget {
                rows_cut += section.entries.len() - entries.len();
                break;
            }
            spent += cost;
            entries.push(frame);
        }
        Self {
            entries,
            unread: section.unread,
            recipient: section.recipient.clone(),
            absent: section.absent.as_deref().map(scrub),
            unreachable: section.unreachable.as_deref().map(scrub),
            rows_cut,
            summaries_cut: section.summaries_cut,
            received_at_ms: section.received_at_ms,
        }
    }
}
