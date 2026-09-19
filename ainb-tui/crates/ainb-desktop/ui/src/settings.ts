// What the settings page draws, projected from the frames the window already
// holds. Projections only: `settings.tsx` draws what these return, and nothing
// here reads the store or keeps anything.
//
//   config.config_screen_state ──visible_nodes, visible_rows──▶ the tree and the form
//   hangar.daemons_state       ──rows, hook health──▶ the daemons panel
//
// The rows, the tree and the selection are the reducer's: what is on screen
// is `visible_nodes` and `visible_rows` (the selected node's subtree, or the
// `/` filter's matches), the selected node and row are the frame's, and a
// click goes back as a pointer command naming the node or the row by id. The
// page keeps no selection of its own, never parses config.toml and never
// saves it whole.

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
/** The command a tree click is sent as, `ainb_app::app::pointer::ids::CONFIG_SELECT_NODE`. */
export const SELECT_NODE = "config.select_node";

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
  /** The row under the reducer's cursor. */
  current: boolean;
}

/** One line of the tree pane, as the reducer has it on screen. */
export interface SettingsNode {
  /** `ConfigTreeNode::id`, what a click names. */
  id: string;
  label: string;
  depth: number;
  hasChildren: boolean;
  expanded: boolean;
  selected: boolean;
}

/**
 * The rows a renderer may edit (#1224), the page's copy of
 * `ainb_app::config::renderer_edit`: two exact lists of registry keys (`*`
 * for a map segment, no prefixes), deny by default, and a secret row refused
 * before either. The reducer judges every edit the same way; this copy only
 * spares a round trip and draws the row inert with the reason. `parity.test.ts`
 * diffs the two copies through `ainb-app/tests/fixtures/renderer_editable_rows.txt`.
 */
export const DENIED_ROWS: readonly string[] = [
  "presets.file",
  "general.skill_install_real_homes",
  "ui_preferences.preferred_editor",
  "container_templates.*.config.image_source.type",
  "container_templates.*.config.image_source.name",
  "container_templates.*.config.image_source.path",
  "container_templates.*.config.image_source.base_image",
  "container_templates.*.config.image_source.build_args.*",
  "container_templates.*.config.command",
  "container_templates.*.config.entrypoint",
  "container_templates.*.config.environment.*",
  "container_templates.*.config.user",
  "container_templates.*.config.system_packages",
  "container_templates.*.config.npm_packages",
  "container_templates.*.config.python_packages",
  "container_templates.*.config.ports",
  "container_templates.*.config.volumes",
  "container_templates.*.config.mount_ssh",
  "container_templates.*.config.mount_git_config",
  "mcp_servers.*.installation.type",
  "mcp_servers.*.installation.package",
  "mcp_servers.*.installation.version",
  "mcp_servers.*.installation.url",
  "mcp_servers.*.installation.branch",
  "mcp_servers.*.installation.install_command",
  "mcp_servers.*.installation.script",
  "mcp_servers.*.definition.type",
  "mcp_servers.*.definition.command",
  "mcp_servers.*.definition.args",
  "mcp_servers.*.definition.env.*",
  "mcp_servers.*.definition.config",
  "workspace_defaults.exclude_paths",
  "workspace_defaults.workspace_scan_paths",
  "docker.host",
  "usage.plan.id",
  "usage.plan.monthly_usd",
  "usage.plan.provider",
  "usage.plan.reset_day",
  "usage.currency.code",
  "usage.currency.symbol",
  "usage.currency.usd_rate",
  "usage.model_aliases.*",
  "plugins.enabled",
  "plugins.disabled",
  "fleet.terminal",
  "fleet.transport",
  "fleet.bridge.outbound_enabled",
  "fleet.bridge.outbound_poll_secs",
  "fleet.bridge.response_timeout",
  "fleet.bridge.telegram.token",
  "fleet.bridge.telegram.user_id",
  "fleet.bridge.telegram.default_target",
  "fleet.bridge.telegram.require_mention_in_groups",
  "fleet.bridge.telegram.response_timeout",
  "fleet.bridge.slack.bot_token",
  "fleet.bridge.slack.app_token",
  "fleet.bridge.slack.user_id",
  "fleet.bridge.slack.default_target",
  "fleet.bridge.slack.listen_mode",
  "fleet.bridge.slack.response_timeout",
  "fleet.bridge.discord.token",
  "fleet.bridge.discord.user_id",
  "fleet.bridge.discord.default_target",
  "fleet.bridge.discord.channel_id",
  "fleet.bridge.discord.response_timeout",
  "usage_client.headroom_port",
  "usage_client.cache_db",
  "notifyd.approval_timeout_secs",
  "web.listen",
  "web.read_only",
  "web.insecure_bind",
  "acp.adapters.*.command",
  "acp.adapters.*.permission_mode",
  "skills.catalog_release",
  "skills.api_key",
  "hangar_daemon.card_agent.default",
];

