# illustration

Hand-drawn **sketchnote** illustrations starring **Popa** — a pink blob with a green sprout and a notepad — for Claude Code.

Digest → shot list → per-image generation → QA → deliver. Popa turns an article, README, system, or idea into an intricate hand-drawn sketchnote: a banner, many linked boxes and notes, arrows and spot icons — busy but legible.

## Featured series — agents-in-a-box in 22 frames

A continuous `illustration:popa` sketchnote storyboard explaining agents-in-a-box end to end — Popa as the guide and a continuous **green** flow-line so all 22 panels read (and animate) as one left-to-right mural, built to be fed to an image-to-video model for a panning walkthrough.

**1 — Agents in a box** · terminal-native, runs local, open source
![1](assets/agents-in-a-box-series-popa/01-intro.png)
**2 — The problem** · two parallel sessions stomping each other's branches
![2](assets/agents-in-a-box-series-popa/02-problem.png)
**3 — The TUI** · one terminal cockpit — workspaces, session rows, preview pane
![3](assets/agents-in-a-box-series-popa/03-tui.png)
**4 — Isolation** · each session in its own box — git worktree + tmux, survives disconnect
![4](assets/agents-in-a-box-series-popa/04-isolation.png)
**5 — Multi-provider** · pick your player — Claude, Codex, Gemini, Copilot, Kiro
![5](assets/agents-in-a-box-series-popa/05-providers.png)
**6 — Toolkit** · write once, deploy to 11 tools — 86 skills, 37 agents
![6](assets/agents-in-a-box-series-popa/06-toolkit.png)
**7 — Plugins (v2)** · native subprocess plugins, JSON-RPC over stdio, capability-gated
![7](assets/agents-in-a-box-series-popa/07-plugins.png)
**8 — Burndown** · token budget + analytics by day / project / model, optimise hints
![8](assets/agents-in-a-box-series-popa/08-burndown.png)
**9 — WITR** · "why is this running" — process-causality tracing (`w`)
![9](assets/agents-in-a-box-series-popa/09-witr.png)
**10 — Hangar** · the managed-agent fleet — issues → kanban → autopilots (`g`)
![10](assets/agents-in-a-box-series-popa/10-hangar.png)
**11 — MCP socket-pool** · one shared unix socket instead of 510 node processes
![11](assets/agents-in-a-box-series-popa/11-mcp-pool.png)
**12 — Skill manager** · browse a remote catalog, one-key install, sync / sandbox
![12](assets/agents-in-a-box-series-popa/12-skill-manager.png)
**13 — Inbox & notifications** · awaiting-input / turn-done / permission as OS banners + a TUI inbox
![13](assets/agents-in-a-box-series-popa/13-inbox.png)
**14 — Attach / foreign-TTY** · suspend the TUI, hand the terminal to an external program, resume
![14](assets/agents-in-a-box-series-popa/14-attach.png)
**15 — Swarm** · multi-agent orchestration — leader / workers, broadcast, ack-gated sequence
![15](assets/agents-in-a-box-series-popa/15-swarm.png)
**16 — reflect-memory: the problem** · memory isn't a bigger `CLAUDE.md`
![16](assets/agents-in-a-box-series-popa/16-reflect-problem.png)
**17 — reflect-memory: capture & index** · a correction → a learning note → keyword · vector · graph
![17](assets/agents-in-a-box-series-popa/17-reflect-capture.png)
**18 — reflect-memory: recall** · fuse → rerank → gate → inject at session start
![18](assets/agents-in-a-box-series-popa/18-reflect-recall.png)
**19 — Beads** · git-backed issues with dependencies that survive context compaction
![19](assets/agents-in-a-box-series-popa/19-beads.png)
**20 — Code review** · multi-agent review of a diff/PR, inline comments, `--format json`
![20](assets/agents-in-a-box-series-popa/20-code-review.png)
**21 — Skills on tap** · the ainb-toolkit slash-skills
![21](assets/agents-in-a-box-series-popa/21-toolkit-skills.png)
**22 — Take your shot** · all local, open source, yours
![22](assets/agents-in-a-box-series-popa/22-finale.png)

## Skill

| Invoke | Look | Best for |
|--------|------|----------|
| `illustration` · `illustration:popa` | **Popa** — pink blob, green sprout, notepad; intricate hand-drawn sketchnotes (a banner, many linked notes, arrows, icons) | Dev/product/system explainers, how-it-works, architecture, walkthroughs, friendly social |

## Examples

<table>
  <tr>
    <td align="center"><img src="skills/popa/assets/examples/01-idea-pipeline.png" width="270"><br><sub>idea pipeline</sub></td>
    <td align="center"><img src="skills/popa/assets/examples/02-chaos-vs-system.png" width="270"><br><sub>chaos vs system</sub></td>
    <td align="center"><img src="skills/popa/assets/examples/03-urgent-vs-important.png" width="270"><br><sub>urgent vs important</sub></td>
  </tr>
  <tr>
    <td align="center"><img src="skills/popa/assets/examples/04-architecture-tour.png" width="270"><br><sub>architecture tour</sub></td>
    <td align="center"><img src="skills/popa/assets/examples/05-three-states.png" width="270"><br><sub>three states</sub></td>
    <td align="center"><img src="skills/popa/assets/examples/06-idea-to-prod-route.png" width="270"><br><sub>idea → prod route</sub></td>
  </tr>
</table>

> Bundled calibration examples (also in `skills/popa/assets/examples/`). They show the style only — the skill invents a fresh composition per request and does not reuse them.

## How it works

- Workflow engine: `references/workflow-engine.md` (digest → shot list → generate → QA → deliver)
- Structures, format presets, and the **likeness rule**: `references/composition-patterns.md`
- The skill: `skills/popa/` with `SKILL.md`, `references/` (IP, style DNA, prompt template, QA), and `assets/` (`popa-ref/` likeness images + `examples/` calibration).

**Likeness rule:** the character can't be reliably produced from text alone — every generation passes Popa's reference image(s) into the image backend (e.g. nano-banana-pro / Gemini 3 Pro Image, or `image_gen`).

## Install

```
/plugin install illustration@agents-in-a-box
```
