---
title: "Hooks & Platform · how reflect captures and recalls across Claude + Codex"
description: "Visual deep-dive into the reflect plugin's hook architecture, recall flows, capture flows, status line integration, and cross-tool drain — across Claude Code and Codex CLI."
---

> **The short version** — Two harnesses (Claude Code, Codex CLI) wire the same hook scripts
> into different config files (`~/.claude/settings.json` vs `~/.codex/hooks.json`) and share
> one on-disk knowledge base (`~/.reflect/` queue + `~/.learnings/` documents + GraphRAG
> index). **SessionStart** fires the baseline recall + the bg-drainer; **UserPromptSubmit**
> fires the intent-sharp recall with per-session dedupe; **PreCompact**, **Stop**, and
> **PostToolUse** capture learnings into the shared store. A codex session can enqueue a
> reflection that a later Claude session drains — and vice versa.

---

## Architecture at a glance

Two harnesses, the same hook scripts, one shared knowledge base. Solid arrows are control
flow; dashed clay arrows are recall (read into context); dashed olive arrows are capture
(write to disk).

<div class="svg-wrap">
      <svg viewBox="0 0 1080 900" xmlns="http://www.w3.org/2000/svg" style="width:100%; height:auto; font-family: system-ui, -apple-system, 'Segoe UI', sans-serif;">
        <defs>
          <marker id="arrowhead" markerWidth="9" markerHeight="9" refX="8" refY="4.5" orient="auto">
            <polygon points="0 0, 9 4.5, 0 9" fill="#3D3D3A"/>
          </marker>
          <marker id="arrowhead-clay" markerWidth="9" markerHeight="9" refX="8" refY="4.5" orient="auto">
            <polygon points="0 0, 9 4.5, 0 9" fill="#D97757"/>
          </marker>
          <marker id="arrowhead-olive" markerWidth="9" markerHeight="9" refX="8" refY="4.5" orient="auto">
            <polygon points="0 0, 9 4.5, 0 9" fill="#788C5D"/>
          </marker>
        </defs>

        <rect x="0" y="0" width="1080" height="160" fill="#FAF9F5"/>
        <rect x="0" y="160" width="1080" height="220" fill="#FFFFFF"/>
        <rect x="0" y="380" width="1080" height="260" fill="#FAF9F5"/>
        <rect x="0" y="640" width="1080" height="260" fill="#FFFFFF"/>

        <text x="12" y="90" font-size="10" font-family="ui-monospace, monospace" fill="#87867F" letter-spacing="0.12em">HARNESS</text>
        <text x="12" y="270" font-size="10" font-family="ui-monospace, monospace" fill="#87867F" letter-spacing="0.12em">HOOK&#160;SCRIPTS</text>
        <text x="12" y="510" font-size="10" font-family="ui-monospace, monospace" fill="#87867F" letter-spacing="0.12em">SHARED&#160;STATE</text>
        <text x="12" y="770" font-size="10" font-family="ui-monospace, monospace" fill="#87867F" letter-spacing="0.12em">HEADLESS</text>

        <!-- ============ HARNESS BAND ============ -->
        <text x="290" y="22" font-size="11" font-family="ui-monospace, monospace" fill="#788C5D" text-anchor="middle">fires SessionStart · fires PreCompact</text>
        <text x="790" y="22" font-size="11" font-family="ui-monospace, monospace" fill="#788C5D" text-anchor="middle">fires SessionStart · fires PreCompact</text>

        <g>
          <rect x="100" y="30" width="380" height="110" rx="12" ry="12" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.5"/>
          <text x="290" y="58" font-size="15" font-weight="600" fill="#141413" text-anchor="middle">Claude Code session</text>
          <text x="290" y="80" font-size="12" fill="#3D3D3A" text-anchor="middle">reads hooks from <tspan font-family="ui-monospace, monospace" fill="#141413">~/.claude/settings.json</tspan></text>
          <text x="290" y="100" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">wired by plugin.json autowire (CLAUDE_PLUGIN_ROOT)</text>
          <text x="290" y="122" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">— OR — manual claude_adapter.py install</text>
        </g>

        <g>
          <rect x="600" y="30" width="380" height="110" rx="12" ry="12" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.5"/>
          <text x="790" y="58" font-size="15" font-weight="600" fill="#141413" text-anchor="middle">Codex CLI session</text>
          <text x="790" y="80" font-size="12" fill="#3D3D3A" text-anchor="middle">reads hooks from <tspan font-family="ui-monospace, monospace" fill="#141413">~/.codex/hooks.json</tspan></text>
          <text x="790" y="100" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">wired by codex_adapter.py install</text>
          <text x="790" y="122" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">(no plugin runtime — adapter autowires itself)</text>
        </g>

        <!-- ============ HOOK SCRIPTS BAND ============ -->
        <g>
          <rect x="70" y="200" width="280" height="130" rx="12" ry="12" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.5"/>
          <text x="210" y="226" font-size="14" font-weight="600" fill="#141413" text-anchor="middle">session_start_recall.py</text>
          <text x="210" y="244" font-size="11" fill="#3D3D3A" text-anchor="middle">fired by: SessionStart</text>
          <text x="210" y="265" font-size="12" fill="#141413" text-anchor="middle" font-style="italic">"what learnings apply here?"</text>
          <text x="210" y="288" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">builds query from cwd · branch</text>
          <text x="210" y="304" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">git log · returns top-3</text>
          <text x="210" y="320" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">via additionalContext</text>
        </g>

        <g>
          <rect x="400" y="200" width="280" height="130" rx="12" ry="12" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.5"/>
          <text x="540" y="226" font-size="14" font-weight="600" fill="#141413" text-anchor="middle">precompact_reflect.py</text>
          <text x="540" y="244" font-size="11" fill="#3D3D3A" text-anchor="middle">fired by: PreCompact</text>
          <text x="540" y="265" font-size="12" fill="#141413" text-anchor="middle" font-style="italic">"save this transcript for later"</text>
          <text x="540" y="288" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">never blocks compaction</text>
          <text x="540" y="304" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">append-only enqueue;</text>
          <text x="540" y="320" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">reflection happens later</text>
        </g>

        <g>
          <rect x="730" y="200" width="280" height="130" rx="12" ry="12" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.5"/>
          <text x="870" y="226" font-size="14" font-weight="600" fill="#141413" text-anchor="middle">reflect-drain-bg.sh</text>
          <text x="870" y="244" font-size="11" fill="#3D3D3A" text-anchor="middle">fired by: SessionStart</text>
          <text x="870" y="265" font-size="12" fill="#141413" text-anchor="middle" font-style="italic">"process any queued reflections"</text>
          <text x="870" y="288" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">detached (nohup &amp;)</text>
          <text x="870" y="304" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">PID-locked · daily-capped</text>
          <text x="870" y="320" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">shells out to claude -p</text>
        </g>

        <!-- ============ SHARED STATE BAND ============ -->
        <g>
          <rect x="100" y="420" width="280" height="190" rx="12" ry="12" fill="#F4E4C1" stroke="#3D3D3A" stroke-width="1.5"/>
          <text x="240" y="448" font-size="14" font-weight="600" fill="#141413" text-anchor="middle">Pending queue</text>
          <text x="240" y="470" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">~/.reflect/pending_reflections.jsonl</text>
          <line x1="120" y1="486" x2="360" y2="486" stroke="#3D3D3A" stroke-width="0.5" stroke-dasharray="3,3"/>
          <text x="240" y="508" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">one line per queued transcript</text>
          <text x="240" y="525" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">{transcript_path, session_id,</text>
          <text x="240" y="540" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">trigger, harness, queued_at}</text>
          <text x="240" y="572" font-size="11" fill="#3D3D3A" text-anchor="middle" font-style="italic">harness-agnostic — any harness</text>
          <text x="240" y="589" font-size="11" fill="#3D3D3A" text-anchor="middle" font-style="italic">writes, any harness drains</text>
        </g>

        <g>
          <rect x="400" y="420" width="280" height="190" rx="12" ry="12" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.5"/>
          <text x="540" y="448" font-size="14" font-weight="600" fill="#141413" text-anchor="middle">Learnings store</text>
          <text x="540" y="470" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">~/.learnings/documents/</text>
          <line x1="420" y1="486" x2="660" y2="486" stroke="#3D3D3A" stroke-width="0.5" stroke-dasharray="3,3"/>
          <text x="540" y="510" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">&lt;slug&gt;.md  (the learning)</text>
          <text x="540" y="527" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">&lt;slug&gt;.entities.yaml (sidecar</text>
          <text x="540" y="544" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">for GraphRAG ingest)</text>
          <text x="540" y="576" font-size="11" fill="#3D3D3A" text-anchor="middle" font-style="italic">written by the headless</text>
          <text x="540" y="593" font-size="11" fill="#3D3D3A" text-anchor="middle" font-style="italic">/reflect run</text>
        </g>

        <g>
          <rect x="700" y="420" width="280" height="190" rx="12" ry="12" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.5"/>
          <text x="840" y="448" font-size="14" font-weight="600" fill="#141413" text-anchor="middle">GraphRAG + vector index</text>
          <text x="840" y="470" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">~/.learnings/graphrag/</text>
          <line x1="720" y1="486" x2="960" y2="486" stroke="#3D3D3A" stroke-width="0.5" stroke-dasharray="3,3"/>
          <text x="840" y="510" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">communities · entities · relations</text>
          <text x="840" y="527" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">nano-vector store (hnswlib)</text>
          <text x="840" y="559" font-size="11" fill="#3D3D3A" text-anchor="middle" font-style="italic">queried by recall · refreshed</text>
          <text x="840" y="576" font-size="11" fill="#3D3D3A" text-anchor="middle" font-style="italic">by `reflect reindex` after each</text>
          <text x="840" y="593" font-size="11" fill="#3D3D3A" text-anchor="middle" font-style="italic">successful drain</text>
        </g>

        <!-- ============ HEADLESS BAND ============ -->
        <g>
          <rect x="320" y="685" width="440" height="160" rx="12" ry="12" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.5"/>
          <text x="540" y="712" font-size="14" font-weight="600" fill="#141413" text-anchor="middle">claude -p "/reflect &lt;transcript&gt;"</text>
          <text x="540" y="734" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">spawned by reflect-drain-bg.sh</text>
          <text x="540" y="756" font-size="12" fill="#141413" text-anchor="middle" font-style="italic">"extract the learnings from this transcript"</text>
          <text x="540" y="782" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">--output-format json · --max-turns 25</text>
          <text x="540" y="799" font-size="11" font-family="ui-monospace, monospace" fill="#3D3D3A" text-anchor="middle">--permission-mode bypassPermissions</text>
          <text x="540" y="823" font-size="11" fill="#3D3D3A" text-anchor="middle" font-style="italic">always claude — even when a codex session triggered the drain</text>
        </g>

        <!-- ============ ARROWS ============ -->

        <!-- 1: SessionStart → recall (both harnesses) -->
        <line x1="240" y1="140" x2="190" y2="200" stroke="#3D3D3A" stroke-width="2" marker-end="url(#arrowhead)"/>
        <line x1="700" y1="140" x2="260" y2="200" stroke="#3D3D3A" stroke-width="2" marker-end="url(#arrowhead)"/>
        <circle cx="230" cy="170" r="12" fill="#D97757" stroke="#141413" stroke-width="1"/>
        <text x="230" y="174" font-size="11" font-weight="700" fill="#fff" text-anchor="middle">1</text>

        <!-- 2: recall → GraphRAG read -->
        <path d="M 290 330 Q 360 380 730 420" fill="none" stroke="#3D3D3A" stroke-width="2" marker-end="url(#arrowhead)"/>
        <circle cx="495" cy="372" r="12" fill="#D97757" stroke="#141413" stroke-width="1"/>
        <text x="495" y="376" font-size="11" font-weight="700" fill="#fff" text-anchor="middle">2</text>

        <!-- 3a: GraphRAG → recall (dashed clay) -->
        <path d="M 720 440 Q 480 350 240 330" fill="none" stroke="#D97757" stroke-width="2" stroke-dasharray="6,4" marker-end="url(#arrowhead-clay)"/>
        <circle cx="540" cy="358" r="12" fill="#D97757" stroke="#141413" stroke-width="1"/>
        <text x="540" y="362" font-size="11" font-weight="700" fill="#fff" text-anchor="middle">3</text>
        <text x="570" y="345" font-size="10" font-family="ui-monospace, monospace" fill="#D97757">top-3 learnings</text>

        <!-- 3b: recall → session (additionalContext) -->
        <line x1="160" y1="200" x2="220" y2="140" stroke="#D97757" stroke-width="2" stroke-dasharray="6,4" marker-end="url(#arrowhead-clay)"/>
        <text x="60" y="195" font-size="10" font-family="ui-monospace, monospace" fill="#D97757">additionalContext</text>

        <!-- 4: PreCompact → precompact_reflect -->
        <line x1="330" y1="140" x2="470" y2="200" stroke="#3D3D3A" stroke-width="2" marker-end="url(#arrowhead)"/>
        <line x1="780" y1="140" x2="600" y2="200" stroke="#3D3D3A" stroke-width="2" marker-end="url(#arrowhead)"/>
        <circle cx="540" cy="170" r="12" fill="#788C5D" stroke="#141413" stroke-width="1"/>
        <text x="540" y="174" font-size="11" font-weight="700" fill="#fff" text-anchor="middle">4</text>

        <!-- 5: precompact → queue write -->
        <line x1="500" y1="330" x2="290" y2="420" stroke="#788C5D" stroke-width="2" stroke-dasharray="6,3" marker-end="url(#arrowhead-olive)"/>
        <circle cx="395" cy="375" r="12" fill="#788C5D" stroke="#141413" stroke-width="1"/>
        <text x="395" y="379" font-size="11" font-weight="700" fill="#fff" text-anchor="middle">5</text>
        <text x="265" y="395" font-size="10" font-family="ui-monospace, monospace" fill="#788C5D">enqueue transcript_path</text>

        <!-- 6: SessionStart → drain (both harnesses) -->
        <path d="M 800 140 Q 830 165 870 200" fill="none" stroke="#3D3D3A" stroke-width="2" marker-end="url(#arrowhead)"/>
        <path d="M 360 140 Q 620 160 830 200" fill="none" stroke="#3D3D3A" stroke-width="2" marker-end="url(#arrowhead)"/>
        <circle cx="830" cy="170" r="12" fill="#D97757" stroke="#141413" stroke-width="1"/>
        <text x="830" y="174" font-size="11" font-weight="700" fill="#fff" text-anchor="middle">6</text>

        <!-- 7: drain reads queue -->
        <path d="M 740 330 Q 540 380 360 425" fill="none" stroke="#3D3D3A" stroke-width="2" marker-end="url(#arrowhead)"/>
        <circle cx="560" cy="370" r="12" fill="#D97757" stroke="#141413" stroke-width="1"/>
        <text x="560" y="374" font-size="11" font-weight="700" fill="#fff" text-anchor="middle">7</text>

        <!-- 8: drain spawns claude -p -->
        <line x1="870" y1="330" x2="700" y2="685" stroke="#3D3D3A" stroke-width="2" marker-end="url(#arrowhead)"/>
        <circle cx="800" cy="510" r="12" fill="#D97757" stroke="#141413" stroke-width="1"/>
        <text x="800" y="514" font-size="11" font-weight="700" fill="#fff" text-anchor="middle">8</text>

        <!-- 9: claude -p writes learnings -->
        <line x1="450" y1="685" x2="540" y2="610" stroke="#788C5D" stroke-width="2" stroke-dasharray="6,3" marker-end="url(#arrowhead-olive)"/>
        <circle cx="495" cy="655" r="12" fill="#788C5D" stroke="#141413" stroke-width="1"/>
        <text x="495" y="659" font-size="11" font-weight="700" fill="#fff" text-anchor="middle">9</text>
        <text x="555" y="650" font-size="10" font-family="ui-monospace, monospace" fill="#788C5D">.md + .entities.yaml</text>

        <!-- 10: reindex updates GraphRAG -->
        <path d="M 680 510 Q 750 540 820 610" fill="none" stroke="#788C5D" stroke-width="2" stroke-dasharray="6,3" marker-end="url(#arrowhead-olive)"/>
        <circle cx="740" cy="545" r="12" fill="#788C5D" stroke="#141413" stroke-width="1"/>
        <text x="740" y="549" font-size="11" font-weight="700" fill="#fff" text-anchor="middle">10</text>
        <text x="710" y="528" font-size="10" font-family="ui-monospace, monospace" fill="#788C5D">reflect reindex</text>

      </svg>
