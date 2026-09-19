// The plugin fallback cell's placeholder half (D3p-f): what a plugin screen
// shows when there is nothing to paint, decided from the framed plugins_host
// section alone. The decision mirrors the terminal's
// `build_placeholder_for_unloaded_plugin` (ainb-core/src/app/screens/builtin.rs):
// a render error counts only while the plugin is registered, and then outranks
// the "connecting" beat.

import type { PluginsHostView_Serialize } from "../../../ainb-app/bindings/AppState";

/**
 * The plugin that owns each plugin screen, as `PLUGIN_SCREENS`
 * (ainb-app/src/app/screens/builtin.rs) registers them. The plugins_host frame
 * carries screen ids, not owners; `plugin_placeholder.test.ts` pins this list.
 */
export const PLUGIN_OWNERS: Readonly<Record<string, string>> = {
  analytics: "burndown",
  witr: "witr",
  learnings: "learnings",
  abtop: "abtop",
  hangar: "hangar-tui",
};

/**
 * One of the three states a plugin screen shows with nothing to paint.
 * `plugin` is `null` for a screen no plugin owns, which the terminal titles
 * "plugin unavailable".
 */
export type Placeholder =
  | { kind: "not_registered"; screen: string; plugin: string | null }
  | { kind: "no_frame"; screen: string; plugin: string | null }
  | { kind: "render_error"; screen: string; plugin: string | null; error: string; cut: boolean };

/** The placeholder `screen` shows, from the plugins_host frame. */
export function placeholderFor(screen: string, pluginsHost: PluginsHostView_Serialize): Placeholder {
  const plugin = PLUGIN_OWNERS[screen] ?? null;
  if (plugin === null || !pluginsHost.plugin_presence[screen]?.registered) {
    return { kind: "not_registered", screen, plugin };
  }
  const error = pluginsHost.plugin_render_errors[screen];
  if (error !== undefined) {
    return { kind: "render_error", screen, plugin, error: error.text, cut: error.cut };
  }
  return { kind: "no_frame", screen, plugin };
}

/** `screen` with its first letter capitalised, as the terminal titles it. */
export function titleCase(screen: string): string {
  return screen.length === 0 ? screen : screen[0].toUpperCase() + screen.slice(1);
}

/** Text as shown: control and format characters dropped, nothing cut. */
export function shown(text: string): string {
  return text.replace(/[\p{Cc}\p{Cf}]/gu, "");
}
