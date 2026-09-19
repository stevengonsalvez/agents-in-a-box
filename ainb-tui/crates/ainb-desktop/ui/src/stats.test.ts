// The stats tab draws section 21, the daemon's usage summary, as the daemon
// counted it: states as states, an unpriced figure as unpriced, and what the
// frame cut as a line that says so.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { UsageBucketFrame, UsageSummaryFrame, UsageView } from "../../../ainb-app/bindings/AppState";
import { formatCost, formatTokens, statsView, tokens } from "./stats.ts";

const bucket = (input: number, cost: number | null = null): UsageBucketFrame => ({
  input_tokens: input,
  cache_creation_tokens: 1,
  cache_read_tokens: 2,
  output_tokens: 3,
  reasoning_tokens: 4,
  call_count: 5,
  session_count: 6,
  project_count: 7,
  cost_usd: cost,
});

const summary = (over: Partial<UsageSummaryFrame> = {}): UsageSummaryFrame => ({
  state: "ready",
  totals: bucket(1000, 12.5),
  daily: [
    { date: "2026-09-18", bucket: bucket(10) },
    { date: "2026-09-19", bucket: bucket(90, 0.5) },
  ],
  daily_cut: 0,
  providers: [{ name: "claude", bucket: bucket(900, 12) }],
  providers_cut: 0,
  models: [{ name: "claude-opus-5", bucket: bucket(800, null) }],
  models_cut: 0,
  projects: [{ name: "agents-in-a-box", repo: "stevengonsalvez/agents-in-a-box", bucket: bucket(700, 3) }],
  projects_cut: 0,
  detail: null,
  ...over,
});

const view = (over: Partial<UsageView> = {}): UsageView => ({
  absent: null,
  failure: null,
  summary: summary(),
  ...over,
});

test("a ready summary draws its totals, its days and its three breakdowns", () => {
  const drawn = statsView(view());
  assert.equal(drawn.state, "ready");
  assert.equal(drawn.status, null, "a ready summary needs no status line");
  assert.deepEqual(
    drawn.totals?.map((figure) => [figure.label, figure.value]),
    [
      ["Tokens", "1,010"],
      ["Input", "1,000"],
      ["Output", "3"],
      ["Cache write", "1"],
      ["Cache read", "2"],
      ["Reasoning", "4"],
      ["Calls", "5"],
      ["Sessions", "6"],
      ["Cost", "$12.50"],
    ],
  );
  assert.deepEqual(
    drawn.days.map((day) => [day.date, day.tokens, day.share]),
    [
      ["2026-09-18", "20", 20 / 100],
      ["2026-09-19", "100", 1],
    ],
    "the strip is scaled to its busiest day",
  );
  assert.deepEqual(drawn.providers.map((row) => [row.name, row.tokens, row.cost]), [["claude", "910", "$12.00"]]);
  assert.deepEqual(drawn.models.map((row) => [row.name, row.cost]), [["claude-opus-5", "not priced"]]);
  assert.equal(drawn.projects[0].detail, "stevengonsalvez/agents-in-a-box");
  assert.equal(drawn.cut, undefined);
});

test("an unpriced figure reads as unpriced, never as a zero cost", () => {
  assert.equal(formatCost(null), "not priced");
  assert.equal(formatCost(0), "$0.00");
  const drawn = statsView(view({ summary: summary({ totals: bucket(5, null) }) }));
  assert.equal(drawn.totals?.find((figure) => figure.label === "Cost")?.value, "not priced");
});

test("scanning draws a state and no numbers", () => {
  const drawn = statsView(
    view({ summary: summary({ state: "scanning", totals: null, daily: [], providers: [], models: [], projects: [], detail: "first scan" }) }),
  );
  assert.equal(drawn.state, "scanning");
  assert.match(drawn.status ?? "", /still scanning/);
  assert.equal(drawn.totals, null, "no zeros while scanning");
  assert.equal(drawn.detail, "first scan");
});

test("partial and unavailable say so beside what they carry", () => {
  const partial = statsView(view({ summary: summary({ state: "partial", detail: "codex logs unreadable" }) }));
  assert.equal(partial.state, "partial");
  assert.match(partial.status ?? "", /not every source/);
  assert.equal(partial.detail, "codex logs unreadable");
  assert.ok(partial.totals, "a partial summary still draws its numbers");

  const unavailable = statsView(view({ summary: summary({ state: "unavailable", totals: null }) }));
  assert.equal(unavailable.state, "unavailable");
  assert.match(unavailable.status ?? "", /cannot summarise/);
});

test("absent, failed and not yet read each say why, in the host's words", () => {
  const absent = statsView(view({ absent: "the daemon does not serve fleet.usage.read", summary: null }));
  assert.equal(absent.state, "absent");
  assert.equal(absent.status, "the daemon does not serve fleet.usage.read");

  const failedEmpty = statsView(view({ failure: "daemon not reachable", summary: null }));
  assert.equal(failedEmpty.state, "failed");
  assert.match(failedEmpty.status ?? "", /daemon not reachable/);

  const failedHeld = statsView(view({ failure: "daemon not reachable" }));
  assert.equal(failedHeld.state, "ready", "the last numbers still draw");
  assert.match(failedHeld.status ?? "", /last read failed: daemon not reachable/);

  assert.equal(statsView(undefined).state, "waiting");
  assert.equal(statsView(view({ summary: null })).state, "waiting");
});

test("what the frame cut is one line of its own counters", () => {
  const drawn = statsView(view({ summary: summary({ daily_cut: 3, models_cut: 2 }) }));
  assert.equal(drawn.cut, "Over the frame's caps: 3 days, 2 models not sent");
});

test("token counts are the five token fields, formatted", () => {
  assert.equal(tokens(bucket(1)), 11);
  assert.equal(formatTokens(1234567), "1,234,567");
});

test("a withheld section says the numbers are the last that fitted", () => {
  const fresh = statsView(view(), false);
  assert.equal(fresh.status, null);
  const stale = statsView(view(), true);
  assert.equal(stale.state, "ready", "the held numbers still draw");
  assert.match(stale.status ?? "", /too large to send.*last numbers that fitted/);
  const staleScanning = statsView(view({ summary: summary({ state: "scanning", totals: null }) }), true);
  assert.match(staleScanning.status ?? "", /still scanning.*too large to send/);
});
