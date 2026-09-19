// What the settings page draws, projected from the frames the window already
// holds. Projections only: `settings.tsx` draws what these return, and nothing
// here reads the store or keeps anything.
//
//   config.config_screen_state ──categories, rows──▶ the form
//   hangar.daemons_state       ──rows, hook health──▶ the daemons panel
//
// The rows are the reducer's: a row's kind, its options and whether it can be
// edited come from the frame, and an edit goes back as `config.set_row` naming
// the row by key. The page never parses config.toml and never saves it whole.

import type {
  ConfigCategory,
  ConfigSetting_Serialize,
  ConfigValue_Serialize,
  ConfigView_Serialize,
  DaemonStatus_Serialize,
  HangarView_Serialize,
} from "../../../ainb-app/bindings/AppState";
import { label } from "./sessions.ts";
import type { RendererIntent } from "./tabs.ts";

/** The command a row edit is sent as, `ainb_app::app::pointer::ids::CONFIG_SET_ROW`. */
export const SET_ROW = "config.set_row";

/** A row's widget, from the reducer's own value kind. */
export type RowKind = "text" | "secret" | "bool" | "choice" | "number";

export interface SettingsRow {
  key: string;
  label: string;
  description: string;
  kind: RowKind;
  /** What the row shows: the value, or a secret's source, never a literal. */
  value: string;
  /** A choice's options, in the reducer's order. */
  options: string[];
  /** A choice's selected option, or a bool's state as 0 or 1. */
  selected: number;
  /** Rows the reducer refuses to write from a renderer (`renderer_edit`). */
  readOnly: boolean;
  /** Why, when it does. */
  readOnlyReason: string | null;
  /** Edited and not yet written. */
  dirty: boolean;
}

export interface SettingsCategory {
  category: ConfigCategory;
  label: string;
  rows: SettingsRow[];
}

/**
 * The rows a renderer may edit (#1224), the page's copy of
 * `ainb_app::config::renderer_edit`: rows whose value reaches a program the
 * host runs are denied, then only rows on the allow list are drawn editable.
 * The reducer judges every edit the same way; this copy only spares a round
 * trip and draws the row inert. `parity.test.ts` diffs the two copies through
 * `ainb-app/tests/fixtures/renderer_editable_rows.txt`.
 */
export const DENIED_ROWS: readonly string[] = [
  "ui_preferences.preferred_editor",
  "acp.adapters.*.command",
  "container_templates.*.config.command",
  "container_templates.*.config.entrypoint",
  "container_templates.*.config.environment.*",
  "container_templates.*.config.image_source.path",
  "container_templates.*.config.image_source.build_args.*",
  "container_templates.*.config.volumes",
  "container_templates.*.config.mount_ssh",
  "container_templates.*.config.mount_git_config",
  "container_templates.*.config.system_packages",
  "container_templates.*.config.npm_packages",
  "container_templates.*.config.python_packages",
  "mcp_servers.*.definition.command",
  "mcp_servers.*.definition.args",
  "mcp_servers.*.definition.env.*",
  "mcp_servers.*.definition.config",
  "mcp_servers.*.installation.install_command",
  "mcp_servers.*.installation.script",
  "mcp_servers.*.installation.package",
  "mcp_servers.*.installation.url",
  "mcp_servers.*.installation.branch",
  "docker.host",
  "fleet.terminal",
  "hangar_daemon.card_agent.default",
  "plugins.enabled",
  "plugins.disabled",
  "plugins.*",
  "presets.file",
  "usage_client.cache_db",
  "web.listen",
  "web.insecure_bind",
  "skills.catalog_release",
];

/** Rows the page edits; a trailing `.` allows every row under the prefix. */
export const ALLOWED_ROWS: readonly string[] = [
  "general.",
  "authentication.",
  "workspace_defaults.",
  "ui_preferences.",
  "ui.",
  "docker.timeout",
  "default_container_template",
  "container_templates.*.name",
  "container_templates.*.description",
  "container_templates.*.config.working_dir",
  "container_templates.*.config.user",
  "container_templates.*.config.memory_limit",
  "container_templates.*.config.cpu_limit",
  "container_templates.*.config.ports",
  "container_templates.*.required_env",
  "container_templates.*.default_mcp_servers",
  "mcp_servers.*.name",
  "mcp_servers.*.description",
  "mcp_servers.*.enabled_by_default",
  "mcp_servers.*.shared",
  "mcp_servers.*.required_env",
  "mcp_servers.*.installation.type",
  "mcp_servers.*.installation.version",
  "mcp_servers.*.definition.type",
  "fleet.",
  "mcp_pool.",
  "usage_client.headroom_port",
  "usage_client.fetch_timeout_secs",
  "usage_client.codex_ttl_secs",
  "daemons.",
  "notifyd.",
  "web.read_only",
  "acp.adapters.*.permission_mode",
  "skills.api_key",
  "session_reader.",
  "hangar_daemon.autostandup.",
  "hangar_daemon.workspace.",
];

