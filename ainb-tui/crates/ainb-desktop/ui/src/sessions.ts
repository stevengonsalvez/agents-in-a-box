// What the sessions sidebar and the header draw from the Sessions frame.
// Projections only, in the manner of `wire::web::rows_from_frame`:
// every function reads the frame bodies it is handed and keeps nothing, so a
// caller passing store proxies stays fine-grained.

import type {
  AttentionKind,
  SessionStatus,
  Session_Serialize,
  SessionsView_Serialize,
  Workspace_Serialize,
} from "../../../ainb-app/bindings/AppState";

/** Precedence, tightest first, as `AttentionKind`'s `Ord` in Rust. */
export const ATTENTION_ORDER: readonly AttentionKind[] = ["Ask", "Wait", "Approve", "Err", "Done"];

/** The kinds that block a turn: the header's attention badge counts these. */
export const BLOCKING: readonly AttentionKind[] = ["Ask", "Wait", "Approve"];

export type RowStatus = "running" | "idle" | "stopped" | "error";

export function rowStatus(status: SessionStatus): RowStatus {
  if (typeof status === "object") return "error";
  switch (status) {
    case "Running":
      return "running";
    case "Idle":
      return "idle";
    case "Stopped":
      return "stopped";
    // A host at another version may send a status this build does not know.
    default:
      return "stopped";
  }
}

/**
 * The attention ring a row paints, or `null` for none: the tightest kind of the
 * merged attention its frame row carries. The host merged it (daemon rows by
 * exact provider id, local hook events, the session's own error, an attached
 * row left silent), so the renderer only picks the kind to paint.
 */
export function ringFor(session: Session_Serialize): AttentionKind | null {
  let ring: AttentionKind | null = null;
  // Optional: a host at another version may not send it.
  for (const mark of session.attention ?? []) {
    if (ring === null || ATTENTION_ORDER.indexOf(mark.kind) < ATTENTION_ORDER.indexOf(ring)) ring = mark.kind;
  }
  return ring;
}

/** The most characters a sidebar label draws. */
export const LABEL_CHARS = 80;

/**
 * A name as the sidebar may draw it: control and format characters removed
 * (a bidi override or an escape in a branch name cannot restyle the row) and
 * cut to `LABEL_CHARS` characters.
 */
export function label(text: string): string {
  return Array.from(text.replace(/[\p{Cc}\p{Cf}]/gu, ""))
    .slice(0, LABEL_CHARS)
    .join("");
}

/** Every session row the Sessions frame lists, across its workspaces. */
export function allSessions(view: SessionsView_Serialize | undefined): Session_Serialize[] {
  return view?.workspaces.flatMap((workspace) => workspace.sessions) ?? [];
}

/**
 * The rows the sidebar draws for `workspace`: exactly the frame's, in frame
 * order. The frame already carries only the rows the reducer's filter shows
 * (#1180), so the window never filters them again, by status or otherwise.
 */
export function drawnRows(workspace: Workspace_Serialize): Session_Serialize[] {
  return workspace.sessions;
}

/**
 * Whether `sessionId` is the session list's selected row.
 *
 * The frame carries only the rows the filter shows, and the selection by id
 * (#1180): an index would be into the reducer's full list, which the window
 * never sees.
 */
export function isSelected(view: SessionsView_Serialize | undefined, sessionId: string): boolean {
  return view?.selected_session_id === sessionId;
}

/** How many rows ring with `kind`: one header count. */
export function ringCount(view: SessionsView_Serialize | undefined, kind: AttentionKind): number {
  return allSessions(view).filter((session) => ringFor(session) === kind).length;
}

/** How many rows are idle, the header's last count. */
export function idleCount(view: SessionsView_Serialize | undefined): number {
  return allSessions(view).filter((session) => session.status === "Idle").length;
}
