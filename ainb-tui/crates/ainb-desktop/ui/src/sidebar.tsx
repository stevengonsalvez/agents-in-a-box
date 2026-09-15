import { For, Show } from "solid-js";
import type { FleetView_Serialize, SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { ringFor, rowStatus } from "./sessions.ts";

interface Props {
  sessions: SessionsView_Serialize | undefined;
  fleet: FleetView_Serialize | undefined;
  stale: boolean;
}

/**
 * The sidebar: the home menu's nav merged into the shell, and the sessions
 * tree grouped by workspace with one attention ring per row. Selection is the
 * Sessions section's own; the sidebar draws it and keeps none.
 */
export function Sidebar(props: Props) {
  return (
    <aside class="sidebar" aria-label="Sessions">
      <div class="nav-title">
        SESSIONS
        <Show when={props.stale}>
          <span class="stale">stale</span>
        </Show>
      </div>
      <Show
        when={(props.sessions?.workspaces.length ?? 0) > 0}
        fallback={<p class="empty">{props.sessions ? "No sessions" : "Loading sessions"}</p>}
      >
        <For each={props.sessions?.workspaces}>
          {(workspace, w) => (
            <section class="workspace" data-workspace={workspace.name}>
              <div class="workspace-name">{workspace.name}</div>
              <ul>
                <For each={workspace.sessions}>
                  {(session, s) => {
                    const selected = () =>
                      props.sessions?.selected_workspace_index === w() &&
                      props.sessions?.selected_session_index === s();
                    const ring = () => ringFor(session, props.fleet);
                    return (
                      <li
                        class="session-row"
                        classList={{ selected: selected() }}
                        data-session={session.id}
                        data-ring={ring()?.toLowerCase() ?? "none"}
                        aria-current={selected() ? "true" : undefined}
                      >
                        <span class="cursor">{selected() ? "▶" : ""}</span>
                        <span class={`ring ${rowStatus(session.status)}`} title={ring() ?? rowStatus(session.status)} />
                        <span class="name">{session.name}</span>
                        <span class="branch">{session.branch_name}</span>
                      </li>
                    );
                  }}
                </For>
              </ul>
            </section>
          )}
        </For>
      </Show>
    </aside>
  );
}
