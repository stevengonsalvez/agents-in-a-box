// ABOUTME: D15 renderer fan-out bench, gated in CI for W0-mirror. A SolidJS store
// holding 19 sections at 100 sessions takes a fixed 2,000-frame burst over a
// simulated 2 s and reports computation runs, apply time, the longest apply
// unit and the number of apply units, then asserts the spike 4 ceilings.
//
//   frames (seeded LCG) ──16 ms drains──▶ MirrorStore.applyDrain ──batch()──▶ effects, memos
//
// Frames have the wire shape of `ainb_app::wire::frame::Frame`: a section name,
// a version, a boot epoch, a host id and the whole section body. One host, one
// epoch: the bench measures the drain shape, not epoch or peer handling. A drain keeps the last frame
// per section (as `MirrorStore::apply_drain` does) and writes them in one
// `batch`, diffing each body against the last one applied so only changed row
// fields are written and only their readers notify.
//
// Modes:
//   per-send   one store transaction per frame; must FAIL both numeric gates
//   per-drain  one transaction per 16 ms drain; must PASS every gate
//   subset-in  per-drain, 5 of 19 sections subscribed, the hot sections among them
//   subset-out per-drain, 5 of 19 subscribed, agent_status not among them
//
// Run: npm ci && npm run bench   (writes results.json, exits non-zero on a gate)
//
// BENCH_APPLY=reconcile swaps the per-path body diff for the desktop store's
// own apply, `reconcile(body, { key: "id" })` per section
// (crates/ainb-desktop/ui/src/store.ts), to measure that store against the same
// burst. It is a measurement, not the gated configuration: CI runs the default,
// and the numbers it gave are recorded in the D1 goal (#1132).

import { batch, createEffect, createMemo, createRoot } from "solid-js";
import { createStore, reconcile } from "solid-js/store";
import { writeFileSync } from "node:fs";
import { performance } from "node:perf_hooks";

export const SESSIONS = 100;
export const BURST_FRAMES = 2000;
export const BURST_MS = 2000;
export const DRAIN_MS = 16;
export const SEEDS = [1234, 1235, 1236, 1237, 1238];

/** The ceilings from research/2026-09-11_multi-surface_SPIKE-4-specta-fanout.md. */
export const CEILINGS = {
  computationRuns: 12800,
  applyMsPer1kFrames: 32.5,
  longestUnitMs: 2.4,
  applyUnits: Math.ceil(BURST_MS / DRAIN_MS),
};

/**
 * The computations `mount` creates: 3 root memos, 3 root effects and 4
 * effects per session. A change to the effects or memos in `mount` changes
 * it; update this to the new count, and say why in the commit.
 */
export const CENSUS = 3 + 3 + 4 * SESSIONS;

const SECTIONS = [
  "sessions", "session_labels", "tmux", "ssh", "git_view", "workspace_load",
  "new_session", "logs", "claude_chat", "fleet", "hangar", "mcp_pool", "inbox",
  "plugins_host", "config", "skills", "recovery", "onboarding", "shell",
  "agent_status", "board",
];
const STATUSES = ["idle", "running", "needs_input", "done"];

/** The body apply to measure; see the header. */
const APPLY = process.env.BENCH_APPLY === "reconcile" ? "reconcile" : "path-diff";
const STATES = ["starting", "running", "turn_complete", "idle"];

/** Park-Miller LCG: the same stream in every mode and on every machine. */
function lcg(seed) {
  let s = seed % 2147483647;
  if (s <= 0) s += 2147483646;
  return () => {
    s = (s * 16807) % 2147483647;
    return (s - 1) / 2147483646;
  };
}

function initialBodies() {
  const rows = (make) => Array.from({ length: SESSIONS }, (_, i) => make(i));
  const bodies = {
    sessions: {
      rows: rows((i) => ({
        id: `s${i}`, name: `session-${i}`, status: STATUSES[i % 4], host: "local",
        updatedAt: 0, ring: 0, lastReply: "",
      })),
    },
    agent_status: {
      rows: rows((i) => ({
        id: `s${i}`, state: STATES[i % 4], provenance: "hook", tier: 1,
        stateSince: 0, observedAt: 0, heartbeatAt: 0,
      })),
    },
    board: { rows: rows((i) => ({ id: `c${i}`, title: `card-${i}`, column: "todo", points: 1 })) },
  };
  for (const name of SECTIONS) {
    if (!bodies[name]) bodies[name] = { version: 0, label: name, flag: false, count: 0 };
  }
  return bodies;
}

