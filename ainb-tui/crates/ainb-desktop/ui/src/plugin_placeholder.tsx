import { Match, Show, Switch } from "solid-js";
import type { PluginsHostView_Serialize } from "../../../ainb-app/bindings/AppState";
import { type Placeholder, placeholderFor, shown, titleCase } from "./plugin_placeholder.ts";

interface Props {
  /** The plugin screen being shown. */
  screen: string;
  /** The framed plugins_host section. */
  pluginsHost: PluginsHostView_Serialize;
}

/**
 * The plugin fallback cell's placeholder (D3p-f): the three states a plugin
 * screen shows with nothing to paint, worded as the terminal words them. The
 * live cells are not drawn here; this shell runs no plugin runtime.
 */
export function PluginPlaceholder(props: Props) {
  const state = () => placeholderFor(props.screen, props.pluginsHost);
  // Frame text, drawn as text: control and format characters dropped.
  const title = () => shown(titleCase(props.screen));
  // The terminal names the screen when no plugin owns it.
  const owner = () => shown(state().plugin ?? props.screen);
  return (
    <section class="plugin-placeholder" data-state={state().kind} data-screen={props.screen}>
      <Switch>
        <Match when={state().kind === "render_error"}>
          <h2>{`${title()} unavailable`}</h2>
          <p class="lead">{`The \`${owner()}\` plugin could not render this screen.`}</p>
          <p class="error">{shown(errorOf(state()))}</p>
          <Show when={state().kind === "render_error" && (state() as { cut: boolean }).cut}>
            <p class="hint">The error was cut; the whole of it is in the log.</p>
          </Show>
          <p class="hint">
            If ainb was upgraded while this session was open, the plugin binary it was discovered from no longer
            exists. Quit and relaunch ainb.
          </p>
          <p class="hint">Logs: ~/.agents-in-a-box/logs/agents-in-a-box-*.jsonl - search `plugin spawn failed`.</p>
        </Match>
        <Match when={state().kind === "no_frame"}>
          <h2>{`${title()} — connecting…`}</h2>
          <p class="hint">waiting for the plugin's first frame</p>
        </Match>
        <Match when={state().kind === "not_registered"}>
          <h2>{state().plugin === null ? "plugin unavailable" : `${owner()} unavailable`}</h2>
          <p class="lead">{`This screen is owned by the \`${owner()}\` plugin, which isn't loaded.`}</p>
          <p class="hint">Check whether plugins are disabled in this session:</p>
          <ul class="hint">
            <li>AINB_DISABLE_PLUGINS=1 — all plugins off (kill switch)</li>
            <li>{`AINB_DISABLE_PLUGIN=${state().plugin === null ? "<id>" : owner()} — this plugin denylisted by env`}</li>
            <li>AINB_ONLY_PLUGINS=… — env allowlist excludes it</li>
            <li>config.toml [plugins] — persistent allow/disable list</li>
          </ul>
          <p class="hint">To restore it: unset the env var(s) and/or edit ~/.agents-in-a-box/config/config.toml</p>
          <p class="hint">Logs: ~/.agents-in-a-box/logs/agents-in-a-box-*.jsonl — search `applying plugin filter`.</p>
          <p class="hint">See docs/plugins.md → Configuration → Enable/disable plugins.</p>
        </Match>
      </Switch>
    </section>
  );
}

/** The recorded error of a render-error placeholder, empty for the others. */
function errorOf(placeholder: Placeholder): string {
  return placeholder.kind === "render_error" ? placeholder.error : "";
}
