// What the stats tab draws from section 21, the daemon's usage summary
// (D3p-e). Projections only: `stats.tsx` draws what these return.
//
//   usage section ──statsView──▶ status line, totals, day strip, breakdowns
//
// The numbers have one producer, the daemon's usage projection, and the host
// folds its reply without computing anything. So this file formats and does not
// count: the one sum it makes is a bucket's five token kinds added up for a
// single "tokens" figure, which is presentation of what the bucket says. The
// states are drawn as states (`fleet.rs`: "Clients must show tokens instead of
// synthesising a zero cost"): scanning shows no numbers, an unpriced figure
// reads "not priced", and a failed read keeps the last numbers and says so.

import type { UsageBucketFrame, UsageView } from "../../../ainb-app/bindings/AppState";

/** Where the tab's read stands, one word the tab can style by. */
export type StatsState = "waiting" | "absent" | "failed" | "scanning" | "ready" | "partial" | "unavailable";

/** One labelled figure in the totals row. */
export interface Figure {
  label: string;
  value: string;
}

/** One day of the strip. */
export interface DayBar {
  date: string;
  tokens: string;
  /** This day's tokens over the busiest day's, 0 to 1, for the bar's height. */
  share: number;
  cost: string;
}

/** One breakdown row: a provider, a model or a project. */
export interface BreakdownRow {
  name: string;
  /** The project's repository, when the daemon resolved one. */
  detail: string | null;
  tokens: string;
  calls: string;
  cost: string;
}

/** The stats tab's picture of section 21. */
export interface StatsView {
  state: StatsState;
  /** One line on where the read stands, or `null` when a ready read needs none. */
  status: string | null;
  /** The daemon's own detail on a partial or unavailable summary. */
  detail: string | undefined;
  /** `null` while there is nothing counted to draw: never zeros. */
  totals: Figure[] | null;
  days: DayBar[];
  providers: BreakdownRow[];
  models: BreakdownRow[];
  projects: BreakdownRow[];
  /** What the frame's caps left out, as one line, or undefined. */
  cut: string | undefined;
}

/** A bucket's tokens: its five token kinds added up. */
export function tokens(bucket: UsageBucketFrame): number {
  return (
    bucket.input_tokens +
    bucket.output_tokens +
    bucket.cache_creation_tokens +
    bucket.cache_read_tokens +
    bucket.reasoning_tokens
  );
}

/** A count with thousands separators. */
export function formatTokens(count: number): string {
  return count.toLocaleString("en-US");
}

/** A cost in dollars, or "not priced" when a call in the bucket had no rate. */
export function formatCost(cost: number | null): string {
  return cost === null ? "not priced" : `$${cost.toFixed(2)}`;
}

function row(name: string, detail: string | null, bucket: UsageBucketFrame): BreakdownRow {
  return {
    name,
    detail,
    tokens: formatTokens(tokens(bucket)),
    calls: formatTokens(bucket.call_count),
    cost: formatCost(bucket.cost_usd),
  };
}

function totals(bucket: UsageBucketFrame): Figure[] {
  return [
    { label: "Tokens", value: formatTokens(tokens(bucket)) },
    { label: "Input", value: formatTokens(bucket.input_tokens) },
    { label: "Output", value: formatTokens(bucket.output_tokens) },
    { label: "Cache write", value: formatTokens(bucket.cache_creation_tokens) },
    { label: "Cache read", value: formatTokens(bucket.cache_read_tokens) },
    { label: "Reasoning", value: formatTokens(bucket.reasoning_tokens) },
    { label: "Calls", value: formatTokens(bucket.call_count) },
    { label: "Sessions", value: formatTokens(bucket.session_count) },
    { label: "Cost", value: formatCost(bucket.cost_usd) },
  ];
}

const EMPTY: Omit<StatsView, "state" | "status"> = {
  detail: undefined,
  totals: null,
  days: [],
  providers: [],
  models: [],
  projects: [],
  cut: undefined,
};

/**
 * What the tab says when the host withheld section 21 as oversize: the store
 * keeps the body it last held, so the numbers drawn are the last that fitted.
 */
export const WITHHELD =
  "The usage summary was too large to send, so these are the last numbers that fitted.";

/**
 * Section 21 as the stats tab draws it. `stale` is whether the host withheld
 * the section since the body held here was framed.
 */
export function statsView(usage: UsageView | undefined, stale = false): StatsView {
  if (usage === undefined) return { ...EMPTY, state: "waiting", status: "Reading usage from the daemon" };
  const summary = usage.summary;
  if (summary === null) {
    if (usage.absent !== null) return { ...EMPTY, state: "absent", status: usage.absent };
    if (usage.failure !== null) {
      return { ...EMPTY, state: "failed", status: `The daemon could not be read: ${usage.failure}` };
    }
    return { ...EMPTY, state: "waiting", status: "Reading usage from the daemon" };
  }
  const said: string[] = [];
  switch (summary.state) {
    case "scanning":
      said.push("The daemon is still scanning provider logs");
      break;
    case "partial":
      said.push("The daemon could not read every source, so not every source is counted");
      break;
    case "unavailable":
      said.push("The daemon cannot summarise usage right now");
      break;
    case "ready":
      break;
  }
  if (usage.failure !== null) said.push(`The last read failed: ${usage.failure}; these are the last numbers read`);
  if (stale) said.push(WITHHELD);
  const busiest = Math.max(0, ...summary.daily.map((day) => tokens(day.bucket)));
  const lost: string[] = [];
  const count = (n: number, one: string, many: string) => {
    if (n > 0) lost.push(`${n} ${n === 1 ? one : many}`);
  };
  count(summary.daily_cut, "day", "days");
  count(summary.providers_cut, "provider", "providers");
  count(summary.models_cut, "model", "models");
  count(summary.projects_cut, "project", "projects");
  return {
    state: summary.state,
    status: said.length === 0 ? null : said.join(". "),
    detail: summary.detail ?? undefined,
    // Scanning has counted nothing yet, whatever the reply carries.
    totals: summary.state === "scanning" || summary.totals === null ? null : totals(summary.totals),
    days: summary.daily.map((day) => ({
      date: day.date,
      tokens: formatTokens(tokens(day.bucket)),
      share: busiest === 0 ? 0 : tokens(day.bucket) / busiest,
      cost: formatCost(day.bucket.cost_usd),
    })),
    providers: summary.providers.map((entry) => row(entry.name, null, entry.bucket)),
    models: summary.models.map((entry) => row(entry.name, null, entry.bucket)),
    projects: summary.projects.map((entry) => row(entry.name, entry.repo, entry.bucket)),
    cut: lost.length === 0 ? undefined : `Over the frame's caps: ${lost.join(", ")} not sent`,
  };
}
