// What the sessions sidebar and the header draw from the Sessions and Fleet
// frames. Projections only, in the manner of `wire::web::rows_from_frame`:
// every function reads the frame bodies it is handed and keeps nothing, so a
// caller passing store proxies stays fine-grained.

import type {
  AttentionKind,
  FleetView_Serialize,
  SessionStatus,
  Session_Serialize,
  SessionsView_Serialize,
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
  }
}

/**
 * The attention ring a row paints, or `null` for none.
 *
 * The daemon rows correlate by the provider session id Fleet recorded for this
 * session, never by cwd or tmux name: a parent and its subagent share a cwd.
 * An attached row never rings, because the operator is looking at it. With no
 * daemon row, a session whose own status is an error rings `Err`.
 */
export function ringFor(session: Session_Serialize, fleet: FleetView_Serialize | undefined): AttentionKind | null {
  if (session.is_attached) return null;
  const provider = fleet?.fleet_metadata[session.id]?.provider_session_id;
  const rows = provider ? (fleet?.daemon_attention.by_session_id[provider] ?? []) : [];
  let ring: AttentionKind | null = typeof session.status === "object" ? "Err" : null;
  for (const row of rows) {
    if (ring === null || ATTENTION_ORDER.indexOf(row.kind) < ATTENTION_ORDER.indexOf(ring)) ring = row.kind;
  }
  return ring;
}

/** Every session row the Sessions frame lists, across its workspaces. */
export function allSessions(view: SessionsView_Serialize | undefined): Session_Serialize[] {
  return view?.workspaces.flatMap((workspace) => workspace.sessions) ?? [];
}

/** How many rows ring with `kind`: one header count. */
export function ringCount(
  view: SessionsView_Serialize | undefined,
  fleet: FleetView_Serialize | undefined,
  kind: AttentionKind,
): number {
  return allSessions(view).filter((session) => ringFor(session, fleet) === kind).length;
}

/** How many rows are idle, the header's last count. */
export function idleCount(view: SessionsView_Serialize | undefined): number {
  return allSessions(view).filter((session) => session.status === "Idle").length;
}
