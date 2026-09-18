import { render } from "solid-js/web";
import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { FrameBatch_Serialize, HostId } from "../../../ainb-app/bindings/AppState";
import { createFrameStore } from "./store.ts";
import { shellAgentStatus, shellFleet, shellSessions, SUBSCRIBED } from "./subscription.ts";
import { allSessions, label } from "./sessions.ts";
import { ROOT_SELECTORS } from "./selectors.ts";
import { AcpCard } from "./acp.tsx";
import { transcriptIntent, transcriptView } from "./acp.ts";
import { AnswerBanner } from "./answer.tsx";
import { phaseOf, questionFor, type Refusal, sendInOrder } from "./answer.ts";
import { newNotices, noticeKey } from "./notices.ts";
import { Board } from "./board.tsx";
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

/** How long batches gather before one drain applies them all. */
const DRAIN_MS = 16;

/** The header's counts, in the order it draws them. */
const HEADER_COUNTS = [
  [ROOT_SELECTORS.askCount, "ASK"],
  [ROOT_SELECTORS.approveCount, "APPROVE"],
  [ROOT_SELECTORS.waitCount, "WAIT"],
  [ROOT_SELECTORS.errCount, "ERR"],
] as const;

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
  // The board is the window's landing surface: what every agent is doing, and
  // what is waiting on a human. A terminal takes the work area while it is
  // chosen, and the board is one click back.
  const [board, setBoard] = createSignal(true);
  // The ACP session whose transcript card holds the work area, if any. It has
  // no tmux pane, so the card stands where its terminal would.
  const [transcriptKey, setTranscriptKey] = createSignal<string | null>(null);
  const focusers = new Map<string, () => void>();
  const tabKeys = createMemo(
    () => tabs().map((tab) => tab.key),
    [],
    { equals: (a, b) => a.length === b.length && a.every((key, i) => key === b[i]) },
  );
  let sidebar: HTMLElement | undefined;

  const activate = (key: string | null) => {
    setActive(key);
    if (key !== null) {
      setBoard(false);
      closeTranscript();
    }
    if (key !== null) requestAnimationFrame(() => focusers.get(key)?.());
  };
  const showTabs = (view: TabsView) => {
    setTabs(view.tabs);
    for (const key of focusers.keys()) {
      if (!view.tabs.some((tab) => tab.key === key)) focusers.delete(key);
    }
    if (view.focus !== null) activate(view.focus);
    else if (!view.tabs.some((tab) => tab.key === active())) {
      // The shown tab ended (an unrelated tmux session died, say): point at
      // the next one WITHOUT leaving the board. `activate` means a person chose
      // a terminal; this is the strip tidying up after itself.
      const next = view.tabs[0]?.key ?? null;
      setActive(next);
      if (next !== null && !board()) requestAnimationFrame(() => focusers.get(next)?.());
    }
  };
  // A refused intent comes back with the row and the reason: say so, or a
  // key the window may not use (onboarding installs, a commit) looks dead.
  const report = (refusal: Refusal | null) => {
    if (refusal) toast(`${refusal.command} is not run from the window: ${refusal.reason}`);
  };
  const dispatch = (intent: RendererIntent) => void invoke<Refusal | null>("dispatch", { intent }).then(report);
  const openTranscript = (sessionKey: string) => {
    dispatch(transcriptIntent(sessionKey));
    setTranscriptKey(sessionKey);
    setBoard(false);
  };
  /** Close the card; the host drops the transcript and frames the default. */
  function closeTranscript() {
    if (transcriptKey() === null) return;
    dispatch(transcriptIntent(null));
    setTranscriptKey(null);
  }
  /**
   * Send `intents` in order, each one applied before the next is sent: the
   * host applies a dispatch before the command returns, so awaiting each keeps
   * a cursor move ahead of the Enter that reads it. A refusal on any of them
   * is reported the same way.
   */
  // Stops at the first refusal: a pick whose cursor move was refused must not
  // go on to send Enter on whatever option the reducer is pointing at.
  const run = async (intents: RendererIntent[]) => {
    report(await sendInOrder(intents, (intent) => invoke<Refusal | null>("dispatch", { intent })));
  };
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
  const sessions = () => shellSessions(store, host());
  const fleet = () => shellFleet(store, host());
  const agentStatus = () => shellAgentStatus(store, host());
  const counts = HEADER_COUNTS.map(([select, label]) => ({
    label,
    count: createMemo(() => select(store, host())),
  }));
  const idle = createMemo(() => ROOT_SELECTORS.idleCount(store, host()));
  const sessionsStale = createMemo(() => ROOT_SELECTORS.sessionsStale(store, host()));
  const loading = createMemo(() => ROOT_SELECTORS.workspacesLoading(store, host()));
  const elsewhere = createMemo(() => ROOT_SELECTORS.attentionElsewhere(store, host()));
  const shell = () => (host() ? store.section(host()!, "shell") : undefined);
  const ask = () => fleet()?.ask_state;
  const question = createMemo(() => questionFor(sessions()));

  // The reducer speaks through its notices: a refused send says why in the
  // reducer's own words (a daemon that is gone, a native picker, nothing typed),
  // so the window shows each new one as a toast rather than a dead button. New
  // by identity, not by count: the list is capped and drained from the front.
  let heard: string[] = [];
  createEffect(() => {
    const notices = shell()?.notifications ?? [];
    // A success notice is the reducer congratulating itself ("Workspaces
    // loaded"), and the window rescans on a cadence: only what went wrong, or
    // what the person needs to know, becomes a toast.
    for (const notice of newNotices(heard, notices)) {
      if (notice.notification_type !== "Success") toast(label(notice.message));
    }
    heard = notices.map(noticeKey);
  });
  // Another surface answered first: the row reads delivered, and the winner is
  // named once, in a toast, however many frames repeat it.
  createEffect(
    on(
      () => {
        const phase = phaseOf(ask());
        return phase.kind === "already_answered" ? [ask()?.request, phase.by].join("\n") : null;
      },
      (winner, previous) => {
        if (winner !== null && winner !== previous) toast(`Already answered by ${winner.slice(winner.indexOf("\n") + 1)}`);
      },
    ),
  );

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
          {/* A development build shows frames the store refused (#1132). */}
          <Show when={import.meta.env.DEV && store.framesIgnored() > 0}>
            <span class="count ignored" title="Frames the store ignored">
              {store.framesIgnored()} ignored
            </span>
          </Show>
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
          <nav class="tabs" aria-label="Board and terminals">
            <span class="tab board-tab" classList={{ active: board() && transcriptKey() === null }}>
              <button
                type="button"
                class="tab-title"
                aria-current={board() && transcriptKey() === null ? "page" : undefined}
                onClick={() => {
                  closeTranscript();
                  setBoard(true);
                }}
              >
                Board
              </button>
            </span>
            {/* The ACP card's own place in the strip, where the session's
                terminal tab would be if it had a pane. */}
            <Show when={transcriptKey()}>
              {(key) => (
                <span class="tab transcript-tab active" data-state="transcript">
                  <button type="button" class="tab-title" aria-current="page">
                    {key()}
                  </button>
                  <button
                    type="button"
                    class="tab-close"
                    aria-label={`Close ${key()}`}
                    onClick={() => {
                      closeTranscript();
                      setBoard(true);
                    }}
                  >
                    ×
                  </button>
                </span>
              )}
            </Show>
            <For each={tabs()}>
              {(tab) => (
                <span
                  class="tab"
                  classList={{ active: !board() && transcriptKey() === null && tab.key === active() }}
                  data-state={tab.state}
                >
                  <button
                    type="button"
                    class="tab-title"
                    aria-current={!board() && transcriptKey() === null && tab.key === active() ? "page" : undefined}
                    onClick={() => choose(tab)}
                  >
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
          <Show when={question()}>
            {(shown) => <AnswerBanner question={shown()} ask={ask()} run={run} />}
          </Show>
          <Show when={transcriptKey()}>
            {(key) => (
              <AcpCard
                sessionKey={key()}
                view={transcriptView(fleet(), key())}
                onClose={() => {
                  closeTranscript();
                  setBoard(true);
                }}
              />
            )}
          </Show>
          <Show when={board() && transcriptKey() === null}>
            <Board
              agentStatus={agentStatus()}
              fleet={fleet()}
              sessions={sessions()}
              elsewhere={elsewhere()}
              onChoose={dispatch}
              onOpenTranscript={openTranscript}
            />
          </Show>
          <Show when={!board() && transcriptKey() === null && tabs().length === 0}>
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
                    active={!board() && transcriptKey() === null && key === active()}
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
