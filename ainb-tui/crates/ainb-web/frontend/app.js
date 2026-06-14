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
      row.appendChild(badges);

      host.appendChild(row);
    }
  }

  // Fleet needs (`ainb fleet needs --format json`) is an array of NeedsRow.
  // Each row has a flattened { kind: "ASK|ERR|WAIT|IDLE", context: {...} }
  // plus a nested `session` with `cwd` etc.
  function needKind(row) {
    return (row.kind || row.context_kind || "").toUpperCase() || "IDLE";
  }

  function needTitle(row) {
    const s = row.session || {};
    return s.workspace_name || s.summary || s.cwd || s.tmux_session || "session";
  }

  function needDetail(row) {
    const ctx = row.context || {};
    // NeedsContext is internally-tagged; the payload differs per kind. Surface
    // the most useful human string we can find without coupling to one shape.
    return (
      ctx.question ||
      ctx.message ||
      ctx.marker ||
      ctx.detail ||
      row.enriched ||
      ""
    );
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
      const body = el("div", "need-body");
      body.appendChild(el("div", "need-title", needTitle(row)));
      const detail = needDetail(row);
      if (detail) body.appendChild(el("div", "need-detail", detail));
      card.appendChild(body);
      host.appendChild(card);
    }
  }

  // Cost (`ainb fleet cost --format json`) — shape is owned by Goal 1 and may
  // be absent (null) in this build. Render whatever rollup numbers we find.
  function fmtUsd(n) {
    if (typeof n !== "number" || !isFinite(n)) return "–";
    return `$${n.toFixed(2)}`;
  }

  function renderCost(cost) {
    const host = $("cost");
    host.replaceChildren();

    if (cost == null) {
      $("stat-cost").textContent = "–";
      $("cost-meta").textContent = "unavailable";
      host.appendChild(emptyState("◷", "Cost surface not available in this build."));
      return;
    }

    // Try common rollup shapes without hard-coding one schema.
    const total =
      cost.total_usd ?? cost.month_usd ?? cost.today_usd ?? cost.cost_usd ?? null;
    $("stat-cost").textContent = total != null ? fmtUsd(total) : "–";
    $("cost-meta").textContent = "";

    const cells = [];
    const push = (key, val) => {
      if (val != null) cells.push([key, fmtUsd(val)]);
    };
    push("today", cost.today_usd);
    push("week", cost.week_usd);
    push("month", cost.month_usd);
    push("projected", cost.projected_usd);
    push("total", cost.total_usd);

    if (!cells.length) {
      // Fall back to dumping numeric top-level fields.
      for (const [k, v] of Object.entries(cost)) {
        if (typeof v === "number") cells.push([k, fmtUsd(v)]);
      }
    }

    if (!cells.length) {
      host.appendChild(emptyState("◷", "No cost data recorded yet."));
      return;
    }

    const grid = el("div", "cost-grid");
    for (const [k, v] of cells) {
      const cell = el("div", "cost-cell");
      cell.appendChild(el("div", "cost-val", v));
      cell.appendChild(el("div", "cost-key", k));
      grid.appendChild(cell);
    }
    host.appendChild(grid);
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

  function startSSE() {
    // EventSource cannot set headers; pass the token via query string. The
    // server accepts it only on this route and we never persist it in the URL.
    const sseUrl = token
      ? `/api/events?token=${encodeURIComponent(token)}`
      : "/api/events";
    const es = new EventSource(sseUrl);

    es.addEventListener("snapshot", (e) => {
      setConn("live");
      try {
        render(JSON.parse(e.data));
      } catch (_) {
        /* ignore malformed frame */
      }
    });
    es.onopen = () => setConn("live");
    es.onerror = () => {
      setConn("down");
      // EventSource auto-reconnects; nothing else to do.
    };
  }

  async function boot() {
    setConn("connecting");
    try {
      render(await loadOnce());
      startSSE();
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

  boot();
})();