/** Rows the page edits. Exact keys, `*` for a map segment. */
export const ALLOWED_ROWS: readonly string[] = [
  "default_container_template",
  "general.syntax_highlight",
  "authentication.cli_provider",
  "authentication.claude_provider",
  "authentication.default_model",
  "authentication.github_method",
  "container_templates.*.name",
  "container_templates.*.description",
  "container_templates.*.config.working_dir",
  "container_templates.*.config.memory_limit",
  "container_templates.*.config.cpu_limit",
  "container_templates.*.required_env",
  "container_templates.*.default_mcp_servers",
  "mcp_servers.*.name",
  "mcp_servers.*.description",
  "mcp_servers.*.required_env",
  "mcp_servers.*.enabled_by_default",
  "mcp_servers.*.shared",
  "workspace_defaults.branch_prefix",
  "workspace_defaults.max_repositories",
  "workspace_defaults.worktree_collision_behavior",
  "workspace_defaults.scan_max_depth",
  "workspace_defaults.scan_cache_ttl_secs",
  "ui_preferences.theme",
  "ui_preferences.show_container_status",
  "ui_preferences.show_git_status",
  "ui_preferences.show_session_menu_bar",
  "ui.tick_rate_ms",
  "ui.app_tick_ms",
  "ui.session_query_limit",
  "ui.session_lookback_hours",
  "ui.attention_err_window_hours",
  "ui.inbox_list_limit",
  "ui.double_click_ms",
  "ui.notice_error_secs",
  "ui.notice_warning_secs",
  "ui.notice_info_secs",
  "docker.timeout",
  "fleet.cost.session_usd",
  "fleet.cost.group_usd",
  "fleet.cost.session_overrides.*",
  "fleet.cost.group_overrides.*",
  "fleet.interview.surface",
  "fleet.idle_min",
  "fleet.enrich",
  "fleet.state_stale_ms",
  "fleet.healthy_state_stale_ms",
  "fleet.tmux_idle_after_secs",
  "mcp_pool.enabled",
  "mcp_pool.idle_grace_secs",
  "mcp_pool.monitor_refresh_secs",
  "mcp_pool.daemon_idle_grace_secs",
  "usage_client.fetch_timeout_secs",
  "usage_client.codex_ttl_secs",
  "daemons.stale_after_ms",
  "daemons.attention_stale_after_ms",
  "notifyd.os_debounce_secs",
  "session_reader.incremental_window_days",
  "hangar_daemon.autostandup.enabled",
  "hangar_daemon.autostandup.stagnant_min",
  "hangar_daemon.autostandup.cooldown_min",
  "hangar_daemon.autostandup.max_concurrent",
  "hangar_daemon.workspace.creation_disabled",
];

/** The marker the frame puts where a value was scrubbed; never written back. */
export const REDACTED = "<redacted>";

/** The most characters a text edit may carry, as the reducer bounds it. */
export const MAX_TEXT_CHARS = 2000;

export const DENIED_REASON = "its value reaches something the host runs, binds or trusts, so the window may not set it";
export const NOT_DRAWN_REASON = "the window's settings page does not edit it";
export const SECRET_REASON = "a secret is set from the terminal, not the window; the page shows only whether it is set";

/** Whether the concrete key `key` meets `pattern`, `*` standing for one map segment. */
function matchesPattern(pattern: string, key: string): boolean {
  const parts = pattern.split(".");
  const segments = key.split(".");
  if (segments.length !== parts.length) return false;
  return parts.every((part, index) => part === "*" || part === segments[index]);
}

/**
 * Why the page draws `key` inert, or `null` when the renderer may edit it.
 * `secret` says the row is a secret, which is refused before the lists. Map
 * keys stand where the pattern has `*`, so `mcp_servers.github.definition.command`
 * meets `mcp_servers.*.definition.command`.
 */
export function editRefusal(key: string, secret = false): string | null {
  if (secret) return SECRET_REASON;
  if (DENIED_ROWS.some((pattern) => matchesPattern(pattern, key))) return DENIED_REASON;
  if (ALLOWED_ROWS.some((pattern) => matchesPattern(pattern, key))) return null;
  return NOT_DRAWN_REASON;
}

/** `text` as the reducer would take it: control and format characters removed. */
export function cleanText(text: string): string {
  return text.replace(/[\p{Cc}\p{Cf}]/gu, "");
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

function row(setting: ConfigSetting_Serialize, dirty: readonly string[], current: boolean): SettingsRow {
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
    readOnly: editRefusal(setting.key, kind === "secret") !== null,
    readOnlyReason: editRefusal(setting.key, kind === "secret"),
    dirty: dirty.includes(setting.key),
    current,
  };
}

