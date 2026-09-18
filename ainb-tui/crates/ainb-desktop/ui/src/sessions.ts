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

/** A row the sidebar draws, with its place in the frame's own list. */
export interface VisibleRow {
  /** The index the reducer selects by, which filtering must not shift. */
  index: number;
  session: Session_Serialize;
}

/**
 * One workspace's rows as the sidebar draws them, in frame order.
 *
 * The frame says which rows the filter hides (`hidden_sessions`), so the rule
 * itself lives in the reducer and nowhere else: a window deciding for itself
 * was a second copy of a rule that grows cases (#1157). Each row keeps its
 * index in the frame's own list, because that index is what the selection is
 * expressed in.
 */
export function visibleRows(
  view: SessionsView_Serialize | undefined,
  workspace: Workspace_Serialize,
): VisibleRow[] {
  const hidden = new Set(view?.hidden_sessions ?? []);
  return workspace.sessions
    .map((session, index) => ({ index, session }))
    .filter((row) => !hidden.has(row.session.id));
}

/** How many rows ring with `kind`: one header count. */
export function ringCount(view: SessionsView_Serialize | undefined, kind: AttentionKind): number {
  return allSessions(view).filter((session) => ringFor(session) === kind).length;
}

/** How many rows are idle, the header's last count. */
export function idleCount(view: SessionsView_Serialize | undefined): number {
  return allSessions(view).filter((session) => session.status === "Idle").length;
}