</div>

---

## Session timeline · one coding session, end to end

A horizontal view of what fires when. Hook events show above the spine; data I/O below.

<div class="svg-wrap">
      <svg viewBox="0 0 1120 270" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <marker id="a1" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#3D3D3A"/>
          </marker>
          <marker id="a-clay" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#D97757"/>
          </marker>
          <marker id="a-teal" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#9DD4C7"/>
          </marker>
          <marker id="a-olive" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#788C5D"/>
          </marker>
        </defs>

        <!-- Background bands -->
        <rect x="0" y="0" width="1120" height="270" fill="#FAFAF8"/>

        <!-- Top band: hook labels -->
        <rect x="0" y="0" width="1120" height="54" fill="#F5F4F0"/>
        <text x="8" y="12" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">HOOK FIRES</text>

        <!-- Bottom band: data I/O -->
        <rect x="0" y="196" width="1120" height="74" fill="#F5F4F0"/>
        <text x="8" y="208" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">DATA READ / WRITTEN</text>

        <!-- Timeline spine -->
        <line x1="56" y1="126" x2="1082" y2="126" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a1)"/>

        <!-- ---- Tick positions ---- -->
        <!-- x positions: Install=80, SessionStart=210, Prompts=370, ToolUses=510, CtxFills=640, PreCompact=770, Compact=900, SessionEnd=1040 -->

        <!-- TICK 1: Install -->
        <line x1="80" y1="114" x2="80" y2="138" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="80" cy="126" r="7" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="80" y="162" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle" font-weight="600">Install</text>
        <text x="80" y="175" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">adapter / plugin</text>
        <!-- data below -->
        <text x="80" y="220" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">writes hooks</text>
        <text x="80" y="233" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">to config file</text>

        <!-- TICK 2: SessionStart -->
        <line x1="210" y1="114" x2="210" y2="138" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="210" cy="126" r="7" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="210" y="162" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle" font-weight="600">Session starts</text>
        <text x="210" y="175" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">harness boots</text>
        <!-- hook label above -->
        <line x1="210" y1="114" x2="210" y2="56" stroke="#9DD4C7" stroke-width="1.5" stroke-dasharray="3,3" marker-end="url(#a-teal)"/>
        <rect x="138" y="16" width="144" height="36" rx="6" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1"/>
        <text x="210" y="30" font-size="10" font-weight="600" fill="#141413" text-anchor="middle">SessionStart fires</text>
        <text x="210" y="44" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">recall + drain (bg)</text>
        <!-- data below -->
        <text x="210" y="220" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">reads graphrag/</text>
        <text x="210" y="233" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">injects top-3</text>
        <text x="210" y="246" font-size="10" font-family="ui-monospace,monospace" fill="#9DD4C7" text-anchor="middle">additionalContext</text>

        <!-- TICK 3: User prompts -->
        <line x1="370" y1="114" x2="370" y2="138" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="370" cy="126" r="7" fill="#FAF9F5" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="370" y="162" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle" font-weight="600">User prompts</text>
        <text x="370" y="175" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">conversation</text>
        <!-- no hook -->
        <text x="370" y="36" font-size="9.5" font-family="ui-monospace,monospace" fill="#D1CFC5" text-anchor="middle">—</text>
        <!-- data below -->
        <text x="370" y="220" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">model context</text>
        <text x="370" y="233" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">+ learnings</text>

        <!-- TICK 4: Tool uses -->
        <line x1="510" y1="114" x2="510" y2="138" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="510" cy="126" r="7" fill="#FAF9F5" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="510" y="162" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle" font-weight="600">Tool uses</text>
        <text x="510" y="175" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">edits / runs</text>
        <text x="510" y="36" font-size="9.5" font-family="ui-monospace,monospace" fill="#D1CFC5" text-anchor="middle">—</text>
        <!-- data below -->
        <text x="510" y="220" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">transcript grows;</text>
        <text x="510" y="233" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">~/.claude/ JSONL</text>

        <!-- TICK 5: Context fills -->
        <line x1="640" y1="114" x2="640" y2="138" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="640" cy="126" r="7" fill="#F4E4C1" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="640" y="162" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle" font-weight="600">Context fills</text>
        <text x="640" y="175" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">approaching limit</text>
        <text x="640" y="36" font-size="9.5" font-family="ui-monospace,monospace" fill="#D1CFC5" text-anchor="middle">—</text>
        <!-- data below -->
        <text x="640" y="220" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">harness detects</text>
        <text x="640" y="233" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">token threshold</text>

        <!-- TICK 6: PreCompact -->
        <line x1="770" y1="114" x2="770" y2="138" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="770" cy="126" r="7" fill="#D97757" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="770" y="162" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle" font-weight="600">PreCompact</text>
        <text x="770" y="175" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">hook fires</text>
        <!-- hook label above -->
        <line x1="770" y1="114" x2="770" y2="56" stroke="#D97757" stroke-width="1.5" stroke-dasharray="3,3" marker-end="url(#a-clay)"/>
        <rect x="694" y="16" width="152" height="36" rx="6" fill="#F4E4C1" stroke="#D97757" stroke-width="1"/>
        <text x="770" y="30" font-size="10" font-weight="600" fill="#141413" text-anchor="middle">PreCompact fires</text>
        <text x="770" y="44" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">precompact_reflect.py</text>
        <!-- data below -->
        <text x="770" y="220" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">appends to</text>
        <text x="770" y="233" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">pending_reflections</text>
        <text x="770" y="246" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">.jsonl (queue)</text>

        <!-- TICK 7: Compaction -->
        <line x1="900" y1="114" x2="900" y2="138" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="900" cy="126" r="7" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="900" y="162" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle" font-weight="600">Compaction</text>
        <text x="900" y="175" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">harness compresses</text>
        <text x="900" y="36" font-size="9.5" font-family="ui-monospace,monospace" fill="#D1CFC5" text-anchor="middle">—</text>
        <!-- data below -->
        <text x="900" y="220" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">transcript archived;</text>
        <text x="900" y="233" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">context window reset</text>

        <!-- TICK 8: Session ends -->
        <line x1="1040" y1="114" x2="1040" y2="138" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="1040" cy="126" r="7" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="1040" y="162" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle" font-weight="600">Session ends</text>
        <text x="1040" y="175" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">process exits</text>
        <text x="1040" y="36" font-size="9.5" font-family="ui-monospace,monospace" fill="#D1CFC5" text-anchor="middle">—</text>
        <!-- data below -->
        <text x="1040" y="220" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">queue entry waits;</text>
        <text x="1040" y="233" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">drained next start</text>

        <!-- "time" label on spine -->
        <text x="1094" y="121" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">time</text>
      </svg>
