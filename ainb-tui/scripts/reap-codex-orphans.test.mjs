import assert from "node:assert/strict";
import { test } from "node:test";

import {
  codexOrphansToReap,
  isStaleCandidateName,
  liveProxySockets,
  parseAppServerSocket,
} from "./reap-codex-orphans.mjs";

const DAEMON_SOCK = "/Users/x/.agents-in-a-box/codex-app-server.sock";

// A realistic `ps -Ao pid,ppid,args` snapshot mixing the two leak sources with
// processes that must NOT be reaped. Column widths are right-aligned like ps.
const PS_FIXTURE = [
  "  PID  PPID ARGS",
  // Source-A orphan: reparented to init, native codex app-server broker.
  "  111     1 /Users/dev/.nvm/versions/node/v22.13.0/lib/node_modules/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex app-server --listen unix:///var/folders/2y/T/codex-app-server.sock",
  // Source-B orphan: node-launched broker into a cxc-* session dir.
  "  222     1 node /Users/dev/.nvm/versions/node/v22.13.0/lib/node_modules/@openai/codex/bin/codex app-server --listen unix:///var/folders/2y/T/cxc-Ab3xQ9/broker.sock",
  // Live child still parented by a running session (ppid != 1) -> keep.
  "  333   999 /Users/dev/.nvm/versions/node/v22.13.0/.../bin/codex app-server --listen unix:///var/folders/2y/T/cxc-Zz0000/broker.sock",
  // Desktop ChatGPT app server: no `bin/codex`, ppid != 1 -> keep.
  "  444   500 /Applications/ChatGPT.app/Contents/Resources/codex -c features.code_mode_host=true app-server --analytics-default-enabled",
  // Orphaned bin/codex CLI without app-server subcommand -> keep.
  "  555     1 /Users/dev/.nvm/versions/node/v22.13.0/.../bin/codex --dangerously-bypass-approvals-and-sandbox",
  "",
].join("\n");

test("codexOrphansToReap selects only ppid==1 bin/codex app-server orphans", () => {
  const pids = codexOrphansToReap(PS_FIXTURE);
  assert.deepEqual(pids, [111, 222]);
});

test("codexOrphansToReap excludes the desktop ChatGPT app-server", () => {
  const pids = codexOrphansToReap(PS_FIXTURE);
  assert.ok(!pids.includes(444), "desktop app server must never be reaped");
});

test("codexOrphansToReap excludes live children (ppid != 1)", () => {
  const pids = codexOrphansToReap(PS_FIXTURE);
  assert.ok(!pids.includes(333), "session-parented broker must be spared");
});

test("codexOrphansToReap excludes bin/codex without app-server", () => {
  const pids = codexOrphansToReap(PS_FIXTURE);
  assert.ok(!pids.includes(555), "plain codex CLI must be spared");
});

test("codexOrphansToReap tolerates empty / non-string input", () => {
  assert.deepEqual(codexOrphansToReap(""), []);
  assert.deepEqual(codexOrphansToReap(undefined), []);
  assert.deepEqual(codexOrphansToReap("  PID  PPID ARGS\n"), []);
});

test("codexOrphansToReap dedupes repeated pids", () => {
  const dup = [
    "  PID  PPID ARGS",
    "  777     1 /x/bin/codex app-server --listen unix:///t/a.sock",
    "  777     1 /x/bin/codex app-server --listen unix:///t/a.sock",
  ].join("\n");
  assert.deepEqual(codexOrphansToReap(dup), [777]);
});

// The daemon's self-daemonized server (ppid==1) WITH a live proxy consumer on
// the same socket -> in use, must be SPARED. The desktop app stays excluded.
const PS_WITH_LIVE_PROXY = [
  "  PID  PPID ARGS",
  `  501     1 /u/.nvm/versions/node/v22.13.0/.../bin/codex app-server --listen unix://${DAEMON_SOCK}`,
  `  502   700 /u/.nvm/versions/node/v22.13.0/.../bin/codex app-server proxy --sock ${DAEMON_SOCK}`,
  "  444   500 /Applications/ChatGPT.app/Contents/Resources/codex -c features.code_mode_host=true app-server --analytics-default-enabled",
  "",
].join("\n");

