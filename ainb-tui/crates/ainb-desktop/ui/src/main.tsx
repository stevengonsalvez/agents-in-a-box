import { render } from "solid-js/web";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./shell.css";

/** One section frame, as `ainb_app::wire::frame::Frame` serialises it. */
interface Frame {
  section: string;
  version: number;
  epoch: number;
  host_id: string;
}

/** What one host tick sends down the channel. */
interface FrameBatch {
  frames: Frame[];
  oversize?: { section: string; version: number; bytes: number }[];
}

/** `ainb_desktop::sidecar::SidecarState`. */
type SidecarState =
  | { state: "starting" }
  | { state: "connected"; daemon_pid: number | null; spawned: boolean }
  | { state: "reconnecting"; error: string }
  | { state: "degraded"; error: string; log: string };

function Shell() {
  const [sidecar, setSidecar] = createSignal<SidecarState>({ state: "starting" });
  // Section name to the latest version and epoch this renderer applied.
  const [sections, setSections] = createSignal<Record<string, Frame>>({});
  const [stale, setStale] = createSignal<string[]>([]);

  onMount(async () => {
    const unlisten = await listen<SidecarState>("sidecar", (event) => setSidecar(event.payload));
    onCleanup(unlisten);
    setSidecar(await invoke<SidecarState>("sidecar_state"));

    const frames = new Channel<FrameBatch>();
    frames.onmessage = (batch) => {
      setSections((held) => {
        const next = { ...held };
        for (const frame of batch.frames) next[frame.section] = frame;
        return next;
      });
      setStale((batch.oversize ?? []).map((section) => section.section));
    };
    await invoke("subscribe", { frames });
  });

  const banner = () => {
    const state = sidecar();
    switch (state.state) {
      case "starting":
        return "Connecting to the hangar daemon";
      case "connected":
        return state.spawned
          ? `Started the hangar daemon (pid ${state.daemon_pid ?? "unknown"})`
          : `Attached to the hangar daemon (pid ${state.daemon_pid ?? "unknown"})`;
      case "reconnecting":
        return `Reconnecting: ${state.error}`;
      case "degraded":
        return `No hangar daemon: ${state.error}`;
    }
  };

  return (
    <main class="shell">
      <header class={`banner ${sidecar().state}`}>
        <span>{banner()}</span>
        <Show when={sidecar().state === "degraded"}>
          <button type="button" onClick={() => invoke("retry_sidecar")}>
            Retry
          </button>
        </Show>
      </header>
      <section class="sections" aria-label="Sections this window holds">
        <For each={Object.values(sections()).sort((a, b) => a.section.localeCompare(b.section))}>
          {(frame) => (
            <div class="section-row" data-section={frame.section}>
              <span class="name">{frame.section}</span>
              <span class="version">v{frame.version}</span>
              <Show when={stale().includes(frame.section)}>
                <span class="stale">stale</span>
              </Show>
            </div>
          )}
        </For>
      </section>
    </main>
  );
}

render(() => <Shell />, document.getElementById("root")!);
