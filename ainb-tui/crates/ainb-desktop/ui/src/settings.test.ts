// The settings page's projections: the form's categories and rows from the
// config frame, the edit a widget sends back, and the daemons panel.

import assert from "node:assert/strict";
import { test } from "node:test";
import type { ConfigView_Serialize, HangarView_Serialize } from "../../../ainb-app/bindings/AppState";
import {
  CLOSE_SETTINGS,
  daemonRows,
  editRefusal,
  hookHealthLines,
  OPEN_SETTINGS,
  REDACTED,
  rowEdit,
  searchIntents,
  searching,
  selectNode,
  SET_ROW,
  settingCount,
  settingsRows,
  settingsTitle,
  settingsTree,
  toggleNode,
} from "./settings.ts";

/** A config frame with two categories and one row of each kind. */
function config(dirty: string[] = []): ConfigView_Serialize {
  return {
    config_screen_state: {
      categories: ["Authentication", "Workspace", "Usage"],
      settings: {
        Authentication: [
          {
            key: "authentication.claude_provider",
            label: "Claude Authentication",
            description: "How Claude authenticates",
            value: { Choice: [["system_auth", "api_key"], 1] },
          },
          {
            key: "fleet.bridge.telegram.token",
            label: "Telegram token",
            description: "",
            value: { Secret: { reference: "$TG_TOKEN", resolved: true } },
          },
        ],
        Workspace: [
          {
            key: "workspace_defaults.branch_prefix",
            label: "Branch prefix",
            description: "Prefix for new branches",
            value: { Text: "agents/" },
          },
          {
            key: "workspace_defaults.scan_max_depth",
            label: "Scan depth",
            description: "",
            value: { Number: 3 },
          },
          {
            key: "ui_preferences.show_git_status",
            label: "Show git status",
            description: "",
            value: { Bool: true },
          },
        ],
        Usage: [{ key: "usage.plan.id", label: "Plan", description: "", value: { Text: "max" } }],
      },
      tree: [
        { category: "Authentication", path: "authentication", label: "Authentication", depth: 0, has_children: false, rows: [0, 1] },
        { category: "Workspace", path: "workspace", label: "Workspace", depth: 0, has_children: true, rows: [0, 1, 2] },
        { category: "Workspace", path: "workspace.defaults", label: "Defaults", depth: 1, has_children: false, rows: [0, 1] },
        { category: "Usage", path: "usage", label: "Usage", depth: 0, has_children: false, rows: [0] },
      ],
      // Workspace is collapsed: its Defaults child is not on screen.
      visible_nodes: [0, 1, 3],
      expanded: [],
      selected_node: 1,
      // The selected node's subtree, as the reducer computed it.
      visible_rows: [["Workspace", 0], ["Workspace", 1], ["Workspace", 2]],
      selected_setting: 1,
      search_len: null,
      dirty,
    },
  } as unknown as ConfigView_Serialize;
}

/** Every row of every category, for the tests that reason about kinds. */
function allRows(view: ConfigView_Serialize) {
  const screen = view.config_screen_state;
  return screen.categories.flatMap((category) =>
    (screen.settings[category] ?? []).map((_, index) => {
      const shown = { ...view, config_screen_state: { ...screen, visible_rows: [[category, index]], selected_setting: 0 } };
      return settingsRows(shown as ConfigView_Serialize)[0]!;
    }),
  );
}

function hangar(rows: unknown[], hook_health: unknown = null): HangarView_Serialize {
  return {
    hangar_daemon_config_loaded: true,
    daemons_state: { shared: { rows, collected_at_ms: 42, hook_health } },
  } as unknown as HangarView_Serialize;
}

