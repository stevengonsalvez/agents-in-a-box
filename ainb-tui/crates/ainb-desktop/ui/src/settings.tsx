import { createMemo, createSignal, For, Show } from "solid-js";
import { keyedList, sameKeys } from "./keyed.ts";
import type { ConfigView_Serialize, HangarView_Serialize } from "../../../ainb-app/bindings/AppState";
import type { SetupView, SetupWrite } from "../../bindings/Desktop.ts";
import {
  DAEMON_COLUMNS,
  daemonRows,
  daemonsCollectedAt,
  hookHealthLines,
  rowEdit,
  searchIntents,
  searching,
  selectNode,
  settingCount,
  settingsRows,
  settingsTitle,
  settingsTree,
  toggleNode,
  type SettingsRow,
} from "./settings.ts";
import { sidecarDaemonLine, type SidecarState } from "./sidecar.ts";
import type { RendererIntent } from "./tabs.ts";

interface Props {
  /** The config frame the form draws. */
  config: ConfigView_Serialize | undefined;
  /** The config section version that frame carried; every edit names it. */
  revision: number;
  /** The hangar frame the daemons panel draws. */
  hangar: HangarView_Serialize | undefined;
  /** The sidecar's state: the daemon this window is on, once connected. */
  sidecar: SidecarState;
  /** The Setup panel's host read, `null` until it answers. */
  setup: SetupView | null;
  /** Send intents in order, each applied before the next. */
  run(intents: RendererIntent[]): void;
  /** Ask the shell to confirm and run one of the onboarding writes. */
  onSetupWrite(write: SetupWrite): void;
  /** Read the Setup panel again. */
  onRefreshSetup(): void;
  onClose(): void;
}

/**
 * The settings page: the config section as the reducer has it on screen (the
 * tree's visible nodes, the selected node's rows or the filter's matches), the
 * daemons panel, and the Setup panel for the writes the window may not run
 * itself. The page keeps no selection: a click names a node or a row and the
 * reducer moves. The DOM half of parity renders this component.
 */