// The SAME daemon server (ppid==1) with NO live proxy -> truly orphaned, plus a
// plugin broker orphan on a cxc-* socket. Both must be reaped.
const PS_NO_PROXY = [
  "  PID  PPID ARGS",
  `  501     1 /u/.nvm/versions/node/v22.13.0/.../bin/codex app-server --listen unix://${DAEMON_SOCK}`,
  "  777     1 node /u/.nvm/versions/node/v22.13.0/.../bin/codex app-server --listen unix:///var/folders/ab/cd/T/cxc-9/broker.sock",
  "  444   500 /Applications/ChatGPT.app/Contents/Resources/codex -c features.code_mode_host=true app-server --analytics-default-enabled",
  "",
].join("\n");

test("codexOrphansToReap spares a ppid==1 server with a live proxy consumer", () => {
  const pids = codexOrphansToReap(PS_WITH_LIVE_PROXY);
  assert.ok(!pids.includes(501), "in-use daemon server must not be reaped");
  assert.deepEqual(pids, [], "nothing is orphaned when a live proxy is present");
});

test("codexOrphansToReap reaps the same server once its proxy is gone", () => {
  const pids = codexOrphansToReap(PS_NO_PROXY);
  assert.deepEqual(pids, [501, 777]);
});

test("codexOrphansToReap never reaps the proxy process itself", () => {
  // proxy line 502 has bin/codex + app-server but is a consumer, not a server.
  assert.ok(!codexOrphansToReap(PS_WITH_LIVE_PROXY).includes(502));
});

test("liveProxySockets collects sockets targeted by live proxies", () => {
  const socks = liveProxySockets(PS_WITH_LIVE_PROXY);
  assert.ok(socks.has(DAEMON_SOCK));
  assert.equal(socks.size, 1);
  assert.equal(liveProxySockets(PS_NO_PROXY).size, 0);
});

test("parseAppServerSocket handles each arg shape", () => {
  assert.equal(
    parseAppServerSocket("/x/bin/codex app-server --listen unix:///Users/x/foo.sock"),
    "/Users/x/foo.sock",
  );
  assert.equal(
    parseAppServerSocket("codex app-server --listen unix:/abs/path.sock"),
    "/abs/path.sock",
  );
  assert.equal(
    parseAppServerSocket("codex app-server proxy --sock /abs/p.sock"),
    "/abs/p.sock",
  );
  assert.equal(
    parseAppServerSocket("worker unix://relative/path.sock"),
    "relative/path.sock",
  );
  assert.equal(
    parseAppServerSocket("codex app-server --listen=unix:///eq/listen.sock"),
    "/eq/listen.sock",
  );
  assert.equal(
    parseAppServerSocket("codex app-server proxy --sock=/eq/sock.sock"),
    "/eq/sock.sock",
  );
  assert.equal(parseAppServerSocket("codex app-server"), null);
  assert.equal(parseAppServerSocket(undefined), null);
});

test("isStaleCandidateName accepts broker session dir names", () => {
  assert.equal(isStaleCandidateName("cxc-abc123"), true);
  assert.equal(isStaleCandidateName("cxc-Ab3xQ9"), true);
});

test("isStaleCandidateName rejects unrelated dir names", () => {
  assert.equal(isStaleCandidateName("codex-foo"), false);
  assert.equal(isStaleCandidateName("cxc-"), false); // prefix with no suffix
  assert.equal(isStaleCandidateName("foo"), false);
  assert.equal(isStaleCandidateName(".cxc-abc"), false);
  assert.equal(isStaleCandidateName(undefined), false);
});