/**
 * The burst: 70% agent_status, 20% sessions, 10% board single-row changes.
 *
 * A generator, so each frame is built (and parsed) when the channel would
 * deliver it, before the drain that applies it is timed: 2,000 whole section
 * bodies held at once would put garbage collection inside the apply units.
 */
function* burst(seed) {
  const rand = lcg(seed);
  const bodies = initialBodies();
  const versions = Object.fromEntries(SECTIONS.map((name) => [name, 0]));
  for (let t = 0; t < BURST_FRAMES; t++) {
    const pick = rand();
    const section = pick < 0.7 ? "agent_status" : pick < 0.9 ? "sessions" : "board";
    const rows = bodies[section].rows;
    const index = Math.floor(rand() * SESSIONS);
    const row = { ...rows[index] };
    if (section === "agent_status") {
      row.state = STATES[Math.floor(rand() * STATES.length)];
      row.observedAt = t;
    } else if (section === "sessions") {
      row.status = STATUSES[Math.floor(rand() * STATUSES.length)];
      row.updatedAt = t;
    } else {
      row.points = 1 + Math.floor(rand() * 8);
    }
    const nextRows = rows.slice();
    nextRows[index] = row;
    bodies[section] = { rows: nextRows };
    versions[section] += 1;
    yield {
      at_ms: (t * BURST_MS) / BURST_FRAMES,
      // A renderer receives parsed JSON: no row object is shared with the
      // previous frame, so the diff below compares every row, as it would.
      frame: {
        section,
        version: versions[section],
        epoch: 1,
        host_id: "local",
        body: JSON.parse(JSON.stringify(bodies[section])),
      },
    };
  }
}

/** The store, its `CENSUS` computations and the counters around them. */
function mount(subscribed) {
  const counters = { runs: 0, created: 0 };
  const [store, setStore] = createStore(initialBodies());
  const held = Object.fromEntries(SECTIONS.map((name) => [name, 0]));

  const dispose = createRoot((disposeRoot) => {
    const effect = (body) => {
      counters.created += 1;
      createEffect(() => {
        counters.runs += 1;
        body();
      });
    };
    const memo = (body) => {
      counters.created += 1;
      return createMemo(() => {
        counters.runs += 1;
        return body();
      });
    };

    // Root selectors: scalars, so a row write that leaves the count alone
    // re-runs nothing downstream (D15 invariant 3).
    const needsInput = memo(() => store.sessions.rows.filter((r) => r.status === "needs_input").length);
    const runningCount = memo(() => store.agent_status.rows.filter((r) => r.state === "running").length);
    const boardTotal = memo(() => store.board.rows.reduce((sum, r) => sum + r.points, 0));

    effect(() => needsInput());
    effect(() => runningCount());
    effect(() => boardTotal());
    for (let i = 0; i < SESSIONS; i++) {
      effect(() => store.sessions.rows[i].status + store.sessions.rows[i].updatedAt);
      effect(() => store.agent_status.rows[i].state + store.agent_status.rows[i].observedAt);
      effect(() => needsInput() + store.sessions.rows[i].status);
      effect(() => store.board.rows[i].points);
    }
    return disposeRoot;
  });

  // The last body applied per section, as plain JSON. A frame is diffed
  // against this, not against the store: reading the store goes through its
  // proxies, and a 100-row section body arrives whole on every frame.
  const plain = initialBodies();

  /**
   * Write only the row fields that differ, by path, so only their readers
   * notify. A key the new body no longer has is cleared, on a row or on the
   * body itself: a whole-body frame drops a field by leaving it out.
   */
  const writeDiff = (section, body) => {
    if (APPLY === "reconcile") {
      setStore(section, reconcile(body, { key: "id" }));
      return;
    }
    const before = plain[section];
    if (!Array.isArray(body.rows) || !Array.isArray(before.rows) || body.rows.length !== before.rows.length) {
      setStore(section, reconcile(body, { key: "id", merge: true }));
      return;
    }
    for (const key in before) {
      if (!(key in body)) setStore(section, key, undefined);
    }
    for (let i = 0; i < body.rows.length; i++) {
      const next = body.rows[i];
      const prev = before.rows[i];
      if (next === prev) continue;
      for (const field in next) {
        if (next[field] !== prev[field]) setStore(section, "rows", i, field, next[field]);
      }
      for (const field in prev) {
        if (!(field in next)) setStore(section, "rows", i, field, undefined);
      }
    }
  };

  /** One store transaction: the last frame per subscribed section, diffed in. */
  const applyUnit = (frames) => {
    const latest = new Map();
    let dropped = 0;
    for (const frame of frames) {
      if (!subscribed.has(frame.section) || frame.version <= held[frame.section]) {
        dropped += 1;
        continue;
      }
      latest.set(frame.section, frame);
    }
    if (latest.size === 0) return { wrote: false, dropped };
    batch(() => {
      for (const [section, frame] of latest) {
        held[section] = frame.version;
        writeDiff(section, frame.body);
        plain[section] = frame.body;
      }
    });
    return { wrote: true, dropped };
  };

  return { counters, applyUnit, dispose, store };
}

