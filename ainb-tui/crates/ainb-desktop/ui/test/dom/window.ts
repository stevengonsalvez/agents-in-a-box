// A document for the DOM component tests: a happy-dom window installed as the
// globals Solid's client runtime reads, and a stand-in for the Tauri bridge so
// a component's `invoke` resolves without a host.
//
// Imported first by each test file, before anything that loads `solid-js/web`.

import { Window } from "happy-dom";

const window = new Window({ url: "http://localhost/" });

/** Install `value` as the global `name`, over a read-only Node global too. */
function install(name: string, value: unknown): void {
  Object.defineProperty(globalThis, name, { value, configurable: true, writable: true });
}

install("window", window);
install("document", window.document);
// Node's own `Event` and `navigator` stay: the test runner uses them, and a
// test builds its events from `window` instead.
for (const name of ["Node", "Element", "HTMLElement", "MutationObserver"]) {
  install(name, (window as unknown as Record<string, unknown>)[name]);
}

/** What the stand-in host answers each command with; a test sets it. */
export const hostReplies = new Map<string, unknown>();

(window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
  invoke: async (command: string) => hostReplies.get(command) ?? null,
  transformCallback: () => 0,
  unregisterCallback: () => undefined,
};

/** Let resources resolve and effects settle. */
export async function settle(): Promise<void> {
  for (let turn = 0; turn < 3; turn += 1) await new Promise((resolve) => setTimeout(resolve, 0));
}
