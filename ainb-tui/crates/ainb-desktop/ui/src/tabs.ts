// Terminal tabs as the webview holds them, and the focus rules (base spec
// `:239-246`): while a terminal has focus only the shell accelerators stay
// with the shell, everything else goes to the pane, and Esc twice within
// 300 ms returns focus to the sidebar.

/** `ainb_desktop::terminal::TabTarget`. */
export type TabTarget = { kind: "session"; id: string; tmux: string } | { kind: "tmux"; tmux: string };

/** `ainb_desktop::terminal::TabView`. */
export type Tab = { key: string; target: TabTarget } & (
  | { state: "attached" }
  | { state: "reconnecting"; attempt: number }
  | { state: "detached" }
);

/** `ainb_desktop::terminal::TabsView`. */
export interface TabsView {
  tabs: Tab[];
  focus: string | null;
}

/** A session-list row, as `ainb_app::app::state::SessionListRowId` spells it. */
export type RowId = { session: string } | { other_tmux: string };

/** The session-list row a tab's target is. */
export function rowOf(target: TabTarget): RowId {
  return target.kind === "session" ? { session: target.id } : { other_tmux: target.tmux };
}

/**
 * What the window's `dispatch` command takes, as
 * `ainb_desktop::intent::RendererIntent` spells it: a key, a named command
 * with its arguments, or pasted text. There is no pointer variant; the webview
 * hit-tests its own DOM and sends the command a press means.
 */
export type RendererIntent =
  | { Key: string }
  | { Command: [string, unknown] }
  | { Text: string };

/**
 * The intent that selects `row` and attaches it. Opening goes through the
 * session list's own row, so the reducer marks the session attached each time.
 */
export function openRowIntent(row: RowId): RendererIntent {
  return { Command: ["session_list.select_row", { target: row, open: true }] };
}

/** Automatic re-attaches before a tab offers "reattach": `REDIAL_DELAYS`. */
export const REDIALS = 3;

/** Esc twice within this long leaves the terminal. */
export const ESC_ESC_MS = 300;

/** What a shell accelerator asks for. */
export type Accelerator =
  | { kind: "tab"; index: number }
  | { kind: "prev" }
  | { kind: "next" }
  | { kind: "close" }
  | { kind: "palette" }
  | { kind: "attention" }
  | { kind: "hosts" }
  | { kind: "copy" }
  | { kind: "paste" };

interface KeyLike {
  /** The physical key (`KeyW`, `Digit1`), so Shift does not change it. */
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

/**
 * The shell accelerator `event` is, or `null` for a key the pane gets. The
 * spec's `cmd` is Command on macOS; elsewhere, where Ctrl belongs to the pane
 * (ctrl+c, ctrl+b), it is Ctrl+Shift, as desktop terminals do, so there the
 * spec's cmd+shift+h is Ctrl+Shift+H.
 */
export function accelerator(event: KeyLike, mac: boolean): Accelerator | null {
  const mod = mac ? event.metaKey && !event.ctrlKey : event.ctrlKey && event.shiftKey && !event.metaKey;
  if (!mod || event.altKey) return null;
  if (event.code === "KeyH" && (!mac || event.shiftKey)) return { kind: "hosts" };
  if (mac && event.shiftKey) return null;
  // Copy and paste: macOS has them on the Edit menu, natively. Elsewhere the
  // pane owns ctrl+c and ctrl+v, so the shell's ctrl+shift pair does it.
  if (!mac && (event.code === "KeyC" || event.code === "KeyV")) {
    return { kind: event.code === "KeyC" ? "copy" : "paste" };
  }
  const digit = /^Digit([1-9])$/.exec(event.code);
  if (digit) return { kind: "tab", index: Number(digit[1]) - 1 };
  switch (event.code) {
    case "BracketLeft":
      return { kind: "prev" };
    case "BracketRight":
      return { kind: "next" };
    case "KeyW":
      return { kind: "close" };
    case "KeyK":
      return { kind: "palette" };
    case "KeyU":
      return { kind: "attention" };
    default:
      return null;
  }
}

/** A tracker that answers `true` for the second Esc within `ESC_ESC_MS`. */
export function escEsc(windowMs = ESC_ESC_MS): (now: number) => boolean {
  let last = -Infinity;
  return (now) => {
    const second = now - last <= windowMs;
    last = second ? -Infinity : now;
    return second;
  };
}

/** The key of the tab `step` places from `current`, wrapping. */
export function stepTab(tabs: readonly Tab[], current: string | null, step: number): string | null {
  if (tabs.length === 0) return null;
  const at = tabs.findIndex((tab) => tab.key === current);
  const from = at < 0 ? 0 : at;
  return tabs[(from + step + tabs.length) % tabs.length].key;
}
