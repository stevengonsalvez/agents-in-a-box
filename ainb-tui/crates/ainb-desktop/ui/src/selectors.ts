// The shell's root selectors: everything the chrome reads from the whole store
// rather than from one row. Each returns a scalar, so a memo over it wakes its
// readers only when the value changes, however much of the store a drain
// rewrote. `selectors.test.ts` holds every entry to that.

import type { AttentionKind, HostId } from "../../../ainb-app/bindings/AppState";
import { idleCount, ringCount } from "./sessions.ts";
import type { FrameStore } from "./store.ts";

type RootSelector = (store: FrameStore, host: HostId | undefined) => number | boolean;

const ring =
  (kind: AttentionKind) =>
  (store: FrameStore, host: HostId | undefined): number =>
  host === undefined ? 0 : ringCount(store.section(host, "sessions"), kind);

export const ROOT_SELECTORS = {
  askCount: ring("Ask"),
  approveCount: ring("Approve"),
  waitCount: ring("Wait"),
  errCount: ring("Err"),
  idleCount: (store, host) => (host === undefined ? 0 : idleCount(store.section(host, "sessions"))),
  sessionsStale: (store, host) => host !== undefined && store.state.stale[host]?.sessions === true,
  workspacesLoading: (store, host) =>
    host !== undefined && store.section(host, "workspace_load")?.is_loading_workspaces === true,
  hostCount: (store) => Object.keys(store.state.hosts).length,
} satisfies Record<string, RootSelector>;
