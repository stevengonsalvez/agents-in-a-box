import { render } from "solid-js/web";
import { createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { FrameBatch_Serialize, HostId } from "../../../ainb-app/bindings/AppState";
import { createFrameStore, type SectionName } from "./store.ts";
import { allSessions, label } from "./sessions.ts";
import { ROOT_SELECTORS } from "./selectors.ts";
import { Palette } from "./palette.tsx";
import { Sidebar } from "./sidebar.tsx";
import {
  accelerator,
  openRowIntent,
  rowOf,
  stepTab,
  type Accelerator,
  type RendererIntent,
  type RowId,
  type Tab,
  type TabsView,
} from "./tabs.ts";
import { TerminalView } from "./terminal.tsx";
import "./shell.css";

/**
 * The one list of sections this window subscribes to; `subscribe` hands it to
 * the host. Sessions feeds the sidebar and the header counts (its rows carry
 * the merged attention), and WorkspaceLoad the sidebar's loading state. Shell,
 * Tmux, Fleet, Config and AgentStatus are subscribed ahead of their readers
 * (the attention list and agent cards in D2, settings in D3).
 */
const SUBSCRIBED: SectionName[] = [
  "sessions",
  "workspace_load",
  "shell",
  "tmux",
  "fleet",
  "config",
  "agent_status",
];

/** How long batches gather before one drain applies them all. */
const DRAIN_MS = 16;

/** The header's counts, in the order it draws them. */
const HEADER_COUNTS = [
  [ROOT_SELECTORS.askCount, "ASK"],
  [ROOT_SELECTORS.approveCount, "APPROVE"],
  [ROOT_SELECTORS.waitCount, "WAIT"],
  [ROOT_SELECTORS.errCount, "ERR"],
] as const;

/**
 * `ainb_desktop::intent::Refusal`: an intent the host did not apply, with the
 * row it would have run and why. Keys that write outside ainb run only from
 * the TUI or the CLI.
 */
type Refusal = { command: string; reason: string };

/** How long a toast stays up. */
const TOAST_MS = 5000;

const MAC = navigator.userAgent.includes("Mac");

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

  // The host the channel is connected to: the `subscribe` answer, then every
  // `host` event. The host sends that event before it re-pins its frames to a
  // new id (#1066), so every drain after it is applied as the new host's.
  const [peer, setPeer] = createSignal<HostId>();

  // Registered synchronously: an `onCleanup` after an `await` has left the
  // owner and never runs.
  const listeners = [
    listen<SidecarState>("sidecar", (event) => setSidecar(event.payload)),
    listen<TabsView>("terminal_tabs", (event) => showTabs(event.payload)),
    listen<string>("toast", (event) => toast(event.payload)),
    listen<HostId>("host", (event) => {
      // The host re-pinned its frames to a new id (#1066). What the old id
      // left in the store is never framed again: drop it, so it neither shows
      // nor holds a MAX_HOSTS slot.
      const stale = peer();
      setPeer(event.payload);
      if (stale !== undefined && stale !== event.payload) store.evictHost(stale);
    }),
  ];
  onCleanup(() => listeners.forEach((unlisten) => void unlisten.then((stop) => stop())));

  // Terminal tabs: the strip is the Rust side's; which tab shows is ours.
  const [tabs, setTabs] = createSignal<Tab[]>([]);
  const [active, setActive] = createSignal<string | null>(null);
  const focusers = new Map<string, () => void>();
  const tabKeys = createMemo(
    () => tabs().map((tab) => tab.key),
    [],
    { equals: (a, b) => a.length === b.length && a.every((key, i) => key === b[i]) },
  );
  let sidebar: HTMLElement | undefined;

  const activate = (key: string | null) => {
    setActive(key);
    if (key !== null) requestAnimationFrame(() => focusers.get(key)?.());
  };
  const showTabs = (view: TabsView) => {
    setTabs(view.tabs);
    for (const key of focusers.keys()) {
      if (!view.tabs.some((tab) => tab.key === key)) focusers.delete(key);
    }
    if (view.focus !== null) activate(view.focus);
    else if (!view.tabs.some((tab) => tab.key === active())) activate(view.tabs[0]?.key ?? null);
  };
  // A refused intent comes back with the row and the reason: say so, or a
  // key the window may not use (onboarding installs, a commit) looks dead.
  const dispatch = (intent: RendererIntent) =>
    void invoke<Refusal | null>("dispatch", { intent }).then((refusal) => {
      if (refusal) toast(`${refusal.command} is not run from the window: ${refusal.reason}`);
    });
  /** Select a session-list row and attach it, so the reducer marks it attached. */
  const openRow = (row: RowId) => dispatch(openRowIntent(row));

  // The palette is mounted only while it is open: each opening lists the
  // commands afresh, with the host's answer for which of them run now.
  const [palette, setPalette] = createSignal(false);
  const closePalette = () => {
    setPalette(false);
    const key = active();
    if (key !== null) focusers.get(key)?.();
    else sidebar?.focus();
  };
  const choose = (tab: Tab) => {
    if (tab.state === "detached") openRow(rowOf(tab.target));
    activate(tab.key);
  };
  const onAccelerator = (shell: Accelerator) => {
    switch (shell.kind) {
      case "tab": {
        const tab = tabs()[shell.index];
        if (tab) choose(tab);
        return;
      }
      case "prev":
      case "next":
        activate(stepTab(tabs(), active(), shell.kind === "next" ? 1 : -1));
        return;
      case "close": {
        const key = active();
        if (key !== null) void invoke("terminal_close", { key });
        return;
      }
      // ponytail: the attention jump is D2, the host switcher R1.
      case "attention":
      case "hosts":
        return;
      // Answered by the terminal that has focus; outside one there is no
      // selection to copy and nowhere to paste.
      case "copy":
      case "paste":
        return;
      case "palette":
        // The second press closes it the way Esc does, so focus goes back to
        // the pane or the sidebar rather than to the body.
        if (palette()) closePalette();
        else setPalette(true);
        return;
    }
  };

  const [toasts, setToasts] = createSignal<{ id: number; text: string }[]>([]);
  let toastId = 0;
  const toast = (text: string) => {
    const id = ++toastId;
    setToasts((shown) => [...shown, { id, text: label(text) }]);
    setTimeout(() => setToasts((shown) => shown.filter((entry) => entry.id !== id)), TOAST_MS);
  };

  // The accelerators work outside a terminal too; a terminal marks the ones
  // it handled, so they do not run twice.
  const onKey = (event: KeyboardEvent) => {
    if (event.defaultPrevented) return;
    const shell = accelerator(event, MAC);
    if (shell) {
      event.preventDefault();
      onAccelerator(shell);
    }
  };
  window.addEventListener("keydown", onKey);
  onCleanup(() => window.removeEventListener("keydown", onKey));

  const openSession = (id: string) => openRow({ session: id });

  onMount(async () => {
    setSidecar(await invoke<SidecarState>("sidecar_state"));
    showTabs(await invoke<TabsView>("terminal_tabs"));

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
      // What the renderer applied, for the proof harness to read from the
      // log. Names and a count, never a body, and never a drain that carried
      // nothing for this window.
      const applied = [...new Set(batches.flatMap(({ frames }) => frames.map((frame) => frame.section)))];
      // A drain carrying nothing but the loading flag is not something the
      // renderer applied for a reader to see, and the window asks for a scan
      // on a cadence, so reporting it would append a line forever.
      if (applied.some((section) => section !== "workspace_load")) {
        void invoke("renderer_applied", {
          sections: applied,
          sessions: allSessions(store.section(host, "sessions")).length,
        });
      }
    };
    const frames = new Channel<FrameBatch_Serialize>();
    frames.onmessage = (batch) => {
      if (queue.push(batch) === 1) setTimeout(drain, DRAIN_MS);
    };
    const answered = await invoke<HostId>("subscribe", { frames, sections: SUBSCRIBED });
    // A `host` event that landed while `subscribe` was in flight is newer than
    // this answer: keep it.
    if (peer() === undefined) setPeer(answered);
  });

  // In this node the window holds exactly one host: this machine's daemon.
  const host = peer;
  const sessions = () => (host() ? store.section(host()!, "sessions") : undefined);
  const counts = HEADER_COUNTS.map(([select, label]) => ({
    label,
    count: createMemo(() => select(store, host())),
  }));
  const idle = createMemo(() => ROOT_SELECTORS.idleCount(store, host()));
  const sessionsStale = createMemo(() => ROOT_SELECTORS.sessionsStale(store, host()));
  const loading = createMemo(() => ROOT_SELECTORS.workspacesLoading(store, host()));

  /** A tab's title: its session's name when the sidebar knows it. */
  const title = (tab: Tab) => {
    const target = tab.target;
    const session = target.kind === "session" ? allSessions(sessions()).find((row) => row.id === target.id) : undefined;
    return label(session?.name ?? target.tmux);
  };

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
        <Sidebar
          sessions={sessions()}
          stale={sessionsStale()}
          loading={loading()}
          onOpen={openSession}
          ref={(element) => (sidebar = element)}
        />
        <section class="workarea">
          <nav class="tabs" aria-label="Terminal tabs">
            <For each={tabs()}>
              {(tab) => (
                <span class="tab" classList={{ active: tab.key === active() }} data-state={tab.state}>
                  <button type="button" class="tab-title" onClick={() => choose(tab)}>
                    {title(tab)}
                  </button>
                  <button
                    type="button"
                    class="tab-close"
                    aria-label={`Close ${title(tab)}`}
                    onClick={() => void invoke("terminal_close", { key: tab.key })}
                  >
                    ×
                  </button>
                </span>
              )}
            </For>
          </nav>
          <Show when={tabs().length === 0}>
            <p class="empty">Choose a session to open its terminal</p>
          </Show>
          {/* Keyed by tab key, not by the tab object each event replaces: a
              terminal stays mounted, and keeps its buffer, while its tab is listed. */}
          <For each={tabKeys()}>
            {(key) => (
              <Show when={tabs().find((tab) => tab.key === key)}>
                {(tab) => (
                  <TerminalView
                    tab={tab()}
                    title={title(tab())}
                    active={key === active()}
                    mac={MAC}
                    onAccelerator={onAccelerator}
                    onLeave={() => sidebar?.focus()}
                    focusRef={(focus) => focusers.set(key, focus)}
                  />
                )}
              </Show>
            )}
          </For>
        </section>
      </div>
      <Show when={palette()}>
        <Palette sessions={sessions()} onChoose={dispatch} onClose={closePalette} />
      </Show>
      <div class="toasts" aria-live="polite">
        <For each={toasts()}>{(entry) => <div class="toast">{entry.text}</div>}</For>
      </div>
    </main>
  );
}

render(() => <Shell />, document.getElementById("root")!);
