import { For, Show } from "solid-js";
import type { SessionsView_Serialize } from "../../../ainb-app/bindings/AppState";
import { label, ringFor, rowStatus, visibleRows } from "./sessions.ts";

interface Props {
  sessions: SessionsView_Serialize | undefined;
  stale: boolean;
  /** The host is loading workspaces (the WorkspaceLoad section). */
  loading: boolean;
  /** A row was chosen: open its session's terminal tab. */
  onOpen(sessionId: string): void;
  /** The sidebar element, for Esc Esc to return focus to. */
  ref(element: HTMLElement): void;
}

/**
 * The sidebar: the home menu's nav merged into the shell, and the sessions
 * tree grouped by workspace with one attention ring per row. Selection is the
 * Sessions section's own; the sidebar draws it and keeps none. A row is a
 * button, so a click or Enter opens its terminal tab.
 */
export function Sidebar(props: Props) {
  return (
    <aside class="sidebar" aria-label="Sessions" tabIndex={-1} ref={props.ref}>
      <div class="nav-title">
        SESSIONS
        <Show when={props.stale}>
          <span class="stale">stale</span>
        </Show>
      </div>
      <Show
        when={(props.sessions?.workspaces.length ?? 0) > 0}
        fallback={<p class="empty">{props.loading || !props.sessions ? "Loading sessions" : "No sessions"}</p>}
      >
        <For each={props.sessions?.workspaces}>
          {(workspace, w) => (
            <Show when={visibleRows(props.sessions, workspace).length > 0}>
              <section class="workspace" data-workspace={workspace.name}>
                <div class="workspace-name">{label(workspace.name)}</div>
                <ul>
                {/* The session list's own filter decides which rows are
                    here, so what the sidebar draws and what the reducer's
                    navigation walks are the same set. */}
                <For each={visibleRows(props.sessions, workspace)}>
                  {(row) => {
                    const session = row.session;
                    const selected = () =>
                      props.sessions?.selected_workspace_index === w() &&
                      props.sessions?.selected_session_index === row.index;
                    const ring = () => ringFor(session);
                    return (
                      <li>
                        <button
                          type="button"
                          class="session-row"
                          classList={{ selected: selected() }}
                          data-session={session.id}
                          data-ring={ring()?.toLowerCase() ?? "none"}
                          aria-current={selected() ? "true" : undefined}
                          onClick={() => props.onOpen(session.id)}
                        >
                          <span class="cursor">{selected() ? "▶" : ""}</span>
                          <span class={`ring ${rowStatus(session.status)}`} title={ring() ?? rowStatus(session.status)} />
                          <span class="name">{label(session.name)}</span>
                          <span class="branch">{label(session.branch_name)}</span>
                        </button>
                      </li>
                    );
                  }}
                </For>
                </ul>
              </section>
            </Show>
          )}
        </For>
      </Show>
    </aside>
  );
}
