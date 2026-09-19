// The window's one subscription list, and the section reads the shell chrome
// (sidebar and header) makes. `subscription.test.ts` holds the two together:
// a section read but not subscribed, or subscribed with neither a reader nor
// a place in `AHEAD_OF_READERS`, fails it (#1132).

import type {
  AgentStatusView,
  ConfigView_Serialize,
  FleetView_Serialize,
  HangarView_Serialize,
  GitViewView_Serialize,
  HostId,
  InboxView_Serialize,
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
  "hangar",
  "git_view",
  "inbox",
];

/**
 * Subscribed ahead of their readers. A section leaves this list when its
 * reader lands, as Fleet and agent status did with the board and config and
 * hangar did with the settings page.
 */
export const AHEAD_OF_READERS: SectionName[] = ["shell", "tmux"];

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

/** The settings page's form: the host's config section. */
export function shellConfig(store: FrameStore, host: HostId | undefined): ConfigView_Serialize | undefined {
  return host === undefined ? undefined : store.section(host, "config");
}

/** The settings page's daemons panel: the host's hangar section. */
export function shellHangar(store: FrameStore, host: HostId | undefined): HangarView_Serialize | undefined {
  return host === undefined ? undefined : store.section(host, "hangar");
}

/**
 * The config section version the settings page drew, named on every edit so
 * the reducer can refuse an edit of a frame it has moved past. 0 before the
 * first frame, which no live section carries.
 */
export function configRevision(store: FrameStore, host: HostId | undefined): number {
  return host === undefined ? 0 : (store.state.hosts[host]?.sections.config?.version ?? 0);
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

/**
 * The inbox page's rows and the header's unread count: section 16, the
 * daemon's inbox as the host folded it (D3p-c). Subscribing to it is what
 * starts the host's inbox reader.
 */
export function shellInbox(store: FrameStore, host: HostId | undefined): InboxView_Serialize | undefined {
  return host === undefined ? undefined : store.section(host, "inbox");
}