</div>

---

## Storage layers · three tiers, one knowledge base

The pending queue, the learnings store, and the GraphRAG index. Append-only, grep-able,
portable.

<div class="svg-wrap">
      <svg viewBox="0 0 1120 310" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <marker id="b1" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#3D3D3A"/>
          </marker>
          <marker id="b-olive" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#788C5D"/>
          </marker>
          <marker id="b-clay" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#D97757"/>
          </marker>
        </defs>

        <rect x="0" y="0" width="1120" height="310" fill="#FAFAF8"/>

        <!-- Left labels col -->
        <text x="14" y="78" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em" transform="rotate(-90,14,78)" text-anchor="middle">QUEUE</text>
        <text x="14" y="170" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em" transform="rotate(-90,14,170)" text-anchor="middle">DOCS</text>
        <text x="14" y="256" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em" transform="rotate(-90,14,256)" text-anchor="middle">INDEX</text>

        <!-- ====== BAR 1: Pending queue ====== -->
        <!-- Bar body -->
        <rect x="32" y="20" width="780" height="74" rx="10" fill="#F4E4C1" stroke="#3D3D3A" stroke-width="1.5"/>
        <!-- Bar label inside -->
        <text x="52" y="45" font-size="13" font-weight="600" fill="#141413">Pending queue</text>
        <text x="52" y="62" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A">~/.reflect/pending_reflections.jsonl</text>
        <text x="52" y="79" font-size="10.5" font-family="ui-monospace,monospace" fill="#87867F">one line per queued transcript · {transcript_path, session_id, trigger, harness, queued_at}</text>

        <!-- Arrow IN: precompact → bar -->
        <line x1="32" y1="57" x2="16" y2="57" stroke="#788C5D" stroke-width="1.5" marker-end="url(#b-olive)"/>
        <!-- Arrow IN label above bar (left) -->
        <text x="34" y="18" font-size="10" font-family="ui-monospace,monospace" fill="#788C5D">IN: precompact_reflect.py (appends on PreCompact)</text>

        <!-- Arrow OUT: bar → drain -->
        <line x1="812" y1="57" x2="828" y2="57" stroke="#3D3D3A" stroke-width="1.5" marker-end="url(#b1)"/>
        <text x="834" y="61" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">OUT: reflect-drain-bg.sh reads + dequeues</text>

        <!-- Stats block -->
        <rect x="862" y="20" width="242" height="74" rx="8" fill="#FAF9F5" stroke="#D1CFC5" stroke-width="1"/>
        <text x="876" y="40" font-size="10.5" font-weight="600" fill="#141413">append-only</text>
        <text x="876" y="55" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">~1 line / compaction</text>
        <text x="876" y="69" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">harness-agnostic</text>
        <text x="876" y="83" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">any harness writes or drains</text>

        <!-- ====== BAR 2: Learnings store ====== -->
        <rect x="32" y="112" width="780" height="74" rx="10" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="52" y="137" font-size="13" font-weight="600" fill="#141413">Learnings store</text>
        <text x="52" y="154" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A">~/.learnings/documents/  &lt;slug&gt;.md + &lt;slug&gt;.entities.yaml</text>
        <text x="52" y="171" font-size="10.5" font-family="ui-monospace,monospace" fill="#87867F">markdown document + YAML entity sidecar per learning · grep-able on disk</text>

        <!-- Arrow IN -->
        <line x1="32" y1="149" x2="16" y2="149" stroke="#788C5D" stroke-width="1.5" marker-end="url(#b-olive)"/>
        <text x="34" y="110" font-size="10" font-family="ui-monospace,monospace" fill="#788C5D">IN: claude -p /reflect (headless · spawned by drain)</text>

        <!-- Arrow OUT: to graphrag below -->
        <line x1="420" y1="186" x2="420" y2="204" stroke="#3D3D3A" stroke-width="1.5" stroke-dasharray="4,3" marker-end="url(#b1)"/>
        <text x="428" y="200" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">reflect reindex</text>

        <!-- Stats -->
        <rect x="862" y="112" width="242" height="74" rx="8" fill="#FAF9F5" stroke="#D1CFC5" stroke-width="1"/>
        <text x="876" y="132" font-size="10.5" font-weight="600" fill="#141413">markdown + YAML</text>
        <text x="876" y="147" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">one .md per learning</text>
        <text x="876" y="161" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">entity sidecar for graph</text>
        <text x="876" y="175" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">human-readable · git-able</text>

        <!-- ====== BAR 3: GraphRAG ====== -->
        <rect x="32" y="208" width="780" height="74" rx="10" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="52" y="233" font-size="13" font-weight="600" fill="#141413">GraphRAG + vector index</text>
        <text x="52" y="250" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A">~/.learnings/graphrag/   communities · entities · relations · hnswlib vectors</text>
        <text x="52" y="267" font-size="10.5" font-family="ui-monospace,monospace" fill="#87867F">nano-graphrag · vector + entity graph · hybrid search · queried on SessionStart</text>

        <!-- Arrow IN already shown above (reindex arrow) -->
        <line x1="32" y1="245" x2="16" y2="245" stroke="#3D3D3A" stroke-width="1.5" marker-end="url(#b1)"/>
        <text x="34" y="206" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">IN: reflect reindex (after each successful drain)</text>

        <!-- Arrow OUT: to recall -->
        <line x1="812" y1="245" x2="828" y2="245" stroke="#D97757" stroke-width="1.5" marker-end="url(#b-clay)"/>
        <text x="834" y="249" font-size="10" font-family="ui-monospace,monospace" fill="#D97757">OUT: session_start_recall.py queries top-3</text>

        <!-- Stats -->
        <rect x="862" y="208" width="242" height="74" rx="8" fill="#FAF9F5" stroke="#D1CFC5" stroke-width="1"/>
        <text x="876" y="228" font-size="10.5" font-weight="600" fill="#141413">vector + entity graph</text>
        <text x="876" y="243" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">queried at SessionStart</text>
        <text x="876" y="257" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">rebuilt by reflect reindex</text>
        <text x="876" y="271" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">dense + graph hybrid</text>

        <!-- Tier connector lines left side -->
        <line x1="26" y1="94" x2="26" y2="112" stroke="#D1CFC5" stroke-width="1.5" stroke-dasharray="3,3"/>
        <line x1="26" y1="186" x2="26" y2="208" stroke="#D1CFC5" stroke-width="1.5" stroke-dasharray="3,3"/>
      </svg>
