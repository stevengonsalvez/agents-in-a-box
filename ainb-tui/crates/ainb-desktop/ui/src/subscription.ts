// The window's one subscription list, and the section reads the shell chrome
// (sidebar and header) makes. `subscription.test.ts` holds the two together:
// a section read but not subscribed, or subscribed with neither a reader nor
// a place in `AHEAD_OF_READERS`, fails it (#1132).

import type { HostId, SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import type { FrameStore, SectionName } from "./store.ts";

/**
 * The one list of sections this window subscribes to; `subscribe` hands it to
 * the host. Sessions feeds the sidebar and the header counts (its rows carry
 * the merged attention), and WorkspaceLoad the sidebar's loading state.
 */
export const SUBSCRIBED: SectionName[] = [
  "sessions",
  "workspace_load",
  "shell",
  "tmux",
  "fleet",
  "config",
  "agent_status",
];

/**
 * Subscribed ahead of their readers: the attention list and agent cards in
 * D2, settings in D3. A section leaves this list when its reader lands.
 */
export const AHEAD_OF_READERS: SectionName[] = ["shell", "tmux", "fleet", "config", "agent_status"];

/** The sidebar's rows and the tab titles: the host's Sessions section. */
export function shellSessions(store: FrameStore, host: HostId | undefined): SessionsView_Serialize | undefined {
  return host === undefined ? undefined : store.section(host, "sessions");
}