test("the tree is the reducer's visible nodes, and the rows are its visible rows", () => {
  const view = config(["workspace_defaults.branch_prefix"]);
  const tree = settingsTree(view);
  assert.deepEqual(
    tree.map((node) => [node.id, node.depth, node.hasChildren, node.expanded, node.selected]),
    [
      ["Authentication|authentication", 0, false, false, false],
      ["Workspace|workspace", 0, true, false, true],
      ["Usage|usage", 0, false, false, false],
    ],
    "the collapsed child is not drawn, and the selection is the frame's",
  );
  assert.equal(settingsTitle(view), "Workspace");
  assert.equal(settingCount(view), 6);
  const rows = settingsRows(view);
  assert.deepEqual(
    rows.map((row) => [row.key, row.current]),
    [
      ["workspace_defaults.branch_prefix", false],
      ["workspace_defaults.scan_max_depth", true],
      ["ui_preferences.show_git_status", false],
    ],
    "only the selected node's rows, with the reducer's cursor",
  );
  const [text, number, bool] = rows;
  assert.equal(text!.value, "agents/");
  assert.ok(text!.dirty, "an edited row reads dirty");
  assert.equal(number!.value, "3");
  assert.equal(bool!.value, "on");
  assert.ok(!bool!.dirty);

  const filtered = {
    ...view,
    config_screen_state: { ...view.config_screen_state, search_len: 3, visible_rows: [["Usage", 0]], selected_setting: 0 },
  } as ConfigView_Serialize;
  assert.ok(searching(filtered));
  assert.equal(settingsTitle(filtered), "Search");
  assert.deepEqual(settingsRows(filtered).map((row) => row.key), ["usage.plan.id"], "the filter's matches, not a category");
  const [choice, secret] = allRows(view);
  assert.equal(choice!.kind, "choice");
  assert.deepEqual(choice!.options, ["system_auth", "api_key"]);
  assert.equal(choice!.selected, 1);
  assert.equal(choice!.value, "api_key");
  assert.equal(secret!.kind, "secret");
  assert.equal(secret!.value, "set ($TG_TOKEN)");
  assert.ok(allRows(view)[5]!.readOnly, "[usage] is the burndown plugin's");
});

test("a click names a node, a chevron names it then toggles, and the filter box types into the reducer's search", () => {
  assert.deepEqual(selectNode("Workspace|workspace"), { Command: ["config.select_node", { id: "Workspace|workspace" }] });
  assert.deepEqual(
    toggleNode("Workspace|workspace").map((intent) => intent.Command?.[0]),
    ["config.select_node", "config.toggle_expand"],
  );
  assert.deepEqual(searchIntents("theme"), [{ Command: ["config.search", null] }, { Text: "theme" }]);
  assert.deepEqual(searchIntents("the\u0007me"), [{ Command: ["config.search", null] }, { Text: "theme" }]);
  assert.deepEqual(searchIntents(""), [{ Command: ["config.search.cancel", null] }]);
});

test("no frame means no tree and no rows", () => {
  assert.deepEqual(settingsTree(undefined), []);
  assert.deepEqual(settingsRows(undefined), []);
  assert.equal(settingsTitle(undefined), "");
  assert.equal(settingCount(undefined), 0);
});

test("an edit names the row by key and carries only what the widget chose", () => {
  const rows = allRows(config());
  const by = (key: string) => rows.find((row) => row.key === key)!;
  assert.deepEqual(rowEdit(by("workspace_defaults.branch_prefix"), "g6a/", 7), {
    Command: [SET_ROW, { key: "workspace_defaults.branch_prefix", value: { Text: "g6a/" }, revision: 7 }],
  });
  assert.deepEqual(rowEdit(by("authentication.claude_provider"), 0, 7), {
    Command: [SET_ROW, { key: "authentication.claude_provider", value: { Choice: 0 }, revision: 7 }],
  });
  assert.equal(rowEdit(by("fleet.bridge.telegram.token"), "keychain:tg", 7), null, "a secret is not set from the window");
  assert.ok(by("fleet.bridge.telegram.token").readOnly);
  assert.match(by("fleet.bridge.telegram.token").readOnlyReason!, /set from the terminal/);
  assert.deepEqual(rowEdit(by("ui_preferences.show_git_status"), false, 7), {
    Command: [SET_ROW, { key: "ui_preferences.show_git_status", value: { Bool: false }, revision: 7 }],
  });
  assert.deepEqual(rowEdit(by("workspace_defaults.scan_max_depth"), "4", 7), {
    Command: [SET_ROW, { key: "workspace_defaults.scan_max_depth", value: { Number: 4 }, revision: 7 }],
  });
});

test("an edit that does not fit the row is not sent", () => {
  const rows = allRows(config());
  const by = (key: string) => rows.find((row) => row.key === key)!;
  assert.equal(rowEdit(by("authentication.claude_provider"), 2, 7), null, "an index past the options");
  assert.equal(rowEdit(by("authentication.claude_provider"), "api_key", 7), null, "a choice is an index");
  assert.equal(rowEdit(by("workspace_defaults.scan_max_depth"), "four", 7), null, "a number that does not parse");
  assert.equal(rowEdit(by("workspace_defaults.scan_max_depth"), "", 7), null, "a cleared number input is not 0");
  assert.equal(rowEdit(by("workspace_defaults.scan_max_depth"), "  ", 7), null, "nor a blank one");
  assert.equal(rowEdit(by("workspace_defaults.scan_max_depth"), 1.5, 7), null, "an integer row");
  assert.equal(rowEdit(by("ui_preferences.show_git_status"), "yes", 7), null, "a bool is a boolean");
  assert.equal(rowEdit(by("usage.plan.id"), "pro", 7), null, "a read-only row");
  assert.equal(rowEdit(by("workspace_defaults.branch_prefix"), "x".repeat(2001), 7), null, "over the text bound");
  assert.deepEqual(rowEdit(by("workspace_defaults.branch_prefix"), "g6a\u202E/\u0007", 7), {
    Command: [SET_ROW, { key: "workspace_defaults.branch_prefix", value: { Text: "g6a/" }, revision: 7 }],
  }, "control and format characters are stripped");
  assert.equal(rowEdit(by("workspace_defaults.branch_prefix"), REDACTED, 7), null, "the scrubbed marker is never written back");
  assert.equal(rowEdit(by("workspace_defaults.branch_prefix"), `x${REDACTED}y`, 7), null, "nor inside a value");
});