</div>

---

## Platform · adapter interface and shared knowledge base

Each harness has its own adapter that wires hooks into its own config file. Both adapters
point at the same hook scripts and the same shared knowledge base.

<div class="svg-wrap">
      <svg viewBox="0 0 1120 360" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <marker id="c1" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#3D3D3A"/>
          </marker>
          <marker id="c-clay" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#D97757"/>
          </marker>
          <marker id="c-olive" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#788C5D"/>
          </marker>
          <marker id="c-gray" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#87867F"/>
          </marker>
        </defs>

        <rect x="0" y="0" width="1120" height="360" fill="#FAFAF8"/>

        <!-- ===== LEFT: ADAPTERS ===== -->
        <text x="24" y="20" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">HARNESS ADAPTERS</text>

        <!-- Row 1: Claude Code -->
        <rect x="24" y="30" width="330" height="80" rx="10" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="44" y="52" font-size="12" font-weight="600" fill="#141413">Claude Code</text>
        <text x="44" y="68" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">.claude-plugin/plugin.json  →  ~/.claude/settings.json</text>
        <text x="44" y="83" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">/plugin install reflect@agents-in-a-box</text>
        <!-- autowire badge -->
        <rect x="240" y="35" width="104" height="18" rx="4" fill="#FAF9F5" stroke="#3D3D3A" stroke-width="1"/>
        <text x="292" y="48" font-size="9" font-family="ui-monospace,monospace" fill="#141413" text-anchor="middle">plugin runtime</text>

        <!-- Row 2: Codex CLI -->
        <rect x="24" y="128" width="330" height="80" rx="10" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="44" y="150" font-size="12" font-weight="600" fill="#141413">Codex CLI</text>
        <text x="44" y="166" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">adapters/codex/codex_adapter.py  →  ~/.codex/hooks.json</text>
        <text x="44" y="181" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">python codex_adapter.py install</text>
        <!-- manual badge -->
        <rect x="233" y="133" width="111" height="18" rx="4" fill="#FAF9F5" stroke="#3D3D3A" stroke-width="1"/>
        <text x="288" y="146" font-size="9" font-family="ui-monospace,monospace" fill="#141413" text-anchor="middle">adapter direct</text>

        <!-- Row 3: Copilot CLI (planned) -->
        <rect x="24" y="226" width="330" height="80" rx="10" fill="#F0EEE6" stroke="#87867F" stroke-width="1.5" stroke-dasharray="6,4"/>
        <text x="44" y="248" font-size="12" font-weight="600" fill="#87867F">Copilot CLI</text>
        <text x="44" y="264" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">adapters/copilot/copilot_adapter.py  →  TBD</text>
        <text x="44" y="279" font-size="10" font-family="ui-monospace,monospace" fill="#B0AFA8">python copilot_adapter.py install  (planned)</text>
        <!-- planned badge -->
        <rect x="256" y="231" width="88" height="18" rx="4" fill="#FAF9F5" stroke="#87867F" stroke-width="1" stroke-dasharray="4,3"/>
        <text x="300" y="244" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">planned</text>

        <!-- ===== CENTER: REFLECT CORE ===== -->
        <text x="468" y="20" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">REFLECT CORE</text>

        <rect x="396" y="30" width="328" height="276" rx="14" fill="#FFFFFF" stroke="#3D3D3A" stroke-width="2"/>
        <!-- Core title -->
        <text x="560" y="60" font-size="18" font-weight="700" fill="#141413" text-anchor="middle" font-family="ui-monospace,monospace">reflect</text>
        <line x1="416" y1="70" x2="704" y2="70" stroke="#E8E6E3" stroke-width="1"/>

        <!-- Three inner components -->
        <!-- Hook scripts -->
        <rect x="414" y="82" width="292" height="58" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1"/>
        <text x="560" y="104" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">Hook Scripts</text>
        <text x="560" y="120" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">session_start_recall.py · precompact_reflect.py</text>
        <text x="560" y="133" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">reflect-drain-bg.sh</text>

        <!-- CLI -->
        <rect x="414" y="156" width="292" height="58" rx="8" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1"/>
        <text x="560" y="178" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">CLI</text>
        <text x="560" y="194" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">reflect · recall · reindex · search</text>
        <text x="560" y="207" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">python -m reflect_kb</text>

        <!-- Headless worker -->
        <rect x="414" y="230" width="292" height="58" rx="8" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1"/>
        <text x="560" y="252" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">Headless Worker</text>
        <text x="560" y="268" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">claude -p /reflect &lt;transcript&gt;</text>
        <text x="560" y="281" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">--output-format json · --max-turns 25</text>

        <!-- ===== RIGHT: KNOWLEDGE BASE ===== -->
        <text x="786" y="20" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">SHARED KNOWLEDGE BASE</text>

        <rect x="776" y="30" width="318" height="276" rx="14" fill="#FFFFFF" stroke="#3D3D3A" stroke-width="2"/>
        <text x="935" y="60" font-size="14" font-weight="600" fill="#141413" text-anchor="middle" font-family="ui-monospace,monospace">~/.reflect/ + ~/.learnings/</text>
        <line x1="794" y1="70" x2="1076" y2="70" stroke="#E8E6E3" stroke-width="1"/>
        <text x="935" y="88" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">shared by ALL harnesses — same directory</text>

        <!-- KB tier 1 -->
        <rect x="794" y="100" width="282" height="52" rx="8" fill="#F4E4C1" stroke="#3D3D3A" stroke-width="1"/>
        <text x="935" y="122" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">pending queue</text>
        <text x="935" y="138" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">~/.reflect/pending_reflections.jsonl</text>

        <!-- KB tier 2 -->
        <rect x="794" y="166" width="282" height="52" rx="8" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1"/>
        <text x="935" y="188" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">learnings store</text>
        <text x="935" y="204" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">~/.learnings/documents/</text>

        <!-- KB tier 3 -->
        <rect x="794" y="232" width="282" height="52" rx="8" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1"/>
        <text x="935" y="254" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">GraphRAG + vector index</text>
        <text x="935" y="270" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">~/.learnings/graphrag/</text>

        <!-- ===== ARROWS: adapters → core ===== -->
        <!-- Claude → core -->
        <line x1="354" y1="70" x2="396" y2="111" stroke="#3D3D3A" stroke-width="1.5" marker-end="url(#c1)"/>
        <!-- Codex → core -->
        <line x1="354" y1="168" x2="396" y2="168" stroke="#3D3D3A" stroke-width="1.5" marker-end="url(#c1)"/>
        <!-- Copilot → core (dashed) -->
        <line x1="354" y1="266" x2="396" y2="225" stroke="#87867F" stroke-width="1.5" stroke-dasharray="5,4" marker-end="url(#c-gray)"/>

        <!-- Arrow label on adapter→core -->
        <text x="360" y="155" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">wires hooks</text>
        <text x="360" y="166" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">into harness</text>
        <text x="360" y="177" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">config file</text>

        <!-- ===== ARROWS: core → KB ===== -->
        <!-- hooks → KB queue (write) -->
        <line x1="724" y1="126" x2="776" y2="126" stroke="#788C5D" stroke-width="2" marker-end="url(#c-olive)"/>
        <text x="750" y="118" font-size="9" font-family="ui-monospace,monospace" fill="#788C5D" text-anchor="middle">enqueue</text>

        <!-- worker → KB docs (write) -->
        <line x1="724" y1="259" x2="776" y2="200" stroke="#788C5D" stroke-width="2" marker-end="url(#c-olive)"/>
        <text x="768" y="222" font-size="9" font-family="ui-monospace,monospace" fill="#788C5D">write .md</text>

        <!-- CLI → KB graphrag (reindex) -->
        <line x1="724" y1="185" x2="776" y2="260" stroke="#3D3D3A" stroke-width="1.5" stroke-dasharray="4,3" marker-end="url(#c1)"/>
        <text x="762" y="240" font-size="9" font-family="ui-monospace,monospace" fill="#87867F">reindex</text>

        <!-- KB → hooks (recall read) -->
        <path d="M 776 255 Q 760 305 560 305 Q 450 305 430 280" fill="none" stroke="#D97757" stroke-width="2" stroke-dasharray="5,4" marker-end="url(#c-clay)"/>
        <text x="640" y="320" font-size="9.5" font-family="ui-monospace,monospace" fill="#D97757" text-anchor="middle">recall · top-3 learnings injected into session context</text>

        <!-- "one shared KB" note -->
        <text x="935" y="326" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">ALL adapters point here — harness-agnostic</text>
      </svg>
</div>

---

## Install paths · Claude Code vs Codex CLI

Claude has a plugin runtime (`/plugin install`) so installation is two slash commands.
Codex has no plugin runtime, so the adapter does the autowire itself with one python
command.