/** A category's label as `ConfigCategory::label` prints it: its root node's. */
function categoryLabel(config: ConfigView_Serialize, category: ConfigCategory): string {
  return config.config_screen_state.tree.find((node) => node.depth === 0 && node.category === category)?.label ?? category;
}

/**
 * The tree pane, exactly the nodes the reducer has on screen and in its
 * order: every collapsed subtree is already skipped in `visible_nodes`.
 */
export function settingsTree(config: ConfigView_Serialize | undefined): SettingsNode[] {
  if (config === undefined) return [];
  const screen = config.config_screen_state;
  const expanded = new Set(screen.expanded);
  return screen.visible_nodes.flatMap((index, position) => {
    const node = screen.tree[index];
    if (node === undefined) return [];
    const id = `${categoryLabel(config, node.category)}|${node.path}`;
    return [
      {
        id,
        label: label(node.label),
        depth: node.depth,
        hasChildren: node.has_children,
        expanded: expanded.has(id),
        selected: position === screen.selected_node,
      },
    ];
  });
}

/**
 * The rows the right pane shows, exactly `visible_rows`: the selected
 * node's subtree, or the `/` filter's matches, in the reducer's order.
 */
export function settingsRows(config: ConfigView_Serialize | undefined): SettingsRow[] {
  if (config === undefined) return [];
  const screen = config.config_screen_state;
  return screen.visible_rows.flatMap(([category, index], position) => {
    const setting = screen.settings[category]?.[index];
    return setting === undefined ? [] : [row(setting, screen.dirty, position === screen.selected_setting)];
  });
}

/** The right pane's title: the selected node's, or the filter's. */
export function settingsTitle(config: ConfigView_Serialize | undefined): string {
  if (config === undefined) return "";
  const screen = config.config_screen_state;
  if (screen.search_len !== null) return "Search";
  const index = screen.visible_nodes[screen.selected_node];
  const node = index === undefined ? undefined : screen.tree[index];
  return node === undefined ? "" : label(node.label);
}

/** How many rows the screen holds in all, as the terminal's title counts them. */
export function settingCount(config: ConfigView_Serialize | undefined): number {
  if (config === undefined) return 0;
  return Object.values(config.config_screen_state.settings).reduce((sum, rows) => sum + (rows?.length ?? 0), 0);
}

/** Whether the `/` filter is open, so the page draws its box as active. */
export function searching(config: ConfigView_Serialize | undefined): boolean {
  return config?.config_screen_state.search_len != null;
}

/**
 * The edit a widget's input becomes, or `null` when it does not fit the row:
 * a number that does not parse or was cleared, a choice index outside the
 * options, an edit of a read-only row, text over the bound. The reducer
 * checks the same, so this only spares a round trip. `revision` is the config
 * section version the page drew, so the reducer can refuse an edit of a frame
 * it has moved past.
 */
export function rowEdit(row: SettingsRow, input: string | number | boolean, revision: number): RendererIntent | null {
  if (row.readOnly) return null;
  // The frame shows a scrubbed value; sending it back would write the marker
  // over the real one. The reducer refuses it too.
  if (typeof input === "string" && input.includes(REDACTED)) return null;
  let value: Record<string, string | number | boolean>;
  switch (row.kind) {
    case "text": {
      if (typeof input !== "string") return null;
      const text = cleanText(input);
      if (Array.from(text).length > MAX_TEXT_CHARS) return null;
      value = { Text: text };
      break;
    }
    // A secret row is never editable here, so `readOnly` returned above.
    case "secret":
      return null;
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
      // A cleared `<input type="number">` reports "", and so does one holding
      // text it could not parse; `Number("")` is 0, which would persist.
      if (typeof input === "string" && input.trim() === "") return null;
      const number = typeof input === "number" ? input : Number(input);
      if (!Number.isSafeInteger(number)) return null;
      value = { Number: number };
      break;
    }
  }
  return { Command: [SET_ROW, { key: row.key, value, revision }] };
}

/** A click on the tree node `id`: the reducer selects it. */
export function selectNode(id: string): RendererIntent {
  return { Command: [SELECT_NODE, { id }] };
}

/** A click on a node's chevron: select it, then the reducer's own expand toggle. */
export function toggleNode(id: string): RendererIntent[] {
  return [selectNode(id), { Command: ["config.toggle_expand", null] }];
}

/**
 * What typing `query` into the filter box sends: the reducer's `/` opens (or
 * reopens, empty) its search, and the text is typed into it as the terminal
 * would; an empty query closes the filter with its own Esc.
 */
export function searchIntents(query: string): RendererIntent[] {
  const text = cleanText(query);
  if (text === "") return [{ Command: ["config.search.cancel", null] }];
  return [{ Command: ["config.search", null] }, { Text: text }];
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