test("a row whose value the host runs is drawn inert, with the reason, whatever its category", () => {
  const view = config();
  view.config_screen_state.settings.Workspace!.push({
    key: "ui_preferences.preferred_editor",
    label: "Editor",
    description: "",
    value: { Text: "code" },
  });
  const rows = allRows(view);
  const editor = rows.find((row) => row.key === "ui_preferences.preferred_editor")!;
  assert.ok(editor.readOnly);
  assert.match(editor.readOnlyReason!, /host runs, binds or trusts/);
  assert.equal(rowEdit(editor, "evil", 7), null);
  const usage = rows.find((row) => row.key === "usage.plan.id")!;
  assert.match(usage.readOnlyReason!, /host runs, binds or trusts/, "[usage] is the burndown plugin's file");
  assert.equal(rows.find((row) => row.key === "workspace_defaults.branch_prefix")!.readOnlyReason, null);
});

test("map keys meet their pattern, there are no prefixes, and a secret is refused first", () => {
  assert.match(editRefusal("mcp_servers.github.definition.command")!, /host runs, binds or trusts/);
  assert.match(editRefusal("container_templates.claude-dev.config.entrypoint")!, /host runs, binds or trusts/);
  assert.match(editRefusal("acp.adapters.claude-agent-acp.permission_mode")!, /host runs, binds or trusts/);
  assert.match(editRefusal("acp.adapters.claude-agent-acp.command")!, /host runs, binds or trusts/);
  assert.match(editRefusal("fleet.bridge.telegram.user_id")!, /host runs, binds or trusts/);
  assert.equal(editRefusal("ui_preferences.theme"), null);
  assert.match(editRefusal("fleet.terminal")!, /host runs, binds or trusts/);
  assert.equal(editRefusal("fleet.idle_min"), null);
  assert.match(editRefusal("no.such.row")!, /does not edit it/);
  assert.match(editRefusal("ui.no_such_row")!, /does not edit it/, "a sibling does not allow a new row");
  assert.match(editRefusal("fleet.bridge.telegram.token", true)!, /set from the terminal/);
});

test("opening and closing the page walk the reducer through its own rows", () => {
  assert.deepEqual(
    OPEN_SETTINGS.map((intent) => intent.Command?.[0]),
    ["global.go_home", "home.config"],
  );
  assert.deepEqual(
    CLOSE_SETTINGS.map((intent) => intent.Command?.[0]),
    ["config.back", "home.sessions"],
  );
});

test("the daemons panel draws the collector's rows and the hook wiring", () => {
  const view = hangar(
    [
      { kind: "notifyd", state: "running", connected: true, version: "1.2.0", error_count: 0, reason: "heartbeat fresh", last_error: null },
      { kind: "bridge", state: "stopped", connected: false, version: null, error_count: 2, reason: "clean stop", last_error: "token rejected" },
    ],
    {
      bundled_version: "1.2.0",
      installed_version: "1.1.0",
      version_current: false,
      script_ready: true,
      notify_socket_live: true,
      approve_socket_live: false,
      agents: [{ agent: "claude", installed: true, wiring_ready: true, detail: "marketplace" }],
      issues: [{ component: "Codex", message: "not wired", repair: "ainb hooks install" }],
    },
  );
  assert.deepEqual(daemonRows(view), [
    { kind: "notifyd", state: "running", connected: true, version: "1.2.0", errors: 0, reason: "heartbeat fresh", lastError: "" },
    { kind: "bridge", state: "stopped", connected: false, version: "", errors: 2, reason: "clean stop", lastError: "token rejected" },
  ]);
  assert.deepEqual(hookHealthLines(view), [
    "hooks outdated (installed 1.1.0, bundled 1.2.0)",
    "script ready",
    "notify socket live, approve socket idle",
    "claude: installed, marketplace",
    "issue Codex: not wired",
  ]);
  assert.deepEqual(daemonRows(undefined), []);
  assert.deepEqual(hookHealthLines(hangar([], null)), []);
});