<div class="svg-wrap">
      <svg viewBox="0 0 1120 380" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <marker id="d1" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#3D3D3A"/>
          </marker>
        </defs>

        <rect x="0" y="0" width="1120" height="380" fill="#FAFAF8"/>

        <!-- Column headers -->
        <text x="24" y="22" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">INSTALL PATH</text>
        <text x="784" y="22" font-size="9" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">LEGEND</text>

        <!-- ===== CLAUDE CODE ROW ===== -->
        <text x="24" y="52" font-size="12" font-weight="600" fill="#141413" font-family="ui-monospace,monospace">Claude Code</text>
        <!-- Complexity scale: shorter bars = fewer steps user must know -->

        <!-- Step 1: marketplace command (user types 1 command) -->
        <rect x="24" y="60" width="196" height="52" rx="8" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="122" y="81" font-size="10.5" font-weight="600" fill="#141413" text-anchor="middle">1. Marketplace add</text>
        <text x="122" y="96" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">/plugin marketplace add</text>
        <text x="122" y="108" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">stevengonsalvez/agents-in-a-box</text>

        <!-- arrow -->
        <line x1="220" y1="86" x2="238" y2="86" stroke="#3D3D3A" stroke-width="1.5" marker-end="url(#d1)"/>

        <!-- Step 2: plugin install -->
        <rect x="240" y="60" width="180" height="52" rx="8" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="330" y="81" font-size="10.5" font-weight="600" fill="#141413" text-anchor="middle">2. Plugin install</text>
        <text x="330" y="96" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">/plugin install</text>
        <text x="330" y="108" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">reflect@agents-in-a-box</text>

        <!-- arrow -->
        <line x1="420" y1="86" x2="438" y2="86" stroke="#3D3D3A" stroke-width="1.5" marker-end="url(#d1)"/>

        <!-- Step 3: plugin runtime (internal, automated) -->
        <rect x="440" y="60" width="268" height="52" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="574" y="78" font-size="10.5" font-weight="600" fill="#141413" text-anchor="middle">3. Plugin runtime (automated)</text>
        <text x="574" y="93" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">extracts plugin.json · expands</text>
        <text x="574" y="107" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">CLAUDE_PLUGIN_ROOT → ~/.claude/settings.json</text>

        <!-- User-effort scale label -->
        <text x="24" y="128" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">user effort: 2 commands · runtime handles the rest</text>

        <!-- "what it writes" strip -->
        <rect x="24" y="136" width="684" height="28" rx="6" fill="#F5F4F0" stroke="#D1CFC5" stroke-width="1"/>
        <text x="36" y="154" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">writes: </text>
        <text x="82" y="154" font-size="10" font-family="ui-monospace,monospace" fill="#141413">~/.claude/settings.json  (hooks block merged)</text>
        <text x="390" y="154" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">  +  skills/ extracted to ~/.claude/plugins/reflect/</text>

        <!-- ===== CODEX CLI ROW ===== -->
        <text x="24" y="192" font-size="12" font-weight="600" fill="#141413" font-family="ui-monospace,monospace">Codex CLI</text>

        <!-- Step 1: git clone / locate adapter -->
        <rect x="24" y="200" width="142" height="52" rx="8" fill="#F4E4C1" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="95" y="218" font-size="10.5" font-weight="600" fill="#141413" text-anchor="middle">1. Clone repo</text>
        <text x="95" y="234" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">or use existing</text>
        <text x="95" y="248" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">agents-in-a-box checkout</text>

        <line x1="166" y1="226" x2="184" y2="226" stroke="#3D3D3A" stroke-width="1.5" marker-end="url(#d1)"/>

        <!-- Step 2: run adapter -->
        <rect x="186" y="200" width="220" height="52" rx="8" fill="#F4E4C1" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="296" y="218" font-size="10.5" font-weight="600" fill="#141413" text-anchor="middle">2. Run adapter directly</text>
        <text x="296" y="234" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">python plugins/reflect/</text>
        <text x="296" y="248" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">adapters/codex/codex_adapter.py install</text>

        <line x1="406" y1="226" x2="424" y2="226" stroke="#3D3D3A" stroke-width="1.5" marker-end="url(#d1)"/>

        <!-- Step 3: adapter copies skills -->
        <rect x="426" y="200" width="188" height="52" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="520" y="218" font-size="10.5" font-weight="600" fill="#141413" text-anchor="middle">3. Adapter copies skills</text>
        <text x="520" y="234" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">plugins/ → ~/.codex/skills/</text>
        <text x="520" y="248" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">merges ~/.codex/hooks.json</text>

        <line x1="614" y1="226" x2="632" y2="226" stroke="#3D3D3A" stroke-width="1.5" marker-end="url(#d1)"/>

        <!-- Step 4: hooks wired -->
        <rect x="634" y="200" width="74" height="52" rx="8" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="671" y="221" font-size="10.5" font-weight="600" fill="#141413" text-anchor="middle">4. Done</text>
        <text x="671" y="237" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">hooks.json</text>
        <text x="671" y="251" font-size="9.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">active</text>

        <!-- User-effort label -->
        <text x="24" y="268" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">user effort: 1 python command · no plugin runtime required</text>

        <!-- "what it writes" strip -->
        <rect x="24" y="276" width="684" height="28" rx="6" fill="#F5F4F0" stroke="#D1CFC5" stroke-width="1"/>
        <text x="36" y="294" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">writes: </text>
        <text x="82" y="294" font-size="10" font-family="ui-monospace,monospace" fill="#141413">~/.codex/hooks.json  (SessionStart + PreCompact entries merged)</text>
        <text x="448" y="294" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">  +  skills/ copied to ~/.codex/skills/reflect/</text>

        <!-- ===== LEGEND (right column) ===== -->
        <!-- Legend box -->
        <rect x="784" y="32" width="312" height="272" rx="12" fill="#FFFFFF" stroke="#D1CFC5" stroke-width="1.5"/>
        <text x="800" y="54" font-size="10.5" font-weight="600" fill="#141413">Step type</text>
        <line x1="800" y1="60" x2="1080" y2="60" stroke="#E8E6E3" stroke-width="1"/>

        <!-- legend item: user command -->
        <rect x="800" y="70" width="14" height="14" rx="3" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1"/>
        <text x="820" y="82" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A">User command (Claude Code)</text>

        <rect x="800" y="94" width="14" height="14" rx="3" fill="#F4E4C1" stroke="#3D3D3A" stroke-width="1"/>
        <text x="820" y="106" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A">User action (Codex)</text>

        <rect x="800" y="118" width="14" height="14" rx="3" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1"/>
        <text x="820" y="130" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A">Automated (no user input)</text>

        <rect x="800" y="142" width="14" height="14" rx="3" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1"/>
        <text x="820" y="154" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A">Config written / result</text>

        <line x1="800" y1="168" x2="1080" y2="168" stroke="#E8E6E3" stroke-width="1"/>
        <text x="800" y="184" font-size="10.5" font-weight="600" fill="#141413">Config files produced</text>

        <text x="800" y="202" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">Claude: ~/.claude/settings.json</text>
        <text x="800" y="218" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">         (hooks block · plugin runtime writes)</text>
        <text x="800" y="238" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">Codex:  ~/.codex/hooks.json</text>
        <text x="800" y="254" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">         (hooks block · adapter writes directly)</text>

        <line x1="800" y1="266" x2="1080" y2="266" stroke="#E8E6E3" stroke-width="1"/>
        <text x="800" y="284" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">Both point at the SAME hook scripts</text>
        <text x="800" y="298" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">and the SAME shared knowledge base.</text>

        <!-- Visual "effort" comparison bar -->
        <text x="24" y="332" font-size="10.5" font-weight="600" fill="#141413">Relative user-facing complexity</text>
        <!-- Claude effort bar (shorter = simpler) -->
        <text x="24" y="352" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">Claude Code</text>
        <rect x="130" y="340" width="180" height="16" rx="4" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1"/>
        <text x="316" y="352" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">  2 slash commands</text>
        <!-- Codex effort bar (longer = more manual steps) -->
        <text x="24" y="372" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">Codex CLI</text>
        <rect x="130" y="360" width="308" height="16" rx="4" fill="#F4E4C1" stroke="#3D3D3A" stroke-width="1"/>
        <text x="444" y="372" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">  1 python command (more explicit path)</text>
      </svg>
</div>

```bash
# Claude Code — managed install via plugin runtime
/plugin marketplace add stevengonsalvez/agents-in-a-box
/plugin install reflect@agents-in-a-box

# Codex CLI — adapter does the autowire
python plugins/reflect/adapters/codex/codex_adapter.py install
# or skip the bg drain on codex-only machines without claude on PATH:
python plugins/reflect/adapters/codex/codex_adapter.py install --no-bg-drain
```

---

## Recall · UserPromptSubmit primary, SessionStart baseline, per-session dedupe

SessionStart fires *before* the user has typed anything — its recall query has to be
inferred from cwd, branch, and recent commits. UserPromptSubmit has the actual user prompt
to query against, which gives much sharper hits. Both fire; UserPromptSubmit dedupes against
learnings already injected this session so the same memory doesn't re-inject on every prompt.

