// ABOUTME: The screens the window builds a surface for, by the reducer's
// screen id, in one list. The parity enumeration
// (`ainb-app/tests/parity/screens.txt`) is walked against it by
// `parity_screens.test.ts`, so a surface added here without a row there is a
// failed test, and `main.tsx` reads its screen ids from here and nowhere else.

/** The reducer's screen ids the window draws a surface for. */
export const SURFACES = {
  /** The session workspace: the sidebar and the board, terminal, review and stats tabs. */
  sessions: "session_list",
  /** The settings page, over the `config` section. */
  settings: "config",
  /** The inbox page, over section 16. */
  inbox: "inbox",
} as const;

/** Every screen id in `SURFACES`. */
export const SURFACE_SCREENS: readonly string[] = Object.values(SURFACES);