/** A frame that leaves a row field out clears it from the store. */
function removedKeysAreCleared() {
  const mounted = mount(new Set(SECTIONS));
  const body = initialBodies().agent_status;
  const { heartbeatAt: _dropped, ...row } = body.rows[0];
  body.rows[0] = row;
  mounted.applyUnit([{ section: "agent_status", version: 1, epoch: 1, host_id: "local", body }]);
  const cleared = !("heartbeatAt" in mounted.store.agent_status.rows[0])
    || mounted.store.agent_status.rows[0].heartbeatAt === undefined;
  const kept = mounted.store.agent_status.rows[1].heartbeatAt === 0;
  mounted.dispose();
  return cleared && kept;
}

/** Best of `REPEATS` for the timing metrics of one mode and seed. */
export const REPEATS = 3;

function measure(mode, seed) {
  let best = null;
  for (let i = 0; i < REPEATS; i++) {
    globalThis.gc?.();
    const result = run(mode, seed);
    if (!best) {
      best = result;
      continue;
    }
    // Counts are deterministic; a slower repeat is a GC pause or a busy
    // runner, not the apply path, so each timing keeps its fastest repeat.
    best.applyMs = Math.min(best.applyMs, result.applyMs);
    best.applyMsPer1kFrames = Math.min(best.applyMsPer1kFrames, result.applyMsPer1kFrames);
    best.longestUnitMs = Math.min(best.longestUnitMs, result.longestUnitMs);
  }
  return best;
}

function run(mode, seed) {
  const subscribed = new Set(
    mode === "subset-in" ? ["agent_status", "sessions", "board", "shell", "config"]
      : mode === "subset-out" ? ["sessions", "board", "shell", "config", "fleet"]
        : SECTIONS,
  );
  const store = mount(subscribed);
  const mountRuns = store.counters.runs;
  const stream = burst(seed);

  let applyMs = 0;
  let longestUnitMs = 0;
  let applyUnits = 0;
  let framesDropped = 0;
  const unit = (frames) => {
    const start = performance.now();
    const { wrote, dropped } = store.applyUnit(frames);
    const elapsed = performance.now() - start;
    framesDropped += dropped;
    applyMs += elapsed;
    longestUnitMs = Math.max(longestUnitMs, elapsed);
    // A drain is one transaction whether or not the subscription let a frame
    // through; per-send counts every frame it wrote.
    if (wrote || mode !== "per-send") applyUnits += 1;
  };

  if (mode === "per-send") {
    for (const { frame } of stream) unit([frame]);
  } else {
    // Drain ticks at every 16 ms boundary the stream crosses, plus the tail.
    let pending = [];
    let boundary = DRAIN_MS;
    for (const { at_ms, frame } of stream) {
      while (at_ms >= boundary) {
        unit(pending);
        pending = [];
        boundary += DRAIN_MS;
      }
      pending.push(frame);
    }
    unit(pending);
  }

  const result = {
    mode,
    seed,
    computationsAtMount: store.counters.created,
    computationRuns: store.counters.runs - mountRuns,
    applyMs: round(applyMs),
    applyMsPer1kFrames: round((applyMs / BURST_FRAMES) * 1000),
    longestUnitMs: round(longestUnitMs),
    applyUnits,
    framesDropped,
  };
  store.dispose();
  return result;
}

