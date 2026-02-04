---
name: debug-bridge
description: Browser automation and inspection for AI agents via WebSocket
triggers:
  - debug the app
  - test this flow
  - click the button
  - take a screenshot
  - inspect the UI
  - capture network requests
  - monitor navigation
type: cli-tool
protocol_version: 1
default_port: 4000
capabilities:
  - ui_tree
  - dom_snapshot
  - console
  - errors
  - network
  - navigation
  - screenshot
  - eval
---

# Debug Bridge Runbook

Control web apps via WebSocket. Click, type, screenshot, inspect, capture network traffic, monitor navigation.

## Prerequisites (Webapp Setup)

**The webapp MUST have the debug-bridge SDK installed and configured before agents can control it.**

### Step 1: Install SDK in Webapp

```bash
npm install debug-bridge-browser
```

### Step 2: Add Initialization Code

Create `src/debug-bridge.ts` (or add to your app's entry point):

```typescript
import { createDebugBridge } from 'debug-bridge-browser';

// ONLY runs in development - safe to leave in production builds
if (import.meta.env.DEV) {
  const params = new URLSearchParams(window.location.search);
  const session = params.get('session');
  const port = params.get('port') || '4000';

  if (session) {
    const bridge = createDebugBridge({
      url: `ws://localhost:${port}/debug?role=app&sessionId=${session}`,
      sessionId: session,
      appName: 'My App',  // Shows in CLI when connected
      appVersion: '1.0.0',

      // Feature toggles (all true by default)
      enableNetwork: true,      // Capture fetch/XHR requests
      enableNavigation: true,   // Track route changes
      enableConsole: true,      // Capture console.log/error
      enableErrors: true,       // Capture unhandled errors

      // Optional: filter which URLs to capture
      networkUrlFilter: (url) => !url.includes('/analytics'),
      maxNetworkBodySize: 10000,  // Truncate large request/response bodies
    });

    bridge.connect();

    // Optional: expose for manual debugging
    (window as any).__debugBridge = bridge;
  }
}
```

### Step 3: Import in App Entry

```typescript
// main.tsx or App.tsx
import './debug-bridge';  // Add this import
```

### How It Works

1. Webapp reads `?session=X&port=Y` from URL
2. If present, connects to debug server as `role=app`
3. Agent connects to same server as `role=agent`
4. Agent sends commands → Server relays → Webapp executes → Results returned

```
┌─────────────┐     WebSocket      ┌─────────────┐     WebSocket      ┌─────────────┐
│   AI Agent  │ ◄─────────────────► │  CLI Server │ ◄─────────────────►│   Webapp    │
│             │   role=agent       │  (port 4000) │   role=app         │  (browser)  │
└─────────────┘                    └─────────────┘                    └─────────────┘
```

---

## Agent Usage

**Once the webapp has the SDK configured, agents can control it.**

### Quick Start

```bash
# 1. Start debug server (in tmux for persistence)
SESSION="debug-$(date +%s)"
PORT=$(shuf -i 4000-4999 -n 1)
tmux new-session -d -s "$SESSION"
tmux send-keys -t "$SESSION" "npx debug-bridge-cli connect --session $SESSION --port $PORT 2>&1 | tee debug-bridge-$PORT.log" C-m

# 2. Open webapp with debug params (webapp must have SDK installed!)
open "http://localhost:5173?session=$SESSION&port=$PORT"

# 3. Attach to tmux to use CLI
tmux attach -t "$SESSION"
```

### CLI Commands

| Command | Example | Description |
|---------|---------|-------------|
| `ui` | `ui` | List interactive elements |
| `click <target>` | `click 3` or `click "Sign In"` | Click element |
| `type <target> <text>` | `type 1 "hello"` or `type "email" "a@b.com"` | Type into input |
| `js <code>` | `js document.title` | Run JavaScript, shows result |
| `screenshot` | `screenshot` | Save viewport as PNG |
| `state` | `state` | Get cookies, localStorage |
| `go <url>` | `go /login` | Navigate to URL |
| `find <query>` | `find email` | Search UI tree |
| `eval <code>` | `eval localStorage.clear()` | Alias for `js` |
| `help` | `help` | Show all available commands |

### AI Agent Usage (via tmux)

**AI agents can't attach to terminals interactively.** Use `tmux send-keys` to send commands and `tmux capture-pane` to read output:

```bash
# Send a command to the debug-bridge CLI
tmux send-keys -t "$SESSION" "screenshot" C-m

# Wait for result and read output
sleep 2
tmux capture-pane -t "$SESSION" -p | tail -20

# Run JavaScript in the browser
tmux send-keys -t "$SESSION" "js document.title" C-m
sleep 1
tmux capture-pane -t "$SESSION" -p | tail -5

# Trigger events (e.g., for testing visibility handlers)
tmux send-keys -t "$SESSION" 'eval window.dispatchEvent(new Event("focus"))' C-m
```

**Key Pattern for AI Agents:**
1. Start CLI in tmux (not foreground)
2. Send commands via `tmux send-keys`
3. Read results via `tmux capture-pane`
4. Screenshots are saved as PNG files in current directory

### Targeting Elements

Elements can be targeted by:
- **Index**: `click 3` (element #3 from `ui` output)
- **Text**: `click "Submit"` (matches button/link text)
- **Placeholder**: `type "email" "test@example.com"`
- **StableId**: `click btn-656b07` (hash shown in `ui` output)

### WebSocket API (Programmatic)

```javascript
// Connect as agent
const ws = new WebSocket(`ws://localhost:4000/debug?role=agent&sessionId=my-session`);

