import { render } from "solid-js/web";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AttentionKind, FrameBatch_Serialize, HostId } from "../../../ainb-app/bindings/AppState";
import { createFrameStore, type SectionName } from "./store.ts";
import { idleCount, ringCount } from "./sessions.ts";
import { Sidebar } from "./sidebar.tsx";
import "./shell.css";

/**
 * The one list of sections this window subscribes to; `subscribe` hands it to
 * the host. Sessions and Fleet feed the sidebar and the header counts. Shell,
 * Tmux, Config and AgentStatus are subscribed ahead of their readers (the
 * terminal tabs in D1c, settings in D3, agent cards in D2).
 */
const SUBSCRIBED: SectionName[] = ["sessions", "shell", "tmux", "fleet", "config", "agent_status"];

/** How long batches gather before one drain applies them all. */
const DRAIN_MS = 16;

/** The header's counts, in the order it draws them. */
const HEADER_KINDS: [AttentionKind, string][] = [
  ["Ask", "ASK"],
  ["Approve", "APPROVE"],
  ["Wait", "WAIT"],
  ["Err", "ERR"],
];

/** `ainb_desktop::sidecar::SidecarView`: no pid and no filesystem path. */
type SidecarState =
  | { state: "starting" }
  | { state: "connected"; spawned: boolean }
  | { state: "reconnecting"; error: string }
  | { state: "degraded"; error: string; has_log: boolean };

function Shell() {
  const store = createFrameStore(SUBSCRIBED);
  const [sidecar, setSidecar] = createSignal<SidecarState>({ state: "starting" });
  const [log, setLog] = createSignal<string | null>(null);

  // The host the channel is connected to, from the `subscribe` answer. Every
  // drain is applied as that host's.
  const [peer, setPeer] = createSignal<HostId>();

  onMount(async () => {
    const unlisten = await listen<SidecarState>("sidecar", (event) => setSidecar(event.payload));
    onCleanup(unlisten);
    setSidecar(await invoke<SidecarState>("sidecar_state"));

    // A timer, not an animation frame: a hidden window still drains, so the
    // queue never grows while nobody looks.
    let queue: FrameBatch_Serialize[] = [];
    const drain = () => {
      const host = peer();
      // The first batches can arrive before `subscribe` answers: hold them.
      if (host === undefined) {
        setTimeout(drain, DRAIN_MS);
        return;
      }
      const batches = queue;
      queue = [];
      store.applyDrain(host, batches);
    };
    const frames = new Channel<FrameBatch_Serialize>();
    frames.onmessage = (batch) => {
      if (queue.push(batch) === 1) setTimeout(drain, DRAIN_MS);
    };
    setPeer(await invoke<HostId>("subscribe", { frames, sections: SUBSCRIBED }));
  });

  // In this node the window holds exactly one host, the local one.
  const host = peer;
  const sessions = () => (host() ? store.section(host()!, "sessions") : undefined);
  const fleet = () => (host() ? store.section(host()!, "fleet") : undefined);
  const counts = HEADER_KINDS.map(([kind, label]) => ({
    label,
    count: createMemo(() => ringCount(sessions(), fleet(), kind)),
  }));
  const idle = createMemo(() => idleCount(sessions()));
  const sessionsStale = createMemo(() => {
    const id = host();
    return id !== undefined && store.state.stale[id]?.sessions === true;
  });

  const banner = () => {
    const state = sidecar();
    switch (state.state) {
      case "starting":
        return "Connecting to the hangar daemon";
      case "connected":
        return state.spawned ? "Started the hangar daemon" : "Attached to the hangar daemon";
      case "reconnecting":
        return `Reconnecting: ${state.error}`;
      case "degraded":
        return `No hangar daemon: ${state.error}`;
    }
  };

  return (
    <main class="shell">
      <header class="header">
        <span class="host" title="Host">
          host: {host() ?? "none"}
        </span>
        <span class="counts" aria-label="Attention">
          <For each={counts}>
            {(entry) => (
              <Show when={entry.count() > 0}>
                <span class="count attention">
                  {entry.count()} {entry.label}
                </span>
              </Show>
            )}
          </For>
          <span class="count">{idle()} IDLE</span>
        </span>
        {/* ponytail: the settings page is D3; the entry is drawn and inert until then. */}
        <button type="button" class="settings" disabled title="Settings">
          ⚙
        </button>
      </header>
      <Show when={sidecar().state !== "connected"}>
        <div class={`banner ${sidecar().state}`} role="status">
          <span>{banner()}</span>
          <Show when={sidecar().state === "degraded"}>
            <span class="actions">
              <Show when={(sidecar() as { has_log?: boolean }).has_log}>
                <button
                  type="button"
                  onClick={async () => setLog((await invoke<string | null>("show_log")) ?? "")}
                >
                  Show log
                </button>
              </Show>
              <button
                type="button"
                onClick={() => {
                  setLog(null);
                  void invoke("retry_sidecar");
                }}
              >
                Retry
              </button>
            </span>
          </Show>
        </div>
      </Show>
      <Show when={log() !== null}>
        <pre class="sidecar-log" aria-label="Sidecar log">
          {log()}
        </pre>
      </Show>
      <div class="body">
        <Sidebar sessions={sessions()} fleet={fleet()} stale={sessionsStale()} />
        <section class="workarea">
          {/* Terminal tabs attach here with D1c. */}
          <nav class="tabs" aria-label="Terminal tabs">
            <button type="button" class="tab new" disabled title="New terminal tab">
              +
            </button>
          </nav>
        </section>
      </div>
    </main>
  );
}

render(() => <Shell />, document.getElementById("root")!);