/** The marker the frame puts where a value was scrubbed; never written back. */
export const REDACTED = "<redacted>";

/** Whether the concrete key `key` meets `pattern`, `*` standing for one map segment. */
function matchesPattern(pattern: string, key: string): boolean {
  const parts = pattern.split(".");
  const prefix = pattern.endsWith(".");
  if (prefix) parts.pop();
  const segments = key.split(".");
  if (prefix ? segments.length <= parts.length : segments.length !== parts.length) return false;
  return parts.every((part, index) => part === "*" || part === segments[index]);
}

/**
 * Why the page draws `key` inert, or `null` when the renderer may edit it.
 * Map keys stand where the pattern has `*`, so `mcp_servers.github.definition.command`
 * meets `mcp_servers.*.definition.command`.
 */
export function editRefusal(key: string): string | null {
  if (DENIED_ROWS.some((pattern) => matchesPattern(pattern, key))) {
    return "its value reaches a program the host runs, so the window may not set it";
  }
  if (ALLOWED_ROWS.some((pattern) => matchesPattern(pattern, key))) return null;
  return "the window's settings page does not edit it";
}

function readOnly(key: string): boolean {
  return editRefusal(key) !== null;
}

function kindOf(value: ConfigValue_Serialize): RowKind {
  if ("Text" in value && value.Text !== undefined) return "text";
  if ("Secret" in value && value.Secret !== undefined) return "secret";
  if ("Bool" in value && value.Bool !== undefined) return "bool";
  if ("Choice" in value && value.Choice !== undefined) return "choice";
  return "number";
}

/** A secret's status line: whether it resolved and where it points. */
function secretShown(reference: string, resolved: boolean): string {
  if (reference === "") return "not set";
  return `${resolved ? "set" : "unresolved"} (${reference})`;
}

function row(setting: ConfigSetting_Serialize, dirty: readonly string[]): SettingsRow {
  const value = setting.value;
  const kind = kindOf(value);
  let shown = "";
  let options: string[] = [];
  let selected = 0;
  switch (kind) {
    case "text":
      shown = label(value.Text ?? "");
      break;
    case "secret":
      shown = secretShown(value.Secret?.reference ?? "", value.Secret?.resolved ?? false);
      break;
    case "bool":
      selected = value.Bool ? 1 : 0;
      shown = value.Bool ? "on" : "off";
      break;
    case "choice": {
      const [list, index] = value.Choice ?? [[], 0];
      options = list.map(label);
      selected = index;
      shown = options[index] ?? "";
      break;
    }
    case "number":
      shown = String(value.Number ?? 0);
      break;
  }
  return {
    key: setting.key,
    label: label(setting.label),
    description: label(setting.description),
    kind,
    value: shown,
    options,
    selected,
    readOnly: readOnly(setting.key),
    readOnlyReason: editRefusal(setting.key),
    dirty: dirty.includes(setting.key),
  };
}

/**
 * The categories the form lists, in the reducer's order, each with its rows.
 * The label is the tree's own root node for the category, so both surfaces
 * print the same words.
 */
export function settingsCategories(config: ConfigView_Serialize | undefined): SettingsCategory[] {
  if (config === undefined) return [];
  const screen = config.config_screen_state;
  return screen.categories.map((category) => ({
    category,
    label: label(screen.tree.find((node) => node.depth === 0 && node.category === category)?.label ?? category),
    rows: (screen.settings[category] ?? []).map((setting) => row(setting, screen.dirty)),
  }));
}

/** How many rows the form holds in all, as the terminal's title counts them. */
export function settingCount(categories: readonly SettingsCategory[]): number {
  return categories.reduce((sum, category) => sum + category.rows.length, 0);
}

/**
 * The edit a widget's input becomes, or `null` when it does not fit the row:
 * a number that does not parse, a choice index outside the options, an edit
 * of a read-only row. The reducer checks the same, so this only spares a
 * round trip.
 */
