// The sidecar banner's words and the daemons panel's line about the daemon
// this window is on, from `ainb_desktop::sidecar::SidecarView`.

/** `ainb_desktop::sidecar::SidecarView`: no pid and no filesystem path. */
export type SidecarState =
  | { state: "starting" }
  | {
      state: "connected";
      spawned: boolean;
      /** The daemon's build version; `null` from one that predates the negotiation. */
      daemon_version: string | null;
      /** The protocol range the daemon speaks. */
      protocol: { min: number; max: number };
    }
  | { state: "reconnecting"; error: string }
  | {
      state: "incompatible";
      /** The daemon's own sentence, which names the fix. */
      message: string;
      /** The daemon's range sits above this app's: the app is the older side. */
      daemon_is_newer: boolean;
    }
  | { state: "degraded"; error: string; has_log: boolean };

/**
 * The stop verb, as the executor runs it (`ainb daemon <daemon> <verb>`):
 * the operator runs it from a terminal, because the app never signals a
 * daemon that may be serving live sessions.
 */
export const STOP_DAEMON = "ainb daemon hangar-daemon stop";

/** The banner's sentence for `state`. */
export function banner(state: SidecarState): string {
  switch (state.state) {
    case "starting":
      return "Connecting to the hangar daemon";
    case "connected":
      return state.spawned ? "Started the hangar daemon" : "Attached to the hangar daemon";
    case "reconnecting":
      return `Reconnecting: ${state.error}`;
    case "incompatible":
      // The daemon's own words first: they name both ranges. Then which of
      // the two binaries to move, and the verb.
      return state.daemon_is_newer
        ? `${state.message}. This app is older than the daemon: update the app, then retry.`
        : `${state.message}. Stop it from a terminal with ${STOP_DAEMON}, then retry: this app starts its own daemon.`;
    case "degraded":
      return `No hangar daemon: ${state.error}`;
  }
}

/** Whether the banner offers Retry: the states the operator leaves by hand. */
export function retryable(state: SidecarState): boolean {
  return state.state === "degraded" || state.state === "incompatible";
}

/**
 * The daemons panel's line about the daemon this window is on, from the hello
 * it answered: its version and its protocol range. `null` until connected.
 */
export function sidecarDaemonLine(state: SidecarState): string | null {
  if (state.state !== "connected") return null;
  const version = state.daemon_version ?? "a version it does not report";
  const range =
    state.protocol.min === state.protocol.max
      ? `protocol ${state.protocol.min}`
      : `protocol ${state.protocol.min}-${state.protocol.max}`;
  return `This window is on hangar daemon ${version}, ${range}${state.spawned ? ", started by this app" : ""}.`;
}