export function SettingsPage(props: Props) {
  const tree = createMemo(() => settingsTree(props.config));
  const rows = createMemo(() => settingsRows(props.config));
  const daemons = createMemo(() => daemonRows(props.hangar));
  // Drawn by key, not by object identity (#1267). Each of these projections
  // builds new objects whenever the frame behind it changes, and `For` keys by
  // identity, so every node, row and daemon line was re-created under whatever
  // the pointer or the keyboard focus was on. The keys are the node's id, the
  // row's key and the daemon's kind, all stable across frames.
  const treeList = createMemo(() => keyedList(tree(), (node) => node.id));
  const treeKeys = createMemo(() => treeList().keys, [], { equals: sameKeys });
  const rowList = createMemo(() => keyedList(rows(), (row) => row.key));
  const rowKeys = createMemo(() => rowList().keys, [], { equals: sameKeys });
  const daemonList = createMemo(() => keyedList(daemons(), (row) => row.kind));
  const daemonKeys = createMemo(() => daemonList().keys, [], { equals: sameKeys });
  const hooks = createMemo(() => hookHealthLines(props.hangar));
  const collected = createMemo(() => daemonsCollectedAt(props.hangar));
  const [otel, setOtel] = createSignal({ otlp_endpoint: "", instance_id: "", api_token: "" });

  const edit = (row: SettingsRow, input: string | number | boolean) => {
    const intent = rowEdit(row, input, props.revision);
    if (intent) props.run([intent]);
  };

  return (
    <section class="settings-page" aria-label="Settings">
      <header class="settings-head">
        <h2>Settings ({settingCount(props.config)} settings)</h2>
        <input
          type="search"
          class="settings-search"
          placeholder="Search every setting"
          aria-label="Search settings"
          classList={{ active: searching(props.config) }}
          onInput={(event) => props.run(searchIntents(event.currentTarget.value))}
        />
        <button type="button" class="close" onClick={() => props.onClose()}>
          Back to board
        </button>
      </header>
      <div class="settings-body">
        <nav class="settings-tree" aria-label="Categories">
          <For each={treeKeys()}>
            {(key) => {
              const node = () => treeList().byKey.get(key);
              return (
                <div
                  class="settings-node"
                  style={{ "padding-left": `${(node()?.depth ?? 0) * 14}px` }}
                  data-node={node()?.id}
                >
                  <Show when={node()?.hasChildren} fallback={<span class="chevron-gap" />}>
                    <button
                      type="button"
                      class="chevron"
                      aria-label={node()?.expanded ? `Collapse ${node()?.label}` : `Expand ${node()?.label}`}
                      aria-expanded={node()?.expanded}
                      onClick={() => props.run(toggleNode(key))}
                    >
                      {node()?.expanded ? "▾" : "▸"}
                    </button>
                  </Show>
                  <button
                    type="button"
                    classList={{ active: node()?.selected }}
                    aria-current={node()?.selected ? "true" : undefined}
                    onClick={() => props.run([selectNode(key)])}
                  >
                    {node()?.label}
                  </button>
                </div>
              );
            }}
          </For>
        </nav>
        <div class="settings-rows">
          <h3>{settingsTitle(props.config)}</h3>
          <Show when={rows().length === 0}>
            <p class="empty">{props.config === undefined ? "No config frame yet" : "No settings here"}</p>
          </Show>
          <For each={rowKeys()}>
            {(key) => {
              // The row as the latest frame has it, read when it draws and
              // again when its widget sends an edit.
              const row = () => rowList().byKey.get(key);
              return (
                <Show when={row()}>
                  {(current) => (
                    <div
                      class="settings-row"
                      classList={{ dirty: current().dirty, readonly: current().readOnly, current: current().current }}
                      data-key={current().key}
                    >
                      <label>
                        <span class="row-label">
                          {current().label}: {current().value}
                        </span>
                        <Widget row={current()} onInput={(input) => edit(current(), input)} />
                      </label>
                      <Show when={current().description !== ""}>
                        <p class="description">{current().description}</p>
                      </Show>
                    </div>
                  )}
                </Show>
              );
            }}
          </For>
        </div>
      </div>
      <section class="daemons-panel" aria-label="Daemons">
        <h3>Daemons: runtime health</h3>
        <table>
          <thead>
            <tr>
              <For each={[...DAEMON_COLUMNS]}>{(column) => <th>{column}</th>}</For>
            </tr>
          </thead>
          <tbody>
            <For each={daemonKeys()}>
              {(key) => {
                const row = () => daemonList().byKey.get(key);
                return (
                  <tr data-daemon={row()?.kind} data-state={row()?.state} data-connected={row()?.connected}>
                    <td>{row()?.kind}</td>
                    <td>{row()?.state}</td>
                    <td>{row()?.version}</td>
                    <td>{row()?.errors}</td>
                    <td>{row()?.reason}</td>
                  </tr>
                );
              }}
            </For>
          </tbody>
        </table>
        <Show when={daemons().length === 0}>
          <p class="empty">{collected() === null ? "reading daemon health" : "no daemons found"}</p>
        </Show>
        <Show when={sidecarDaemonLine(props.sidecar)}>{(line) => <p class="sidecar-daemon">{line()}</p>}</Show>
        <ul class="hook-health">
          <For each={hooks()}>{(line) => <li>{line}</li>}</For>
        </ul>
      </section>
      <section class="setup-panel" aria-label="Setup">
        <h3>Setup</h3>
        <p class="description">
          These steps write outside ainb, so the window does not run them itself: each one asks in a dialog
          the shell owns before it runs.
        </p>
        <Show when={props.setup} fallback={<p class="empty">reading setup status</p>}>
          {(setup) => (
            <>
              <ul class="dependencies">
                <For each={setup().dependencies}>
                  {(dep) => (
                    <li data-dependency={dep.id} data-satisfied={dep.satisfied}>
                      <span class="dep-name">
                        {dep.satisfied ? "✓" : dep.required ? "✗" : "○"} {dep.name}
                      </span>
                      <span class="description">{dep.why}</span>
                      <Show when={!dep.satisfied}>
                        <Show when={dep.auto_installable} fallback={<code>{dep.hint}</code>}>
                          <button
                            type="button"
                            onClick={() => props.onSetupWrite({ kind: "install_dependency", id: dep.id })}
                          >
                            Install
                          </button>
                        </Show>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
              <div class="setup-actions">
                <Show when={setup().dependencies.some((dep) => !dep.satisfied && dep.auto_installable)}>
                  <button type="button" onClick={() => props.onSetupWrite({ kind: "install_all_dependencies" })}>
                    Install all missing
                  </button>
                </Show>
                <button type="button" onClick={() => props.onSetupWrite({ kind: "write_tmux_config" })}>
                  {setup().tmux_conf_present ? "Rewrite ~/.tmux.conf" : "Write ~/.tmux.conf"}
                </button>
                <button type="button" onClick={() => props.onRefreshSetup()}>
                  Check again
                </button>
              </div>
              <div class="otel">
                <h4>OpenTelemetry</h4>
                <p class="description">
                  {setup().otel.settings_env_present
                    ? "Claude Code's settings carry the OTLP environment."
                    : "Not set up: fill the three fields from your Grafana Cloud OTLP page."}
                  {setup().otel.alloy_installed ? " Collector installed." : ""}
                </p>
                <label>
                  OTLP endpoint
                  <input
                    type="url"
                    value={otel().otlp_endpoint}
                    onInput={(event) => setOtel({ ...otel(), otlp_endpoint: event.currentTarget.value })}
                  />
                </label>
                <label>
                  Instance ID
                  <input
                    type="text"
                    value={otel().instance_id}
                    onInput={(event) => setOtel({ ...otel(), instance_id: event.currentTarget.value })}
                  />
                </label>
                <label>
                  Token
                  <input
                    type="password"
                    value={otel().api_token}
                    onInput={(event) => setOtel({ ...otel(), api_token: event.currentTarget.value })}
                  />
                </label>
                <button
                  type="button"
                  onClick={() => {
                    props.onSetupWrite({ kind: "finish_open_telemetry", ...otel() });
                    setOtel({ otlp_endpoint: "", instance_id: "", api_token: "" });
                  }}
                >
                  Finish OpenTelemetry setup
                </button>
              </div>
            </>
          )}
        </Show>
      </section>
    </section>
  );
}

/** The widget a row's kind calls for. An input commits on change, so a keystroke is not a write. */
function Widget(props: { row: SettingsRow; onInput(input: string | number | boolean): void }) {
  const row = () => props.row;
  return (
    <Show when={!row().readOnly} fallback={<span class="readonly-note">read-only: {row().readOnlyReason}</span>}>
      <Show when={row().kind === "bool"}>
        <input
          type="checkbox"
          checked={row().selected === 1}
          onChange={(event) => props.onInput(event.currentTarget.checked)}
        />
      </Show>
      <Show when={row().kind === "choice"}>
        <select
          value={String(row().selected)}
          onChange={(event) => props.onInput(Number(event.currentTarget.value))}
        >
          <For each={row().options}>{(option, index) => <option value={String(index())}>{option}</option>}</For>
        </select>
      </Show>
      <Show when={row().kind === "number"}>
        <input
          type="number"
          step="1"
          value={row().value}
          onChange={(event) => props.onInput(event.currentTarget.value)}
        />
      </Show>
      {/* Empty, with the frame's value as the placeholder: that value is scrubbed
          and cut for the frame, so prefilling it would write the marker or a
          truncated value over the real one. */}
      <Show when={row().kind === "text"}>
        <input type="text" placeholder={row().value} onChange={(event) => props.onInput(event.currentTarget.value)} />
      </Show>
    </Show>
  );
}