// Base message (required fields)
const msg = {
  protocolVersion: 1,
  sessionId: 'my-session',
  timestamp: Date.now(),
  requestId: crypto.randomUUID()
};

// Send commands
ws.send(JSON.stringify({ ...msg, type: 'request_ui_tree' }));
ws.send(JSON.stringify({ ...msg, type: 'click', target: { stableId: 'btn-abc' } }));
ws.send(JSON.stringify({ ...msg, type: 'type', target: { selector: '#email' }, text: 'test@example.com' }));
ws.send(JSON.stringify({ ...msg, type: 'evaluate', code: 'document.title' }));
ws.send(JSON.stringify({ ...msg, type: 'request_screenshot' }));
ws.send(JSON.stringify({ ...msg, type: 'navigate', url: '/dashboard' }));
```

### Response Types

```javascript
// UI Tree response
{ type: 'ui_tree', items: [{ stableId, role, text, label, visible, meta }] }

// Command success
{ type: 'command_result', success: true, result: any, duration: 5 }

// Screenshot
{ type: 'screenshot', data: 'base64...', width: 1920, height: 1080 }

// Error
{ type: 'command_result', success: false, error: { code: 'TARGET_NOT_FOUND', message: '...' } }

// Network request (auto-streamed)
{ type: 'network_request', requestId: 'net-1-1234', method: 'POST', url: '/api/users', initiator: 'fetch' }

// Network response (auto-streamed)
{ type: 'network_response', requestId: 'net-1-1234', status: 200, statusText: 'OK', duration: 45, ok: true, body: '{"id":1}' }

// Navigation event (auto-streamed)
{ type: 'navigation', url: '/dashboard', previousUrl: '/login', trigger: 'pushstate' }

// Console message (auto-streamed)
{ type: 'console', level: 'error', args: ['Failed to fetch user', '{"status":401}'] }
```

---

## Example Workflows

### Login Flow
```
ui                           # Discover form elements
type "email" "user@test.com" # Fill email field
type "password" "secret123"  # Fill password field
click "Sign In"              # Click submit button
screenshot                   # Capture result for verification
```

### Form Testing
```
go /register                 # Navigate to page
ui                           # List interactive elements
type 1 "John"                # Fill first input by index
type 2 "john@test.com"       # Fill second input
click "Submit"               # Submit form
state                        # Check localStorage for saved data
```

### Debugging
```
ui                           # See current page state
js localStorage.getItem('token')  # Check auth token
js window.__REDUX_STATE__    # Inspect app state
screenshot                   # Capture for analysis
```

### API Debugging (Network Capture)
```
# Network requests are auto-captured - watch the CLI output:
# 🌐 [POST] /api/login
#    ✓ 200 OK (45ms)
# 🌐 [GET] /api/users/me
#    ✗ 401 Unauthorized (12ms)

# Navigation is also tracked:
# 🔀 [pushstate] /dashboard
# 🔀 [popstate] /login
```

### Monitoring API Calls
```javascript
// Connect as agent and filter for network events
ws.onmessage = (e) => {
  const msg = JSON.parse(e.data);
  if (msg.type === 'network_response' && !msg.ok) {
    console.log(`API Error: ${msg.status} on request ${msg.requestId}`);
  }
  if (msg.type === 'navigation') {
    console.log(`User navigated to: ${msg.url}`);
  }
};
```

---

## Error Recovery

| Error | Cause | Fix |
|-------|-------|-----|
| `TARGET_NOT_FOUND` | Element not in DOM | Run `ui` to refresh, verify element exists |
| `TARGET_NOT_VISIBLE` | Element off-screen | `scroll 0 500` first, then retry |
| `EVAL_DISABLED` | App disabled eval | Use DOM commands instead |
| `SCREENSHOT_FAILED` | Modern CSS (oklch) | Use `js` to inspect DOM directly |
| No connection | Webapp missing SDK | Verify SDK is installed and initialized |
| Missing network events | SDK config | Check `enableNetwork: true` in config |
| Missing navigation events | SDK config | Check `enableNavigation: true` in config |

## Telemetry Reference

| Telemetry | Auto-sent | Description |
|-----------|-----------|-------------|
| `ui_tree` | On connect + changes | Interactive elements list |
| `dom_mutations` | Continuous | DOM changes (batched) |
| `console` | Continuous | Console.log/warn/error |
| `error` | On error | Unhandled errors |
| `network_request` | On fetch/XHR | Outgoing API calls |
| `network_response` | On response | API responses with status/body |
| `navigation` | On route change | URL changes (push/pop/replace/hash) |
| `state_update` | On change | Custom app state (if configured) |

## Troubleshooting

```bash
# Port already in use
lsof -ti:4000 | xargs kill -9

# Check if server is running
tmux attach -t debug-*

# View server logs
tail -f debug-bridge-*.log

# Webapp not connecting?
# 1. Check URL has ?session=X&port=Y
# 2. Check browser console for [DebugBridge] logs
# 3. Verify SDK is imported in dev mode
```