const round = (value) => Math.round(value * 100) / 100;
const median = (values) => {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
};

function summarise(runs) {
  const pick = (key) => runs.map((r) => r[key]);
  return {
    computationsAtMount: runs[0].computationsAtMount,
    computationRunsMax: Math.max(...pick("computationRuns")),
    computationRunsMedian: median(pick("computationRuns")),
    applyMsPer1kFramesMedian: median(pick("applyMsPer1kFrames")),
    applyMsPer1kFramesMax: Math.max(...pick("applyMsPer1kFrames")),
    longestUnitMsMedian: median(pick("longestUnitMs")),
    longestUnitMsMax: Math.max(...pick("longestUnitMs")),
    applyUnits: [...new Set(pick("applyUnits"))],
    framesDropped: median(pick("framesDropped")),
  };
}

function main() {
  // Warm the JIT on one throwaway burst so the first measured seed is not
  // paying for compilation.
  for (const mode of ["per-send", "per-drain"]) run(mode, 1);

  const modes = ["per-send", "per-drain", "subset-in", "subset-out"];
  const results = {};
  for (const mode of modes) {
    const runs = SEEDS.map((seed) => measure(mode, seed));
    results[mode] = { summary: summarise(runs), runs };
  }

  const drain = results["per-drain"].summary;
  const send = results["per-send"].summary;
  // Timing gates use the median over five seeds of each seed's best of three
  // repeats: the ceilings already carry 20% over the spike's worst case, and a
  // shared runner's one slow sample is noise, not a regression. Counts are
  // deterministic and gate on the maximum.
  const gates = [
    [
      `census is ${CENSUS} computations (a changed mount changes it: update CENSUS in bench.mjs)`,
      drain.computationsAtMount === CENSUS,
    ],
    ["a frame that leaves a row field out clears it", removedKeysAreCleared()],
    [`per-drain computation runs <= ${CEILINGS.computationRuns}`, drain.computationRunsMax <= CEILINGS.computationRuns],
    [`per-drain apply ms per 1k frames <= ${CEILINGS.applyMsPer1kFrames}`, drain.applyMsPer1kFramesMedian <= CEILINGS.applyMsPer1kFrames],
    [`per-drain longest apply unit <= ${CEILINGS.longestUnitMs} ms`, drain.longestUnitMsMedian <= CEILINGS.longestUnitMs],
    [`per-drain apply units == ${CEILINGS.applyUnits}`, drain.applyUnits.length === 1 && drain.applyUnits[0] === CEILINGS.applyUnits],
    [`subset-out drops the unsubscribed hot section`, results["subset-out"].summary.framesDropped > 0],
    [`per-send fails the computation-run gate`, send.computationRunsMedian > CEILINGS.computationRuns],
    [`per-send fails the apply-time gate`, send.applyMsPer1kFramesMedian > CEILINGS.applyMsPer1kFrames],
  ];

  const report = { node: process.version, apply: APPLY, ceilings: CEILINGS, gates: Object.fromEntries(gates), results };
  writeFileSync(new URL("./results.json", import.meta.url), `${JSON.stringify(report, null, 2)}\n`);

  console.log(`apply: ${APPLY}`);
  console.log("mode        runs(med/max)   ms/1k(med/max)   longest(med/max)  units  dropped");
  for (const mode of modes) {
    const s = results[mode].summary;
    console.log(
      `${mode.padEnd(11)} ${String(s.computationRunsMedian).padStart(6)}/${String(s.computationRunsMax).padEnd(7)}` +
        `  ${String(s.applyMsPer1kFramesMedian).padStart(6)}/${String(s.applyMsPer1kFramesMax).padEnd(8)}` +
        `  ${String(s.longestUnitMsMedian).padStart(5)}/${String(s.longestUnitMsMax).padEnd(10)}` +
        `  ${String(s.applyUnits).padStart(5)}  ${s.framesDropped}`,
    );
  }
  let failed = false;
  for (const [name, ok] of gates) {
    console.log(`${ok ? "PASS" : "FAIL"}  ${name}`);
    failed ||= !ok;
  }
  process.exit(failed ? 1 : 0);
}

main();