export function rowEdit(row: SettingsRow, input: string | number | boolean): RendererIntent | null {
  if (row.readOnly) return null;
  // The frame shows a scrubbed value; sending it back would write the marker
  // over the real one. The reducer refuses it too.
  if (typeof input === "string" && input.includes(REDACTED)) return null;
  let value: Record<string, string | number | boolean>;
  switch (row.kind) {
    case "text":
      if (typeof input !== "string") return null;
      value = { Text: input };
      break;
    case "secret":
      if (typeof input !== "string") return null;
      value = { Secret: input };
      break;
    case "bool":
      if (typeof input !== "boolean") return null;
      value = { Bool: input };
      break;
    case "choice": {
      const index = typeof input === "number" ? input : Number.NaN;
      if (!Number.isInteger(index) || index < 0 || index >= row.options.length) return null;
      value = { Choice: index };
      break;
    }
    case "number": {
      const number = typeof input === "number" ? input : Number(input);
      if (!Number.isSafeInteger(number)) return null;
      value = { Number: number };
      break;
    }
  }
  return { Command: [SET_ROW, { key: row.key, value }] };
}

/**
 * The rows that put the reducer on the Config screen, in order: home, then
 * the sidebar's config item as its key opens it. Pointer rows on the config
 * screen run only while the reducer is there, so the page opens it first.
 */
export const OPEN_SETTINGS: RendererIntent[] = [
  { Command: ["global.go_home", null] },
  { Command: ["home.config", null] },
];

/** The rows that leave the Config screen and put the reducer back on sessions. */
export const CLOSE_SETTINGS: RendererIntent[] = [
  { Command: ["config.back", null] },
  { Command: ["home.sessions", null] },
];

/** One daemon's line in the panel, as the terminal's table prints it. */
export interface DaemonRow {
  kind: string;
  state: string;
  connected: boolean;
  version: string;
  errors: number;
  reason: string;
  lastError: string;
}

/** The daemons panel's rows, in the collector's order. */
export function daemonRows(hangar: HangarView_Serialize | undefined): DaemonRow[] {
  const rows: DaemonStatus_Serialize[] = hangar?.daemons_state.shared?.rows ?? [];
  return rows.map((status) => ({
    kind: status.kind,
    state: String(status.state),
    connected: status.connected,
    version: status.version ?? "",
    errors: status.error_count,
    reason: label(status.reason),
    lastError: label(status.last_error ?? ""),
  }));
}

/** Hook wiring, one line per fact the terminal's panel prints. */
export function hookHealthLines(hangar: HangarView_Serialize | undefined): string[] {
  const health = hangar?.daemons_state.shared?.hook_health;
  if (!health) return [];
  const lines = [
    `hooks ${health.version_current ? "current" : "outdated"} (installed ${health.installed_version ?? "none"}, bundled ${health.bundled_version})`,
    `script ${health.script_ready ? "ready" : "missing"}`,
    `notify socket ${health.notify_socket_live ? "live" : "idle"}, approve socket ${health.approve_socket_live ? "live" : "idle"}`,
  ];
  for (const agent of health.agents) {
    lines.push(`${agent.agent}: ${agent.installed ? "installed" : "not installed"}, ${label(agent.detail)}`);
  }
  for (const issue of health.issues) {
    lines.push(`issue ${issue.component}: ${label(issue.message)}`);
  }
  return lines;
}

/** When the daemon rows were collected, epoch ms, or `null` before the first collect. */
export function daemonsCollectedAt(hangar: HangarView_Serialize | undefined): number | null {
  return hangar?.daemons_state.shared?.collected_at_ms ?? null;
}

/** The column titles the panel draws, as the terminal's table does. */
export const DAEMON_COLUMNS = ["DAEMON", "STATE", "VERSION", "ERR", "HEALTH"] as const;

/**
 * Every line of text the settings page draws, in reading order: the DOM half
 * of parity compares this against the fixture's expected facts. `settings.tsx`
 * prints exactly these strings, so a fact absent here is absent from the page.
 */
export function settingsLines(config: ConfigView_Serialize | undefined, hangar: HangarView_Serialize | undefined): string[] {
  const categories = settingsCategories(config);
  const lines = [`Settings (${settingCount(categories)} settings)`];
  for (const category of categories) {
    lines.push(category.label);
    for (const row of category.rows) {
      lines.push(`${row.label}: ${row.value}`);
      if (row.description !== "") lines.push(row.description);
    }
  }
  lines.push("Daemons: runtime health");
  lines.push(DAEMON_COLUMNS.join(" "));
  for (const row of daemonRows(hangar)) {
    lines.push([row.kind, row.state, row.version, String(row.errors), row.connected ? "connected" : row.reason].join(" "));
  }
  lines.push(...hookHealthLines(hangar));
  return lines;
}
