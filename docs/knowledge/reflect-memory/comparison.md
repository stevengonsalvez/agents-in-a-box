---
title: "Why build, not adopt — the agent-memory landscape"
description: "Eight coding-agent memory systems (Hindsight, Mem0, ByteRover, claude-mem, agentmemory, Honcho, OpenViking) compared on four deciding axes, with a LOCOMO benchmark and an honest build-vs-adopt verdict for reflect."
---

> Eight coding-agent memory systems, four deciding axes, and an honest build-vs-adopt verdict. The
> short version: the tools that need no second API key tend to hoard noise; the ones that curate well
> make you run a server and pay a second provider; and none capture corrections, tests, git events or
> skill upgrades as typed signals. reflect is the row that holds all four.

:::tip[Interactive version]
A richer, standalone version of this page (option cards, scored matrix, the full critique) is published at
**[explainers.stevengonsalvez.com/agent-memory-landscape](https://explainers.stevengonsalvez.com/agent-memory-landscape/)**.
For the newcomer framing see [Problem & fit](/knowledge/reflect-memory/problem-and-fit/).
:::

## The problem: context engineering, not a bigger file

Every coding harness gives you one persistent-memory primitive: a static instructions file you
maintain by hand (`CLAUDE.md`, `AGENTS.md`, `MEMORY.md`), front-loaded into the context window every
session. The window is finite and **shared with the task** — every line you front-load "just in case"
is bloat the moment this session doesn't need it. The real problem is **retrieval**: store the
signal-bearing moments and query the right one at the right time. That's why a class of "agent memory"
products exists. This page compares them and explains why we built rather than adopted.

## The four deciding axes

1. **Store everything vs selectively** — whole-session / fire-hose capture is useful but accretes
   noise and unbounded data over time.
2. **Separate LLM/embedding key** — your coding agent already has a model + subscription; most memory
   tools make you configure and pay for a *second* provider just for memory.
3. **Infrastructure** — an always-on server (Postgres, Redis, a Rust/Bun daemon, a vector DB) is
   operational weight, and often cloud-bound.
4. **First-class coding-signal capture** — corrections, test outcomes, git events, skill upgrades
   captured *structurally*, or only as prose an LLM might (or might not) extract?

## The eight systems

We score each system on the four facets shown as bars on every card; the **weighted total** also
folds in two more criteria (selective capture / noise-control and license / portability).

- **Signals** — captures coding signals (corrections, tests, git, skills) as *typed* events, or hopes an LLM extracts them from prose?
- **No key** — runs on the agent's own model, or needs a *second* LLM/embedding key + subscription?
- **Local** — runs from local files, or needs an always-on server / cloud?
- **Retrieval** — hybrid + rerank + graph + temporal quality.

<div class="rm-opts">

  <article class="rm-opt me">
    <span class="lab">Built</span>
    <h4>reflect</h4>
    <div class="lic">MIT · local-first · cross-harness</div>
    <p class="desc">Selective learnings; markdown is the source of truth. Reuses the agent's own model (<code>claude -p</code>) + a local embedder — no extra key. First-class typed signals.</p>
    <div class="bars">
      <div class="brow"><span class="bl">Signals</span><span class="track"><span class="fill" style="width:100%"></span></span><span class="bv">5</span></div>
      <div class="brow"><span class="bl">No key</span><span class="track"><span class="fill" style="width:100%"></span></span><span class="bv">5</span></div>
      <div class="brow"><span class="bl">Local</span><span class="track"><span class="fill" style="width:100%"></span></span><span class="bv">5</span></div>
      <div class="brow"><span class="bl">Retrieval</span><span class="track"><span class="fill" style="width:80%"></span></span><span class="bv">4</span></div>
    </div>
    <div class="tot"><span class="tl">weighted</span><span class="tn">4.72<small> / 5</small></span></div>
  </article>

  <article class="rm-opt">
    <span class="lab">Strongest adopt</span>
    <h4><a href="https://github.com/vectorize-io/hindsight">Hindsight</a></h4>
    <div class="lic">MIT · vectorize-io</div>
    <p class="desc">LLM-extracted facts + mental models. Best-in-class multi-strategy retrieval (94.6% LongMemEval), ~48 integrations. <code>retain</code> needs an LLM; reuse-your-sub loopback is ToS personal-use-only.</p>
    <div class="bars">
      <div class="brow"><span class="bl">Signals</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">No key</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">Local</span><span class="track"><span class="fill" style="width:60%"></span></span><span class="bv">3</span></div>
      <div class="brow"><span class="bl">Retrieval</span><span class="track"><span class="fill" style="width:100%"></span></span><span class="bv">5</span></div>
    </div>
    <div class="tot"><span class="tl">weighted</span><span class="tn">3.03<small> / 5</small></span></div>
  </article>

  <article class="rm-opt">
    <span class="lab">Closest on cost</span>
    <h4><a href="https://github.com/thedotmack/claude-mem">claude-mem</a></h4>
    <div class="lic">Apache-2.0 · thedotmack</div>
    <p class="desc">Compresses every tool call into observations. Reuses Claude auth + local embeddings (no extra key — ties reflect here). But unbounded growth, no native prune, ChromaDB fragility, probabilistic extraction.</p>
    <div class="bars">
      <div class="brow"><span class="bl">Signals</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">No key</span><span class="track"><span class="fill" style="width:100%"></span></span><span class="bv">5</span></div>
      <div class="brow"><span class="bl">Local</span><span class="track"><span class="fill" style="width:60%"></span></span><span class="bv">3</span></div>
      <div class="brow"><span class="bl">Retrieval</span><span class="track"><span class="fill" style="width:60%"></span></span><span class="bv">3</span></div>
    </div>
    <div class="tot"><span class="tl">weighted</span><span class="tn">3.08<small> / 5</small></span></div>
  </article>

  <article class="rm-opt">
    <span class="lab">Curated tree</span>
    <h4><a href="https://github.com/campfirein/byterover-cli">ByteRover</a></h4>
    <div class="lic">Elastic-2.0 (not OSS)</div>
    <p class="desc">Curate-before-store markdown tree, local-first, importance decay. But curation makes its own LLM calls and is <em>agent-directed</em>, not passive — nothing captured unless the agent calls <code>curate</code>.</p>
    <div class="bars">
      <div class="brow"><span class="bl">Signals</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">No key</span><span class="track"><span class="fill" style="width:60%"></span></span><span class="bv">3</span></div>
      <div class="brow"><span class="bl">Local</span><span class="track"><span class="fill" style="width:80%"></span></span><span class="bv">4</span></div>
      <div class="brow"><span class="bl">Retrieval</span><span class="track"><span class="fill" style="width:60%"></span></span><span class="bv">3</span></div>
    </div>
    <div class="tot"><span class="tl">weighted</span><span class="tn">3.06<small> / 5</small></span></div>
  </article>

  <article class="rm-opt">
    <span class="lab">Fact extractor</span>
    <h4><a href="https://github.com/mem0ai/mem0">Mem0</a></h4>
    <div class="lic">Apache-2.0 + SaaS</div>
    <p class="desc">LLM-extracted facts; OpenAI by default at ingest (separate key). Full stack is Postgres + Neo4j; graph memory gated to the $249/mo Pro tier. Documented dedup gap.</p>
    <div class="bars">
      <div class="brow"><span class="bl">Signals</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">No key</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">Local</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">Retrieval</span><span class="track"><span class="fill" style="width:80%"></span></span><span class="bv">4</span></div>
    </div>
    <div class="tot"><span class="tl">weighted</span><span class="tn">2.50<small> / 5</small></span></div>
  </article>

  <article class="rm-opt">
    <span class="lab">Fire-hose</span>
    <h4><a href="https://github.com/rohitg00/agentmemory">agentmemory</a></h4>
    <div class="lic">Apache-2.0 · rohitg00</div>
    <p class="desc">Captures every tool call verbatim, then consolidates via opt-in LLM. Always-on Rust daemon (4 ports, pinned binary). O(N²) forget ceiling ~1k; documented token-burn incidents.</p>
    <div class="bars">
      <div class="brow"><span class="bl">Signals</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">No key</span><span class="track"><span class="fill" style="width:60%"></span></span><span class="bv">3</span></div>
      <div class="brow"><span class="bl">Local</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">Retrieval</span><span class="track"><span class="fill" style="width:60%"></span></span><span class="bv">3</span></div>
    </div>
    <div class="tot"><span class="tl">weighted</span><span class="tn">2.28<small> / 5</small></span></div>
  </article>

  <article class="rm-opt">
    <span class="lab">Personalization</span>
    <h4><a href="https://github.com/plastic-labs/honcho">Honcho</a></h4>
    <div class="lic">AGPL-3.0 · Plastic Labs</div>
    <p class="desc">Theory-of-mind models of users/peers — built for end-user personalization, not coding signals. Self-host needs 1–3 provider keys + Postgres + Redis + a deriver worker; cloud is per-query.</p>
    <div class="bars">
      <div class="brow"><span class="bl">Signals</span><span class="track"><span class="fill" style="width:20%"></span></span><span class="bv">1</span></div>
      <div class="brow"><span class="bl">No key</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">Local</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">Retrieval</span><span class="track"><span class="fill" style="width:80%"></span></span><span class="bv">4</span></div>
    </div>
    <div class="tot"><span class="tl">weighted</span><span class="tn">1.99<small> / 5</small></span></div>
  </article>

  <article class="rm-opt">
    <span class="lab">Context DB</span>
    <h4><a href="https://github.com/volcengine/OpenViking">OpenViking</a></h4>
    <div class="lic">AGPL-3.0 · ByteDance</div>
    <p class="desc">Tiered (L0/L1/L2) LLM-extracted memories/skills with conflict-aware dedup. Strong on noise — but OpenAI/Volcengine by default (separate key) and a Rust+Go+C++ always-on server.</p>
    <div class="bars">
      <div class="brow"><span class="bl">Signals</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">No key</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">Local</span><span class="track"><span class="fill" style="width:40%"></span></span><span class="bv">2</span></div>
      <div class="brow"><span class="bl">Retrieval</span><span class="track"><span class="fill" style="width:80%"></span></span><span class="bv">4</span></div>
    </div>
    <div class="tot"><span class="tl">weighted</span><span class="tn">2.56<small> / 5</small></span></div>
  </article>

</div>

¹ Hindsight's `retain` needs an LLM; its "reuse your Claude subscription" loopback is documented as
**personal-use-only per Anthropic's terms** — not shippable as a default.

The pattern: the only other system that matches reflect on *no extra key* (claude-mem) collapses on
*selective/noise*; the systems that curate well (Hindsight, OpenViking, ByteRover) lose on *no extra
key* or *local-first*; and **first-class signal capture is a wall of "no."**

## Scored matrix

Weighted to a **coding-agent self-improvement** workflow (capture corrections → behaviour, cheaply,
no infra). Darker green = stronger fit, rust = weak; bottom row is the weighted total out of 5.
Re-weight toward pure retrieval quality and Hindsight leads — see the sensitivity note below.

<div class="rm-matrix">
<table>
<thead><tr><th class="rowh">Criterion (weight)</th><th>reflect</th><th>Hindsight</th><th>claude-mem</th><th>ByteRover</th><th>Mem0</th><th>agentmem</th><th>Honcho</th><th>OpenViking</th></tr></thead>
<tbody>
<tr><td class="rowh">Signal capture (0.25)</td><td class="s5">5</td><td class="s2">2</td><td class="s2">2</td><td class="s2">2</td><td class="s2">2</td><td class="s2">2</td><td class="s1">1</td><td class="s2">2</td></tr>
<tr><td class="rowh">No extra key (0.22)</td><td class="s5">5</td><td class="s2">2</td><td class="s5">5</td><td class="s3">3</td><td class="s2">2</td><td class="s3">3</td><td class="s2">2</td><td class="s2">2</td></tr>
<tr><td class="rowh">Local-first (0.20)</td><td class="s5">5</td><td class="s3">3</td><td class="s3">3</td><td class="s4">4</td><td class="s2">2</td><td class="s2">2</td><td class="s2">2</td><td class="s2">2</td></tr>
<tr><td class="rowh">Selective / noise (0.16)</td><td class="s4">4</td><td class="s4">4</td><td class="s2">2</td><td class="s4">4</td><td class="s3">3</td><td class="s1">1</td><td class="s2">2</td><td class="s4">4</td></tr>
<tr><td class="rowh">Retrieval quality (0.12)</td><td class="s4">4</td><td class="s5">5</td><td class="s3">3</td><td class="s3">3</td><td class="s4">4</td><td class="s3">3</td><td class="s4">4</td><td class="s4">4</td></tr>
<tr><td class="rowh">License / portability (0.05)</td><td class="s5">5</td><td class="s5">5</td><td class="s4">4</td><td class="s2">2</td><td class="s4">4</td><td class="s4">4</td><td class="s2">2</td><td class="s2">2</td></tr>
<tr class="tot"><td class="rowh">Weighted total</td><td>4.72</td><td>3.03</td><td>3.08</td><td>3.06</td><td>2.50</td><td>2.28</td><td>1.99</td><td>2.56</td></tr>
</tbody>
</table>
</div>

## How reflect works

![Timeline of one coding session and where reflect operates — recall at SessionStart and each prompt; typed signals + capture at tool calls, Stop and PreCompact; the index closing the loop to the next session](../../assets/reflect-session-timeline.svg)

![reflect component topology — local by default (QMD sqlite + nano-graphrag), optional shared Postgres; the markdown KB stays the source of truth either way](../../assets/reflect-topology.svg)

## Benchmark — LOCOMO

reflect 4.1.0 on [LOCOMO](https://github.com/snap-research/locomo) (long-term conversational memory).
**Preliminary**: a category-stratified pilot graded by an **Opus** reference LLM-judge. Retrieval runs
reflect's real engine; the dialogue→note extraction is a documented LOCOMO-domain adapter.

<div class="rm-matrix">
<table>
<thead><tr><th class="rowh">config · Opus judge</th><th>single-hop</th><th>multi-hop</th><th>temporal</th><th>open-domain</th><th>adversarial</th><th>overall</th></tr></thead>
<tbody>
<tr class="me"><td class="rowh">reflect 4.1.0 + retrieval fixes</td><td class="s4">0.80</td><td class="s4">0.80</td><td class="s4">0.80</td><td class="s3">0.70</td><td class="s5">0.90</td><td class="s4"><strong>0.80</strong></td></tr>
</tbody>
</table>
</div>

The retrieval fixes are two additive, env-gated, **zero-new-API-key** knobs: a stronger local embedder
(`REFLECT_EMBED_MODEL=BAAI/bge-base-en-v1.5`) and **HyDE** query-expansion (`REFLECT_RECALL_HYDE=1`,
reusing reflect's own `claude -p`). Both default off — shipped behaviour is unchanged.

![LOCOMO positioning — reflect vs other memory systems](../../assets/locomo-positioning.png)

reflect lands mid-field — on par with Memobase / Zep, above Mem0 — while the newest systems (ByteRover,
Honcho, Hindsight) sit higher but are self-reported on their own harnesses. Judges and harnesses differ
across the field, so treat this as **directional placement, not a strict ranking**. Full methodology:
[`tests/eval/locomo/REPORT.md`](https://github.com/stevengonsalvez/ainb-reflect-memory/blob/main/tests/eval/locomo/REPORT.md).

## Token economics — both sides of the ledger

Memory looks cheap if you only price retrieval. The real cost is the **write side** (LLM
extraction/consolidation) plus the **read side** (context injection).

<div class="rm-matrix">
<table>
<thead><tr><th class="rowh">System</th><th class="wrap">Write side</th><th class="wrap">Read side</th><th class="wrap">Net extra spend</th></tr></thead>
<tbody>
<tr class="me"><td class="rowh">reflect</td><td class="wrap">reuses harness LLM (<code>claude -p</code>), gated + queued; local embed</td><td class="wrap">local — vector + BM25 + graph, no LLM</td><td class="wrap"><strong>$0 extra</strong> (rides your existing sub)</td></tr>
<tr><td class="rowh">claude-mem</td><td class="wrap">Haiku on your Claude auth, per tool-call batch</td><td class="wrap">FTS5 + local ONNX vectors</td><td class="wrap">$0 extra, but high write volume</td></tr>
<tr><td class="rowh">Hindsight</td><td class="wrap"><code>retain</code> = LLM extraction; cloud $15/M tokens</td><td class="wrap">vector/graph; synthesis costs</td><td class="wrap">separate key or $15/M retain</td></tr>
<tr><td class="rowh">Mem0</td><td class="wrap">OpenAI extraction at ingest</td><td class="wrap">vector/BM25; graph = Pro $249/mo</td><td class="wrap">separate OpenAI key + tier</td></tr>
<tr><td class="rowh">OpenViking</td><td class="wrap">VLM extraction + L0/L1 summaries</td><td class="wrap">vector recursive (no LLM)</td><td class="wrap">separate provider key</td></tr>
<tr><td class="rowh">Honcho</td><td class="wrap">deriver LLM ("dreaming") per batch</td><td class="wrap"><code>context()</code> free; dialectic up to $0.50/query</td><td class="wrap">1–3 keys (self-host) or per-query</td></tr>
<tr><td class="rowh">agentmemory</td><td class="wrap">optional LLM compression (off by default)</td><td class="wrap">local embed; triple-stream RRF</td><td class="wrap">$0 if LLM off — value degrades</td></tr>
<tr><td class="rowh">ByteRover</td><td class="wrap">curation = own LLM calls</td><td class="wrap">BM25 → LLM fallback</td><td class="wrap">own key tokens or Pro $19/mo</td></tr>
</tbody>
</table>
</div>

:::caution[Dangerous default to watch]
**Auto-retain every turn + large auto-recall** is how memory tools quietly burn a subscription —
agentmemory has documented incidents of exhausting a Claude Pro quota in a handful of messages.
reflect's capture is **gated and queued** (not every turn), and recall is **OOD-gated +
token-budgeted** — it injects nothing when nothing fits.
:::

## Observability & correction

When the memory is wrong, can you see it, edit it, delete it, and trace where it came from?

- **reflect** — learnings are plain markdown you open, edit or delete in your editor; volatile signals
  live in a DB sidecar (clean git diffs); each learning links back to its source transcript + chunk;
  per-row TTL + contradiction handling expire stale beliefs.
- **Most others** — facts live in Postgres + vector DBs, inspected via API or SQL, not your editor.
  claude-mem ships a web viewer but observations are DB rows; ByteRover is the exception (an editable
  markdown tree). Correcting a wrong memory usually means an API call or a re-curation pass.

## Self-improvement — how a correction becomes behaviour

This is the wedge. In every other system a correction is, at best, a sentence in a transcript an
extraction LLM *might* turn into a fact — Hindsight literally files capturing a skill update as a
*bug*. reflect routes it as **typed signals** at hook time (`SG1`–`SG8`: contradiction, git
commit/revert, test pass/fail, tool-loop, permission reply, idle sweep, negative-recall gaps) and
**auto-refreshes the skills those learnings affect** (`R13`/`R14`).

> Everyone else remembers **what was said**. reflect captures **what changed** — and turns it into the
> next session's behaviour.

## Recommendation & decision rule

:::note[Decision rule]
Adopt an external memory provider **only if** it (a) needs no second API key or subscription, (b) runs
without a mandatory always-on server, and (c) captures coding signals as typed events. **No single
external tool clears all three.** So: build the thin, differentiated layer (capture + typed signals +
local-first), and **port — don't re-invent forever — the commodity retrieval**, keeping it behind a
seam so a stronger backend can be swapped in later.
:::

**Verdict: build + port, with eyes open.** reflect ported Hindsight's retrieval ideas
(graph-expansion arm, RRF fusion, cross-encoder rerank, temporal arm) across a
[57-port effort](/knowledge/reflect-memory/recall/) — so it has the retrieval brains without the
infra/LLM/ToS baggage, at the cost of owning that code.

:::caution[Honest sensitivity note]
These weights favour self-improvement and zero-friction operation — reflect's strengths. Re-weight
toward **pure retrieval quality and ecosystem maturity** and **Hindsight wins**: MIT, production-grade,
94.6% LongMemEval, ~48 integrations, 62 releases. If your priority is "best recall, least code I
maintain," adopting Hindsight (as a retrieval backend) is the smarter call. The build case rests on
valuing *no-extra-key capture* and *typed signals* — which is why retrieval is kept swappable.
:::

## If you'd adopt instead — four pilot acceptance tests

1. **Zero extra credentials** — install on a fresh machine with only the coding agent's existing auth.
   Does capture + recall work with no new key or account? *(reflect / claude-mem: pass · most: fail)*
2. **No always-on server** — reboot. Does memory work with nothing running in the background?
   *(reflect: pass · daemon-based tools: fail)*
3. **Correction → behaviour** — correct the agent once; next session, does the rule resurface
   *without* manual curation? *(the typed-signal test — reflect's wedge)*
4. **Noise over 30 days** — run daily for a month. Is recall still sharp, or flooded with stale /
   duplicate / fire-hosed entries? *(fire-hose tools degrade here)*
