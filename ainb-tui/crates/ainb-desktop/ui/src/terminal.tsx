import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import "@xterm/xterm/css/xterm.css";
import { accelerator, escEsc, openRowIntent, REDIALS, rowOf, type Accelerator, type Tab } from "./tabs.ts";
import { tauriTransport } from "./transport.ts";

interface Props {
  tab: Tab;
  title: string;
  active: boolean;
  mac: boolean;
  onAccelerator(accelerator: Accelerator): void;
  /** Esc Esc: focus goes back to the sidebar. */
  onLeave(): void;
  /** Hands the parent a way to focus this tab's terminal. */
  focusRef(focus: () => void): void;
}

/**
 * One tab's terminal. It stays mounted while the tab is listed, hidden when
 * another is active, so its buffer survives a tab switch and a reconnect.
 */
export function TerminalView(props: Props) {
  let host!: HTMLDivElement;
  const attempt = () => (props.tab.state === "reconnecting" ? props.tab.attempt : 0);
  // Bytes this pane has painted, kept on the element. It says the pane is
  // live without reaching into the renderer's canvas, and it is what the
  // journey times a large read by.
  const [painted, setPainted] = createSignal(0);

  onMount(() => {
    const term = new Terminal({
      cursorBlink: true,
      fontFamily: "ui-monospace, monospace",
      fontSize: 13,
      scrollback: 5000,
      theme: { background: "rgb(25, 25, 35)", foreground: "rgb(220, 220, 230)" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    try {
      const webgl = new WebglAddon();
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      // No WebGL in this webview: xterm keeps its DOM renderer.
    }

    const transport = tauriTransport(props.tab.key);
    transport.onBytes(
      (bytes) =>
        new Promise<void>((done) =>
          term.write(bytes, () => {
            setPainted((total) => total + bytes.byteLength);
            done();
          }),
        ),
    );
    term.onData((data) => transport.send(data));

    // Focus rules: the shell accelerators stay with the shell, Esc Esc leaves,
    // everything else (ctrl+c, ctrl+b, arrows) goes to the pane.
    const leave = escEsc();
    term.attachCustomKeyEventHandler((event) => {
      if (event.type !== "keydown") return true;
      const shell = accelerator(event, props.mac);
      if (shell) {
        // Marked handled, so the window's own listener does not act twice.
        event.preventDefault();
        // Copy and paste act on this pane, so they are answered here; the rest
        // is the shell's.
        if (shell.kind === "copy") {
          const selection = term.getSelection();
          if (selection) void invoke("clipboard_write", { text: selection });
        } else if (shell.kind === "paste") {
          // Through xterm, not straight to the transport: the terminal wraps a
          // paste in the bracketed-paste markers the pane asked for, so a
          // multi-line payload arrives as text rather than as lines the shell
          // runs one by one. The macOS menu's own paste takes the same path.
          void invoke<string>("clipboard_read", { key: props.tab.key }).then((text) => {
            if (text) term.paste(text);
          });
        } else {
          props.onAccelerator(shell);
        }
        return false;
      }
      if (event.key === "Escape" && leave(event.timeStamp)) {
        event.preventDefault();
        props.onLeave();
        return false;
      }
      return true;
    });

    const resize = () => {
      // A hidden tab has no size; it fits when it is shown.
      if (host.offsetParent === null) return;
      fit.fit();
      transport.resize(term.cols, term.rows);
    };
    const observer = new ResizeObserver(resize);
    observer.observe(host);
    props.focusRef(() => {
      resize();
      term.focus();
    });

    onCleanup(() => {
      observer.disconnect();
      term.dispose();
    });
  });

  return (
    <div class="terminal" hidden={!props.active} data-tab={props.tab.key} data-painted={painted()}>
      <div class="xterm-host" ref={host} />
      <Show when={props.tab.state !== "attached"}>
        <div class="terminal-overlay" role="status">
          <Show
            when={attempt() > 0}
            fallback={
              <>
                <span>{props.title} is detached</span>
                <button
                  type="button"
                  onClick={() => void invoke("dispatch", { intent: openRowIntent(rowOf(props.tab.target)) })}
                >
                  Reattach
                </button>
              </>
            }
          >
            <span>
              Reconnecting to {props.title} ({attempt()} of {REDIALS})
            </span>
          </Show>
        </div>
      </Show>
    </div>
  );
}
