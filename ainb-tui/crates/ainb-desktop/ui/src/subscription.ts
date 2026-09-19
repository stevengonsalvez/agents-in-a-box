// The window's one subscription list, and the section reads the shell chrome
// (sidebar and header) makes. `subscription.test.ts` holds the two together:
// a section read but not subscribed, or subscribed with neither a reader nor
// a place in `AHEAD_OF_READERS`, fails it (#1132).

import type {
  AgentStatusView,
  FleetView_Serialize,
  GitViewView_Serialize,
  HostId,
  SessionsView_Serialize,
} from "../../../ainb-app/bindings/AppState";
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
  "git_view",
];

/**
 * Subscribed ahead of their readers: settings in D3. A section leaves this
 * list when its reader lands, as Fleet and agent status did with the board.
 */
export const AHEAD_OF_READERS: SectionName[] = ["shell", "tmux", "config"];

/** The sidebar's rows and the tab titles: the host's Sessions section. */
export function shellSessions(store: FrameStore, host: HostId | undefined): SessionsView_Serialize | undefined {
  return host === undefined ? undefined : store.section(host, "sessions");
}

/** The board's cards and their health: the host's agent status section. */
export function shellAgentStatus(store: FrameStore, host: HostId | undefined): AgentStatusView | undefined {
  return host === undefined ? undefined : store.section(host, "agent_status");
}

/** The board's lines, the attention list and the answer banner: Fleet. */
export function shellFleet(store: FrameStore, host: HostId | undefined): FleetView_Serialize | undefined {
  return host === undefined ? undefined : store.section(host, "fleet");
}

/**
 * The review tab's files, hunks and rows: the host's GitView section.
 *
 * Bounded before it is sent (`ainb-app/src/wire/git_view.rs`), so what arrives
 * is a window on the diff with counters saying what it left out, never the
 * whole of a large one.
 */
export function shellGitView(store: FrameStore, host: HostId | undefined): GitViewView_Serialize | undefined {
  return host === undefined ? undefined : store.section(host, "git_view");
}
