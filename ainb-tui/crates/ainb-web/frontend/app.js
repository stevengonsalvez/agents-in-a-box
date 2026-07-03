// ainb fleet dashboard — vanilla JS, no framework, no build step.
//
// Boot: read ?token= from the URL (strip it), fetch the initial snapshot,
// render the three panels, then open an SSE stream for live updates. If a
// token is required and absent/invalid, show the auth prompt.

(() => {
  "use strict";

  const $ = (id) => document.getElementById(id);

  // ── Auth token: from URL query, then sessionStorage. ──────────────
  const url = new URL(window.location.href);
  let token = url.searchParams.get("token") || sessionStorage.getItem("ainb_token") || "";
  if (url.searchParams.has("token")) {
    sessionStorage.setItem("ainb_token", token);
    url.searchParams.delete("token");
    window.history.replaceState({}, "", url.toString());
  }

  const authHeaders = () => (token ? { Authorization: `Bearer ${token}` } : {});

  // Server posture, learned from /healthz on boot. When read-only the terminal
  // attach affordance is never shown (the WS route 403s anyway).
  let readOnly = true;

  // ── Rendering helpers ─────────────────────────────────────────────
  const el = (tag, cls, text) => {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  };

  const emptyState = (icon, msg) => {
    const e = el("div", "empty");
    e.appendChild(el("span", "empty-icon", icon));
    e.appendChild(document.createTextNode(msg));
    return e;
  };

  // The CLI session list (`ainb list --format json`) is an array of
  // SessionInfo: { session_id, tmux_session_name, workspace_name,
  // worktree_path, created_at, is_running, claude_active }.
  function sessionStatus(s) {
    if (!s.is_running) return { label: "stopped", cls: "status-stopped" };
    if (s.claude_active) return { label: "running", cls: "status-running" };
    return { label: "idle", cls: "status-idle" };
  }

  function renderSessions(sessions) {
    const host = $("sessions");
    host.replaceChildren();
    const list = Array.isArray(sessions) ? sessions : [];
    $("stat-sessions").textContent = list.length;
    $("sessions-meta").textContent = list.length ? `${list.length} tracked` : "";

    if (!list.length) {
      host.appendChild(emptyState("◇", "No sessions tracked yet."));
      return;
    }

    for (const s of list) {
      const st = sessionStatus(s);
      const row = el("div", "row");
      row.appendChild(el("span", `status-dot ${st.cls}`));

      const main = el("div", "row-main");
      main.appendChild(el("div", "row-name", s.workspace_name || s.tmux_session_name || "session"));
      const meta = el("div", "row-meta");
      if (s.worktree_path) {
        const code = el("code", null, s.worktree_path);
        meta.appendChild(code);
      }
      if (s.tmux_session_name) meta.appendChild(el("span", null, s.tmux_session_name));
      main.appendChild(meta);
      row.appendChild(main);

      const badges = el("div", "badges");
      badges.appendChild(el("span", "badge badge-status", st.label));
      // The terminal write surface only exists when the server is NOT
      // read-only and the session is running. Offer an "attach" button there.
      if (!readOnly && s.is_running && s.session_id) {
        const open = el("button", "attach-btn", "▸ terminal");
        open.title = "Attach a live terminal to this session";
        open.addEventListener("click", (ev) => {
          ev.stopPropagation();
          openTerminal(s.session_id, s.workspace_name || s.tmux_session_name || "session");
        });
        badges.appendChild(open);
      }
      row.appendChild(badges);

      host.appendChild(row);
    }
  }

  // Needs come from the daemon attention inbox (`attention/list`, spec P8/D18),
  // mapped in crates/ainb-web/src/daemon.rs to cards of shape:
  //   { attentionId, kind: "ASK|ERR|WAIT", wireKind, sessionId, cwd,
  //     workspaceId, degraded, createdAt, payload }
  // `payload` is the parsed request context (ASK → { question, options }) or the
  // raw string when it did not parse. An ASK card with an `attentionId` is
  // answerable inline via POST /api/answer.
  function needKind(row) {
    return (row.kind || "").toUpperCase() || "ASK";
  }

  function needTitle(row) {
    return row.cwd || row.workspaceId || row.sessionId || "session";
  }

  function needDetail(row) {
    const p = row.payload;
    if (p && typeof p === "object") {
      return (
        p.question || p.snippet || p.pattern || p.text || p.marker || p.message || ""
      );
    }
    return typeof p === "string" ? p : "";
  }

  // The option list an ASK card offers, if any. `payload.options` is an array of
  // option strings (AskUserQuestion); we answer with the option's 1-based number
  // to match the session picker + the bridge "reply N" contract.
  function needOptions(row) {
    const p = row.payload;
    const opts = p && typeof p === "object" ? p.options : null;
    return Array.isArray(opts) ? opts : [];
  }

  // POST an answer for one attention row through the daemon (D18). The daemon
  // runs the first-answer-wins + C1 guards and performs the ONE verified send;
  // on any resolution the row leaves the open inbox, so we re-pull the snapshot
  // to reconcile. Never touches tmux from the browser.
  async function postAnswer(attentionId, answer, card) {
    if (!attentionId) return;
    card.classList.add("answering");
    try {
      const res = await fetch("/api/answer", {
        method: "POST",
        headers: { "content-type": "application/json", ...authHeaders() },
        body: JSON.stringify({ attentionId, answer }),
      });
      const data = await res.json().catch(() => ({}));
      card.dataset.outcome = (data && data.outcome) || (res.ok ? "sent" : "failed");
      // Reconcile from the source of truth (an answered row drops from the inbox).
      loadOnce().then(render).catch(() => {});
    } catch (_) {
      card.classList.remove("answering");
      card.dataset.outcome = "failed";
    }
  }

  function renderNeeds(needs) {
    const host = $("needs");
    host.replaceChildren();
    const list = Array.isArray(needs) ? needs : [];
    // Count only the actionable kinds for the headline stat.
    const attn = list.filter((r) => ["ASK", "ERR", "WAIT"].includes(needKind(r)));
    $("stat-needs").textContent = attn.length;
    $("needs-meta").textContent = list.length ? `${list.length} cards` : "";

    if (!list.length) {
      host.appendChild(emptyState("✓", "Nothing needs attention."));
      return;
    }

    // Sort by priority ASK > ERR > WAIT > IDLE.
    const order = { ASK: 0, ERR: 1, WAIT: 2, IDLE: 3 };
    list.sort((a, b) => (order[needKind(a)] ?? 9) - (order[needKind(b)] ?? 9));

    for (const row of list) {
      const kind = needKind(row);
      const card = el("div", "need");
      card.appendChild(el("span", `need-kind kind-${kind}`, kind));
      if (row.degraded) card.appendChild(el("span", "need-badge", "degraded"));
      const body = el("div", "need-body");
      body.appendChild(el("div", "need-title", needTitle(row)));
      const detail = needDetail(row);
      if (detail) body.appendChild(el("div", "need-detail", detail));

      // Answer affordances: only for answerable ASK cards carrying an id.
      if (kind === "ASK" && row.attentionId) {
        const actions = el("div", "need-actions");
        needOptions(row).forEach((opt, i) => {
          const n = String(i + 1);
          const b = el("button", "need-answer", `${n}. ${opt}`);
          b.type = "button";
          b.addEventListener("click", () => postAnswer(row.attentionId, n, card));
          actions.appendChild(b);
        });
        // Free-text reply (Codex request-user / anything without options).
        const form = el("form", "need-reply");
        const input = el("input", "need-reply-input");
        input.type = "text";
        input.placeholder = "type a reply…";
        const send = el("button", "need-answer", "Send");
        send.type = "submit";
        form.addEventListener("submit", (e) => {
          e.preventDefault();
          const v = input.value.trim();
          if (v) postAnswer(row.attentionId, v, card);
        });
        form.appendChild(input);
        form.appendChild(send);
        actions.appendChild(form);
        body.appendChild(actions);
      }

      card.appendChild(body);
      host.appendChild(card);
    }
  }

  // Cost (`ainb fleet cost --format json`). The payload is the CLI's
  // `CostReport` (crates/ainb-core/src/cli/fleet/cost.rs), serialized with
  // serde's default snake_case field names. It is NOT a flat rollup — the
  // numbers are nested:
  //   cost.totals  : { cost_usd, session_count, model_count, bucket: TokenBucket }
  //   cost.models[]: { model, cost_usd, bucket }
  //   cost.groups[]: { group, cost_usd, session_count, bucket }
  //   cost.sessions[], cost.daily[], cost.budget_breaches[]  (not surfaced here)
  // where a TokenBucket is { input_tokens, cache_creation_tokens,
  // cache_read_tokens, output_tokens, reasoning_tokens, call_count, cost_usd }.
  // `cost` is `null` when the `fleet cost` verb is absent from this build.
  function fmtUsd(n) {
    if (typeof n !== "number" || !isFinite(n)) return "–";
    return `$${n.toFixed(2)}`;
  }

  // Compact token counts: 1234 → "1.2k", 6_114_855 → "6.1M".
  function fmtTokens(n) {
    if (typeof n !== "number" || !isFinite(n)) return "0";
    const abs = Math.abs(n);
    if (abs >= 1e9) return `${(n / 1e9).toFixed(1)}B`;
    if (abs >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
    if (abs >= 1e3) return `${(n / 1e3).toFixed(1)}k`;
    return String(n);
  }

  // Sum every token category in a TokenBucket, tolerating missing fields.
  function bucketTokens(bucket) {
    if (!bucket || typeof bucket !== "object") return 0;
    return (
      (bucket.input_tokens || 0) +
      (bucket.cache_creation_tokens || 0) +
      (bucket.cache_read_tokens || 0) +
      (bucket.output_tokens || 0) +
      (bucket.reasoning_tokens || 0)
    );
  }

  function renderCost(cost) {
    const host = $("cost");
    host.replaceChildren();

    // `null`/absent verb → the surface genuinely isn't available in this build.
    if (cost == null) {
      $("stat-cost").textContent = "–";
      $("cost-meta").textContent = "unavailable";
      host.appendChild(emptyState("◷", "Cost surface not available in this build."));
      return;
    }

    const totals = cost.totals && typeof cost.totals === "object" ? cost.totals : null;
    const models = Array.isArray(cost.models) ? cost.models : [];
    const groups = Array.isArray(cost.groups) ? cost.groups : [];

    // A genuinely empty report: no totals object at all. (A zero-spend report
    // still carries a `totals` object with cost_usd 0, which we render.)
    if (!totals) {
      $("stat-cost").textContent = "–";
      $("cost-meta").textContent = "";
      host.appendChild(emptyState("◷", "No cost data recorded yet."));
      return;
    }

    const totalUsd = typeof totals.cost_usd === "number" ? totals.cost_usd : 0;
    const totalTokens = bucketTokens(totals.bucket);
    $("stat-cost").textContent = fmtUsd(totalUsd);
    const sessionCount = totals.session_count || 0;
    const modelCount = totals.model_count || models.length;
    $("cost-meta").textContent = `${sessionCount} sessions · ${modelCount} models`;

    // Headline tiles: total spend + total tokens across the window.
    const grid = el("div", "cost-grid");
    const tile = (val, key) => {
      const cell = el("div", "cost-cell");
      cell.appendChild(el("div", "cost-val", val));
      cell.appendChild(el("div", "cost-key", key));
      grid.appendChild(cell);
    };
    tile(fmtUsd(totalUsd), "total");
    tile(fmtTokens(totalTokens), "tokens");
    host.appendChild(grid);

    // Per-model breakdown (already sorted by descending cost server-side).
    if (models.length) {
      host.appendChild(el("div", "cost-section", "By model"));
      for (const m of models.slice(0, 8)) {
        const row = el("div", "cost-row");
        const main = el("div", "cost-row-main");
        main.appendChild(el("div", "cost-row-name", m.model || "model"));
        main.appendChild(el("div", "cost-row-meta", `${fmtTokens(bucketTokens(m.bucket))} tokens`));
        row.appendChild(main);
        row.appendChild(el("div", "cost-row-val", fmtUsd(m.cost_usd)));
        host.appendChild(row);
      }
    }

    // Per-group (workspace) rollup, when the fleet join produced any groups.
    if (groups.length) {
      host.appendChild(el("div", "cost-section", "By group"));
      for (const g of groups.slice(0, 8)) {
        const row = el("div", "cost-row");
        const main = el("div", "cost-row-main");
        main.appendChild(el("div", "cost-row-name", g.group || "group"));
        const n = g.session_count || 0;
        main.appendChild(el("div", "cost-row-meta", `${n} session${n === 1 ? "" : "s"}`));
        row.appendChild(main);
        row.appendChild(el("div", "cost-row-val", fmtUsd(g.cost_usd)));
        host.appendChild(row);
      }
    }
  }

  function render(snap) {
    renderSessions(snap.sessions);
    renderNeeds(snap.needs);
    renderCost(snap.cost);
    $("updated").textContent = `Updated ${new Date().toLocaleTimeString()}`;
  }

  // ── Connection state ──────────────────────────────────────────────
  function setConn(state) {
    const c = $("conn");
    c.classList.remove("live", "down");
    const label = c.querySelector(".conn-label");
    if (state === "live") {
      c.classList.add("live");
      label.textContent = "";
    } else if (state === "down") {
      c.classList.add("down");
      label.textContent = "disconnected";
    } else {
      label.textContent = "connecting…";
    }
  }

  function showAuthPrompt() {
    $("auth-prompt").hidden = false;
  }

  // ── Data fetch + SSE ──────────────────────────────────────────────
  async function loadOnce() {
    const res = await fetch("/api/snapshot", { headers: authHeaders() });
    if (res.status === 401) {
      showAuthPrompt();
      throw new Error("unauthorized");
    }
    if (!res.ok) throw new Error(`snapshot failed: ${res.status}`);
    return res.json();
  }

  // After this many consecutive failed connection attempts with no successful
  // open in between, we stop trusting EventSource's silent auto-reconnect and
  // re-check auth posture (a rotated/expired token 401s the stream, which
  // EventSource would otherwise retry against forever without re-prompting).
  const SSE_MAX_FAILURES = 3;

  function startSSE() {
    // EventSource cannot set headers; pass the token via query string. The
    // server accepts it only on this route and we never persist it in the URL.
    const sseUrl = token
      ? `/api/events?token=${encodeURIComponent(token)}`
      : "/api/events";
    const es = new EventSource(sseUrl);
    let failures = 0;

    es.addEventListener("snapshot", (e) => {
      failures = 0; // a real frame proves the connection (and token) is good
      setConn("live");
      try {
        render(JSON.parse(e.data));
      } catch (_) {
        /* ignore malformed frame */
      }
    });
    // The server emits an explicit `error` event (distinct from EventSource's
    // own transport `onerror`) when it cannot serialize a snapshot frame. Mark
    // the connection as not-live so the UI stops claiming fresh data.
    es.addEventListener("error", (e) => {
      // Only act on a server-sent `error` *message* (has data); the bare
      // transport-error event has no `.data` and is handled by `onerror`.
      if (e && typeof e.data === "string" && e.data.length) {
        setConn("down");
      }
    });
    es.onopen = () => {
      failures = 0;
      setConn("live");
    };
    es.onerror = () => {
      setConn("down");
      failures += 1;
      // A 401 (rotated/expired token) lands here as an error; EventSource would
      // otherwise retry the stale token forever and never re-prompt. When the
      // browser has given up (CLOSED) or we've failed repeatedly, stop the
      // stream and re-verify auth posture so a fresh token can be supplied.
      if (es.readyState === EventSource.CLOSED || failures >= SSE_MAX_FAILURES) {
        es.close();
        reauthOrReconnect();
      }
    };
  }

  // Re-probe the auth posture after the SSE stream gives up. If the token is
  // now rejected, surface the auth prompt; otherwise reboot the data path.
  async function reauthOrReconnect() {
    try {
      const res = await fetch("/api/snapshot", { headers: authHeaders() });
      if (res.status === 401) {
        showAuthPrompt();
        return;
      }
    } catch (_) {
      /* network down — fall through to a delayed reboot */
    }
    setTimeout(boot, 3000);
  }

  async function learnPosture() {
    // /healthz is auth-gated when a token is set; a 401 here just means we keep
    // the conservative read-only default until the user supplies a token.
    try {
      const res = await fetch("/healthz", { headers: authHeaders() });
      if (res.ok) {
        const h = await res.json();
        readOnly = h.readOnly !== false;
      }
    } catch (_) {
      /* keep default */
    }
  }

  async function boot() {
    setConn("connecting");
    try {
      await learnPosture();
      render(await loadOnce());
      startSSE();
      initPush();
    } catch (e) {
      if (String(e).includes("unauthorized")) {
        setConn("down");
        return; // auth prompt already shown
      }
      setConn("down");
      // Retry the one-shot load after a short delay.
      setTimeout(boot, 3000);
    }
  }

  // Auth prompt wiring.
  $("auth-go").addEventListener("click", () => {
    const val = $("auth-input").value.trim();
    if (!val) return;
    token = val;
    sessionStorage.setItem("ainb_token", token);
    $("auth-prompt").hidden = true;
    boot();
  });
  $("auth-input").addEventListener("keydown", (e) => {
    if (e.key === "Enter") $("auth-go").click();
  });

  // ── Live terminal (xterm.js over WebSocket) ───────────────────────────
  //
  // Wire protocol (see crates/ainb-web/src/terminal.rs):
  //   S→C binary  : raw PTY bytes → term.write()
  //   S→C text    : {type:"status"|"error", ...} control frames
  //   C→S text    : {type:"input",data} | {type:"resize",cols,rows} | {type:"ping"}
  let termSession = null; // { ws, term, fit, resizeObs }

  function closeTerminal() {
    if (!termSession) return;
    try { termSession.ws.close(); } catch (_) {}
    try { termSession.resizeObs.disconnect(); } catch (_) {}
    try { termSession.term.dispose(); } catch (_) {}
    termSession = null;
    $("term-overlay").hidden = true;
    $("term-body").replaceChildren();
  }

  function openTerminal(sessionId, title) {
    if (typeof window.Terminal !== "function") {
      console.error("xterm.js not loaded");
      return;
    }
    closeTerminal();
    $("term-title").textContent = `terminal · ${title}`;
    $("term-status").textContent = "connecting…";
    $("term-overlay").hidden = false;

    const term = new window.Terminal({
      cursorBlink: true,
      fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
      fontSize: 13,
      theme: { background: "#0b0f17", foreground: "#dbe4f0" },
    });
    const FitAddon = window.FitAddon && window.FitAddon.FitAddon;
    const fit = FitAddon ? new FitAddon() : null;
    if (fit) term.loadAddon(fit);
    term.open($("term-body"));
    if (fit) fit.fit();

    // EventSource-style query token: the WS upgrade cannot send a header, so the
    // server honors ?token= on this route only (scoped, same as SSE).
    const proto = window.location.protocol === "https:" ? "wss" : "ws";
    const base = `${proto}://${window.location.host}/ws/session/${encodeURIComponent(sessionId)}`;
    const wsUrl = token ? `${base}?token=${encodeURIComponent(token)}` : base;
    const ws = new WebSocket(wsUrl);
    ws.binaryType = "arraybuffer";

    const sendResize = () => {
      if (fit) fit.fit();
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
      }
    };

    ws.onopen = () => {
      $("term-status").textContent = "live";
      sendResize();
    };
    ws.onmessage = (ev) => {
      if (typeof ev.data === "string") {
        try {
          const msg = JSON.parse(ev.data);
          if (msg.type === "error") {
            $("term-status").textContent = `error: ${msg.code}`;
            term.writeln(`\r\n\x1b[31m[${msg.code}] ${msg.message}\x1b[0m`);
          } else if (msg.type === "status" && msg.event === "attached") {
            $("term-status").textContent = `attached · ${msg.session}`;
          }
        } catch (_) {
          /* ignore */
        }
        return;
      }
      // Binary PTY bytes → straight into the emulator.
      term.write(new Uint8Array(ev.data));
    };
    ws.onclose = () => {
      $("term-status").textContent = "closed";
    };
    ws.onerror = () => {
      $("term-status").textContent = "connection error";
    };

    term.onData((data) => {
      if (ws.readyState === WebSocket.OPEN) {
        ws.send(JSON.stringify({ type: "input", data }));
      }
    });

    const resizeObs = new ResizeObserver(() => sendResize());
    resizeObs.observe($("term-body"));

    termSession = { ws, term, fit, resizeObs };
    term.focus();
  }

  $("term-close").addEventListener("click", closeTerminal);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && termSession) closeTerminal();
  });

  // ── Web-push (VAPID) + service worker + PWA ───────────────────────────
  let swReg = null;
  let pushEndpoint = null;

  function urlBase64ToUint8Array(base64String) {
    const padding = "=".repeat((4 - (base64String.length % 4)) % 4);
    const base64 = (base64String + padding).replace(/-/g, "+").replace(/_/g, "/");
    const raw = atob(base64);
    return Uint8Array.from([...raw].map((c) => c.charCodeAt(0)));
  }

  async function initPush() {
    if (!("serviceWorker" in navigator)) return;
    try {
      swReg = await navigator.serviceWorker.register("/sw.js");
    } catch (e) {
      console.warn("service worker registration failed", e);
      return;
    }
    if (!("PushManager" in window)) return;

    // Is push even configured server-side?
    let cfg;
    try {
      const res = await fetch("/api/push/config", { headers: authHeaders() });
      if (!res.ok) return;
      cfg = await res.json();
    } catch (_) {
      return;
    }
    if (!cfg.enabled || !cfg.vapidPublicKey) return;

    const btn = $("push-toggle");
    btn.hidden = false;

    const existing = await swReg.pushManager.getSubscription();
    if (existing) {
      pushEndpoint = existing.endpoint;
      markPushOn(true);
      reportPresence(); // tell the server our focus state
    }

    btn.addEventListener("click", () => togglePush(cfg.vapidPublicKey));
  }

  function markPushOn(on) {
    const btn = $("push-toggle");
    btn.classList.toggle("on", on);
    btn.querySelector(".push-label").textContent = on ? "notifying" : "notify";
  }

  async function togglePush(vapidPublicKey) {
    if (!swReg) return;
    const existing = await swReg.pushManager.getSubscription();
    if (existing) {
      await fetch("/api/push/unsubscribe", {
        method: "POST",
        headers: { "Content-Type": "application/json", ...authHeaders() },
        body: JSON.stringify({ endpoint: existing.endpoint }),
      }).catch(() => {});
      await existing.unsubscribe().catch(() => {});
      pushEndpoint = null;
      markPushOn(false);
      return;
    }
    const perm = await Notification.requestPermission();
    if (perm !== "granted") return;
    const sub = await swReg.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey: urlBase64ToUint8Array(vapidPublicKey),
    });
    const json = sub.toJSON();
    const res = await fetch("/api/push/subscribe", {
      method: "POST",
      headers: { "Content-Type": "application/json", ...authHeaders() },
      body: JSON.stringify({ endpoint: json.endpoint, keys: json.keys }),
    });
    if (res.ok) {
      pushEndpoint = json.endpoint;
      markPushOn(true);
      reportPresence();
    }
  }

  function reportPresence() {
    if (!pushEndpoint) return;
    fetch("/api/push/presence", {
      method: "POST",
      headers: { "Content-Type": "application/json", ...authHeaders() },
      body: JSON.stringify({ endpoint: pushEndpoint, focused: !document.hidden }),
    }).catch(() => {});
  }

  document.addEventListener("visibilitychange", reportPresence);

  boot();
})();
