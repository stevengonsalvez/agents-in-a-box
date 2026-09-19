import { createMemo, For, Show } from "solid-js";
import type { AgentStatusView, FleetView_Serialize, SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  attentionRows,
  boardColumns,
  boardHealth,
  COLUMN_TITLES,
  daemonReachable,
  showIntents,
  type BoardCard,
  type BoardColumn,
} from "./board.ts";
import { label } from "./sessions.ts";
import type { RendererIntent } from "./tabs.ts";

interface Props {
  /** The status frame the cards come from. */
  agentStatus: AgentStatusView | undefined;
  /** The Fleet frame: models, the daemon's open rows, the elsewhere count. */
  fleet: FleetView_Serialize | undefined;
  /** The sessions frame: a card's title and its row's chips. */
  sessions: SessionsView_Serialize | undefined;
  /** Open rows no session in this window can show: a root selector's memo. */
  elsewhere: number;
  /** A row was chosen: dispatch its intent. */
  onChoose(intent: RendererIntent): void;
  /** An ACP card was chosen: open its transcript where a terminal would be. */
  onOpenTranscript(sessionKey: string): void;
}

/**
 * Whether two projections draw the same board. A drain that touches Sessions,
 * Fleet or agent_status recomputes the columns; without this every card button
 * would be rebuilt, and a keyboard user's focus dropped, on every one.
 */
function sameColumns(a: BoardColumn[], b: BoardColumn[]): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

/**
 * The board: every agent the host knows about, in the column its state names,
 * beside the list of what is waiting on a human.
 *
 * Clicking a card selects its session list row WITHOUT attaching it (opening a
 * terminal is a separate decision) and shows the pane the card is about, which
 * is the `ask` pane when something is open on that agent.
 */
export function Board(props: Props) {
  const columns = createMemo(() => boardColumns(props.agentStatus, props.fleet, props.sessions), undefined, {
    equals: sameColumns,
  });
  const health = createMemo(() => boardHealth(props.agentStatus));
  const waiting = createMemo(() => attentionRows(props.fleet, props.sessions));

  const show = (sessionId: string | null, openRequest: boolean) => {
    if (sessionId === null) return;
    for (const intent of showIntents(sessionId, openRequest)) props.onChoose(intent);
  };

  const healthLine = () => {
    const state = health();
    switch (state.kind) {
      case "absent":
        return `No agent status: ${state.detail}`;
      case "stale":
        return `Agent status is ${state.behind} revision${state.behind === 1 ? "" : "s"} behind`;
      case "unreachable":
        return `Agent status unreachable: ${state.reason}`;
      default:
        return null;
    }
  };

  return (
    <section class="board" aria-label="Agent board">
      <Show when={healthLine()}>
        {(line) => (
          <p class="board-health" role="status" data-health={health().kind}>
            {line()}
          </p>
        )}
      </Show>
      <div class="board-columns">
        <For each={columns()}>
          {(column) => (
            <div class="board-column" data-state={column.state}>
              <h2>
                {COLUMN_TITLES[column.state]}
                <span class="board-count">{column.cards.length}</span>
              </h2>
              <Show when={column.cards.length > 0} fallback={<p class="empty">Nothing here</p>}>
                <ul>
                  <For each={column.cards}>
                    {(card) => (
                      <li>
                        <button
                          type="button"
                          class="board-card"
                          classList={{ open: card.hasOpenRequest }}
                          data-card={card.key}
                          disabled={card.sessionId === null && !card.acp}
                          onClick={() =>
                            card.sessionId === null && card.acp
                              ? props.onOpenTranscript(card.key)
                              : show(card.sessionId, card.hasOpenRequest)
                          }
                        >
                          <span class="card-title">{card.title}</span>
                          <span class="card-line">{cardLine(card)}</span>
                          <Show when={card.attention.length > 0}>
                            <span class="card-chips">
                              <For each={card.attention}>
                                {(kind) => (
                                  <span class="chip" data-kind={kind}>
                                    {kind}
                                  </span>
                                )}
                              </For>
                            </span>
                          </Show>
                        </button>
                      </li>
                    )}
                  </For>
                </ul>
              </Show>
            </div>
          )}
        </For>
      </div>
      <aside class="attention-list" aria-label="Waiting on you">
        <h2>Waiting on you</h2>
        <Show
          when={waiting().length > 0}
          fallback={
            <p class="empty">{daemonReachable(props.fleet) ? "Nothing is waiting" : "The daemon has not answered"}</p>
          }
        >
          <ul>
            <For each={waiting()}>
              {(row) => (
                <li>
                  <button
                    type="button"
                    class="attention-row"
                    data-row={row.key}
                    data-kind={row.kind}
                    disabled={row.sessionId === null}
                    onClick={() => show(row.sessionId, true)}
                  >
                    <span class="row-kind">{row.kind}</span>
                    <span class="row-title">{row.title}</span>
                    <Show when={row.detail}>
                      <span class="row-detail">{row.detail}</span>
                    </Show>
                  </button>
                </li>
              )}
            </For>
          </ul>
        </Show>
        {/* Never swallowed: a row this window cannot show is still a human
            being waited on somewhere. */}
        <Show when={props.elsewhere > 0}>
          <p class="elsewhere">{props.elsewhere} waiting elsewhere</p>
        </Show>
      </aside>
    </section>
  );
}

/** The line under a card's title: what it is and how it is reachable. */
function cardLine(card: BoardCard): string {
  // The model is free text off the frame, drawn through the same rule as a
  // session name, so a bidi override or an escape cannot restyle the card.
  const parts = [
    card.provider,
    card.model === null ? null : label(card.model),
    card.lifecycle.toLowerCase(),
    card.waitKind,
  ];
  if (card.transport !== "HEALTHY") parts.push(`transport ${card.transport.toLowerCase()}`);
  return parts.filter((part): part is string => Boolean(part)).join(" · ");
}