<div class="svg-wrap">
      <svg viewBox="0 0 1120 340" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <marker id="a5" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#3D3D3A"/>
          </marker>
          <marker id="a5-clay" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#D97757"/>
          </marker>
        </defs>

        <rect x="0" y="0" width="1120" height="340" fill="#FAFAF8"/>

        <!-- Lane labels -->
        <text x="14" y="34" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">SESSION&#160;TIMELINE</text>

        <!-- Time axis -->
        <line x1="70" y1="80" x2="1080" y2="80" stroke="#3D3D3A" stroke-width="1.5" marker-end="url(#a5)"/>
        <text x="60" y="86" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="end">t →</text>

        <!-- Ticks: SessionStart -->
        <line x1="120" y1="70" x2="120" y2="90" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="120" cy="80" r="6" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="120" y="60" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">SessionStart</text>
        <text x="120" y="106" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">baseline recall</text>
        <text x="120" y="120" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">cwd · branch</text>

        <!-- Ticks: User prompt 1 -->
        <line x1="300" y1="70" x2="300" y2="90" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="300" cy="80" r="6" fill="#D97757" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="300" y="60" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">UserPromptSubmit ①</text>
        <text x="300" y="106" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">"fix OAuth bug"</text>

        <!-- Ticks: User prompt 2 (same topic) -->
        <line x1="560" y1="70" x2="560" y2="90" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="560" cy="80" r="6" fill="#D97757" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="560" y="60" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">UserPromptSubmit ②</text>
        <text x="560" y="106" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">"add refresh token"</text>

        <!-- Ticks: User prompt 3 (topic pivot) -->
        <line x1="820" y1="70" x2="820" y2="90" stroke="#3D3D3A" stroke-width="2"/>
        <circle cx="820" cy="80" r="6" fill="#D97757" stroke="#3D3D3A" stroke-width="1.5"/>
        <text x="820" y="60" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">UserPromptSubmit ③</text>
        <text x="820" y="106" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">"now the rate-limiter"</text>

        <!-- Recall actions per tick -->
        <rect x="36" y="148" width="168" height="80" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="120" y="166" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">session_start_recall</text>
        <text x="120" y="184" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">query: cwd + branch</text>
        <text x="120" y="200" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">inject top-3 baseline</text>
        <text x="120" y="218" font-size="10" font-family="ui-monospace,monospace" fill="#D97757" text-anchor="middle">→ L₁ L₂ L₃</text>

        <rect x="216" y="148" width="168" height="80" rx="8" fill="#D97757" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="300" y="166" font-size="11" font-weight="600" fill="#FFFFFF" text-anchor="middle">user_prompt_recall</text>
        <text x="300" y="184" font-size="10" font-family="ui-monospace,monospace" fill="#FFFFFF" text-anchor="middle">query: prompt text</text>
        <text x="300" y="200" font-size="10" font-family="ui-monospace,monospace" fill="#FFFFFF" text-anchor="middle">dedupe vs {L₁,L₂,L₃}</text>
        <text x="300" y="218" font-size="10" font-family="ui-monospace,monospace" fill="#F4E4C1" text-anchor="middle">→ L₄ (L₁ skipped)</text>

        <rect x="476" y="148" width="168" height="80" rx="8" fill="#D97757" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="560" y="166" font-size="11" font-weight="600" fill="#FFFFFF" text-anchor="middle">user_prompt_recall</text>
        <text x="560" y="184" font-size="10" font-family="ui-monospace,monospace" fill="#FFFFFF" text-anchor="middle">query: prompt text</text>
        <text x="560" y="200" font-size="10" font-family="ui-monospace,monospace" fill="#FFFFFF" text-anchor="middle">dedupe vs {L₁..L₄}</text>
        <text x="560" y="218" font-size="10" font-family="ui-monospace,monospace" fill="#F4E4C1" text-anchor="middle">→ L₅ (L₂,L₄ skipped)</text>

        <rect x="736" y="148" width="168" height="80" rx="8" fill="#D97757" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="820" y="166" font-size="11" font-weight="600" fill="#FFFFFF" text-anchor="middle">user_prompt_recall</text>
        <text x="820" y="184" font-size="10" font-family="ui-monospace,monospace" fill="#FFFFFF" text-anchor="middle">query: prompt text</text>
        <text x="820" y="200" font-size="10" font-family="ui-monospace,monospace" fill="#FFFFFF" text-anchor="middle">topic pivot → new hits</text>
        <text x="820" y="218" font-size="10" font-family="ui-monospace,monospace" fill="#F4E4C1" text-anchor="middle">→ L₆ L₇ L₈</text>

        <!-- Dedupe state evolution -->
        <text x="36" y="260" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">DEDUPE STATE</text>
        <text x="36" y="276" font-size="11" font-family="ui-monospace,monospace" fill="#3D3D3A">~/.reflect/session-injected/&lt;session_id&gt;.json</text>

        <rect x="36" y="292" width="168" height="30" rx="6" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1"/>
        <text x="120" y="312" font-size="10.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">{L₁, L₂, L₃}</text>

        <rect x="216" y="292" width="168" height="30" rx="6" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1"/>
        <text x="300" y="312" font-size="10.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">{L₁, L₂, L₃, L₄}</text>

        <rect x="476" y="292" width="168" height="30" rx="6" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1"/>
        <text x="560" y="312" font-size="10.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">{L₁..L₄, L₅}</text>

        <rect x="736" y="292" width="168" height="30" rx="6" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1"/>
        <text x="820" y="312" font-size="10.5" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">{L₁..L₅, L₆, L₇, L₈}</text>

        <!-- vertical hook→state arrows -->
        <line x1="120" y1="228" x2="120" y2="292" stroke="#788C5D" stroke-width="1.5" stroke-dasharray="3,2"/>
        <line x1="300" y1="228" x2="300" y2="292" stroke="#788C5D" stroke-width="1.5" stroke-dasharray="3,2"/>
        <line x1="560" y1="228" x2="560" y2="292" stroke="#788C5D" stroke-width="1.5" stroke-dasharray="3,2"/>
        <line x1="820" y1="228" x2="820" y2="292" stroke="#788C5D" stroke-width="1.5" stroke-dasharray="3,2"/>

        <!-- explainer block on the right -->
        <rect x="940" y="148" width="160" height="174" rx="8" fill="#FFFFFF" stroke="#D1CFC5" stroke-width="1.2"/>
        <text x="952" y="166" font-size="10" font-weight="600" fill="#141413">Why two layers?</text>
        <text x="952" y="184" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">SessionStart fires</text>
        <text x="952" y="196" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">BEFORE user types</text>
        <text x="952" y="208" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">→ query is coarse</text>
        <line x1="952" y1="218" x2="1088" y2="218" stroke="#E8E6E3" stroke-width="1"/>
        <text x="952" y="234" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">UserPromptSubmit has</text>
        <text x="952" y="246" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">actual intent</text>
        <text x="952" y="258" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">→ sharp query</text>
        <line x1="952" y1="268" x2="1088" y2="268" stroke="#E8E6E3" stroke-width="1"/>
        <text x="952" y="284" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">Dedupe stops the</text>
        <text x="952" y="296" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">same learning being</text>
        <text x="952" y="308" font-size="9.5" font-family="ui-monospace,monospace" fill="#87867F">re-injected per prompt</text>
      </svg>
</div>

**Dedupe state** lives at `~/.reflect/session-injected/<session_id>.json` — a per-session
set of learning IDs already injected. UserPromptSubmit recall queries the KB, intersects
with the dedupe set, and only injects new hits as `additionalContext`.

---

## Capture · PostToolUse mini-learnings + Stop reflection enqueue

PreCompact handles the high-cost full reflection (`claude -p /reflect`). Two more hooks
cover gaps:

- **PostToolUse** captures cheap mini-learnings inline — on tool failure, arms a watcher
  for the next user prompt; if the prompt looks like a correction (`"try X instead"`),
  write a low-confidence learning directly to disk. No LLM run needed.
- **Stop** catches short sessions that end before PreCompact ever fires. Enqueues the
  transcript on agent finish; dedupes against any PreCompact entry for the same session.

