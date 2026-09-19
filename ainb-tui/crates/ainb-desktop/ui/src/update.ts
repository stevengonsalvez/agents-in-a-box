// The updater's framed states, as the host emits them on the `update` event.

export type UpdatePhase =
  | { phase: "checking" }
  | { phase: "downloading"; received: number; total: number | null }
  | { phase: "verifying" }
  | { phase: "applying" }
  | { phase: "installed"; version: string }
  | { phase: "failed"; reason: string };

const MB = 1024 * 1024;

const megabytes = (bytes: number): string => {
  const mb = bytes / MB;
  return mb >= 10 ? `${Math.round(mb)} MB` : `${Math.round(mb * 10) / 10} MB`;
};

/** One line for the status area, or null when there is nothing to say. */
export const updateLine = (phase: UpdatePhase | null): string | null => {
  if (phase === null) return null;
  switch (phase.phase) {
    case "checking":
      return "Checking for updates...";
    case "downloading": {
      if (phase.total === null || phase.total === 0) return `Downloading update: ${megabytes(phase.received)}`;
      const percent = Math.floor((phase.received / phase.total) * 100);
      return `Downloading update: ${megabytes(phase.received)} of ${megabytes(phase.total)} (${percent}%)`;
    }
    case "verifying":
      return "Verifying the download...";
    case "applying":
      return "Installing the update...";
    case "installed":
      return `Update ${phase.version} installed: restarting`;
    case "failed":
      return `Update failed: ${phase.reason}`;
  }
};

/** Whether the phase ends the run, so the line may clear after a while. */
export const terminal = (phase: UpdatePhase | null): boolean =>
  phase !== null && (phase.phase === "installed" || phase.phase === "failed");
