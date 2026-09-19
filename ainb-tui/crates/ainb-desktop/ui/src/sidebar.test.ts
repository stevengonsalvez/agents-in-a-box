// The sidebar rendered over a Sessions frame draws exactly the frame's rows,
// in frame order, with no filter of its own (#1180): the host already sent
// only the rows its filter shows. Rendered for real, through Vite's SSR loader
// and the Solid plugin, so a filter re-added in `sidebar.tsx` fails here.

import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";
import solid from "vite-plugin-solid";

const session = (id: string, status: unknown = "Running") => ({
  id,
  name: id,
  branch_name: `ainb/${id}`,
  status,
  attention: [],
});

test("the sidebar draws exactly the frame's rows", async () => {
  const frame = {
    workspaces: [
      { name: "repo", sessions: [session("live"), session("gone", "Stopped"), session("boss", "Stopped")] },
      { name: "empty", sessions: [] },
    ],
    selected_workspace_index: 0,
    selected_session_id: "boss",
    // The TUI's own filter reads active_only; the window must not apply it.
    session_filter: "active_only",
  };
  const server = await createServer({
    configFile: false,
    root: fileURLToPath(new URL("..", import.meta.url)),
    plugins: [solid({ ssr: true })],
    server: { middlewareMode: true, hmr: false },
    appType: "custom",
    // Vite resolves Solid itself, so the component and `renderToString` share
    // one server build whatever conditions the test runner was started with.
    ssr: { noExternal: ["solid-js"] },
    logLevel: "silent",
  });
  try {
    const { Sidebar } = await server.ssrLoadModule("/src/sidebar.tsx");
    const { renderToString } = await server.ssrLoadModule("solid-js/web");
    const html: string = renderToString(() =>
      Sidebar({ sessions: frame, stale: false, loading: false, onOpen() {}, ref() {} }),
    );
    const drawn = [...html.matchAll(/data-session="([^"]+)"/g)].map((match) => match[1]);
    assert.deepEqual(drawn, ["live", "gone", "boss"], html);
    assert.equal(html.match(/aria-current="true"/g)?.length, 1, html);
    assert.doesNotMatch(html, /data-workspace="empty"/, "a workspace with no rows is not drawn");
  } finally {
    await server.close();
  }
});