<div class="svg-wrap">
      <svg viewBox="0 0 1120 300" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <marker id="a6" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#3D3D3A"/>
          </marker>
          <marker id="a6-olive" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#788C5D"/>
          </marker>
        </defs>

        <rect x="0" y="0" width="1120" height="300" fill="#FAFAF8"/>

        <!-- Two parallel sub-flows -->
        <!-- TOP: PostToolUse mini-learning -->
        <text x="36" y="32" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">POSTTOOLUSE · MINI-LEARNING</text>

        <rect x="36" y="48" width="200" height="68" rx="8" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="136" y="68" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">Tool call fails</text>
        <text x="136" y="84" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">Bash exit≠0 · Edit error</text>
        <text x="136" y="100" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">PostToolUse hook fires</text>

        <line x1="236" y1="82" x2="290" y2="82" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a6)"/>

        <rect x="290" y="48" width="200" height="68" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="390" y="68" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">posttooluse_minilearning.py</text>
        <text x="390" y="84" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">arms next-prompt watcher</text>
        <text x="390" y="100" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">writes ~/.reflect/armed.json</text>

        <line x1="490" y1="82" x2="544" y2="82" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a6)"/>

        <rect x="544" y="48" width="220" height="68" rx="8" fill="#D97757" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="654" y="68" font-size="11" font-weight="600" fill="#FFFFFF" text-anchor="middle">User: "try X instead"</text>
        <text x="654" y="84" font-size="10" font-family="ui-monospace,monospace" fill="#FFFFFF" text-anchor="middle">UserPromptSubmit fires</text>
        <text x="654" y="100" font-size="10" font-family="ui-monospace,monospace" fill="#F4E4C1" text-anchor="middle">arming detected</text>

        <line x1="764" y1="82" x2="818" y2="82" stroke="#788C5D" stroke-width="2" stroke-dasharray="4,2" marker-end="url(#a6-olive)"/>

        <rect x="818" y="48" width="240" height="68" rx="8" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="938" y="68" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">~/.learnings/documents/</text>
        <text x="938" y="84" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">&lt;slug&gt;.md · conf=low</text>
        <text x="938" y="100" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">low-cost · no /reflect run</text>

        <!-- Divider -->
        <line x1="36" y1="142" x2="1080" y2="142" stroke="#E8E6E3" stroke-width="1"/>

        <!-- BOTTOM: Stop reflection enqueue -->
        <text x="36" y="170" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">STOP · REFLECTION ENQUEUE (short sessions)</text>

        <rect x="36" y="186" width="200" height="68" rx="8" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="136" y="206" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">Agent finishes</text>
        <text x="136" y="222" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">Stop hook fires</text>
        <text x="136" y="238" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">(context never filled)</text>

        <line x1="236" y1="220" x2="290" y2="220" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a6)"/>

        <rect x="290" y="186" width="200" height="68" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="390" y="206" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">stop_reflect.py</text>
        <text x="390" y="222" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">enqueue if not already</text>
        <text x="390" y="238" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">dedupe by session_id</text>

        <line x1="490" y1="220" x2="544" y2="220" stroke="#788C5D" stroke-width="2" stroke-dasharray="4,2" marker-end="url(#a6-olive)"/>

        <rect x="544" y="186" width="240" height="68" rx="8" fill="#F4E4C1" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="664" y="206" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">~/.reflect/pending_reflections</text>
        <text x="664" y="222" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">.jsonl (shared queue)</text>
        <text x="664" y="238" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">drained next SessionStart</text>

        <line x1="784" y1="220" x2="818" y2="220" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a6)"/>

        <rect x="818" y="186" width="240" height="68" rx="8" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="938" y="206" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">claude -p /reflect (headless)</text>
        <text x="938" y="222" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">writes learnings + reindex</text>
        <text x="938" y="238" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">same drain path</text>

        <!-- Footer note -->
        <text x="36" y="284" font-size="10.5" font-family="ui-monospace,monospace" fill="#87867F">
          ★ Mini-learning is cheap (no LLM run); Stop-enqueue is full /reflect (same as PreCompact). Both write to the SAME learnings store.
        </text>
      </svg>
</div>

---

## Status line · making recall + capture activity visible

Both harnesses give visual feedback, but through different mechanisms.

<div class="svg-wrap">
      <svg viewBox="0 0 1120 320" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <marker id="a7" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#3D3D3A"/>
          </marker>
        </defs>

        <rect x="0" y="0" width="1120" height="320" fill="#FAFAF8"/>

        <!-- Claude side -->
        <text x="36" y="32" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">CLAUDE CODE · CUSTOM SHELL STATUS LINE</text>

        <rect x="36" y="48" width="240" height="68" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="156" y="68" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">Any reflect hook fires</text>
        <text x="156" y="84" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">recall · enqueue · drain</text>
        <text x="156" y="100" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">writes ~/.reflect/last-event.json</text>

        <line x1="276" y1="82" x2="330" y2="82" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a7)"/>

        <rect x="330" y="48" width="200" height="68" rx="8" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="430" y="68" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">~/.reflect/last-event.json</text>
        <text x="430" y="84" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">{event, ts, detail}</text>
        <text x="430" y="100" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">small atomic file</text>

        <line x1="530" y1="82" x2="584" y2="82" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a7)"/>

        <rect x="584" y="48" width="240" height="68" rx="8" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="704" y="68" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">~/.claude/statusline.sh</text>
        <text x="704" y="84" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">reads last-event.json</text>
        <text x="704" y="100" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">renders reflect fragment</text>

        <line x1="824" y1="82" x2="878" y2="82" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a7)"/>

        <rect x="878" y="48" width="200" height="68" rx="8" fill="#141413" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="978" y="68" font-size="11" font-weight="600" fill="#FAF9F5" text-anchor="middle" font-family="ui-monospace,monospace">claude · main</text>
        <text x="978" y="84" font-size="10" font-family="ui-monospace,monospace" fill="#9DD4C7" text-anchor="middle">🧠 recalled 3 · queued 1</text>
        <text x="978" y="100" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">8% ctx · 2h limit</text>

        <!-- Divider -->
        <line x1="36" y1="146" x2="1080" y2="146" stroke="#E8E6E3" stroke-width="1"/>

        <!-- Codex side -->
        <text x="36" y="174" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">CODEX CLI · HOOK statusMessage FIELD (PER-CALL ONLY)</text>

        <rect x="36" y="190" width="240" height="68" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="156" y="210" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">Any reflect hook fires</text>
        <text x="156" y="226" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">recall · enqueue · drain</text>
        <text x="156" y="242" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">hook entry has statusMessage</text>

        <line x1="276" y1="224" x2="330" y2="224" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a7)"/>

        <rect x="330" y="190" width="200" height="68" rx="8" fill="#E8E6E3" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="430" y="210" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">codex reads</text>
        <text x="430" y="226" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">hooks.json entry</text>
        <text x="430" y="242" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">"statusMessage":"🧠 recalling…"</text>

        <line x1="530" y1="224" x2="584" y2="224" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a7)"/>

        <rect x="584" y="190" width="240" height="68" rx="8" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="704" y="210" font-size="11" font-weight="600" fill="#141413" text-anchor="middle">codex TUI · ephemeral</text>
        <text x="704" y="226" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A" text-anchor="middle">shows DURING hook only</text>
        <text x="704" y="242" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">no persistent fragment yet</text>

        <line x1="824" y1="224" x2="878" y2="224" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a7)"/>

        <rect x="878" y="190" width="200" height="68" rx="8" fill="#141413" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="978" y="210" font-size="11" font-weight="600" fill="#FAF9F5" text-anchor="middle" font-family="ui-monospace,monospace">codex · gpt-5.5</text>
        <text x="978" y="226" font-size="10" font-family="ui-monospace,monospace" fill="#9DD4C7" text-anchor="middle">🧠 recalling…</text>
        <text x="978" y="242" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" text-anchor="middle">main · ./repo · 4%</text>

        <!-- Asymmetry callout -->
        <text x="36" y="286" font-size="10.5" font-family="ui-monospace,monospace" fill="#87867F">
          ★ Claude has a custom shell statusline — we get persistent counters (recalled N · queued M). Codex's status line is a fixed token list;
        </text>
        <text x="36" y="302" font-size="10.5" font-family="ui-monospace,monospace" fill="#87867F">
          we use the per-hook statusMessage field — shows ephemerally during hook execution only. Full parity blocked on a codex custom-token API.
        </text>
      </svg>
</div>

- **Claude Code** — hooks write `~/.reflect/last-event.json`; the user's
  `~/.claude/statusline.sh` reads it and renders a persistent reflect fragment
  (`🧠 3 recalled · 1 queued`).
- **Codex CLI** — hooks declare a `statusMessage` field in `hooks.json`. Codex shows it
  ephemerally *during* hook execution (`🧠 recalling...`). The static `[tui] status_line`
  config can't carry a custom token yet, so persistent codex-side counters wait on a
  codex API extension.

---

## Worked example · "fix the OAuth redirect bug"

A concrete walkthrough showing every hook firing in order across a morning Claude session
and an afternoon Codex session on the same repo.

