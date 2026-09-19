import { Match, Switch } from "solid-js";
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
  return (
    <section class="plugin-placeholder" data-state={state().kind} data-screen={props.screen}>
      <Switch>
        <Match when={state().kind === "render_error"}>
          <h2>{`${titleCase(props.screen)} unavailable`}</h2>
          <p class="lead">{`The \`${state().plugin}\` plugin could not render this screen.`}</p>
          <p class="error">{shown(errorOf(state()))}</p>
          <p class="hint">
            If ainb was upgraded while this session was open, the plugin binary it was discovered from no longer
            exists. Quit and relaunch ainb.
          </p>
          <p class="hint">Logs: ~/.agents-in-a-box/logs/agents-in-a-box-*.jsonl - search `plugin spawn failed`.</p>
        </Match>
        <Match when={state().kind === "no_frame"}>
          <h2>{`${titleCase(props.screen)} — connecting…`}</h2>
          <p class="hint">waiting for the plugin's first frame</p>
        </Match>
        <Match when={state().kind === "not_registered"}>
          <h2>{`${state().plugin} unavailable`}</h2>
          <p class="lead">{`This screen is owned by the \`${state().plugin}\` plugin, which isn't loaded.`}</p>
          <p class="hint">Check whether plugins are disabled in this session:</p>
          <ul class="hint">
            <li>AINB_DISABLE_PLUGINS=1 — all plugins off (kill switch)</li>
            <li>{`AINB_DISABLE_PLUGIN=${state().plugin} — this plugin denylisted by env`}</li>
            <li>AINB_ONLY_PLUGINS=… — env allowlist excludes it</li>
            <li>config.toml [plugins] — persistent allow/disable list</li>
          </ul>
        </Match>
      </Switch>
    </section>
  );
}

/** The recorded error of a render-error placeholder, empty for the others. */
function errorOf(placeholder: Placeholder): string {
  return placeholder.kind === "render_error" ? placeholder.error : "";
}
