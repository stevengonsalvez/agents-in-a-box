import { For, Show } from "solid-js";
import type { UsageView } from "../../../ainb-app/bindings/AppState";
import { type BreakdownRow, statsView } from "./stats.ts";

interface Props {
  /** Section 21, or undefined until the host frames it. */
  usage: UsageView | undefined;
}

/**
 * The stats tab: the daemon's usage summary for the trailing thirty days, drawn
 * natively from section 21 (D3p-e).
 *
 * Read-only, and it sends no intent: the host reads the daemon on the daemon's
 * own refresh, so there is nothing for a click to ask for. What the tab does
 * own is saying what it does not have: a state line when the summary is still
 * scanning, partial or unavailable, the host's reason when the read is absent
 * or failed, "not priced" for a figure with no rate, and the frame's cut
 * counters when a list was longer than the frame carries.
 */
export function Stats(props: Props) {
  const view = () => statsView(props.usage);
  return (
    <section class="stats" aria-label="Stats" data-state={view().state}>
      <Show when={view().status}>
        {(line) => (
          <p class="stats-status" role="status">
            {line()}
          </p>
        )}
      </Show>
      <Show when={view().detail}>{(detail) => <p class="stats-detail">{detail()}</p>}</Show>
      <Show when={view().cut}>
        {(line) => (
          <p class="stats-cut" role="status">
            {line()}
          </p>
        )}
      </Show>

      <Show when={view().totals}>
        {(figures) => (
          <dl class="stats-totals" aria-label="Totals, trailing 30 days">
            <For each={figures()}>
              {(figure) => (
                <div class="stats-figure">
                  <dt>{figure.label}</dt>
                  <dd>{figure.value}</dd>
                </div>
              )}
            </For>
          </dl>
        )}
      </Show>

      <Show when={view().days.length > 0}>
        <ol class="stats-days" aria-label="Tokens per day">
          <For each={view().days}>
            {(day) => (
              <li class="stats-day" title={`${day.date}: ${day.tokens} tokens, ${day.cost}`}>
                <span class="stats-bar" style={{ height: `${Math.round(day.share * 100)}%` }} />
                <span class="stats-date">{day.date.slice(5)}</span>
              </li>
            )}
          </For>
        </ol>
      </Show>

      <div class="stats-breakdowns">
        <Breakdown title="Providers" rows={view().providers} />
        <Breakdown title="Models" rows={view().models} />
        <Breakdown title="Projects" rows={view().projects} />
      </div>
    </section>
  );
}

function Breakdown(props: { title: string; rows: BreakdownRow[] }) {
  return (
    <Show when={props.rows.length > 0}>
      <table class="stats-breakdown" aria-label={props.title}>
        <caption>{props.title}</caption>
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">Tokens</th>
            <th scope="col">Calls</th>
            <th scope="col">Cost</th>
          </tr>
        </thead>
        <tbody>
          <For each={props.rows}>
            {(row) => (
              <tr>
                <th scope="row">
                  {row.name}
                  <Show when={row.detail}>{(detail) => <span class="stats-repo">{detail()}</span>}</Show>
                </th>
                <td>{row.tokens}</td>
                <td>{row.calls}</td>
                <td>{row.cost}</td>
              </tr>
            )}
          </For>
        </tbody>
      </table>
    </Show>
  );
}