<div class="svg-wrap">
      <svg viewBox="0 0 1120 460" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <marker id="a8" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto">
            <polygon points="0 0,8 4,0 8" fill="#3D3D3A"/>
          </marker>
        </defs>

        <rect x="0" y="0" width="1120" height="460" fill="#FAFAF8"/>

        <!-- Timeline header -->
        <text x="14" y="32" font-size="10" font-family="ui-monospace,monospace" fill="#87867F" letter-spacing="0.1em">SCENARIO · CLAUDE CODE SESSION, MORNING</text>

        <!-- Step 1: session starts -->
        <rect x="36" y="56" width="220" height="64" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="46" y="74" font-size="9" font-family="ui-monospace,monospace" fill="#788C5D">09:14 · SESSIONSTART</text>
        <text x="46" y="92" font-size="11" font-weight="600" fill="#141413">Session starts in ./auth-service</text>
        <text x="46" y="108" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">recall(cwd=auth) → L₁ "OAuth state"</text>

        <line x1="256" y1="88" x2="290" y2="88" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a8)"/>

        <!-- Step 2: User prompt -->
        <rect x="290" y="56" width="240" height="64" rx="8" fill="#D97757" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="300" y="74" font-size="9" font-family="ui-monospace,monospace" fill="#F4E4C1">09:14 · USERPROMPTSUBMIT</text>
        <text x="300" y="92" font-size="11" font-weight="600" fill="#FFFFFF">"fix the OAuth redirect bug"</text>
        <text x="300" y="108" font-size="10" font-family="ui-monospace,monospace" fill="#F4E4C1">recall(prompt) → L₂ L₃ (L₁ skipped)</text>

        <line x1="530" y1="88" x2="564" y2="88" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a8)"/>

        <!-- Step 3: Tool fails -->
        <rect x="564" y="56" width="220" height="64" rx="8" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="574" y="74" font-size="9" font-family="ui-monospace,monospace" fill="#788C5D">09:18 · POSTTOOLUSE</text>
        <text x="574" y="92" font-size="11" font-weight="600" fill="#141413">Bash exit=1 (curl 500)</text>
        <text x="574" y="108" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">posttooluse arms watcher</text>

        <line x1="784" y1="88" x2="818" y2="88" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a8)"/>

        <!-- Step 4: User correction -->
        <rect x="818" y="56" width="240" height="64" rx="8" fill="#D97757" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="828" y="74" font-size="9" font-family="ui-monospace,monospace" fill="#F4E4C1">09:18 · USERPROMPTSUBMIT</text>
        <text x="828" y="92" font-size="11" font-weight="600" fill="#FFFFFF">"use --insecure for local dev"</text>
        <text x="828" y="108" font-size="10" font-family="ui-monospace,monospace" fill="#F4E4C1">mini-learning written ✓</text>

        <!-- Arrow down to next row -->
        <line x1="938" y1="124" x2="938" y2="172" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a8)"/>
        <path d="M 938 172 L 938 184 L 156 184 L 156 196" fill="none" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a8)"/>

        <!-- Step 5: Context fills -->
        <rect x="36" y="200" width="220" height="64" rx="8" fill="#F4E4C1" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="46" y="218" font-size="9" font-family="ui-monospace,monospace" fill="#788C5D">10:42 · PRECOMPACT</text>
        <text x="46" y="236" font-size="11" font-weight="600" fill="#141413">Context 90% full</text>
        <text x="46" y="252" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">enqueued transcript ✓</text>

        <line x1="256" y1="232" x2="290" y2="232" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a8)"/>

        <!-- Step 6: Session ends -->
        <rect x="290" y="200" width="240" height="64" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="300" y="218" font-size="9" font-family="ui-monospace,monospace" fill="#788C5D">11:05 · STOP</text>
        <text x="300" y="236" font-size="11" font-weight="600" fill="#141413">Agent finishes · session ends</text>
        <text x="300" y="252" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">stop_reflect skips (dedupe)</text>

        <line x1="530" y1="232" x2="564" y2="232" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a8)"/>

        <!-- Step 7: Later - codex session opens -->
        <rect x="564" y="200" width="240" height="64" rx="8" fill="#A8C5E6" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="574" y="218" font-size="9" font-family="ui-monospace,monospace" fill="#788C5D">14:30 · CODEX · SESSIONSTART</text>
        <text x="574" y="236" font-size="11" font-weight="600" fill="#141413">Open codex on same repo</text>
        <text x="574" y="252" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">drain-bg picks up morning queue</text>

        <line x1="804" y1="232" x2="838" y2="232" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a8)"/>

        <!-- Step 8: drain runs claude -p -->
        <rect x="838" y="200" width="220" height="64" rx="8" fill="#9DD4C7" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="848" y="218" font-size="9" font-family="ui-monospace,monospace" fill="#788C5D">14:30 · BG DRAIN</text>
        <text x="848" y="236" font-size="11" font-weight="600" fill="#141413">claude -p /reflect (headless)</text>
        <text x="848" y="252" font-size="10" font-family="ui-monospace,monospace" fill="#3D3D3A">writes L₄ "OAuth state mismatch"</text>

        <!-- Arrow down to next row -->
        <line x1="938" y1="268" x2="938" y2="316" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a8)"/>
        <path d="M 938 316 L 938 328 L 156 328 L 156 340" fill="none" stroke="#3D3D3A" stroke-width="2" marker-end="url(#a8)"/>

        <!-- Step 9: codex user prompts -->
        <rect x="36" y="344" width="240" height="64" rx="8" fill="#D97757" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="46" y="362" font-size="9" font-family="ui-monospace,monospace" fill="#F4E4C1">14:31 · CODEX USERPROMPTSUBMIT</text>
        <text x="46" y="380" font-size="11" font-weight="600" fill="#FFFFFF">"add OAuth refresh tokens"</text>
        <text x="46" y="396" font-size="10" font-family="ui-monospace,monospace" fill="#F4E4C1">recall returns L₂, L₃, L₄ ✓</text>

        <!-- Outcome -->
        <rect x="296" y="344" width="424" height="64" rx="8" fill="#FFFFFF" stroke="#788C5D" stroke-width="1.5" stroke-dasharray="4,2"/>
        <text x="312" y="362" font-size="10" font-family="ui-monospace,monospace" fill="#788C5D">OUTCOME</text>
        <text x="312" y="380" font-size="11" font-weight="600" fill="#141413">Codex session benefits from this morning's Claude work</text>
        <text x="312" y="396" font-size="10" font-family="ui-monospace,monospace" fill="#87867F">L₄ "OAuth state mismatch (Claude AM)" is now in the index. Cross-tool.</text>

        <!-- Status line strip at top right corner -->
        <rect x="740" y="344" width="318" height="64" rx="8" fill="#141413" stroke="#3D3D3A" stroke-width="1.2"/>
        <text x="752" y="362" font-size="9" font-family="ui-monospace,monospace" fill="#9DD4C7">STATUS LINE · CODEX TUI</text>
        <text x="752" y="380" font-size="11" font-family="ui-monospace,monospace" fill="#FAF9F5">codex · gpt-5.5  </text>
        <text x="752" y="396" font-size="10" font-family="ui-monospace,monospace" fill="#9DD4C7">🧠 3 recalled · ↗ queue empty</text>
      </svg>
</div>

1. **09:14 · Claude SessionStart** — recall on `cwd=auth-service` returns L₁ ("OAuth state
   handling"). Injected as baseline.
2. **09:14 · UserPromptSubmit** — user types "fix the OAuth redirect bug". Sharp query
   pulls L₂, L₃. L₁ skipped (already injected). Dedupe set: `{L₁, L₂, L₃}`.
3. **09:18 · PostToolUse** — `curl` returns 500. Mini-learning watcher arms.
4. **09:18 · UserPromptSubmit** — user types "use `--insecure` for local dev". Watcher
   sees correction pattern, writes mini-learning directly to disk. No LLM run.
5. **10:42 · PreCompact** — context 90% full. Transcript path enqueued to
   `~/.reflect/pending_reflections.jsonl`.
6. **11:05 · Stop** — agent finishes. `stop_reflect.py` checks queue, sees PreCompact
   already enqueued this session_id → skips.
7. **14:30 · Codex SessionStart** — different harness, same repo. `reflect-drain-bg.sh`
   starts in background, finds the morning queue entry, spawns `claude -p /reflect`
   headless. Writes L₄ ("OAuth state mismatch on redirect").
8. **14:31 · Codex UserPromptSubmit** — user types "add OAuth refresh tokens". Recall pulls
   L₂, L₃, L₄ — including the learning Claude just wrote this morning.

The codex session benefits from the morning's Claude work without anyone moving files
around. The queue and the learnings store are the only handoff.

---

## Files involved

| File | Role |
|---|---|
| `plugins/reflect/skills/recall/hooks/session_start_recall.py` | SessionStart recall (baseline, cwd-based query) |
| `plugins/reflect/skills/recall/hooks/user_prompt_submit_recall.py` | UserPromptSubmit recall (intent-sharp, with dedupe) |
| `plugins/reflect/hooks/precompact_reflect.py` | PreCompact enqueue (full reflection deferred) |
| `plugins/reflect/hooks/stop_reflect.py` | Stop enqueue (short-session fallback) |
| `plugins/reflect/hooks/posttooluse_minilearning.py` | PostToolUse mini-learning capture |
| `plugins/reflect/hooks/reflect-drain-bg.sh` | SessionStart bg-drainer (shells out to `claude -p`) |
| `plugins/reflect/.claude-plugin/plugin.json` | Claude plugin autowire (`/plugin install`) |
| `plugins/reflect/adapters/codex/codex_adapter.py` | Codex installer (writes `~/.codex/hooks.json`) |
| `~/.reflect/pending_reflections.jsonl` | Shared queue (any harness writes, any drains) |
| `~/.reflect/session-injected/<session_id>.json` | Per-session dedupe state |
| `~/.reflect/last-event.json` | Status line fragment source |
| `~/.learnings/documents/` | Markdown learnings + entity sidecars |
| `~/.learnings/graphrag/` | GraphRAG + vector index |

---

## FAQ

**Why doesn't SessionStart recall use the user's first prompt?**
SessionStart fires *before* the user has typed anything. Its query has to be inferred from
cwd, branch, and recent commits — coarse but immediate. UserPromptSubmit fills the
prompt-aware recall slot.

**What stops UserPromptSubmit recall from re-injecting the same learning every prompt?**
The per-session dedupe set at `~/.reflect/session-injected/<session_id>.json`. Each hit
becomes a `{learning_id: ts}` entry; future prompts skip already-injected learnings
unless they'd be the top hit anyway.

**Why does the drainer always shell out to `claude` instead of `codex`?**
The `/reflect` skill is a Claude skill. Codex is the trigger (any SessionStart fires the
bg drainer), Claude is the worker. On codex-only machines without `claude` on PATH, pass
`--no-bg-drain` to the codex adapter to skip the drain hook.

**Does Stop also fire on long sessions that hit PreCompact?**
Yes, but `stop_reflect.py` dedupes against the queue by session_id. PreCompact gets in
first; Stop is a fallback for sessions that never compact.

**Where does `~/.reflect/` live, and is it portable?**
Under `$HOME/.reflect/` by default; overridable via `REFLECT_STATE_DIR`. Contents are
JSONL/Markdown/YAML — fully grep-able, version-control friendly, and portable across
machines via filesystem sync.

---

> **Try it** — see the standalone visual posters with the same diagrams:
> - Prose explainer + small SVG: <https://unfold-ledger-qjhe.here.now/>
> - Full platform poster: <https://saffron-mesa-9nz2.here.now/>

<style>
.svg-wrap {
  border: 1.5px solid var(--sl-color-gray-5);
  border-radius: 12px;
  background: #fff;
  padding: 16px;
  margin: 20px 0;
  overflow-x: auto;
}
.svg-wrap svg {
  display: block;
  width: 100%;
  max-width: 1120px;
  height: auto;
  margin: 0 auto;
}
</style>
