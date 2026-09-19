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
  SET_ROW,
  settingCount,
  settingsCategories,
  settingsLines,
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
      dirty,
    },
  } as unknown as ConfigView_Serialize;
}

function hangar(rows: unknown[], hook_health: unknown = null): HangarView_Serialize {
  return {
    hangar_daemon_config_loaded: true,
    daemons_state: { shared: { rows, collected_at_ms: 42, hook_health } },
  } as unknown as HangarView_Serialize;
}

test("the categories are the frame's, labelled by their tree root, with the reducer's rows", () => {
  const categories = settingsCategories(config(["workspace_defaults.branch_prefix"]));
  assert.deepEqual(
    categories.map((category) => [category.label, category.rows.length]),
    [
      ["Authentication", 2],
      ["Workspace", 3],
      ["Usage", 1],
    ],
  );
  assert.equal(settingCount(categories), 6);
  const [choice, secret] = categories[0]!.rows;
  assert.equal(choice!.kind, "choice");
  assert.deepEqual(choice!.options, ["system_auth", "api_key"]);
  assert.equal(choice!.selected, 1);
  assert.equal(choice!.value, "api_key");
  assert.equal(secret!.kind, "secret");
  assert.equal(secret!.value, "set ($TG_TOKEN)");
  const [text, number, bool] = categories[1]!.rows;
  assert.equal(text!.value, "agents/");
  assert.ok(text!.dirty, "an edited row reads dirty");
  assert.equal(number!.value, "3");
  assert.equal(bool!.value, "on");
  assert.ok(!bool!.dirty);
  assert.ok(categories[2]!.rows[0]!.readOnly, "[usage] is the burndown plugin's");
});

test("no frame means no categories", () => {
  assert.deepEqual(settingsCategories(undefined), []);
});

test("an edit names the row by key and carries only what the widget chose", () => {
  const rows = settingsCategories(config()).flatMap((category) => category.rows);
  const by = (key: string) => rows.find((row) => row.key === key)!;
  assert.deepEqual(rowEdit(by("workspace_defaults.branch_prefix"), "g6a/"), {
    Command: [SET_ROW, { key: "workspace_defaults.branch_prefix", value: { Text: "g6a/" } }],
  });
  assert.deepEqual(rowEdit(by("authentication.claude_provider"), 0), {
    Command: [SET_ROW, { key: "authentication.claude_provider", value: { Choice: 0 } }],
  });
  assert.deepEqual(rowEdit(by("fleet.bridge.telegram.token"), "keychain:tg"), {
    Command: [SET_ROW, { key: "fleet.bridge.telegram.token", value: { Secret: "keychain:tg" } }],
  });
  assert.deepEqual(rowEdit(by("ui_preferences.show_git_status"), false), {
    Command: [SET_ROW, { key: "ui_preferences.show_git_status", value: { Bool: false } }],
  });
  assert.deepEqual(rowEdit(by("workspace_defaults.scan_max_depth"), "4"), {
    Command: [SET_ROW, { key: "workspace_defaults.scan_max_depth", value: { Number: 4 } }],
  });
});

test("an edit that does not fit the row is not sent", () => {
  const rows = settingsCategories(config()).flatMap((category) => category.rows);
  const by = (key: string) => rows.find((row) => row.key === key)!;
  assert.equal(rowEdit(by("authentication.claude_provider"), 2), null, "an index past the options");
  assert.equal(rowEdit(by("authentication.claude_provider"), "api_key"), null, "a choice is an index");
  assert.equal(rowEdit(by("workspace_defaults.scan_max_depth"), "four"), null, "a number that does not parse");
  assert.equal(rowEdit(by("workspace_defaults.scan_max_depth"), 1.5), null, "an integer row");
  assert.equal(rowEdit(by("ui_preferences.show_git_status"), "yes"), null, "a bool is a boolean");
  assert.equal(rowEdit(by("usage.plan.id"), "pro"), null, "a read-only row");
  assert.equal(rowEdit(by("workspace_defaults.branch_prefix"), REDACTED), null, "the scrubbed marker is never written back");
  assert.equal(rowEdit(by("workspace_defaults.branch_prefix"), `x${REDACTED}y`), null, "nor inside a value");
});

test("a row whose value the host runs is drawn inert, with the reason, whatever its category", () => {
  const view = config();
  view.config_screen_state.settings.Workspace!.push({
    key: "ui_preferences.preferred_editor",
    label: "Editor",
    description: "",
    value: { Text: "code" },
  });
  const rows = settingsCategories(view).flatMap((category) => category.rows);
  const editor = rows.find((row) => row.key === "ui_preferences.preferred_editor")!;
  assert.ok(editor.readOnly);
  assert.match(editor.readOnlyReason!, /program the host runs/);
  assert.equal(rowEdit(editor, "evil"), null);
  const usage = rows.find((row) => row.key === "usage.plan.id")!;
  assert.match(usage.readOnlyReason!, /does not edit it/);
  assert.equal(rows.find((row) => row.key === "workspace_defaults.branch_prefix")!.readOnlyReason, null);
});

test("map keys meet their pattern, and the deny list wins over an allowed prefix", () => {
  assert.match(editRefusal("mcp_servers.github.definition.command")!, /program the host runs/);
  assert.match(editRefusal("container_templates.claude-dev.config.entrypoint")!, /program the host runs/);
  assert.equal(editRefusal("acp.adapters.claude-agent-acp.permission_mode"), null);
  assert.match(editRefusal("acp.adapters.claude-agent-acp.command")!, /program the host runs/);
  assert.equal(editRefusal("ui_preferences.theme"), null);
  assert.match(editRefusal("fleet.terminal")!, /program the host runs/);
  assert.equal(editRefusal("fleet.idle_min"), null);
  assert.match(editRefusal("no.such.row")!, /does not edit it/);
  assert.match(editRefusal("ui_preferences")!, /does not edit it/, "a prefix needs a row under it");
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

test("the page's lines carry every category, row and daemon it draws", () => {
  const lines = settingsLines(config(), hangar([{ kind: "notifyd", state: "running", connected: true, version: "1", error_count: 0, reason: "", last_error: null }]));
  assert.equal(lines[0], "Settings (6 settings)");
  assert.ok(lines.includes("Authentication"));
  assert.ok(lines.includes("Claude Authentication: api_key"));
  assert.ok(lines.includes("How Claude authenticates"));
  assert.ok(lines.includes("Daemons: runtime health"));
  assert.ok(lines.includes("DAEMON STATE VERSION ERR HEALTH"));
  assert.ok(lines.includes("notifyd running 1 0 connected"));
});
