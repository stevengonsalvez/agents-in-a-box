// What the board and the attention list draw, projected from the frames the
// window already holds. Projections only: `board.tsx` draws what these return,
// and nothing here reads the store or keeps anything.
//
//   agent_status.view.cards[] ──state──▶ columns
//   fleet.fleet_snapshot[]    ──model, session_key──▶ the card's line
//   sessions.workspaces[]     ──name, attention──▶ the card's title and chips
//
// The grouping is the host's: a card sits in the column its `state` names, and
// the renderer never infers a state from a lifecycle or an attention chip. Two
// surfaces guessing differently from the same frame is the drift this seam
// exists to remove.

import type {
  AgentCardFrame,
  AgentState,
  AgentStatusView,
  AttentionKind,
  FleetView_Serialize,
  SessionAttention_Serialize,
  SessionsView_Serialize,
  Session_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { allSessions, ATTENTION_ORDER, label } from "./sessions.ts";

/**
 * What each column is called, in the operator's words rather than the wire's,
 * left to right. Typed over every `AgentState`, so a state Rust adds fails to
 * compile here instead of every card in it vanishing from the board.
 */
export const COLUMN_TITLES: Record<AgentState, string> = {
  waiting: "Waiting on you",
  working: "Working",
  idle: "Idle",
  unverifiable: "Unverified",
  exited: "Exited",
};

/** The columns the board draws, left to right. */
export const COLUMNS = Object.keys(COLUMN_TITLES) as AgentState[];

/** One card on the board. */
export interface BoardCard {
  /** `provider:session-id`, the host's own identity for the agent. */
  key: string;
  /** The sidebar's name for the row, when the window holds one. */
  title: string;
  /** The session list row a click selects, when this card has one. */
  sessionId: string | null;
  state: AgentState;
  /** The line under the title: provider, model, lifecycle, transport. */
  provider: string;
  model: string | null;
  lifecycle: string;
  transport: string;
  /** What the agent is waiting for, when it is waiting. */
  waitKind: string | null;
  /** Something is open on this agent, so it floats to the top of its column. */
  hasOpenRequest: boolean;
  /** The chips its session row carries, tightest first. */
  attention: AttentionKind[];
}

/** One column: the state it holds and the cards in it. */
export interface BoardColumn {
  state: AgentState;
  cards: BoardCard[];
}

/**
 * What the board says about itself when it cannot draw agents: the section is
 * absent, the read is behind the daemon's head, or the daemon is unreachable.
 *
 * A board with no cards and no explanation reads as "nothing is running",
 * which is the one thing it must never say when it simply does not know.
 */
export type BoardHealth =
  | { kind: "live" }
  | { kind: "absent"; detail: string }
  | { kind: "stale"; behind: number }
  | { kind: "unreachable"; reason: string };

/** The provider session id inside a `provider:session-id` key. */
function providerId(sessionKey: string): string {
  const at = sessionKey.indexOf(":");
  return at < 0 ? sessionKey : sessionKey.slice(at + 1);
}

/** The session row a card belongs to, by the provider id the host correlated. */
function rowFor(
  card: AgentCardFrame,
  sessions: SessionsView_Serialize | undefined,
  fleet: FleetView_Serialize | undefined,
): Session_Serialize | undefined {
  const wanted = providerId(card.session_key);
  const metadata = fleet?.fleet_metadata ?? {};
  return allSessions(sessions).find((session) => metadata[session.id]?.provider_session_id === wanted);
}

/** The chips a session row carries, tightest first. */
function chipsOf(session: Session_Serialize | undefined): AttentionKind[] {
  return [...(session?.attention ?? [])]
    .map((mark) => mark.kind)
    .sort((a, b) => ATTENTION_ORDER.indexOf(a) - ATTENTION_ORDER.indexOf(b));
}

/**
 * The board's columns, in `COLUMNS` order, from the host's own cards.
 *
 * A card with no matching session row still draws: the agent exists whether or
 * not this window's sidebar has caught up with it, and a board that hid it
 * would hide exactly the agent a stale scan has not listed yet. Within a
 * column, an agent with something open floats to the top, then by title, so
 * the row that wants a human is the first one read.
 */
export function boardColumns(
  agentStatus: AgentStatusView | undefined,
  fleet: FleetView_Serialize | undefined,
  sessions: SessionsView_Serialize | undefined,
): BoardColumn[] {
  const models = new Map((fleet?.fleet_snapshot ?? []).map((row) => [row.session_key, row.model]));
  const cards: BoardCard[] = (agentStatus?.view?.cards ?? []).map((card) => {
    const session = rowFor(card, sessions, fleet);
    return {
      key: card.session_key,
      title: label(session?.name ?? card.session_key),
      sessionId: session?.id ?? null,
      state: card.state,
      provider: card.provider,
      model: models.get(card.session_key) ?? null,
      lifecycle: card.lifecycle,
      transport: card.transport_health,
      waitKind: card.wait_kind,
      hasOpenRequest: card.has_open_request,
      attention: chipsOf(session),
    };
  });
  return COLUMNS.map((state) => ({
    state,
    cards: cards
      .filter((card) => card.state === state)
      .sort(
        (a, b) =>
          Number(b.hasOpenRequest) - Number(a.hasOpenRequest) || a.title.localeCompare(b.title),
      ),
  }));
}

/** What the board can say about the picture it is drawing. */
export function boardHealth(agentStatus: AgentStatusView | undefined): BoardHealth {
  if (agentStatus === undefined) return { kind: "absent", detail: "no status frame yet" };
  if (agentStatus.absent !== null) return { kind: "absent", detail: label(agentStatus.absent) };
  const view = agentStatus.view;
  if (view === null) return { kind: "absent", detail: "no status read yet" };
  switch (view.health.kind) {
    case "stale":
      return { kind: "stale", behind: view.health.head_revision - view.health.read_revision };
    case "unreachable":
      return { kind: "unreachable", reason: label(view.health.reason) };
    default:
      return { kind: "live" };
  }
}

/** One row of the attention list: an agent waiting on a human. */
export interface AttentionRow {
  /** Unique within the list, so it keys the rendered rows. */
  key: string;
  title: string;
  kind: AttentionKind;
  detail: string | null;
  /** The session list row a click selects, when the window holds one. */
  sessionId: string | null;
}

/**
 * The attention list the header's counts summarise: the daemon's open rows,
 * keyed by the provider session id they were raised against.
 *
 * The daemon's rows, not the sidebar's chips, because a row whose session this
 * window has not listed still needs a human. The title falls back to the
 * provider id for exactly that case.
 */
export function attentionRows(
  fleet: FleetView_Serialize | undefined,
  sessions: SessionsView_Serialize | undefined,
): AttentionRow[] {
  const metadata = fleet?.fleet_metadata ?? {};
  const rows: AttentionRow[] = [];
  for (const [providerSessionId, chips] of Object.entries(fleet?.daemon_attention?.by_session_id ?? {})) {
    const session = allSessions(sessions).find(
      (row) => metadata[row.id]?.provider_session_id === providerSessionId,
    );
    (chips as SessionAttention_Serialize[]).forEach((chip, index) => {
      rows.push({
        key: `${providerSessionId}:${index}`,
        title: label(session?.name ?? providerSessionId),
        kind: chip.kind,
        detail: chip.detail === null ? null : label(chip.detail),
        sessionId: session?.id ?? null,
      });
    });
  }
  return rows.sort(
    (a, b) => ATTENTION_ORDER.indexOf(a.kind) - ATTENTION_ORDER.indexOf(b.kind) || a.title.localeCompare(b.title),
  );
}

/**
 * How many open rows belong to no session this window can show, as the host
 * counted them.
 *
 * Its own line rather than folded into the list: a count of things happening
 * somewhere else is not a row anyone can act on here, and dropping it would
 * quietly shrink the fleet to what this window happens to hold.
 */
export function elsewhereCount(fleet: FleetView_Serialize | undefined): number {
  return fleet?.attention_elsewhere ?? 0;
}

/** Whether the daemon answered the last attention poll. */
export function daemonReachable(fleet: FleetView_Serialize | undefined): boolean {
  return fleet?.daemon_attention?.reachable ?? false;
}
